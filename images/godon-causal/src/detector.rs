use crate::graph::ChannelId;
use crate::trial_reader::{mad, median, ProbeTrials, ProbeTrial, ReceiverTrial};
use serde::Serialize;

// ─── Detection Result ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct DetectionResult {
    pub sender_id: String,
    pub receiver_id: String,
    pub channel: ChannelId,
    pub detected: bool,
    pub confidence: f64,
    pub rounds_detected: usize,
    pub rounds_total: usize,
    pub rising_edge: f64,
    pub falling_edge: f64,
    pub baseline_median: f64,
    pub push_median: f64,
    pub pause_median: f64,
    pub baseline_mad: f64,
    pub n_push_samples: usize,
    pub n_pause_samples: usize,
    pub n_baseline_samples: usize,
    pub method: String,
}

// ─── Edge Detector Trait ────────────────────────────────────────────

pub trait EdgeDetector: Send + Sync {
    fn detect(&self, sender: &ProbeTrials, receiver: &ProbeTrials) -> Vec<DetectionResult>;
    fn name(&self) -> &str;
    fn params(&self) -> serde_json::Value;
}

// ─── CFAR Block-Step Detector ───────────────────────────────────────
//
// Ported from observer's detect_watermark_coupling (optuna_reader.rs).
// Method: per-round block-step detection with dynamic CFAR threshold.
//
// 1. Group sender push trials into rounds by temporal gaps
// 2. Per round, per channel: extract baseline (hold_calib), push, pause
//    windows from receiver trials, aligned by timestamp + propagation lag
// 3. Dynamic CFAR: k = N_ref * (Pfa^(-1/N_ref) - 1), threshold = k * MAD
// 4. Rising edge (push - baseline) AND falling edge (push - pause) must
//    both exceed threshold (reversibility check)
// 5. Majority vote across rounds

pub struct CfarDetector {
    pub propagation_lag: f64,
    pub min_ref_cells: usize,
    pub min_test_cells: usize,
    pub detection_confidence: f64,
}

impl Default for CfarDetector {
    fn default() -> Self {
        Self {
            propagation_lag: 20.0,
            min_ref_cells: 3,
            min_test_cells: 3,
            detection_confidence: 0.95,
        }
    }
}

impl CfarDetector {
    pub fn new(confidence: f64) -> Self {
        Self {
            detection_confidence: confidence,
            ..Default::default()
        }
    }
}

impl EdgeDetector for CfarDetector {
    fn name(&self) -> &str {
        "cfar_block_step"
    }

    fn params(&self) -> serde_json::Value {
        serde_json::json!({
            "propagation_lag": self.propagation_lag,
            "min_ref_cells": self.min_ref_cells,
            "min_test_cells": self.min_test_cells,
            "detection_confidence": self.detection_confidence,
        })
    }

    fn detect(&self, sender: &ProbeTrials, receiver: &ProbeTrials) -> Vec<DetectionResult> {
        if sender.push_trials.is_empty() {
            return Vec::new();
        }

        // 1. Build rounds from sender push/pause timestamps
        let rounds = build_rounds(sender);
        if rounds.is_empty() {
            return Vec::new();
        }

        // 2. Determine number of channels
        let n_obj = receiver.receiver_hold_trials.iter()
            .map(|rt| rt.values.len())
            .max()
            .unwrap_or(0);

        let n_obs = receiver.receiver_hold_trials.iter()
            .map(|rt| rt.observations.len())
            .max()
            .unwrap_or(0);

        if n_obj == 0 && n_obs == 0 {
            return Vec::new();
        }

        let pfa = 1.0 - self.detection_confidence;
        let mut results = Vec::new();

        // 3. Per-channel detection
        for obj_idx in 0..n_obj {
            let result = self.detect_channel(
                sender, receiver, &rounds, pfa,
                ChannelId::Objective(obj_idx), obj_idx, true,
            );
            results.push(result);
        }

        for obs_idx in 0..n_obs {
            let channel = ChannelId::Observation(format!("obs_{}", obs_idx));
            let result = self.detect_channel(
                sender, receiver, &rounds, pfa,
                channel, obs_idx, false,
            );
            results.push(result);
        }

        results
    }
}

impl CfarDetector {
    fn detect_channel(
        &self,
        _sender: &ProbeTrials,
        receiver: &ProbeTrials,
        rounds: &[Round],
        pfa: f64,
        channel: ChannelId,
        idx: usize,
        is_objective: bool,
    ) -> DetectionResult {
        let mut round_detected = 0usize;
        let mut rising_edges: Vec<f64> = Vec::new();
        let mut falling_edges: Vec<f64> = Vec::new();
        let mut last_baseline_median = 0.0;
        let mut last_push_median = 0.0;
        let mut last_pause_median = 0.0;
        let mut last_baseline_mad = 0.0;
        let mut total_push_samples = 0usize;
        let mut total_pause_samples = 0usize;
        let mut total_baseline_samples = 0usize;

        for round in rounds {
            // Baseline: receiver hold_calib trials before this round's push
            let baseline_vals: Vec<f64> = receiver.receiver_hold_trials.iter()
                .filter(|rt| rt.phase == "hold_calib")
                .filter(|rt| rt.timestamp < round.push_start - 10.0)
                .filter_map(|rt| extract_channel(rt, idx, is_objective))
                .collect();

            // Push window
            let push_vals: Vec<f64> = receiver.receiver_hold_trials.iter()
                .filter(|rt| rt.timestamp >= round.push_start - 10.0
                        && rt.timestamp <= round.push_end + self.propagation_lag)
                .filter_map(|rt| extract_channel(rt, idx, is_objective))
                .collect();

            // Pause window
            let pause_vals: Vec<f64> = receiver.receiver_hold_trials.iter()
                .filter(|rt| rt.timestamp >= round.pause_start - 10.0
                        && rt.timestamp <= round.pause_end + self.propagation_lag + 30.0)
                .filter_map(|rt| extract_channel(rt, idx, is_objective))
                .collect();

            if baseline_vals.len() < self.min_ref_cells
                || push_vals.len() < self.min_test_cells
                || pause_vals.len() < self.min_test_cells
            {
                continue;
            }

            let baseline_median = median(&baseline_vals);
            let push_median = median(&push_vals);
            let pause_median = median(&pause_vals);
            let baseline_mad = mad(&baseline_vals).max(0.001);

            // Dynamic CFAR threshold
            let n_ref = baseline_vals.len() as f64;
            let k = n_ref * (pfa.powf(-1.0 / n_ref) - 1.0);
            let threshold = k * baseline_mad;

            let rising_edge = push_median - baseline_median;
            let falling_edge = push_median - pause_median;

            let rising_exceeds = rising_edge.abs() >= threshold;
            let falling_exceeds = falling_edge.abs() >= threshold;

            if rising_exceeds && falling_exceeds {
                round_detected += 1;
                rising_edges.push(rising_edge);
                falling_edges.push(falling_edge);
            }

            last_baseline_median = baseline_median;
            last_push_median = push_median;
            last_pause_median = pause_median;
            last_baseline_mad = baseline_mad;
            total_push_samples += push_vals.len();
            total_pause_samples += pause_vals.len();
            total_baseline_samples += baseline_vals.len();
        }

        let n_rounds = rounds.len();
        let majority = n_rounds / 2 + 1;
        let detected = round_detected >= majority;

        let avg_rising = if rising_edges.is_empty() { 0.0 } else {
            rising_edges.iter().sum::<f64>() / rising_edges.len() as f64
        };
        let avg_falling = if falling_edges.is_empty() { 0.0 } else {
            falling_edges.iter().sum::<f64>() / falling_edges.len() as f64
        };

        let confidence = if n_rounds > 0 {
            round_detected as f64 / n_rounds as f64
        } else {
            0.0
        };

        DetectionResult {
            sender_id: _sender.breeder_id.clone(),
            receiver_id: receiver.breeder_id.clone(),
            channel,
            detected,
            confidence,
            rounds_detected: round_detected,
            rounds_total: n_rounds,
            rising_edge: avg_rising,
            falling_edge: avg_falling,
            baseline_median: last_baseline_median,
            push_median: last_push_median,
            pause_median: last_pause_median,
            baseline_mad: last_baseline_mad,
            n_push_samples: total_push_samples,
            n_pause_samples: total_pause_samples,
            n_baseline_samples: total_baseline_samples,
            method: "cfar_block_step".to_string(),
        }
    }
}

// ─── Round structure ────────────────────────────────────────────────

#[derive(Clone)]
struct Round {
    push_start: f64,
    push_end: f64,
    pause_start: f64,
    pause_end: f64,
}

fn build_rounds(sender: &ProbeTrials) -> Vec<Round> {
    let push_ts: Vec<f64> = sender.push_trials.iter()
        .map(|t| t.timestamp)
        .collect();

    if push_ts.is_empty() {
        return Vec::new();
    }

    // Determine round boundaries by temporal gaps
    let mut round_starts: Vec<usize> = vec![0];
    if push_ts.len() > 2 {
        let mut intervals: Vec<f64> = Vec::new();
        for i in 1..push_ts.len() {
            intervals.push(push_ts[i] - push_ts[i - 1]);
        }
        intervals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median_interval = intervals[intervals.len() / 2];

        for i in 1..push_ts.len() {
            let gap = push_ts[i] - push_ts[i - 1];
            if gap > median_interval * 2.0 && median_interval > 0.0 {
                round_starts.push(i);
            }
        }
    }

    let mut rounds = Vec::new();

    for (ri, &start_idx) in round_starts.iter().enumerate() {
        let end_idx = if ri + 1 < round_starts.len() {
            round_starts[ri + 1]
        } else {
            push_ts.len()
        };

        let round_push = &push_ts[start_idx..end_idx];
        if round_push.is_empty() {
            continue;
        }
        let push_start = round_push[0];
        let push_end = round_push[round_push.len() - 1];

        // Find pause trials after this push block
        let next_push_start = if ri + 1 < round_starts.len() {
            push_ts[round_starts[ri + 1]]
        } else {
            f64::MAX
        };

        let pause_ts: Vec<f64> = sender.pause_trials.iter()
            .filter(|t| t.timestamp > push_end - 10.0 && t.timestamp < next_push_start)
            .map(|t| t.timestamp)
            .collect();

        let (pause_start, pause_end) = if pause_ts.is_empty() {
            (push_end, push_end)
        } else {
            (pause_ts[0], pause_ts[pause_ts.len() - 1])
        };

        rounds.push(Round {
            push_start,
            push_end,
            pause_start,
            pause_end,
        });
    }

    rounds
}

fn extract_channel(rt: &ReceiverTrial, idx: usize, is_objective: bool) -> Option<f64> {
    if is_objective {
        rt.values.get(idx).copied()
    } else {
        rt.observations.get(idx).copied()
    }
}
