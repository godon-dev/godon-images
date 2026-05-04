use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

    pub async fn detect_watermark_coupling(
        &self,
        sender_id: &str,
        receiver_id: &str,
    ) -> Result<serde_json::Value, Error> {
        let sender_study = format!("{}_study", sender_id);
        let receiver_study = format!("{}_study", receiver_id);

        let sender_trials = self.get_trials(sender_id, &sender_study, 0, 10000).await?;
        let receiver_trials = self.get_trials(receiver_id, &receiver_study, 0, 10000).await?;

        let wm_trials: Vec<&TrialRecord> = sender_trials.iter()
            .filter(|t| t.user_attrs.contains_key("watermark"))
            .collect();

        if wm_trials.is_empty() {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no watermark trials found",
                "sender_id": sender_id,
                "receiver_id": receiver_id,
            }));
        }

        let wm_meta = &wm_trials[0].user_attrs["watermark"];
        let wm_type = wm_meta.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");

        let wm_signal: Vec<f64> = match wm_type {
            "on_off" => {
                let period = wm_meta.get("period").and_then(|v| as_f64(v)).unwrap_or(10.0) as usize;
                wm_trials.iter().map(|t| {
                    let idx = t.user_attrs.get("watermark_trial_idx")
                        .and_then(|v| as_f64(v)).unwrap_or(0.0) as usize;
                    if (idx / period) % 2 == 0 { 1.0 } else { 0.0 }
                }).collect()
            },
            "sinusoidal" => {
                let period = wm_meta.get("period").and_then(|v| as_f64(v)).unwrap_or(20.0);
                let amplitude = wm_meta.get("amplitude").and_then(|v| as_f64(v)).unwrap_or(0.1);
                wm_trials.iter().map(|t| {
                    let idx = t.user_attrs.get("watermark_trial_idx")
                        .and_then(|v| as_f64(v)).unwrap_or(0.0);
                    amplitude * (2.0 * std::f64::consts::PI * idx / period).sin()
                }).collect()
            },
            "step" => {
                let period = wm_meta.get("period").and_then(|v| as_f64(v)).unwrap_or(10.0) as usize;
                let step_fraction = wm_meta.get("step_fraction").and_then(|v| as_f64(v)).unwrap_or(0.2);
                wm_trials.iter().map(|t| {
                    let idx = t.user_attrs.get("watermark_trial_idx")
                        .and_then(|v| as_f64(v)).unwrap_or(0.0) as usize;
                    if (idx / period) % 2 == 0 { step_fraction } else { 0.0 }
                }).collect()
            },
            "multi_frequency" => {
                let base_period = wm_meta.get("base_period").and_then(|v| as_f64(v)).unwrap_or(20.0);
                let amplitude = wm_meta.get("amplitude").and_then(|v| as_f64(v)).unwrap_or(0.1);
                wm_trials.iter().map(|t| {
                    let idx = t.user_attrs.get("watermark_trial_idx")
                        .and_then(|v| as_f64(v)).unwrap_or(0.0);
                    amplitude * (2.0 * std::f64::consts::PI * idx / base_period).sin()
                }).collect()
            },
            _ => {
                return Ok(serde_json::json!({
                    "detected": false,
                    "reason": format!("unsupported watermark type: {}", wm_type),
                }));
            }
        };

        if wm_signal.len() < 4 {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "too few watermark trials",
                "watermark_trials": wm_signal.len(),
            }));
        }

        let receiver_quality: Vec<(String, f64)> = receiver_trials.iter()
            .filter(|t| t.state == "COMPLETE")
            .filter(|t| t.datetime_start.is_some())
            .filter(|t| t.values.first().map_or(false, |v| v.is_some() && v.is_finite()))
            .filter_map(|t| {
                let ts = t.datetime_start.clone()?;
                let val = t.values.first().and_then(|v| *v)?;
                Some((ts, val))
            })
            .collect();

        let wm_timestamps: Vec<&str> = wm_trials.iter()
            .filter_map(|t| t.datetime_start.as_deref())
            .collect();

        if receiver_quality.len() < 4 {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "too few receiver quality values",
                "receiver_trials": receiver_quality.len(),
            }));
        }

        let wm_start = wm_timestamps.first().copied().unwrap_or("");
        let wm_end = wm_timestamps.last().copied().unwrap_or("");
        let rcv_start = receiver_quality.first().map(|(ts, _)| ts.as_str()).unwrap_or("");
        let rcv_end = receiver_quality.last().map(|(ts, _)| ts.as_str()).unwrap_or("");
        let overlap_start = if wm_start.cmp(rcv_start).is_gt() { wm_start } else { rcv_start };
        let overlap_end = if wm_end.cmp(rcv_end).is_lt() { wm_end } else { rcv_end };

        if overlap_start >= overlap_end {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no temporal overlap between watermark and receiver trials",
                "watermark_range": [wm_start, wm_end],
                "receiver_range": [rcv_start, rcv_end],
            }));
        }

        let mut aligned_signal: Vec<f64> = Vec::new();
        for (i, t) in wm_trials.iter().enumerate() {
            let ts = t.datetime_start.as_deref().unwrap_or("");
            if ts >= overlap_start && ts <= overlap_end {
                if let Some(&v) = wm_signal.get(i) {
                    aligned_signal.push(v);
                }
            }
        }

        let aligned_quality: Vec<f64> = receiver_quality.iter()
            .filter(|(ts, _)| ts.as_str() >= overlap_start && ts.as_str() <= overlap_end)
            .map(|(_, v)| *v)
            .collect();

        let n_align = aligned_signal.len().min(aligned_quality.len());
        if n_align < 4 {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "too few temporally aligned trials",
                "aligned_trials": n_align,
            }));
        }

        let sig = &aligned_signal[..n_align];
        let qual = &aligned_quality[..n_align];

        let max_lag = (n_align / 3).max(1).min(20);
        let (pearson, pearson_lag) = best_cross_correlation(sig, qual, max_lag);
        let (spearman, spearman_lag) = best_spearman_lag(sig, qual, max_lag);
        let matched = matched_filter(sig, qual);

        let best_corr = pearson.abs().max(spearman.abs()).max(matched.abs());
        let best_method = if best_corr == matched.abs() { "matched_filter" }
                          else if best_corr == spearman.abs() { "spearman" }
                          else { "pearson" };
        let best_lag = if best_method == "spearman" { spearman_lag } else { pearson_lag };

        let n_perm = 5000usize;
        let mut rng = fastrand::Rng::new();
        let mut exceed_count = 0usize;
        for _ in 0..n_perm {
            let mut shuffled: Vec<f64> = qual.to_vec();
            shuffle_vec(&mut shuffled, &mut rng);
            let perm_pearson = best_cross_correlation(sig, &shuffled, max_lag).0;
            let perm_spearman = best_spearman_lag(sig, &shuffled, max_lag).0;
            let perm_matched = matched_filter(sig, &shuffled);
            let perm_best = perm_pearson.abs().max(perm_spearman.abs()).max(perm_matched.abs());
            if perm_best >= best_corr {
                exceed_count += 1;
            }
        }
        let p_value = (exceed_count + 1) as f64 / (n_perm + 1) as f64;

        let detected = p_value < 0.05 && best_corr > 0.1;

        Ok(serde_json::json!({
            "detected": detected,
            "sender_id": sender_id,
            "receiver_id": receiver_id,
            "watermark_type": wm_type,
            "watermark_trials": wm_signal.len(),
            "receiver_trials": receiver_quality.len(),
            "aligned_trials": n_align,
            "temporal_overlap": [overlap_start, overlap_end],
            "pearson": {"correlation": round4(pearson), "lag": pearson_lag},
            "spearman": {"correlation": round4(spearman), "lag": spearman_lag},
            "matched_filter": round4(matched),
            "best_method": best_method,
            "best_correlation": round4(best_corr),
            "best_lag": best_lag,
            "p_value": round4(p_value),
            "permutations": n_perm,
        }))
    }

    pub async fn get_choreography_status(&self) -> Result<serde_json::Value, Error> {
        let client = self.connect("archive_db").await?;

        let choreo_rows = client
            .query(
                "SELECT CAST(id AS TEXT), array_to_string(participants, ','), phases::text, current_phase, status, \
                 CAST(created_at AS TEXT), CAST(updated_at AS TEXT) \
                 FROM interference_choreography ORDER BY created_at DESC",
                &[],
            )
            .await?;

        let mut choreographies = Vec::new();
        for row in &choreo_rows {
            let id: String = row.get(0);
            let participants_str: String = row.get(1);
            let participants: Vec<String> = if participants_str.is_empty() { vec![] } else { participants_str.split(',').map(|s| s.to_string()).collect() };
            let phases_str: String = row.get(2);
            let current_phase: i32 = row.get(3);
            let status: String = row.get(4);
            let created_at: Option<String> = row.try_get(5).ok();
            let updated_at: Option<String> = row.try_get(6).ok();

            let phases: Vec<serde_json::Value> = serde_json::from_str(&phases_str).unwrap_or_default();

            let total_phases = phases.len() as i32;
            let phase_label = if (current_phase as usize) < phases.len() {
                phases[current_phase as usize].get("label").and_then(|v| v.as_str()).unwrap_or("unknown").to_string()
            } else {
                "completed".to_string()
            };
            let observe_breeder = if (current_phase as usize) < phases.len() {
                phases[current_phase as usize].get("observe_breeder").and_then(|v| v.as_str()).map(|s| s.to_string())
            } else {
                None
            };

            choreographies.push(serde_json::json!({
                "id": id,
                "participants": participants,
                "current_phase": current_phase,
                "total_phases": total_phases,
                "phase_label": phase_label,
                "observe_breeder": observe_breeder,
                "status": status,
                "phases": phases,
                "created_at": created_at,
                "updated_at": updated_at,
            }));
        }

        let breeder_rows = client
            .query(
                "SELECT breeder_id, CAST(last_seen AS TEXT) FROM interference_active_breeders",
                &[],
            )
            .await?;

        let mut active_breeders = Vec::new();
        for row in &breeder_rows {
            let breeder_id: String = row.get(0);
            let last_seen: Option<String> = row.try_get(1).ok();
            active_breeders.push(serde_json::json!({
                "breeder_id": breeder_id,
                "last_seen": last_seen,
            }));
        }

        Ok(serde_json::json!({
            "choreographies": choreographies,
            "active_breeders": active_breeders,
        }))
    }
}

fn as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

fn pearson_correlation(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len().min(y.len());
    if n < 2 { return 0.0; }
    let mx = x.iter().sum::<f64>() / n as f64;
    let my = y.iter().sum::<f64>() / n as f64;
    let mut cov = 0.0_f64;
    let mut vx = 0.0_f64;
    let mut vy = 0.0_f64;
    for i in 0..n {
        let dx = x[i] - mx;
        let dy = y[i] - my;
        cov += dx * dy;
        vx += dx * dx;
        vy += dy * dy;
    }
    let denom = vx.sqrt() * vy.sqrt();
    if denom == 0.0 { return 0.0; }
    cov / denom
}

fn best_cross_correlation(signal: &[f64], quality: &[f64], max_lag: usize) -> (f64, i32) {
    let n = signal.len().min(quality.len());
    if n < 2 { return (0.0, 0); }
    let mut best_corr = 0.0_f64;
    let mut best_lag = 0_i32;
    for lag in 0..=max_lag {
        if lag >= n { break; }
        let s_end = n - lag;
        let corr = pearson_correlation(&signal[..s_end], &quality[lag..lag + s_end]);
        if corr.abs() > best_corr.abs() {
            best_corr = corr;
            best_lag = lag as i32;
        }
        if lag > 0 && n - lag >= 2 {
            let corr_rev = pearson_correlation(&signal[lag..n], &quality[..n - lag]);
            if corr_rev.abs() > best_corr.abs() {
                best_corr = corr_rev;
                best_lag = -(lag as i32);
            }
        }
    }
    (best_corr, best_lag)
}

fn rank_data(data: &[f64]) -> Vec<f64> {
    let mut indexed: Vec<(usize, f64)> = data.iter().enumerate().map(|(i, &v)| (i, v)).collect();
    indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut ranks = vec![0.0_f64; data.len()];
    let mut i = 0;
    while i < indexed.len() {
        let mut j = i + 1;
        while j < indexed.len() && indexed[j].1 == indexed[i].1 {
            j += 1;
        }
        let avg_rank = (i + j - 1) as f64 / 2.0 + 1.0;
        for k in i..j {
            ranks[indexed[k].0] = avg_rank;
        }
        i = j;
    }
    ranks
}

fn spearman_correlation(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len().min(y.len());
    if n < 2 { return 0.0; }
    let rx = rank_data(&x[..n]);
    let ry = rank_data(&y[..n]);
    pearson_correlation(&rx, &ry)
}

fn best_spearman_lag(signal: &[f64], quality: &[f64], max_lag: usize) -> (f64, i32) {
    let n = signal.len().min(quality.len());
    if n < 2 { return (0.0, 0); }
    let mut best_corr = 0.0_f64;
    let mut best_lag = 0_i32;
    for lag in 0..=max_lag {
        if lag >= n { break; }
        let s_end = n - lag;
        let corr = spearman_correlation(&signal[..s_end], &quality[lag..lag + s_end]);
        if corr.abs() > best_corr.abs() {
            best_corr = corr;
            best_lag = lag as i32;
        }
        if lag > 0 && n - lag >= 2 {
            let corr_rev = spearman_correlation(&signal[lag..n], &quality[..n - lag]);
            if corr_rev.abs() > best_corr.abs() {
                best_corr = corr_rev;
                best_lag = -(lag as i32);
            }
        }
    }
    (best_corr, best_lag)
}

fn matched_filter(template: &[f64], signal: &[f64]) -> f64 {
    let n = template.len().min(signal.len());
    if n < 2 { return 0.0; }
    let t_mean = template[..n].iter().sum::<f64>() / n as f64;
    let s_mean = signal[..n].iter().sum::<f64>() / n as f64;
    let t_norm: f64 = template[..n].iter().map(|v| (v - t_mean).powi(2)).sum::<f64>().sqrt();
    let s_norm: f64 = signal[..n].iter().map(|v| (v - s_mean).powi(2)).sum::<f64>().sqrt();
    if t_norm == 0.0 || s_norm == 0.0 { return 0.0; }
    let mut sum = 0.0_f64;
    for i in 0..n {
        sum += (template[i] - t_mean) * (signal[i] - s_mean);
    }
    sum / (t_norm * s_norm)
}

fn shuffle_vec(v: &mut [f64], rng: &mut fastrand::Rng) {
    for i in (1..v.len()).rev() {
        let j = rng.usize(..=i);
        v.swap(i, j);
    }
}
