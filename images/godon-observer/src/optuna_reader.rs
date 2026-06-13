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

    /// Detect coupling between sender and receiver using impulse stacking.
    ///
    /// Method: seismological stack-and-threshold.
    /// 1. Find sender's impulse trials (watermark active=true)
    /// 2. For each impulse, extract receiver's objective values in a post-impulse window
    /// 3. Stack (average) all post-impulse windows — coherent signal sums, noise cancels
    /// 4. Compare stacked signal against baseline (non-impulse trials)
    /// 5. SNR = |stacked_mean - baseline_mean| / baseline_std
    /// 6. If SNR > threshold, coupling detected
    ///
    /// SNR improves as sqrt(N_impulses). With 4 impulses: 2x gain. With 20: 4.5x.
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
        let sender_study = format!("{}_study", sender_id);
        let receiver_study = format!("{}_study", receiver_id);

        let sender_trials = self.get_trials(sender_id, &sender_study, 0, 10000).await?;
        let receiver_trials = self.get_trials(receiver_id, &receiver_study, 0, 10000).await?;

        // Find impulse trials from sender — either coordinated detection_mode=impulse
        // or legacy watermark active=true
        let impulse_indices: Vec<usize> = sender_trials.iter()
            .filter(|t| t.state == "COMPLETE")
            .filter_map(|t| {
                // Check coordinated detection mode first
                let dm = t.user_attrs.get("detection_mode");
                if let Some(dm_val) = dm {
                    let mode = if dm_val.is_string() { dm_val.as_str().unwrap_or("") } else { "" };
                    if mode == "impulse" {
                        return Some(t.number as usize);
                    }
                }
                // Fallback: legacy watermark active=true
                let wm_raw = t.user_attrs.get("watermark")?;
                let wm_meta: serde_json::Value = if wm_raw.is_string() {
                    serde_json::from_str(wm_raw.as_str().unwrap_or("{}")).ok()?
                } else {
                    wm_raw.clone()
                };
                if wm_meta.get("active").and_then(|v| v.as_bool()).unwrap_or(false) {
                    Some(t.number as usize)
                } else {
                    None
                }
            })
            .collect();

        let n_impulses = impulse_indices.len();

        if n_impulses == 0 {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no impulse trials found",
                "method": "impulse_stacking",
                "sender_id": sender_id,
                "receiver_id": receiver_id,
            }));
        }

        // Extract watermark metadata for context
        let wm_meta: serde_json::Value = sender_trials.iter()
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

        // Post-impulse window: how many receiver trials after each impulse to include
        let window_size = 3usize;

        // Get receiver complete trials sorted by number, with their values
        let receiver_complete: Vec<(usize, Vec<f64>)> = receiver_trials.iter()
            .filter(|t| t.state == "COMPLETE")
            .filter_map(|t| {
                let vals: Vec<f64> = t.values.iter()
                    .filter_map(|v| *v)
                    .collect();
                if vals.is_empty() {
                    None
                } else {
                    Some((t.number as usize, vals))
                }
            })
            .collect();

        if receiver_complete.is_empty() {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no complete receiver trials",
                "method": "impulse_stacking",
                "sender_id": sender_id,
                "receiver_id": receiver_id,
            }));
        }

        let n_obj = receiver_complete[0].1.len();

        // Build a lookup: trial_number -> values for receiver
        let receiver_map: HashMap<usize, &Vec<f64>> = receiver_complete.iter()
            .map(|(num, vals)| (*num, vals))
            .collect();

        // Collect post-impulse windows
        // For each impulse at sender trial T, take receiver trials [T, T+window_size)
        let mut post_impulse_windows: Vec<Vec<f64>> = vec![Vec::new(); n_obj];
        let mut window_count = 0usize;

        for impulse_num in &impulse_indices {
            let mut found_in_window = false;
            for offset in 0..window_size {
                if let Some(vals) = receiver_map.get(&(impulse_num + offset)) {
                    for (obj_idx, v) in vals.iter().enumerate() {
                        if obj_idx < n_obj {
                            post_impulse_windows[obj_idx].push(*v);
                        }
                    }
                    found_in_window = true;
                }
            }
            if found_in_window {
                window_count += 1;
            }
        }

        if window_count == 0 {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no receiver trials found in post-impulse windows",
                "method": "impulse_stacking",
                "sender_id": sender_id,
                "receiver_id": receiver_id,
                "impulse_count": n_impulses,
            }));
        }

        // Baseline: all receiver trial values NOT in any post-impulse window
        let impulse_window_set: HashSet<usize> = impulse_indices.iter()
            .flat_map(|impulse_num| {
                (*impulse_num..*impulse_num + window_size).collect::<Vec<usize>>()
            })
            .collect();

        let mut baseline_values: Vec<Vec<f64>> = vec![Vec::new(); n_obj];
        for (num, vals) in &receiver_complete {
            if !impulse_window_set.contains(num) {
                for (obj_idx, v) in vals.iter().enumerate() {
                    if obj_idx < n_obj {
                        baseline_values[obj_idx].push(*v);
                    }
                }
            }
        }

        // Compute stacked signal and baseline statistics per objective
        let snr_threshold = 2.5_f64;
        let mut per_objective: Vec<serde_json::Value> = Vec::new();
        let mut any_detected = false;
        let mut best_snr = 0.0_f64;
        let mut best_obj = 0usize;

        for obj_idx in 0..n_obj {
            let stacked = &post_impulse_windows[obj_idx];
            let baseline = &baseline_values[obj_idx];

            if stacked.is_empty() || baseline.len() < 3 {
                per_objective.push(serde_json::json!({
                    "objective_index": obj_idx,
                    "detected": false,
                    "reason": "insufficient data",
                    "post_impulse_samples": stacked.len(),
                    "baseline_samples": baseline.len(),
                }));
                continue;
            }

            let stacked_mean = stacked.iter().sum::<f64>() / stacked.len() as f64;
            let baseline_mean = baseline.iter().sum::<f64>() / baseline.len() as f64;
            let baseline_std = {
                let n = baseline.len() as f64;
                let variance = baseline.iter()
                    .map(|v| (v - baseline_mean).powi(2))
                    .sum::<f64>() / n;
                variance.sqrt()
            };

            if baseline_std < 1e-12 {
                // Zero variance baseline — stacked mean must differ to detect
                let diff = (stacked_mean - baseline_mean).abs();
                let detected = diff > 1e-12;
                per_objective.push(serde_json::json!({
                    "objective_index": obj_idx,
                    "detected": detected,
                    "method": "impulse_stacking",
                    "stacked_mean": round4(stacked_mean),
                    "baseline_mean": round4(baseline_mean),
                    "baseline_std": 0.0,
                    "shift": round4(stacked_mean - baseline_mean),
                    "snr": if detected { f64::MAX } else { 0.0 },
                    "post_impulse_samples": stacked.len(),
                    "baseline_samples": baseline.len(),
                    "impulses_used": window_count,
                }));
                if detected && !any_detected {
                    any_detected = true;
                    best_snr = f64::MAX;
                    best_obj = obj_idx;
                }
                continue;
            }

            let shift = stacked_mean - baseline_mean;
            let snr = shift.abs() / baseline_std;
            let detected = snr >= snr_threshold;

            per_objective.push(serde_json::json!({
                "objective_index": obj_idx,
                "detected": detected,
                "method": "impulse_stacking",
                "stacked_mean": round4(stacked_mean),
                "baseline_mean": round4(baseline_mean),
                "baseline_std": round4(baseline_std),
                "shift": round4(shift),
                "snr": round4(snr),
                "post_impulse_samples": stacked.len(),
                "baseline_samples": baseline.len(),
                "impulses_used": window_count,
            }));

            if detected {
                any_detected = true;
            }
            if snr > best_snr {
                best_snr = snr;
                best_obj = obj_idx;
            }
        }

        // Overall result
        let result = serde_json::json!({
            "detected": any_detected,
            "method": "impulse_stacking",
            "sender_id": sender_id,
            "receiver_id": receiver_id,
            "impulse_count": n_impulses,
            "impulses_used": window_count,
            "window_size": window_size,
            "snr_threshold": snr_threshold,
            "best_snr": round4(best_snr),
            "best_objective": best_obj,
            "watermark": wm_meta,
            "per_objective": per_objective,
            "sender_trials": sender_trials.len(),
            "receiver_trials": receiver_trials.len(),
        });

        info!(
            "Impulse detection: sender={} receiver={} impulses={} used={} detected={} best_snr={:.2}",
            sender_id, receiver_id, n_impulses, window_count, any_detected, best_snr
        );

        Ok(result)
    }
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
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
}
