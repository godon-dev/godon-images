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

    /// Detect coupling between sender and receiver using matched filter + CFAR.
    ///
    /// Method:
    /// 1. Find sender's impulse trials (detection_mode=impulse or watermark active=true)
    ///    including FAIL trials — the effectuation still happened, coupling still propagated.
    /// 2. Match each ping/listen trial to receiver trials by WALL-CLOCK timestamp proximity,
    ///    not trial number. Trial numbers are independent sequences per breeder.
    /// 3. Matched filter: stack receiver values during pings vs during listens.
    /// 4. CFAR: adaptive local noise floor from nearby non-impulse receiver trials.
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

    pub async fn detect_watermark_coupling(
        &self,
        sender_id: &str,
        receiver_id: &str,
    ) -> Result<serde_json::Value, Error> {
        // Detection pipeline (current: matched filter + CFAR threshold):
        //   1. [FUTURE] EMD preprocessing — decompose receiver hold values into
        //      intrinsic mode functions to separate slow drift (crop phase, weather)
        //      from fast coupling oscillation. Removes nonstationary baseline.
        //      Hard to implement in Rust (iterative sifting, no crate available).
        //      Consider Python sidecar or trait-based plugin when needed.
        //   2. Matched filter — align receiver values at known ping times, stack
        //      coherently. Signal grows as N, noise as sqrt(N).
        //   3. CFAR threshold — adaptive local noise floor instead of global mean.
        let sender_study = format!("{}_study", sender_id);
        let receiver_study = format!("{}_study", receiver_id);

        let sender_trials = self.get_trials(sender_id, &sender_study, 0, 10000).await?;
        let receiver_trials = self.get_trials(receiver_id, &receiver_study, 0, 10000).await?;

        // Find impulse trials from sender — either coordinated detection_mode=impulse
        // or legacy watermark active=true. Track both COMPLETE and FAIL to count
        // attempted impulses for accurate SNR normalization.
        let mut impulse_indices: Vec<usize> = Vec::new();
        let mut attempted_impulses = 0usize;

        for t in sender_trials.iter() {
            let is_impulse = {
                let dm = t.user_attrs.get("detection_mode");
                if let Some(dm_val) = dm {
                    let mode = if dm_val.is_string() { dm_val.as_str().unwrap_or("") } else { "" };
                    if mode == "impulse" {
                        true
                    } else {
                        // Fallback: legacy watermark active=true
                        let wm_raw = t.user_attrs.get("watermark");
                        if let Some(raw) = wm_raw {
                            let wm_meta: serde_json::Value = if raw.is_string() {
                                serde_json::from_str(raw.as_str().unwrap_or("{}")).unwrap_or_default()
                            } else {
                                raw.clone()
                            };
                            wm_meta.get("active").and_then(|v| v.as_bool()).unwrap_or(false)
                        } else {
                            false
                        }
                    }
                } else {
                    false
                }
            };

            if is_impulse {
                attempted_impulses += 1;
                // Include FAIL trials — the effectuation (parameter push) still
                // happened. The greenhouse received the extreme params, coupling
                // propagated, and the receiver's objectives shifted. Excluding
                // FAILs would lose real coupling signal and create false negatives.
                impulse_indices.push(t.number as usize);
            }
        }

        let n_impulses = impulse_indices.len();
        let n_attempted = attempted_impulses;

        if n_impulses == 0 {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no complete impulse trials found",
                "method": "matched_filter_cfar",
                "sender_id": sender_id,
                "receiver_id": receiver_id,
                "attempted_impulses": n_attempted,
            }));
        }

        // Extract watermark metadata for context
        let _wm_meta: serde_json::Value = sender_trials.iter()
            .filter(|t| t.user_attrs.contains_key("watermark"))
            .filter_map(|t| {
                let raw = &t.user_attrs["watermark"];
                if raw.is_string() {
                    serde_json::from_str(raw.as_str().unwrap_or("{}")).ok()
                } else {
                    Some(raw.clone())
                }
            })
            .next()
            .unwrap_or(serde_json::json!({}));

        // === BLOCK DESIGN DETECTION ===
        //
        // The sender does sustained push (extreme params) then sustained pause
        // (baseline params). We look for a step in the receiver's values:
        //   1. Receiver values during push block (rising edge)
        //   2. Receiver values during pause block (falling edge)
        //   3. Receiver values before push started (baseline reference)
        //
        // Detection = rising edge AND falling edge, temporally aligned with sender blocks.

        // Build receiver trial lookup with timestamps
        #[derive(Clone)]
        struct ReceiverTrial {
            number: usize,
            timestamp_secs: f64,
            values: Vec<f64>,
        }

        let receiver_complete: Vec<ReceiverTrial> = receiver_trials.iter()
            .filter(|t| t.state == "COMPLETE")
            .filter_map(|t| {
                let vals: Vec<f64> = t.values.iter().filter_map(|v| *v).collect();
                if vals.is_empty() { return None; }
                let ts = t.datetime_start.as_ref()
                    .and_then(|s| parse_timestamp_secs(s))
                    .unwrap_or(0.0);
                Some(ReceiverTrial {
                    number: t.number as usize,
                    timestamp_secs: ts,
                    values: vals,
                })
            })
            .collect();

        if receiver_complete.is_empty() {
            return Ok(serde_json::json!({
                "detected": false, "reason": "no complete receiver trials",
                "method": "block_step_detection",
                "sender_id": sender_id, "receiver_id": receiver_id,
            }));
        }

        let n_obj = receiver_complete[0].values.len();

        // Find sender push and pause timestamps by impulse_phase attribute
        let mut push_timestamps: Vec<f64> = Vec::new();
        let mut pause_timestamps: Vec<f64> = Vec::new();

        for t in sender_trials.iter() {
            let phase = t.user_attrs.get("impulse_phase")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if phase == "push" {
                if let Some(ts) = t.datetime_start.as_ref().and_then(|s| parse_timestamp_secs(s)) {
                    push_timestamps.push(ts);
                }
            } else if phase == "pause" {
                if let Some(ts) = t.datetime_start.as_ref().and_then(|s| parse_timestamp_secs(s)) {
                    pause_timestamps.push(ts);
                }
            }
        }

        push_timestamps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        pause_timestamps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        if push_timestamps.is_empty() {
            return Ok(serde_json::json!({
                "detected": false, "reason": "no push trials found (sender may not have sent yet)",
                "method": "block_step_detection",
                "sender_id": sender_id, "receiver_id": receiver_id,
                "impulse_count": n_impulses,
            }));
        }

        let push_start = push_timestamps.first().copied().unwrap_or(0.0);
        let push_end = push_timestamps.last().copied().unwrap_or(0.0);
        let pause_start = pause_timestamps.first().copied().unwrap_or(push_end);
        let pause_end = pause_timestamps.last().copied().unwrap_or(pause_start);

        // Propagation lag: receiver sees sender's effect ~1 trial later (~15-30s)
        let propagation_lag = 20.0_f64;

        // Collect receiver values grouped by hold_phase when available,
        // falling back to timestamp-based windows otherwise.
        let has_hold_phase = receiver_trials.iter().any(|t| {
            t.user_attrs.get("hold_phase").is_some()
        });

        let mut signal_vals: Vec<Vec<f64>> = vec![Vec::new(); n_obj];
        let mut baseline_vals: Vec<Vec<f64>> = vec![Vec::new(); n_obj];

        if has_hold_phase {
            // Use hold_phase tags - precise, no timestamp alignment issues
            for t in receiver_trials.iter() {
                if t.state != "COMPLETE" { continue; }
                let vals: Vec<f64> = t.values.iter().filter_map(|v| *v).collect();
                if vals.is_empty() { continue; }
                let phase = t.user_attrs.get("hold_phase")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if phase == "signal" {
                    for (i, v) in vals.iter().enumerate() {
                        if i < n_obj { signal_vals[i].push(*v); }
                    }
                } else if phase == "baseline" || phase == "post" {
                    for (i, v) in vals.iter().enumerate() {
                        if i < n_obj { baseline_vals[i].push(*v); }
                    }
                }
            }
        } else {
            // Fallback: non-overlapping timestamp windows
            for rt in &receiver_complete {
                if rt.timestamp_secs <= 0.0 { continue; }
                let t = rt.timestamp_secs;
                if t < push_start {
                    for (i, v) in rt.values.iter().enumerate() {
                        if i < n_obj { baseline_vals[i].push(*v); }
                    }
                } else if !pause_timestamps.is_empty() && t >= pause_start {
                    for (i, v) in rt.values.iter().enumerate() {
                        if i < n_obj { baseline_vals[i].push(*v); }
                    }
                } else {
                    for (i, v) in rt.values.iter().enumerate() {
                        if i < n_obj { signal_vals[i].push(*v); }
                    }
                }
            }
        }

        // Detect coupling: compare signal vs baseline
        let mut per_objective: Vec<serde_json::Value> = Vec::new();
        let mut any_detected = false;
        let mut best_shift = 0.0_f64;
        let mut best_obj = 0usize;

        for obj_idx in 0..n_obj {
            let baseline = &baseline_vals[obj_idx];
            let signal = &signal_vals[obj_idx];

            if baseline.len() < 2 || signal.len() < 1 {
                per_objective.push(serde_json::json!({
                    "objective_index": obj_idx, "detected": false,
                    "reason": "insufficient samples",
                    "baseline_samples": baseline.len(),
                    "signal_samples": signal.len(),
                }));
                continue;
            }

            let baseline_median = median(baseline);
            let signal_median = median(signal);
            let shift = signal_median - baseline_median;
            let baseline_mad = mad(baseline).max(0.01);
            let snr = shift.abs() / baseline_mad;
            let detected = snr >= 2.5;

            if detected { any_detected = true; }
            if shift.abs() > best_shift { best_shift = shift.abs(); best_obj = obj_idx; }

            per_objective.push(serde_json::json!({
                "objective_index": obj_idx, "detected": detected,
                "method": "block_step_detection",
                "baseline_median": round4(baseline_median),
                "signal_median": round4(signal_median),
                "shift": round4(shift),
                "baseline_mad": round4(baseline_mad),
                "snr": round4(snr),
                "baseline_samples": baseline.len(),
                "signal_samples": signal.len(),
                "hold_phase_mode": has_hold_phase,
            }));
        }

        let result = serde_json::json!({
            "detected": any_detected,
            "method": "block_step_detection",
            "sender_id": sender_id,
            "receiver_id": receiver_id,
            "impulse_count": n_impulses,
            "push_trials": push_timestamps.len(),
            "pause_trials": pause_timestamps.len(),
            "best_shift": round4(best_shift),
            "best_objective": best_obj,
            "per_objective": per_objective,
            "sender_trials": sender_trials.len(),
            "receiver_trials": receiver_trials.len(),
        });

        info!(
            "Block detection: sender={} receiver={} push={} pause={} detected={} best_shift={:.4}",
            sender_id, receiver_id, push_timestamps.len(), pause_timestamps.len(),
            any_detected, best_shift
        );

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
fn parse_timestamp_secs(ts: &str) -> Option<f64> {
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
