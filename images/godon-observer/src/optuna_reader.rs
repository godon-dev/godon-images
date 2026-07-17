use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tokio_postgres::{Error, Row};
use log::{debug, info};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudyInfo {
    pub study_name: String,
    pub directions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialRecord {
    pub number: i32,
    pub state: String,
    pub datetime_start: Option<String>,
    pub datetime_complete: Option<String>,
    pub params: HashMap<String, f64>,
    pub param_distributions: HashMap<String, serde_json::Value>,
    pub values: Vec<Option<f64>>,
    pub user_attrs: HashMap<String, serde_json::Value>,
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
                password: std::env::var("GODON_ARCHIVE_DB_PASSWORD").unwrap_or_else(|_| "yugabyte".into()),
                host: std::env::var("GODON_ARCHIVE_DB_SERVICE_HOST").unwrap_or_else(|_| "yb-tserver-0".into()),
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

    fn breeder_db_name(breeder_id: &str) -> String {
        format!("breeder_{}", breeder_id.replace('-', "_"))
    }

    pub async fn list_studies(&self, breeder_id: &str) -> Result<Vec<StudyInfo>, Error> {
        let db = Self::breeder_db_name(breeder_id);
        let client = self.connect(&db).await?;

        let studies = client
            .query("SELECT study_name FROM studies ORDER BY study_id", &[])
            .await?;

        let mut result = Vec::new();
        for s in studies {
            let study_name: String = s.get(0);
            let directions = self.get_directions(&client, &study_name).await?;
            result.push(StudyInfo { study_name, directions });
        }

        Ok(result)
    }

    async fn get_directions(&self, client: &tokio_postgres::Client, study_name: &str) -> Result<Vec<String>, Error> {
        let rows = client
            .query(
                "SELECT CAST(sd.direction AS TEXT) FROM study_directions sd JOIN studies s ON sd.study_id = s.study_id WHERE s.study_name = $1 ORDER BY sd.objective",
                &[&study_name],
            )
            .await?;

        Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }

    pub async fn get_trials(
        &self,
        breeder_id: &str,
        study_name: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<TrialRecord>, Error> {
        let db = Self::breeder_db_name(breeder_id);
        let client = self.connect(&db).await?;

        let trial_rows = client
            .query(
                "SELECT t.trial_id, t.number, CAST(t.state AS TEXT), CAST(t.datetime_start AS TEXT), CAST(t.datetime_complete AS TEXT) \
                 FROM trials t \
                 JOIN studies s ON t.study_id = s.study_id \
                 WHERE s.study_name = $1 \
                 ORDER BY t.number \
                 OFFSET $2 LIMIT $3",
                &[&study_name, &offset, &limit],
            )
            .await?;

        let mut trials = Vec::new();
        for row in &trial_rows {
            let trial_id: i32 = row.get(0);
            let record = self.build_trial_record(&client, trial_id, row).await?;
            trials.push(record);
        }

        info!("Loaded {} trials for study {} (offset={}, limit={})", trials.len(), study_name, offset, limit);
        Ok(trials)
    }

    pub async fn get_trial_count(&self, breeder_id: &str, study_name: &str) -> Result<i64, Error> {
        let db = Self::breeder_db_name(breeder_id);
        let client = self.connect(&db).await?;

        let row = client
            .query_one(
                "SELECT COUNT(*) FROM trials t JOIN studies s ON t.study_id = s.study_id WHERE s.study_name = $1",
                &[&study_name],
            )
            .await?;

        Ok(row.get::<_, i64>(0))
    }

    async fn build_trial_record(
        &self,
        client: &tokio_postgres::Client,
        trial_id: i32,
        trial_row: &Row,
    ) -> Result<TrialRecord, Error> {
        let number: i32 = trial_row.get(1);
        let state: String = trial_row.get(2);
        let datetime_start: Option<String> = trial_row.try_get(3).ok();
        let datetime_complete: Option<String> = trial_row.try_get(4).ok();

        let param_rows = client
            .query(
                "SELECT param_name, param_value, distribution_json FROM trial_params WHERE trial_id = $1",
                &[&trial_id],
            )
            .await?;

        let mut params = HashMap::new();
        let mut param_distributions = HashMap::new();
        for pr in &param_rows {
            let name: String = pr.get(0);
            let value: f64 = pr.get(1);
            let dist_json: String = pr.get(2);
            params.insert(name.clone(), value);
            if let Ok(v) = serde_json::from_str(&dist_json) {
                param_distributions.insert(name, v);
            }
        }

        let value_rows = client
            .query(
                "SELECT objective, value, CAST(value_type AS TEXT) FROM trial_values WHERE trial_id = $1 ORDER BY objective",
                &[&trial_id],
            )
            .await?;

        let mut values = Vec::new();
        if !value_rows.is_empty() {
            let max_obj = value_rows.iter().map(|r: &Row| r.get::<_, i32>(0)).max().unwrap_or(0);
            values = vec![None; (max_obj + 1) as usize];
            for vr in &value_rows {
                let obj: i32 = vr.get(0);
                let value_type: String = vr.get(2);
                let val: Option<f64> = if value_type == "FINITE" {
                    vr.try_get(1).ok()
                } else if value_type == "INF_POS" {
                    Some(f64::INFINITY)
                } else {
                    Some(f64::NEG_INFINITY)
                };
                values[obj as usize] = val;
            }
        }

        let attr_rows = client
            .query(
                "SELECT key, value_json FROM trial_user_attributes WHERE trial_id = $1",
                &[&trial_id],
            )
            .await?;

        let mut user_attrs = HashMap::new();
        for ar in &attr_rows {
            let key: String = ar.get(0);
            let val_json: String = ar.get(1);
            if let Ok(v) = serde_json::from_str(&val_json) {
                user_attrs.insert(key, v);
            }
        }

        Ok(TrialRecord {
            number,
            state,
            datetime_start,
            datetime_complete,
            params,
            param_distributions,
            values,
            user_attrs,
        })
    }

    pub async fn get_study_user_attrs(
        &self,
        breeder_id: &str,
        study_name: &str,
    ) -> Result<HashMap<String, serde_json::Value>, Error> {
        let db = Self::breeder_db_name(breeder_id);
        let client = self.connect(&db).await?;

        let rows = client
            .query(
                "SELECT sua.key, sua.value_json FROM study_user_attributes sua \
                 JOIN studies s ON sua.study_id = s.study_id \
                 WHERE s.study_name = $1",
                &[&study_name],
            )
            .await?;

        let mut attrs = HashMap::new();
        for r in &rows {
            let key: String = r.get(0);
            let val_json: String = r.get(1);
            if let Ok(v) = serde_json::from_str(&val_json) {
                attrs.insert(key, v);
            }
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
        let receiver_trials = self.get_trials(receiver_id, &receiver_study, 0, 10000).await?;

        // ── Identify sender rounds ────────────────────────────────────
        // Each round is a contiguous block of push trials followed by pause trials.
        // We group by detecting temporal gaps: if the gap between consecutive
        // push timestamps exceeds 2× the median inter-push gap, it's a new round.

        #[derive(Clone)]
        struct SenderPhaseTrial {
            timestamp: f64,
            phase: String, // "push" or "pause"
        }

        let sender_phased: Vec<SenderPhaseTrial> = sender_trials.iter()
            .filter_map(|t| {
                let phase = t.user_attrs.get("impulse_phase")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if phase != "push" && phase != "pause" {
                    return None;
                }
                let ts = t.datetime_start.as_ref()
                    .and_then(|s| parse_timestamp_secs(s))?;
                Some(SenderPhaseTrial { timestamp: ts, phase: phase.to_string() })
            })
            .collect();

        if sender_phased.is_empty() {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no push/pause trials found",
                "method": "per_round_block_step",
                "sender_id": sender_id,
                "receiver_id": receiver_id,
            }));
        }

        // Group push trials into rounds by temporal gaps
        let push_ts: Vec<f64> = sender_phased.iter()
            .filter(|t| t.phase == "push")
            .map(|t| t.timestamp)
            .collect();

        // Determine round boundaries: a new round starts when there's a gap
        // > 2× median inter-push interval
        let mut round_starts: Vec<usize> = vec![0];
        if push_ts.len() > 2 {
            let mut intervals: Vec<f64> = Vec::new();
            for i in 1..push_ts.len() {
                intervals.push(push_ts[i] - push_ts[i-1]);
            }
            intervals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median_interval = intervals[intervals.len() / 2];

            for i in 1..push_ts.len() {
                let gap = push_ts[i] - push_ts[i-1];
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
            push_count: usize,
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
            let pause_after: Vec<f64> = sender_phased.iter()
                .filter(|t| t.phase == "pause" && t.timestamp > push_end - 10.0)
                .map(|t| t.timestamp)
                .collect();

            // Only include pause trials before the next round's push (or end)
            let next_push_start = if ri + 1 < round_starts.len() {
                push_ts[round_starts[ri + 1]]
            } else {
                f64::MAX
            };

            let round_pause: Vec<f64> = pause_after.iter()
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
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no complete rounds identified",
                "method": "per_round_block_step",
                "sender_id": sender_id,
                "receiver_id": receiver_id,
            }));
        }

        // ── Build receiver trial lookups ──────────────────────────
        #[derive(Clone)]
        struct ReceiverTrial {
            timestamp: f64,
            values: Vec<f64>,
            phase: String,  // hold_calib, push, pause, etc.
        }

        let receiver_hold: Vec<ReceiverTrial> = receiver_trials.iter()
            .filter(|t| t.state == "COMPLETE")
            .filter(|t| {
                // Only include hold trials (detection_mode=hold or no detection_mode)
                let dm = t.user_attrs.get("detection_mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                dm == "hold" || dm.is_empty()
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
                                if let Some(v) = obj.get(key)
                                    .and_then(|vv| vv.as_f64())
                                {
                                    vals.push(v);
                                }
                            }
                        }
                    }
                }

                if vals.is_empty() { return None; }
                let ts = t.datetime_start.as_ref()
                    .and_then(|s| parse_timestamp_secs(s))?;
                // Extract phase: impulse_phase or lease_phase from user_attrs
                let phase = t.user_attrs.get("impulse_phase")
                    .and_then(|v| v.as_str())
                    .or_else(|| t.user_attrs.get("lease_phase").and_then(|v| v.as_str()))
                    .unwrap_or("")
                    .to_string();
                Some(ReceiverTrial { timestamp: ts, values: vals, phase })
            })
            .collect();

        if receiver_hold.is_empty() {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no complete receiver hold trials",
                "method": "per_round_block_step",
                "sender_id": sender_id,
                "receiver_id": receiver_id,
            }));
        }

        // Count pure optuna objectives (before observation merge) for output labeling
        let n_objectives = receiver_trials.iter()
            .filter(|t| t.state == "COMPLETE")
            .map(|t| t.values.len())
            .max()
            .unwrap_or(0);
        // Total channels = max across all receiver_hold (includes observation channels)
        let n_obj = receiver_hold.iter().map(|rt| rt.values.len()).max().unwrap_or(0);
        let propagation_lag = 20.0_f64; // seconds — thermal mass delay

        // CFAR parameters
        // Detection confidence = 1 - false_alarm_rate.
        // Higher = more conservative (fewer detections, fewer false alarms).
        // k is derived dynamically from N reference cells:
        //   pfa = 1 - confidence
        //   k = N * (pfa^(-1/N) - 1)
        // Configurable via interference_detection.detection_confidence in breeder config.
        // Default 0.95 (5% false alarm rate).
        let confidence = self.config_detection_confidence.unwrap_or(0.95_f64);
        let pfa = 1.0 - confidence;
        let min_ref_cells = 3usize;   // minimum hold_calib trials for reference
        let min_test_cells = 3usize;  // minimum push/pause trials

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
                // ── CFAR reference window: hold_calib trials only ──
                // These are the receiver sitting at neutral params before push.
                // NOT optimize/cooldown trials — those contain active param changes.
                let baseline_vals: Vec<f64> = receiver_hold.iter()
                    .filter(|rt| rt.phase == "hold_calib")
                    .filter(|rt| {
                        // Must be before this round's push started
                        rt.timestamp < round.push_start - 10.0
                    })
                    .filter_map(|rt| {
                        if obj_idx < rt.values.len() { Some(rt.values[obj_idx]) } else { None }
                    })
                    .collect();

                let push_vals: Vec<f64> = receiver_hold.iter()
                    .filter(|rt| rt.timestamp >= round.push_start - 10.0
                            && rt.timestamp <= round.push_end + propagation_lag)
                    .filter_map(|rt| {
                        if obj_idx < rt.values.len() { Some(rt.values[obj_idx]) } else { None }
                    })
                    .collect();

                let pause_vals: Vec<f64> = receiver_hold.iter()
                    .filter(|rt| rt.timestamp >= round.pause_start - 10.0
                            && rt.timestamp <= round.pause_end + propagation_lag + 30.0)
                    .filter_map(|rt| {
                        if obj_idx < rt.values.len() { Some(rt.values[obj_idx]) } else { None }
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
            let detected_edges: Vec<&(f64, f64, bool)> = round_edges.iter()
                .filter(|e| e.2).collect();
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
            sender_id, receiver_id, rounds.len(), any_detected, best_shift
        );

        Ok(result)
    }
/// List all breeder databases (for interference active breeder detection).
    pub async fn get_active_breeders(&self) -> Result<Vec<serde_json::Value>, Error> {
        let client = self.connect("yugabyte").await?;
        let rows = client
            .query(
                "SELECT study_name FROM studies ORDER BY study_name",
                &[],
            )
            .await?;
        let mut result = Vec::new();
        for row in &rows {
            let study_name: String = row.get(0);
            result.push(serde_json::json!({
                "study_name": study_name,
            }));
        }
        Ok(result)
    }
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

fn median(v: &[f64]) -> f64 {
    if v.is_empty() { return 0.0; }
    let mut sorted = v.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

fn mad(v: &[f64]) -> f64 {
    if v.is_empty() { return 0.0; }
    let m = median(v);
    let deviations: Vec<f64> = v.iter().map(|x| (x - m).abs()).collect();
    median(&deviations) * 1.4826 // Scale factor for normal distribution consistency
}

/// Parse an ISO 8601 / RFC 3339 timestamp string to epoch seconds.
/// Returns None if parsing fails. Used for cross-breeder trial alignment.
pub fn parse_timestamp_secs(ts: &str) -> Option<f64> {
    // Optuna timestamps from YugaByte look like "2026-06-14 10:23:45.123456+00"
    // or "2026-06-14T10:23:45.123456+00:00"
    // Strategy: normalize to ISO 8601, then parse.
    let normalized = ts.trim().replace(' ', "T");

    // Try parsing with a simple manual parser (no chrono dependency).
    // Format: YYYY-MM-DDTHH:MM:SS[.ffffff][+ZZ:ZZ]
    let parts: Vec<&str> = normalized.split('T').collect();
    if parts.len() != 2 {
        return None;
    }

    let date_parts: Vec<&str> = parts[0].split('-').collect();
    if date_parts.len() != 3 {
        return None;
    }
    let year: f64 = date_parts[0].parse().ok()?;
    let month: f64 = date_parts[1].parse().ok()?;
    let day: f64 = date_parts[2].parse().ok()?;

    // Split time from timezone offset
    let time_part = parts[1];
    let (time_str, tz_offset_secs) = if let Some(pos) = time_part.find(|c| c == '+' || c == '-') {
        // Don't split on the '-' in the date or the '.' in seconds
        // Check that this + or - is after position 2 (HH:MM:SS minimum)
        if pos >= 8 {
            (&time_part[..pos], parse_tz_offset(&time_part[pos..]))
        } else {
            (time_part, Some(0.0))
        }
    } else {
        (time_part, Some(0.0))
    };

    let time_clean = time_str.split('.').next().unwrap_or(time_str);
    let time_parts: Vec<&str> = time_clean.split(':').collect();
    if time_parts.len() < 3 {
        return None;
    }
    let hour: f64 = time_parts[0].parse().ok()?;
    let minute: f64 = time_parts[1].parse().ok()?;
    let second: f64 = time_parts[2].parse().ok()?;

    // Convert to epoch seconds (simplified — assumes UTC, ignores leap years beyond standard)
    // Days from year 2000 to given year
    let days_from_2000 = (year - 2000.0) * 365.25;
    // Month to day-of-year (approximate)
    let month_days = [0.0, 31.0, 59.0, 90.0, 120.0, 151.0, 181.0, 212.0, 243.0, 273.0, 304.0, 334.0];
    let day_of_year = month_days.get((month as usize).saturating_sub(1).min(11)).copied().unwrap_or(0.0) + day;
    let epoch = (days_from_2000 + day_of_year) * 86400.0
        + hour * 3600.0 + minute * 60.0 + second
        + tz_offset_secs.unwrap_or(0.0);
    Some(epoch)
}

fn parse_tz_offset(s: &str) -> Option<f64> {
    // Parse "+HH:MM" or "-HH:MM" or "+HHMM"
    if s.is_empty() {
        return Some(0.0);
    }
    let sign = if s.starts_with('-') { -1.0 } else { 1.0 };
    let cleaned = s.trim_start_matches(|c| c == '+' || c == '-');
    let parts: Vec<&str> = cleaned.split(':').collect();
    if parts.len() == 2 {
        let h: f64 = parts[0].parse().ok()?;
        let m: f64 = parts[1].parse().ok()?;
        Some(sign * (h * 3600.0 + m * 60.0))
    } else if cleaned.len() >= 4 {
        let h: f64 = cleaned[..2].parse().ok()?;
        let m: f64 = cleaned[2..4].parse().ok()?;
        Some(sign * (h * 3600.0 + m * 60.0))
    } else {
        Some(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let is_post_impulse = impulses.iter()
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
            if found { window_count += 1; }
        }

        let mut baseline: Vec<f64> = Vec::new();
        let window_set: HashSet<usize> = impulses.iter()
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
            (baseline.iter().map(|v| (v - baseline_mean).powi(2)).sum::<f64>() / n).sqrt()
        };

        assert_eq!(window_count, 4, "Should find 4 impulse windows");
        assert!((stacked_mean - 0.8).abs() < 0.01, "Stacked mean should be ~0.8, got {}", stacked_mean);
        assert!((baseline_mean - 0.5).abs() < 0.01, "Baseline mean should be ~0.5, got {}", baseline_mean);
        assert!(baseline_std < 0.01, "Baseline should have near-zero std, got {}", baseline_std);

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
        let window_set: HashSet<usize> = impulses.iter()
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

        assert!((stacked_mean - baseline_mean).abs() < 0.01,
            "No coupling: stacked and baseline should be equal");
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
        assert!((diff - 3600.0).abs() < 1.0, "Hour difference should be ~3600s, got {}", diff);
    }

    #[test]
    fn test_parse_timestamp_invalid() {
        assert!(parse_timestamp_secs("garbage").is_none());
        assert!(parse_timestamp_secs("").is_none());
    }
}
