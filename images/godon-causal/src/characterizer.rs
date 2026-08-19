use crate::detector::DetectionResult;
use crate::graph::{CharacterizedEdge, ResponseFunction};
use crate::trial_reader::{median, ProbeTrials};

pub fn characterize(detection: &DetectionResult, sender: &ProbeTrials) -> CharacterizedEdge {
    let rising_edge = detection.rising_edge;
    let falling_edge = detection.falling_edge;

    let impulse_scale = sender
        .push_trials
        .first()
        .map(|t| t.impulse_scale)
        .unwrap_or(1.0);

    // Compute sender's actual objective shift from its own trial values.
    // This is the true coupling denominator: how much the sender MOVED,
    // not how hard it pushed its parameters.
    //
    // impulse_scale (param push magnitude) ≠ sender_delta (objective shift)
    // because the sender's base function translates params→objectives
    // nonlinearly. For composition we need the ratio of receiver shift to
    // sender shift, which is the true coupling coefficient and composes
    // multiplicatively along graph paths.
    let sender_push_median = sender_median(sender, "push", 0);
    let sender_pause_median = sender_median(sender, "pause", 0);
    let sender_delta = sender_push_median - sender_pause_median;

    let has_sender_data = !sender.push_trials.is_empty() && !sender.pause_trials.is_empty();
    let sensitivity = if has_sender_data && sender_delta.abs() > 1e-10 {
        rising_edge / sender_delta
    } else if impulse_scale > 0.0 {
        // Fallback: no sender values recorded, use impulse_scale
        rising_edge / impulse_scale
    } else {
        rising_edge
    };

    let recovery_fraction = if rising_edge.abs() > 1e-12 {
        (falling_edge / rising_edge).abs()
    } else {
        0.0
    };

    let response = ResponseFunction::StepResponse {
        sensitivity,
        baseline: detection.baseline_median,
        recovery_fraction,
    };

    CharacterizedEdge {
        sender_id: detection.sender_id.clone(),
        receiver_id: detection.receiver_id.clone(),
        channel: detection.channel.clone(),
        detected: detection.detected,
        confidence: detection.confidence,
        method: detection.method.clone(),

        response,
        noise_floor: detection.baseline_mad,
        impulse_scale,

        rising_edge,
        falling_edge,
        baseline_median: detection.baseline_median,
        push_median: detection.push_median,
        pause_median: detection.pause_median,

        n_push_samples: detection.n_push_samples,
        n_pause_samples: detection.n_pause_samples,
        n_baseline_samples: detection.n_baseline_samples,

        characterized_at: chrono::Utc::now().to_rfc3339(),
        rounds_total: detection.rounds_total,
    }
}

/// Compute the median of a specific objective channel from the sender's
/// own trials during a given phase.
fn sender_median(sender: &ProbeTrials, phase: &str, channel: usize) -> f64 {
    let trials: Vec<f64> = match phase {
        "push" => &sender.push_trials,
        "pause" => &sender.pause_trials,
        "hold_calib" => &sender.hold_calib_trials,
        _ => return 0.0,
    }
    .iter()
    .filter_map(|t| t.values.get(channel).copied())
    .collect();

    if trials.is_empty() {
        0.0
    } else {
        median(&trials)
    }
}

/// Build a complete graph from all detection results for all pairs.
pub fn build_edges(
    sender_id: &str,
    receiver_id: &str,
    sender: &ProbeTrials,
    detections: &[DetectionResult],
) -> Vec<CharacterizedEdge> {
    detections
        .iter()
        .map(|d| characterize(d, sender))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::ChannelId;
    use crate::trial_reader::ProbeTrial;
    use std::collections::HashMap;

    fn make_probe_trial(value: f64, scale: f64) -> ProbeTrial {
        ProbeTrial {
            timestamp: 1000.0,
            trial_number: 1,
            params: HashMap::new(),
            values: vec![value],
            observations: vec![],
            impulse_scale: scale,
        }
    }

    fn make_detection(rising_edge: f64) -> DetectionResult {
        DetectionResult {
            sender_id: "sender".to_string(),
            receiver_id: "receiver".to_string(),
            channel: ChannelId::Objective(0),
            detected: true,
            confidence: 1.0,
            rounds_detected: 2,
            rounds_total: 3,
            rising_edge,
            falling_edge: rising_edge * 0.9,
            baseline_median: 0.5,
            push_median: 0.5 + rising_edge,
            pause_median: 0.5,
            baseline_mad: 0.02,
            n_push_samples: 15,
            n_pause_samples: 15,
            n_baseline_samples: 15,
            method: "cfar_block_step".to_string(),
        }
    }

    #[test]
    fn test_sensitivity_uses_sender_delta_not_impulse_scale() {
        // Sender pushes: own objective goes from ~0.4 (pause) to ~0.8 (push)
        // Receiver rising_edge = 0.28
        // impulse_scale = 1.0
        //
        // OLD formula: 0.28 / 1.0 = 0.28 (wrong — bakes in base function)
        // NEW formula: 0.28 / (0.8 - 0.4) = 0.28 / 0.4 = 0.7 (true coefficient)
        let sender = ProbeTrials {
            breeder_id: "sender".to_string(),
            push_trials: vec![
                make_probe_trial(0.80, 1.0),
                make_probe_trial(0.82, 1.0),
                make_probe_trial(0.78, 1.0),
            ],
            pause_trials: vec![
                make_probe_trial(0.40, 1.0),
                make_probe_trial(0.42, 1.0),
                make_probe_trial(0.38, 1.0),
            ],
            hold_calib_trials: vec![],
            receiver_hold_trials: vec![],
        };

        let detection = make_detection(0.28);
        let edge = characterize(&detection, &sender);

        let sens = edge.response.predict_shift(1.0);

        // sender_push_median = 0.80, sender_pause_median = 0.40
        // sender_delta = 0.40
        // sensitivity = 0.28 / 0.40 = 0.7
        assert!(
            (sens - 0.7).abs() < 0.01,
            "sensitivity should be ~0.7 (true coefficient), got {}",
            sens
        );
    }

    #[test]
    fn test_sensitivity_fallback_to_impulse_scale() {
        // No sender pause trials → sender_delta = 0 → fallback to impulse_scale
        let sender = ProbeTrials {
            breeder_id: "sender".to_string(),
            push_trials: vec![make_probe_trial(0.8, 1.0)],
            pause_trials: vec![], // empty
            hold_calib_trials: vec![],
            receiver_hold_trials: vec![],
        };

        let detection = make_detection(0.3);
        let edge = characterize(&detection, &sender);

        let sens = edge.response.predict_shift(1.0);

        // Fallback: 0.3 / 1.0 = 0.3
        assert!(
            (sens - 0.3).abs() < 0.01,
            "fallback sensitivity should be ~0.3, got {}",
            sens
        );
    }
}
