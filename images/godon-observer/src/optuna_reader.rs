use ledger_reader::{
    record_from_row, TrialRecord as LedgerRecord, COUNT_TRIALS_SQL, FETCH_STATE_SQL,
    FETCH_TRIALS_PAGED_SQL,
};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio_postgres::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyInfo {
    pub study_name: String,
    pub directions: Vec<String>,
}

/// The API projection — byte-identical to the study-era payload
/// (param_distributions stays, empty as it has been since the
/// samplers died).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialRecord {
    pub number: i64,
    pub state: String,
    pub datetime_start: Option<String>,
    pub datetime_complete: Option<String>,
    pub params: HashMap<String, f64>,
    pub param_distributions: HashMap<String, serde_json::Value>,
    pub values: Vec<Option<f64>>,
    pub user_attrs: HashMap<String, serde_json::Value>,
}

impl From<LedgerRecord> for TrialRecord {
    fn from(r: LedgerRecord) -> Self {
        TrialRecord {
            number: r.number,
            state: r.state,
            datetime_start: r.datetime_start,
            datetime_complete: r.datetime_complete,
            params: r
                .params
                .iter()
                .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                .collect(),
            param_distributions: HashMap::new(),
            values: r.values,
            user_attrs: r.user_attrs,
        }
    }
}

pub struct OptunaReader {
    config: DbConfig,
    config_detection_confidence: Option<f64>,
}

#[derive(Debug, Clone)]
struct DbConfig {
    user: String,
    password: String,
    host: String,
    port: u16,
}

impl OptunaReader {
    pub fn from_env() -> Self {
        Self {
            config: DbConfig {
                user: std::env::var("GODON_ARCHIVE_DB_USER").unwrap_or_else(|_| "yugabyte".into()),
                password: std::env::var("GODON_ARCHIVE_DB_PASSWORD")
                    .unwrap_or_else(|_| "yugabyte".into()),
                host: std::env::var("GODON_ARCHIVE_DB_SERVICE_HOST")
                    .unwrap_or_else(|_| "yb-tserver-0".into()),
                port: std::env::var("GODON_ARCHIVE_DB_SERVICE_PORT")
                    .ok()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(5433),
            },
            config_detection_confidence: std::env::var("GODON_DETECTION_CONFIDENCE")
                .ok()
                .and_then(|v| v.parse().ok()),
        }
    }

    async fn connect(&self, dbname: &str) -> Result<tokio_postgres::Client, Error> {
        let (client, connection) = tokio_postgres::connect(
            &format!(
                "host={} port={} user={} password={} dbname={}",
                self.config.host, self.config.port, self.config.user, self.config.password, dbname
            ),
            tokio_postgres::NoTls,
        )
        .await?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                log::error!("connection error: {}", e);
            }
        });

        Ok(client)
    }

    fn systemtender_db_name(systemtender_id: &str) -> String {
        format!("systemtender_{}", systemtender_id.replace('-', "_"))
    }

    pub async fn list_studies(&self, systemtender_id: &str) -> Result<Vec<StudyInfo>, Error> {
        // One ledger per tender; the study name is a synthesized label,
        // directions died with the multi-objective era.
        Ok(vec![StudyInfo {
            study_name: format!("{}_study", systemtender_id),
            directions: Vec::new(),
        }])
    }

    pub async fn get_trials(
        &self,
        systemtender_id: &str,
        _study_name: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<TrialRecord>, Error> {
        let db = Self::systemtender_db_name(systemtender_id);
        let client = self.connect(&db).await?;

        let rows = client
            .query(FETCH_TRIALS_PAGED_SQL, &[&offset, &limit])
            .await?;
        let trials: Vec<TrialRecord> = rows
            .iter()
            .map(|r| TrialRecord::from(record_from_row(r)))
            .collect();

        info!(
            "Loaded {} trials (offset={}, limit={})",
            trials.len(),
            offset,
            limit
        );
        Ok(trials)
    }

    pub async fn get_trial_count(
        &self,
        systemtender_id: &str,
        _study_name: &str,
    ) -> Result<i64, Error> {
        let db = Self::systemtender_db_name(systemtender_id);
        let client = self.connect(&db).await?;
        let row = client.query_one(COUNT_TRIALS_SQL, &[]).await?;
        Ok(row.get::<_, i64>(0))
    }

    pub async fn get_study_user_attrs(
        &self,
        systemtender_id: &str,
        _study_name: &str,
    ) -> Result<HashMap<String, serde_json::Value>, Error> {
        let db = Self::systemtender_db_name(systemtender_id);
        let client = self.connect(&db).await?;

        let rows = client.query(FETCH_STATE_SQL, &[]).await?;
        let mut attrs = HashMap::new();
        for r in &rows {
            let key: String = r.get(0);
            let v: serde_json::Value = r.get(1);
            attrs.insert(key, v);
        }
        Ok(attrs)
    }

    pub async fn health_check(&self) -> bool {
        match self.connect("yugabyte").await {
            Ok(_) => true,
            Err(e) => {
                debug!("DB health check failed: {}", e);
                false
            }
        }
    }

    /// Detect coupling between sender and receiver using per-round block-step detection.
    ///
    /// Method:
    /// 1. Identify sender rounds — contiguous push+pause blocks from impulse_phase tags
    /// 2. For each round, split receiver hold trials by sender timestamps + propagation lag
    ///    into baseline / push / pause windows
    /// 3. Per round, compute rising edge (push - baseline) and falling edge (push - pause)
    /// 4. Both edges must exceed threshold (2.0 × MAD) — falling edge is never bypassed
    /// 5. Stack per-round results across rounds for combined confidence
    pub async fn detect_watermark_coupling(
        &self,
        sender_id: &str,
        receiver_id: &str,
    ) -> Result<serde_json::Value, Error> {
        let sender_study = format!("{}_study", sender_id);
        let receiver_study = format!("{}_study", receiver_id);

        let sender_trials = self.get_trials(sender_id, &sender_study, 0, 10000).await?;
        let receiver_trials = self
            .get_trials(receiver_id, &receiver_study, 0, 10000)
            .await?;

        Ok(coupling_verdict(
            &sender_trials,
            &receiver_trials,
            sender_id,
            receiver_id,
            self.config_detection_confidence,
        ))
    }

    /// List all systemtender databases (for interference active systemtender detection).
    pub async fn get_active_systemtenders(&self) -> Result<Vec<serde_json::Value>, Error> {
        let client = self.connect("yugabyte").await?;
        let rows = client
            .query(
                "SELECT datname FROM pg_database WHERE datname LIKE 'systemtender_%' ORDER BY datname",
                &[],
            )
            .await?;
        let mut result = Vec::new();
        for row in &rows {
            let dbname: String = row.get(0);
            if let Some(uuid) = dbname.strip_prefix("systemtender_") {
                let id = uuid.replace('_', "-");
                result.push(serde_json::json!({
                    "study_name": format!("{}_study", id),
                }));
            }
        }
        Ok(result)
    }
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut sorted = v.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

fn mad(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let m = median(v);
    let deviations: Vec<f64> = v.iter().map(|x| (x - m).abs()).collect();
    median(&deviations) * 1.4826 // Scale factor for normal distribution consistency
}

/// Parse an ISO 8601 / RFC 3339 timestamp string to epoch seconds.
/// Returns None if parsing fails. Used for cross-systemtender trial alignment.
pub fn parse_timestamp_secs(ts: &str) -> Option<f64> {
    let mut normalized = ts.trim().replace(' ', "T");
    // the estate's YugaByte stamps sometimes carry a two-digit offset
    // ("+00"); RFC 3339 wants "+00:00"
    if let Some(pos) = normalized.rfind(['+', '-']) {
        if pos >= 8 && normalized[pos..].len() == 3 {
            normalized.push_str(":00");
        }
    }
    // seconds are not optional in this door: "T10:23" is not a
    // trial stamp (jiff's civil parse would accept it)
    let time_half = normalized.split_once('T')?.1;
    if time_half.split(':').count() < 3 {
        return None;
    }
    let t = match normalized.parse::<jiff::Timestamp>() {
        Ok(t) => t,
        Err(_) => {
            // no offset in the stamp: civil time read as UTC
            let dt = normalized.parse::<jiff::civil::DateTime>().ok()?;
            dt.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp()
        }
    };
    Some(t.as_second() as f64 + t.subsec_nanosecond() as f64 / 1e9)
}
/// The detection core, pure: sender and receiver trials in, the
/// per-round CFAR verdict out. The DB path fetches and feeds it —
/// the arithmetic owns no connection. Exposed crate-internal for
/// the golden fixtures.
pub(crate) fn coupling_verdict(
    sender_trials: &[TrialRecord],
    receiver_trials: &[TrialRecord],
    sender_id: &str,
    receiver_id: &str,
    detection_confidence: Option<f64>,
) -> serde_json::Value {
    // ── Identify sender rounds ────────────────────────────────────
    // Each round is a contiguous block of push trials followed by pause trials.
    // We group by detecting temporal gaps: if the gap between consecutive
    // push timestamps exceeds 2× the median inter-push gap, it's a new round.

    #[derive(Clone)]
    struct SenderPhaseTrial {
        timestamp: f64,
        phase: String, // "push" or "pause"
    }

    let sender_phased: Vec<SenderPhaseTrial> = sender_trials
        .iter()
        .filter_map(|t| {
            let phase = t
                .user_attrs
                .get("impulse_phase")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if phase != "push" && phase != "pause" {
                return None;
            }
            let ts = t
                .datetime_start
                .as_ref()
                .and_then(|s| parse_timestamp_secs(s))?;
            Some(SenderPhaseTrial {
                timestamp: ts,
                phase: phase.to_string(),
            })
        })
        .collect();

    if sender_phased.is_empty() {
        return serde_json::json!({
            "detected": false,
            "reason": "no push/pause trials found",
            "method": "per_round_block_step",
            "sender_id": sender_id,
            "receiver_id": receiver_id,
        });
    }

    // Group push trials into rounds by temporal gaps
    let push_ts: Vec<f64> = sender_phased
        .iter()
        .filter(|t| t.phase == "push")
        .map(|t| t.timestamp)
        .collect();

    // Determine round boundaries: a new round starts when there's a gap
    // > 2× median inter-push interval
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

    // Build per-round push/pause timestamp ranges
    #[derive(Clone)]
    struct Round {
        push_start: f64,
        push_end: f64,
        pause_start: f64,
        pause_end: f64,
        #[allow(dead_code)]
        // kept: scaffolding counters — the reads ride the diagnostic debug tier
        push_count: usize,
        #[allow(dead_code)]
        pause_count: usize,
    }

    let mut rounds: Vec<Round> = Vec::new();

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

        // Find pause trials that follow this push block (within a reasonable window)
        let pause_after: Vec<f64> = sender_phased
            .iter()
            .filter(|t| t.phase == "pause" && t.timestamp > push_end - 10.0)
            .map(|t| t.timestamp)
            .collect();

        // Only include pause trials before the next round's push (or end)
        let next_push_start = if ri + 1 < round_starts.len() {
            push_ts[round_starts[ri + 1]]
        } else {
            f64::MAX
        };

        let round_pause: Vec<f64> = pause_after
            .iter()
            .filter(|&&ts| ts < next_push_start)
            .cloned()
            .collect();

        let (pause_start, pause_end) = if round_pause.is_empty() {
            (push_end, push_end)
        } else {
            (round_pause[0], round_pause[round_pause.len() - 1])
        };

        rounds.push(Round {
            push_start,
            push_end,
            pause_start,
            pause_end,
            push_count: round_push.len(),
            pause_count: round_pause.len(),
        });
    }

    if rounds.is_empty() {
        return serde_json::json!({
            "detected": false,
            "reason": "no complete rounds identified",
            "method": "per_round_block_step",
            "sender_id": sender_id,
            "receiver_id": receiver_id,
        });
    }

    // ── Build receiver trial lookups ──────────────────────────
    #[derive(Clone)]
    struct ReceiverTrial {
        timestamp: f64,
        values: Vec<f64>,
        phase: String, // hold_calib, push, pause, etc.
    }

    let receiver_hold: Vec<ReceiverTrial> = receiver_trials
        .iter()
        .filter(|t| t.state == "COMPLETE")
        .filter(|t| {
            // Only include RECEIVER hold trials — not the sender's own hold_calib.
            // Receiver has coord_state=hold; sender has coord_state=hold_calib.
            let dm = t
                .user_attrs
                .get("detection_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let cs = t
                .user_attrs
                .get("coord_state")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            (dm == "hold" || dm.is_empty()) && cs != "hold_calib"
        })
        .filter_map(|t| {
            // Start with objective values from trial.values
            let mut vals: Vec<f64> = t.values.iter().filter_map(|v| *v).collect();

            // Merge observation values from user_attrs.
            if let Some(g) = t.user_attrs.get("observations").and_then(|v| v.as_str()) {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(g) {
                    if let Some(obj) = parsed.as_object() {
                        let mut keys: Vec<&String> = obj.keys().collect();
                        keys.sort();
                        for key in keys {
                            if let Some(v) = obj.get(key).and_then(|vv| vv.as_f64()) {
                                vals.push(v);
                            }
                        }
                    }
                }
            }

            if vals.is_empty() {
                return None;
            }
            let ts = t
                .datetime_start
                .as_ref()
                .and_then(|s| parse_timestamp_secs(s))?;
            // Extract phase: impulse_phase or lease_phase from user_attrs
            let phase = t
                .user_attrs
                .get("impulse_phase")
                .and_then(|v| v.as_str())
                .or_else(|| t.user_attrs.get("lease_phase").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            Some(ReceiverTrial {
                timestamp: ts,
                values: vals,
                phase,
            })
        })
        .collect();

    if receiver_hold.is_empty() {
        return serde_json::json!({
            "detected": false,
            "reason": "no complete receiver hold trials",
            "method": "per_round_block_step",
            "sender_id": sender_id,
            "receiver_id": receiver_id,
        });
    }

    // Count pure optuna objectives (before observation merge) for output labeling
    let n_objectives = receiver_trials
        .iter()
        .filter(|t| t.state == "COMPLETE")
        .map(|t| t.values.len())
        .max()
        .unwrap_or(0);
    // Total channels = max across all receiver_hold (includes observation channels)
    let n_obj = receiver_hold
        .iter()
        .map(|rt| rt.values.len())
        .max()
        .unwrap_or(0);
    let propagation_lag = 20.0_f64; // seconds — thermal mass delay

    // CFAR parameters
    // Detection confidence = 1 - false_alarm_rate.
    // Higher = more conservative (fewer detections, fewer false alarms).
    // k is derived dynamically from N reference cells:
    //   pfa = 1 - confidence
    //   k = N * (pfa^(-1/N) - 1)
    // Configurable via interference_detection.detection_confidence in systemtender config.
    // Default 0.95 (5% false alarm rate).
    let confidence = detection_confidence.unwrap_or(0.95_f64);
    let pfa = 1.0 - confidence;
    let min_ref_cells = 3usize; // minimum hold_calib trials for reference
    let min_test_cells = 3usize; // minimum push/pause trials

    // ── Per-round detection (CFAR) ───────────────────────────────
    let mut per_round_results: Vec<serde_json::Value> = Vec::new();
    let mut per_objective: Vec<serde_json::Value> = Vec::new();
    let mut any_detected = false;
    let mut rounds_detected = 0usize;
    let mut best_shift = 0.0_f64;
    let mut best_obj = 0usize;

    for obj_idx in 0..n_obj {
        let mut round_edges: Vec<(f64, f64, bool)> = Vec::new();
        let mut obj_detected_rounds = 0usize;

        for (ri, round) in rounds.iter().enumerate() {
            // ── CFAR reference window: receiver hold trials that see
            // the sender in hold_calib phase. These are the receiver
            // sitting at neutral params — pure noise reference.
            // NOT the sender's own hold_calib (state=hold_calib) which
            // may include unstable early readings.
            let baseline_vals: Vec<f64> = receiver_hold
                .iter()
                .filter(|rt| rt.phase == "hold_calib")
                .filter(|rt| {
                    // Must be before this round's push started
                    rt.timestamp < round.push_start - 10.0
                })
                .filter_map(|rt| {
                    if obj_idx < rt.values.len() {
                        Some(rt.values[obj_idx])
                    } else {
                        None
                    }
                })
                .collect();

            let push_vals: Vec<f64> = receiver_hold
                .iter()
                .filter(|rt| {
                    rt.timestamp >= round.push_start - 10.0
                        && rt.timestamp <= round.push_end + propagation_lag
                })
                .filter_map(|rt| {
                    if obj_idx < rt.values.len() {
                        Some(rt.values[obj_idx])
                    } else {
                        None
                    }
                })
                .collect();

            let pause_vals: Vec<f64> = receiver_hold
                .iter()
                .filter(|rt| {
                    rt.timestamp >= round.pause_start - 10.0
                        && rt.timestamp <= round.pause_end + propagation_lag + 30.0
                })
                .filter_map(|rt| {
                    if obj_idx < rt.values.len() {
                        Some(rt.values[obj_idx])
                    } else {
                        None
                    }
                })
                .collect();

            // Need minimum samples
            if baseline_vals.len() < min_ref_cells
                || push_vals.len() < min_test_cells
                || pause_vals.len() < min_test_cells
            {
                round_edges.push((0.0, 0.0, false));
                per_round_results.push(serde_json::json!({
                    "round": ri,
                    "objective_index": obj_idx,
                    "detected": false,
                    "reason": "insufficient samples",
                    "ref_samples": baseline_vals.len(),
                    "push_samples": push_vals.len(),
                    "pause_samples": pause_vals.len(),
                }));
                continue;
            }

            let baseline_median = median(&baseline_vals);
            let push_median = median(&push_vals);
            let pause_median = median(&pause_vals);
            let baseline_mad = mad(&baseline_vals).max(0.001); // floor

            // ── Dynamic CFAR threshold ──
            // k = N * (Pfa^(-1/N) - 1) where N = reference cell count
            let n_ref = baseline_vals.len() as f64;
            let k = n_ref * (pfa.powf(-1.0 / n_ref) - 1.0);
            let threshold = k * baseline_mad;

            // Detection: push or pause median must be outside
            // baseline_median ± threshold (the CFAR band)
            let rising_edge = push_median - baseline_median;
            let falling_edge = push_median - pause_median;

            let rising_exceeds = rising_edge.abs() >= threshold;
            let falling_exceeds = falling_edge.abs() >= threshold;
            let detected = rising_exceeds && falling_exceeds;

            // Report SNR-style metrics for observability
            let rising_snr = rising_edge.abs() / baseline_mad;
            let falling_snr = falling_edge.abs() / baseline_mad;

            round_edges.push((rising_edge, falling_edge, detected));

            if detected {
                obj_detected_rounds += 1;
                if falling_edge.abs() > best_shift {
                    best_shift = falling_edge.abs();
                    best_obj = obj_idx;
                }
            }

            per_round_results.push(serde_json::json!({
                "round": ri,
                "objective_index": obj_idx,
                "detected": detected,
                "baseline_median": round4(baseline_median),
                "push_median": round4(push_median),
                "pause_median": round4(pause_median),
                "rising_edge": round4(rising_edge),
                "falling_edge": round4(falling_edge),
                "baseline_mad": round4(baseline_mad),
                "cfar_k": round4(k),
                "cfar_threshold": round4(threshold),
                "rising_snr": round4(rising_snr),
                "falling_snr": round4(falling_snr),
                "pfa": pfa,
                "ref_cells": baseline_vals.len(),
                "push_samples": push_vals.len(),
                "pause_samples": pause_vals.len(),
            }));
        }

        // Stack across rounds: majority must agree
        let n_rounds = rounds.len();
        let majority = n_rounds / 2 + 1;
        let obj_detected = obj_detected_rounds >= majority;

        if obj_detected {
            any_detected = true;
            rounds_detected = rounds_detected.max(obj_detected_rounds);
        }

        // Compute stacked edges (average across detected rounds)
        let detected_edges: Vec<&(f64, f64, bool)> = round_edges.iter().filter(|e| e.2).collect();
        let (avg_rising, avg_falling) = if !detected_edges.is_empty() {
            let n = detected_edges.len() as f64;
            let r: f64 = detected_edges.iter().map(|e| e.0).sum::<f64>() / n;
            let f: f64 = detected_edges.iter().map(|e| e.1).sum::<f64>() / n;
            (r, f)
        } else {
            (0.0, 0.0)
        };

        per_objective.push(serde_json::json!({
            "objective_index": obj_idx,
            "source": if obj_idx < n_objectives { "objective" } else { "observation" },
            "detected": obj_detected,
            "method": "cfar_per_round",
            "rounds_detected": obj_detected_rounds,
            "rounds_total": n_rounds,
            "stacked_rising_edge": round4(avg_rising),
            "stacked_falling_edge": round4(avg_falling),
            "pfa": pfa,
        }));
    }

    let result = serde_json::json!({
        "detected": any_detected,
        "method": "cfar_per_round",
        "sender_id": sender_id,
        "receiver_id": receiver_id,
        "rounds": rounds.len(),
        "rounds_with_detection": rounds_detected,
        "n_objectives": n_objectives,
        "n_channels": n_obj,
        "best_shift": round4(best_shift),
        "best_objective": best_obj,
        "per_objective": per_objective,
        "per_round_details": per_round_results,
        "sender_trials": sender_trials.len(),
        "receiver_trials": receiver_trials.len(),
    });

    info!(
        "Per-round detection: sender={} receiver={} rounds={} detected={} best_shift={:.4}",
        sender_id,
        receiver_id,
        rounds.len(),
        any_detected,
        best_shift
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_impulse_detection_basic() {
        // Simulate: sender impulses at trial 0, 50, 100, 150
        // Receiver shows elevated growth_rate in post-impulse window
        // This test validates the stacking logic conceptually
        let impulses = vec![0usize, 50, 100, 150];
        let window_size = 3;

        // Simulated receiver values: baseline ~0.5, post-impulse ~0.8
        let mut receiver_vals: HashMap<usize, Vec<f64>> = HashMap::new();
        for i in 0..200 {
            let is_post_impulse = impulses
                .iter()
                .any(|imp| i >= *imp && i < *imp + window_size);
            let val = if is_post_impulse { 0.8 } else { 0.5 };
            receiver_vals.insert(i, vec![val]);
        }

        // Collect post-impulse
        let mut post_impulse: Vec<f64> = Vec::new();
        let mut window_count = 0;
        for imp in &impulses {
            let mut found = false;
            for offset in 0..window_size {
                if let Some(vals) = receiver_vals.get(&(imp + offset)) {
                    post_impulse.push(vals[0]);
                    found = true;
                }
            }
            if found {
                window_count += 1;
            }
        }

        let mut baseline: Vec<f64> = Vec::new();
        let window_set: HashSet<usize> = impulses
            .iter()
            .flat_map(|imp| (*imp..*imp + window_size).collect::<Vec<usize>>())
            .collect();
        for i in 0..200 {
            if !window_set.contains(&i) {
                if let Some(vals) = receiver_vals.get(&i) {
                    baseline.push(vals[0]);
                }
            }
        }

        let stacked_mean = post_impulse.iter().sum::<f64>() / post_impulse.len() as f64;
        let baseline_mean = baseline.iter().sum::<f64>() / baseline.len() as f64;
        let baseline_std = {
            let n = baseline.len() as f64;
            (baseline
                .iter()
                .map(|v| (v - baseline_mean).powi(2))
                .sum::<f64>()
                / n)
                .sqrt()
        };

        assert_eq!(window_count, 4, "Should find 4 impulse windows");
        assert!(
            (stacked_mean - 0.8).abs() < 0.01,
            "Stacked mean should be ~0.8, got {}",
            stacked_mean
        );
        assert!(
            (baseline_mean - 0.5).abs() < 0.01,
            "Baseline mean should be ~0.5, got {}",
            baseline_mean
        );
        assert!(
            baseline_std < 0.01,
            "Baseline should have near-zero std, got {}",
            baseline_std
        );

        let snr = (stacked_mean - baseline_mean).abs() / baseline_std.max(1e-12);
        assert!(snr > 100.0, "SNR should be very high, got {}", snr);
    }

    #[test]
    fn test_no_coupling() {
        // No impulse signal — receiver is random noise throughout
        let impulses = vec![0usize, 50, 100, 150];
        let window_size = 3;

        let mut receiver_vals: HashMap<usize, Vec<f64>> = HashMap::new();
        for i in 0..200 {
            // All same distribution — no coupling
            receiver_vals.insert(i, vec![0.5]);
        }

        let mut post_impulse: Vec<f64> = Vec::new();
        for imp in &impulses {
            for offset in 0..window_size {
                if let Some(vals) = receiver_vals.get(&(imp + offset)) {
                    post_impulse.push(vals[0]);
                }
            }
        }

        let mut baseline: Vec<f64> = Vec::new();
        let window_set: HashSet<usize> = impulses
            .iter()
            .flat_map(|imp| (*imp..*imp + window_size).collect::<Vec<usize>>())
            .collect();
        for i in 0..200 {
            if !window_set.contains(&i) {
                if let Some(vals) = receiver_vals.get(&i) {
                    baseline.push(vals[0]);
                }
            }
        }

        let stacked_mean = post_impulse.iter().sum::<f64>() / post_impulse.len() as f64;
        let baseline_mean = baseline.iter().sum::<f64>() / baseline.len() as f64;

        assert!(
            (stacked_mean - baseline_mean).abs() < 0.01,
            "No coupling: stacked and baseline should be equal"
        );
    }

    #[test]
    fn test_parse_timestamp_basic() {
        // Basic ISO format
        let ts = parse_timestamp_secs("2026-06-14 10:23:45.123456+00").unwrap();
        assert!(ts > 0.0, "Timestamp should be positive epoch, got {}", ts);

        // T separator
        let ts2 = parse_timestamp_secs("2026-06-14T10:23:45+00:00").unwrap();
        assert!(ts2 > 0.0);

        // Two timestamps from same day, different times — should differ by ~3600s
        let ts3 = parse_timestamp_secs("2026-06-14 10:00:00+00").unwrap();
        let ts4 = parse_timestamp_secs("2026-06-14 11:00:00+00").unwrap();
        let diff = (ts4 - ts3).abs();
        assert!(
            (diff - 3600.0).abs() < 1.0,
            "Hour difference should be ~3600s, got {}",
            diff
        );
    }

    #[test]
    fn test_parse_timestamp_invalid() {
        assert!(parse_timestamp_secs("garbage").is_none());
        assert!(parse_timestamp_secs("").is_none());
    }

    // ── the parsers and stats: hand-computed, the numbers from the
    // declared formulas — never from running the code under test ──

    #[test]
    fn round4_rounds_to_four_decimals_half_away_from_zero() {
        assert_eq!(round4(0.123456), 0.1235);
        assert_eq!(round4(2.00004), 2.0);
        assert_eq!(round4(1234.56789), 1234.5679);
        assert_eq!(round4(-2.00005), -2.0001);
    }

    #[test]
    fn median_sorts_then_takes_the_middle() {
        assert_eq!(median(&[]), 0.0);
        assert_eq!(median(&[7.0]), 7.0);
        assert_eq!(median(&[1.0, 3.0, 2.0]), 2.0); // odd: the middle
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 2.5); // even: mean of middles
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
    }

    #[test]
    fn mad_scales_the_median_deviation() {
        assert_eq!(mad(&[]), 0.0);
        assert_eq!(mad(&[10.0, 10.0, 10.0]), 0.0); // no spread, no mad
                                                   // median 3, deviations [2,1,0,1,2], median deviation 1
        assert_eq!(mad(&[1.0, 2.0, 3.0, 4.0, 5.0]), 1.4826);
        // median 5, deviations [3,1,1,3], median deviation 2
        assert_eq!(mad(&[2.0, 4.0, 6.0, 8.0]), 2.9652);
    }

    #[test]
    fn timestamps_are_true_unix_epoch() {
        // values computed independently (python datetime, UTC):
        // datetime(2026,6,14,10,23,45) -> 1_781_432_625
        assert_eq!(
            parse_timestamp_secs("2026-06-14 10:23:45"),
            Some(1_781_432_625.0)
        );
        // microseconds ride along as the fraction
        let frac = parse_timestamp_secs("2026-06-14T10:23:45.123456+00:00").unwrap();
        assert!((frac - 1_781_432_625.123_456).abs() < 1e-6, "got {frac}");
        // the year's first second
        assert_eq!(
            parse_timestamp_secs("2026-01-01T00:00:00"),
            Some(1_767_225_600.0)
        );
        // the estate's compact two-digit offset ("+00") parses as UTC
        let compact = parse_timestamp_secs("2026-06-14 10:23:45.123456+00").unwrap();
        assert!(
            (compact - 1_781_432_625.123_456).abs() < 1e-6,
            "got {compact}"
        );
    }

    #[test]
    fn timestamp_offsets_convert_per_the_spec() {
        // RFC 3339: a +02:00 stamp is two hours EAST — its UTC instant
        // is 7200s EARLIER than the same wall time read bare.
        assert_eq!(
            parse_timestamp_secs("2026-06-14 10:23:45+02:00"),
            Some(1_781_425_425.0) // 1_781_432_625 - 7200
        );
        assert_eq!(
            parse_timestamp_secs("2026-06-14T10:23:45-03:00"),
            Some(1_781_443_425.0) // 1_781_432_625 + 10800
        );
    }

    #[test]
    fn malformed_timestamps_refuse_instead_of_guessing() {
        assert_eq!(parse_timestamp_secs("not-a-timestamp"), None);
        assert_eq!(parse_timestamp_secs("2026-06-14"), None); // no time half
        assert_eq!(parse_timestamp_secs("2026-06-14T10:23"), None); // no seconds
        assert_eq!(parse_timestamp_secs("2026-06-14T99:99:99"), None); // impossible clock
        assert_eq!(parse_timestamp_secs("xx-yy-zzT10:23:45"), None); // not numbers
        assert_eq!(parse_timestamp_secs(""), None);
    }

    // ── the detection core's golden: planted couplings, recorded verdicts ──
    //
    // coupling_verdict is pure now (the extraction ruling on #408) —
    // synthetic trials with PLANTED couplings drive it, the verdicts
    // are committed as the answer key, replayed at 1e-9 relative.
    // Timestamps ride the true-epoch parser through jiff round-trips.

    use std::collections::HashMap as GoldHashMap;

    fn iso(epoch: i64) -> String {
        jiff::Timestamp::new(epoch, 0).unwrap().to_string()
    }

    fn sender_trial(number: i64, epoch: i64, phase: &str) -> TrialRecord {
        TrialRecord {
            number,
            state: "COMPLETE".into(),
            datetime_start: Some(iso(epoch)),
            datetime_complete: None,
            params: GoldHashMap::new(),
            param_distributions: GoldHashMap::new(),
            values: vec![],
            user_attrs: [("impulse_phase".to_string(), serde_json::json!(phase))]
                .into_iter()
                .collect(),
        }
    }

    fn receiver_trial(number: i64, epoch: i64, value: f64, calib: bool) -> TrialRecord {
        let mut attrs = GoldHashMap::new();
        if calib {
            attrs.insert("impulse_phase".to_string(), serde_json::json!("hold_calib"));
        }
        TrialRecord {
            number,
            state: "COMPLETE".into(),
            datetime_start: Some(iso(epoch)),
            datetime_complete: None,
            params: GoldHashMap::new(),
            param_distributions: GoldHashMap::new(),
            values: vec![Some(value)],
            user_attrs: attrs,
        }
    }

    /// The planted scenario: two push/pause rounds on a 170s gap,
    /// the receiver stepping 10.0 (calib) -> 14.0 (round-1 push) ->
    /// 10.2 (recovered) -> 14.3 (round-2 push) -> 10.1 (rest).
    fn planted_sender() -> Vec<TrialRecord> {
        let b = 1_781_432_000i64;
        let mut trials = Vec::new();
        let mut n = 0i64;
        for t in [0, 10, 20, 30] {
            trials.push(sender_trial(
                {
                    n += 1;
                    n
                },
                b + t,
                "push",
            ));
        }
        for t in [40, 50] {
            trials.push(sender_trial(
                {
                    n += 1;
                    n
                },
                b + t,
                "pause",
            ));
        }
        for t in [200, 210, 220, 230] {
            trials.push(sender_trial(
                {
                    n += 1;
                    n
                },
                b + t,
                "push",
            ));
        }
        for t in [240, 250] {
            trials.push(sender_trial(
                {
                    n += 1;
                    n
                },
                b + t,
                "pause",
            ));
        }
        trials
    }

    fn planted_receiver() -> Vec<TrialRecord> {
        let b = 1_781_432_000i64;
        let mut trials = Vec::new();
        let mut n = 1000i64;
        // the reference wing: hold_calib-tagged, before round one
        for t in [-80, -70, -60, -50, -40, -30] {
            trials.push(receiver_trial(
                {
                    n += 1;
                    n
                },
                b + t,
                10.0,
                true,
            ));
        }
        // round-one push regime (the lag keeps 0..50 inside it)
        for t in [0, 10, 20, 30, 40, 50] {
            trials.push(receiver_trial(
                {
                    n += 1;
                    n
                },
                b + t,
                14.0,
                false,
            ));
        }
        // recovered between rounds
        for k in 6..=19 {
            trials.push(receiver_trial(
                {
                    n += 1;
                    n
                },
                b + k * 10,
                10.2,
                false,
            ));
        }
        // round-two push regime
        for t in [200, 210, 220, 230, 240, 250] {
            trials.push(receiver_trial(
                {
                    n += 1;
                    n
                },
                b + t,
                14.3,
                false,
            ));
        }
        // rest
        for k in 26..=32 {
            trials.push(receiver_trial(
                {
                    n += 1;
                    n
                },
                b + k * 10,
                10.1,
                false,
            ));
        }
        trials
    }

    fn flat_receiver() -> Vec<TrialRecord> {
        let b = 1_781_432_000i64;
        let mut trials = Vec::new();
        for (i, t) in (-80..=320).step_by(10).enumerate() {
            trials.push(receiver_trial(2000 + i as i64, b + t, 10.0, t < -10));
        }
        trials
    }

    fn replay(name: &str, sender: &[TrialRecord], receiver: &[TrialRecord]) -> serde_json::Value {
        coupling_verdict(sender, receiver, "sender-x", "receiver-y", None)
            .as_object_mut()
            .unwrap()
            .insert("_scenario".into(), serde_json::json!(name));
        coupling_verdict(sender, receiver, "sender-x", "receiver-y", None)
    }

    fn first_float_drift(
        path: &str,
        a: &serde_json::Value,
        b: &serde_json::Value,
    ) -> Option<String> {
        match (a, b) {
            (serde_json::Value::Number(x), serde_json::Value::Number(y)) => {
                let (fx, fy) = (x.as_f64()?, y.as_f64()?);
                let scale = fx.abs().max(fy.abs().max(1.0));
                if (fx - fy).abs() <= 1e-9 * scale {
                    None
                } else {
                    Some(format!("{path}: {fx} vs {fy}"))
                }
            }
            (serde_json::Value::Array(xs), serde_json::Value::Array(ys)) => {
                if xs.len() != ys.len() {
                    return Some(format!("{path}: len {} vs {}", xs.len(), ys.len()));
                }
                xs.iter()
                    .zip(ys.iter())
                    .enumerate()
                    .find_map(|(i, (x, y))| first_float_drift(&format!("{path}[{i}]"), x, y))
            }
            (serde_json::Value::Object(xm), serde_json::Value::Object(ym)) => {
                if xm.len() != ym.len() {
                    return Some(format!("{path}: keys {} vs {}", xm.len(), ym.len()));
                }
                xm.iter().find_map(|(k, x)| {
                    let y = ym.get(k)?;
                    first_float_drift(&format!("{path}.{k}"), x, y)
                })
            }
            _ => {
                if a == b {
                    None
                } else {
                    Some(format!("{path}: {a} vs {b}"))
                }
            }
        }
    }

    #[test]
    #[ignore = "writes the committed artifact — run deliberately, review the diff"]
    fn regenerate_the_coupling_golden() {
        let artifact = serde_json::json!({
            "note": "the detection core's golden — planted couplings, recorded verdicts",
            "scenarios": [
                replay("planted_two_rounds", &planted_sender(), &planted_receiver()),
                replay("flat_no_coupling", &planted_sender(), &flat_receiver()),
            ],
        });
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
        std::fs::create_dir_all(dir).expect("tests dir");
        let path = format!("{dir}/coupling_golden.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&artifact).expect("pretty"))
            .expect("write artifact");
        println!("wrote {path}");
    }

    #[test]
    fn the_coupling_golden_replays_within_tolerance() {
        let path = format!("{}/tests/coupling_golden.json", env!("CARGO_MANIFEST_DIR"));
        let raw = std::fs::read_to_string(&path).expect("committed artifact present");
        let artifact: serde_json::Value = serde_json::from_str(&raw).expect("artifact parses");
        let scenarios = artifact["scenarios"].as_array().expect("scenarios");
        assert_eq!(scenarios.len(), 2);

        let replayed = [
            replay("planted_two_rounds", &planted_sender(), &planted_receiver()),
            replay("flat_no_coupling", &planted_sender(), &flat_receiver()),
        ];
        for (committed, now) in scenarios.iter().zip(replayed.iter()) {
            if let Some(drift) = first_float_drift("coupling", now, committed) {
                panic!("golden drift at {drift}");
            }
        }

        // the hand-reasoned spine, asserted on top of the full replay:
        let planted = &replayed[0];
        assert_eq!(
            planted["detected"],
            serde_json::json!(true),
            "the planted coupling is seen"
        );
        let flat = &replayed[1];
        assert_eq!(
            flat["detected"],
            serde_json::json!(false),
            "the flat receiver is clean"
        );
    }

    #[test]
    fn the_core_refuses_with_named_reasons() {
        // no push/pause trials at all
        let bare = vec![sender_trial(1, 1_781_432_000, "other")];
        let v = coupling_verdict(&bare, &planted_receiver(), "s", "r", None);
        assert_eq!(v["reason"], serde_json::json!("no push/pause trials found"));

        // phases present but no push block ever forms
        let pauses = vec![sender_trial(1, 1_781_432_000, "pause")];
        let v = coupling_verdict(&pauses, &planted_receiver(), "s", "r", None);
        assert_eq!(
            v["reason"],
            serde_json::json!("no complete rounds identified")
        );

        // receiver never completes a hold trial
        let mut incomplete = planted_receiver();
        for t in incomplete.iter_mut() {
            t.state = "RUNNING".into();
        }
        let v = coupling_verdict(&planted_sender(), &incomplete, "s", "r", None);
        assert_eq!(
            v["reason"],
            serde_json::json!("no complete receiver hold trials")
        );
    }
}
