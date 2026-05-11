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

        let wm_raw = &wm_trials[0].user_attrs["watermark"];
        let wm_meta: serde_json::Value = if wm_raw.is_string() {
            serde_json::from_str(wm_raw.as_str().unwrap_or("{}")).unwrap_or(serde_json::json!({}))
        } else {
            wm_raw.clone()
        };
        let wm_type = wm_meta.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
        let wm_period = wm_meta.get("period").and_then(|v| as_f64(v)).unwrap_or(10.0) as usize;

        let wm_signal: Vec<f64> = {
            let period = wm_meta.get("period").and_then(|v| as_f64(v)).unwrap_or(20.0);
            let amplitude = wm_meta.get("amplitude").and_then(|v| as_f64(v)).unwrap_or(0.1);
            let phase_offset = wm_meta.get("phase_offset").and_then(|v| as_f64(v)).unwrap_or(0.0);
            wm_trials.iter().map(|t| {
                let idx = t.user_attrs.get("watermark_trial_idx")
                    .and_then(|v| as_f64(v)).unwrap_or(0.0);
                amplitude * (2.0 * std::f64::consts::PI * idx / period + phase_offset).sin()
            }).collect()
        };

        if wm_signal.len() < 4 {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "too few watermark trials",
                "watermark_trials": wm_signal.len(),
            }));
        }

        let n_obj = receiver_trials.iter()
            .filter(|t| t.state == "COMPLETE")
            .map(|t| t.values.len())
            .max()
            .unwrap_or(0);
        if n_obj == 0 {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no objective values in receiver trials",
            }));
        }

        let wm_timestamps: Vec<&str> = wm_trials.iter()
            .filter_map(|t| t.datetime_start.as_deref())
            .collect();

        let wm_start = wm_timestamps.first().copied().unwrap_or("");
        let wm_end = wm_timestamps.last().copied().unwrap_or("");

        let mut per_objective: Vec<serde_json::Value> = Vec::new();
        let mut overall_detected = false;
        let mut overall_best_method = "";
        let mut overall_best_corr = 0.0_f64;
        let mut overall_best_lag = 0_i32;
        let mut overall_p_value = 1.0_f64;

        for obj_idx in 0..n_obj {
            let receiver_quality: Vec<(String, f64)> = receiver_trials.iter()
                .filter(|t| t.state == "COMPLETE")
                .filter(|t| t.datetime_start.is_some())
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .filter_map(|t| {
                    let ts = t.datetime_start.clone()?;
                    let val = t.values.get(obj_idx).and_then(|v| *v)?;
                    Some((ts, val))
                })
                .collect();

            if receiver_quality.len() < 4 { continue; }

            let rcv_start = receiver_quality.first().map(|(ts, _)| ts.as_str()).unwrap_or("");
            let rcv_end = receiver_quality.last().map(|(ts, _)| ts.as_str()).unwrap_or("");
            let overlap_start = if wm_start.cmp(rcv_start).is_gt() { wm_start } else { rcv_start };
            let overlap_end = if wm_end.cmp(rcv_end).is_lt() { wm_end } else { rcv_end };

            if overlap_start >= overlap_end { continue; }

            let mut aligned_idx_sig: Vec<(usize, f64)> = Vec::new();
            for (i, t) in wm_trials.iter().enumerate() {
                let ts = t.datetime_start.as_deref().unwrap_or("");
                if ts >= overlap_start && ts <= overlap_end {
                    if let Some(&v) = wm_signal.get(i) {
                        let idx = t.user_attrs.get("watermark_trial_idx")
                            .and_then(|v| as_f64(v)).unwrap_or(i as f64) as usize;
                        aligned_idx_sig.push((idx, v));
                    }
                }
            }
            aligned_idx_sig.sort_by_key(|(idx, _)| *idx);
            let aligned_signal: Vec<f64> = aligned_idx_sig.iter().map(|(_, v)| *v).collect();

            let aligned_quality: Vec<f64> = receiver_quality.iter()
                .filter(|(ts, _)| ts.as_str() >= overlap_start && ts.as_str() <= overlap_end)
                .map(|(_, v)| *v)
                .collect();

            let aligned_params: Vec<HashMap<String, f64>> = receiver_trials.iter()
                .filter(|t| t.state == "COMPLETE")
                .filter(|t| t.datetime_start.is_some())
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .filter(|t| {
                    let ts = t.datetime_start.as_deref().unwrap_or("");
                    ts >= overlap_start && ts <= overlap_end
                })
                .map(|t| t.params.clone())
                .collect();

            let n_align = aligned_signal.len().min(aligned_quality.len());
            if n_align < 4 { continue; }

            let sig = &aligned_signal[..n_align];
            let qual = &aligned_quality[..n_align];

            let detrended = if aligned_params.len() >= n_align && n_align > 6 {
                param_detrend(qual, &aligned_params[..n_align])
            } else {
                let detrend_window = (wm_period * 2).max(4).min(qual.len());
                moving_median_detrend(qual, detrend_window)
            };
            let residuals = &detrended[..n_align];

            let max_lag = (n_align / 3).max(1).min(20);
            let (pearson, pearson_lag) = best_cross_correlation(sig, residuals, max_lag);
            let (matched, matched_lag) = lagged_matched_filter(sig, residuals, max_lag);

            let mut corr_results = vec![
                ("pearson", pearson.abs(), pearson_lag),
                ("matched_filter", matched.abs(), matched_lag),
            ];

            let (te_value, te_p) = if n_align >= 20 {
                let te_observed = transfer_entropy(sig, residuals, 1);
                let te_base = transfer_entropy(sig, residuals, 0);
                let te_effect = te_observed - te_base;
                let mut te_exceed = 0usize;
                let mut rng_te = fastrand::Rng::new();
                for _ in 0..1000 {
                    let mut shuf: Vec<f64> = residuals.to_vec();
                    shuffle_vec(&mut shuf, &mut rng_te);
                    let te_perm = transfer_entropy(sig, &shuf, 1);
                    let te_perm_base = transfer_entropy(sig, &shuf, 0);
                    if te_perm - te_perm_base >= te_effect {
                        te_exceed += 1;
                    }
                }
                let te_p_val = (te_exceed + 1) as f64 / 1001.0;
                if te_p_val < 0.05 && te_effect > 0.001 {
                    corr_results.push(("transfer_entropy", te_effect, 0));
                }
                (round4(te_effect), round4(te_p_val))
            } else {
                (0.0, 1.0)
            };

            let best = corr_results.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)).unwrap();
            let best_method = best.0;
            let best_corr = best.1;
            let best_lag = best.2;

            let n_perm = 5000usize;
            let mut rng = fastrand::Rng::new();
            let mut exceed_count = 0usize;
            for _ in 0..n_perm {
                let mut shuffled: Vec<f64> = residuals.to_vec();
                shuffle_vec(&mut shuffled, &mut rng);
                let perm_pearson = best_cross_correlation(sig, &shuffled, max_lag).0;
                let perm_matched = lagged_matched_filter(sig, &shuffled, max_lag).0;
                let perm_best = perm_pearson.abs().max(perm_matched.abs());
                if perm_best >= best_corr {
                    exceed_count += 1;
                }
            }
            let p_value = (exceed_count + 1) as f64 / (n_perm + 1) as f64;

            let pearson_detected = p_value < 0.05 && pearson.abs() > 0.3;
            let mf_detected = matched.abs() > 0.3;
            let te_detected = te_p < 0.05 && te_value > 0.001;
            let n_agree = pearson_detected as usize + mf_detected as usize + te_detected as usize;
            let detected = mf_detected && n_agree >= 2;

            let mut obj_result = serde_json::json!({
                "objective_index": obj_idx,
                "detected": detected,
                "aligned_trials": n_align,
                "receiver_trials": receiver_quality.len(),
                "pearson": {"correlation": round4(pearson), "lag": pearson_lag, "detected": pearson_detected},
                "matched_filter": round4(matched),
                "matched_filter_lag": matched_lag,
                "matched_filter_detected": mf_detected,
                "transfer_entropy": {"value": te_value, "p_value": te_p, "detected": te_detected},
                "best_method": best_method,
                "best_correlation": round4(best_corr),
                "best_lag": best_lag,
                "p_value": round4(p_value),
                "permutations": n_perm,
                "residuals": residuals.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
                "sender_signal": sig.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
                "raw_quality": qual.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
            });

            if detected && !overall_detected {
                overall_detected = true;
                overall_best_method = best_method;
                overall_best_corr = best_corr;
                overall_best_lag = best_lag;
                overall_p_value = p_value;
            } else if !overall_detected && best_corr > overall_best_corr {
                overall_best_method = best_method;
                overall_best_corr = best_corr;
                overall_best_lag = best_lag;
                overall_p_value = p_value;
            }

            per_objective.push(obj_result);
        }

        if per_objective.is_empty() {
            return Ok(serde_json::json!({
                "detected": false,
                "reason": "no objective had enough aligned trials",
            }));
        }

        let mut result = serde_json::json!({
            "detected": overall_detected,
            "sender_id": sender_id,
            "receiver_id": receiver_id,
            "watermark_type": wm_type,
            "watermark_trials": wm_signal.len(),
            "best_method": overall_best_method,
            "best_correlation": round4(overall_best_corr),
            "best_lag": overall_best_lag,
            "p_value": round4(overall_p_value),
            "per_objective": per_objective,
        });

        Ok(result)
    }

    pub async fn get_active_breeders(&self) -> Result<serde_json::Value, Error> {
        let client = self.connect("archive_db").await?;

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

fn lagged_matched_filter(template: &[f64], signal: &[f64], max_lag: usize) -> (f64, i32) {
    let n = template.len().min(signal.len());
    if n < 2 { return (0.0, 0); }
    let t_mean = template[..n].iter().sum::<f64>() / n as f64;
    let t_demean: Vec<f64> = template[..n].iter().map(|v| v - t_mean).collect();
    let t_norm: f64 = t_demean.iter().map(|v| v * v).sum::<f64>().sqrt();
    if t_norm == 0.0 { return (0.0, 0); }
    let mut best_corr = 0.0_f64;
    let mut best_lag = 0_i32;
    for lag in 0..=max_lag {
        if lag >= n { break; }
        let s_end = n - lag;
        let s_slice = &signal[lag..lag + s_end];
        let s_mean = s_slice.iter().sum::<f64>() / s_end as f64;
        let s_norm: f64 = s_slice.iter().map(|v| (v - s_mean).powi(2)).sum::<f64>().sqrt();
        if s_norm == 0.0 { continue; }
        let mut sum = 0.0_f64;
        for i in 0..s_end {
            sum += t_demean[i] * (s_slice[i] - s_mean);
        }
        let corr = sum / (t_norm * s_norm);
        if corr.abs() > best_corr.abs() {
            best_corr = corr;
            best_lag = lag as i32;
        }
        if lag > 0 && n - lag >= 2 {
            let s_slice_rev = &signal[..n - lag];
            let s_mean_rev = s_slice_rev.iter().sum::<f64>() / (n - lag) as f64;
            let s_norm_rev: f64 = s_slice_rev.iter().map(|v| (v - s_mean_rev).powi(2)).sum::<f64>().sqrt();
            if s_norm_rev > 0.0 {
                let mut sum_rev = 0.0_f64;
                for i in 0..n - lag {
                    sum_rev += t_demean[i] * (s_slice_rev[i] - s_mean_rev);
                }
                let corr_rev = sum_rev / (t_norm * s_norm_rev);
                if corr_rev.abs() > best_corr.abs() {
                    best_corr = corr_rev;
                    best_lag = -(lag as i32);
                }
            }
        }
    }
    (best_corr, best_lag)
}

fn shuffle_vec(v: &mut [f64], rng: &mut fastrand::Rng) {
    for i in (1..v.len()).rev() {
        let j = rng.usize(..=i);
        v.swap(i, j);
    }
}

fn moving_median_detrend(data: &[f64], window: usize) -> Vec<f64> {
    let n = data.len();
    if n == 0 { return vec![]; }
    let half = window / 2;
    let mut trend = vec![0.0_f64; n];
    for i in 0..n {
        let lo = if i >= half { i - half } else { 0 };
        let hi = (i + half + 1).min(n);
        let mut w: Vec<f64> = data[lo..hi].to_vec();
        w.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        trend[i] = w[w.len() / 2];
    }
    data.iter().zip(trend.iter()).map(|(v, t)| v - t).collect()
}

fn param_detrend(qualities: &[f64], params_list: &[HashMap<String, f64>]) -> Vec<f64> {
    let n = qualities.len();
    if n < 5 { return qualities.to_vec(); }
    if params_list.len() != n { return qualities.to_vec(); }

    let param_names: Vec<&String> = {
        let mut names: Vec<&String> = params_list.iter()
            .flat_map(|p| p.keys())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        names.sort();
        names
    };

    let ranges: Vec<(f64, f64)> = param_names.iter().map(|name| {
        let vals: Vec<f64> = params_list.iter()
            .filter_map(|p| p.get(*name).copied())
            .collect();
        let lo = vals.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (lo, hi - lo)
    }).collect();

    let k = ((n as f64).sqrt().ceil() as usize).min(15).min(n / 2).max(3);

    let mut residuals = Vec::with_capacity(n);
    for i in 0..n {
        let mut dists: Vec<(f64, usize)> = (0..n)
            .filter(|&j| j != i)
            .map(|j| {
                let d: f64 = param_names.iter().enumerate()
                    .map(|(pi, name)| {
                        let vi = params_list[i].get(*name).copied().unwrap_or(0.0);
                        let vj = params_list[j].get(*name).copied().unwrap_or(0.0);
                        let range = ranges[pi].1;
                        if range > 0.0 { ((vi - vj) / range).powi(2) } else { 0.0 }
                    })
                    .sum();
                (d, j)
            })
            .collect();
        dists.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let neighbors = &dists[..k.min(dists.len())];
        let total_weight: f64 = neighbors.iter()
            .map(|(d, _)| if *d > 0.0 { 1.0 / d } else { 1e6 })
            .sum();
        let predicted: f64 = if total_weight > 0.0 {
            neighbors.iter()
                .map(|(d, j)| {
                    let w = if *d > 0.0 { 1.0 / *d } else { 1e6 };
                    w * qualities[*j]
                })
                .sum::<f64>() / total_weight
        } else {
            qualities.iter().sum::<f64>() / n as f64
        };
        residuals.push(qualities[i] - predicted);
    }

    residuals
}

fn transfer_entropy(source: &[f64], target: &[f64], lag: usize) -> f64 {
    let n = source.len();
    if n < lag + 2 { return 0.0; }
    let n_bins = (n as f64).sqrt().max(3.0) as usize;

    let src_min = source.iter().cloned().fold(f64::INFINITY, f64::min);
    let src_max = source.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let tgt_min = target.iter().cloned().fold(f64::INFINITY, f64::min);
    let tgt_max = target.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    if src_max - src_min < 1e-12 || tgt_max - tgt_min < 1e-12 { return 0.0; }

    let src_bins: Vec<usize> = source.iter()
        .map(|v| (((v - src_min) / (src_max - src_min) * (n_bins as f64 - 1.0)).round() as usize).min(n_bins - 1))
        .collect();
    let tgt_bins: Vec<usize> = target.iter()
        .map(|v| (((v - tgt_min) / (tgt_max - tgt_min) * (n_bins as f64 - 1.0)).round() as usize).min(n_bins - 1))
        .collect();

    let n_bins_cubed = n_bins * n_bins * n_bins;
    let mut count_joint = vec![0u32; n_bins_cubed];
    let mut count_tgt_next_given_tgt = vec![0u32; n_bins * n_bins];
    let mut count_tgt_next = vec![0u32; n_bins];
    let mut count_src_given_tgt = vec![0u32; n_bins * n_bins];
    let mut count_tgt = vec![0u32; n_bins];
    let mut total: u32 = 0;

    for i in lag..n - 1 {
        let s = src_bins[i];
        let t_curr = tgt_bins[i];
        let t_next = tgt_bins[i + 1];

        count_joint[s * n_bins * n_bins + t_curr * n_bins + t_next] += 1;
        count_tgt_next_given_tgt[t_curr * n_bins + t_next] += 1;
        count_tgt_next[t_next] += 1;
        count_src_given_tgt[s * n_bins + t_curr] += 1;
        count_tgt[t_curr] += 1;
        total += 1;
    }

    if total == 0 { return 0.0; }

    let mut te = 0.0_f64;
    for i in lag..n - 1 {
        let s = src_bins[i];
        let t_curr = tgt_bins[i];
        let t_next = tgt_bins[i + 1];

        let c_joint = count_joint[s * n_bins * n_bins + t_curr * n_bins + t_next] as f64;
        let c_tgt_next_tgt = count_tgt_next_given_tgt[t_curr * n_bins + t_next] as f64;
        let c_src_tgt = count_src_given_tgt[s * n_bins + t_curr] as f64;
        let c_tgt = count_tgt[t_curr] as f64;

        if c_joint > 0.0 && c_tgt_next_tgt > 0.0 && c_src_tgt > 0.0 && c_tgt > 0.0 {
            let p_joint = c_joint / total as f64;
            let p_tgt_next_given_tgt = c_tgt_next_tgt / c_tgt;
            let p_cond_joint = c_joint / c_tgt;

            te += p_joint * (p_tgt_next_given_tgt / p_cond_joint).ln();
        }
    }

    te.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trial(number: i32, datetime: &str, params: Vec<(&str, f64)>, values: Vec<f64>, wm: Option<(&str, i32)>) -> TrialRecord {
        let mut user_attrs = HashMap::new();
        if let Some((wm_str, idx)) = wm {
            user_attrs.insert("watermark".to_string(), serde_json::json!(wm_str));
            user_attrs.insert("watermark_trial_idx".to_string(), serde_json::json!(idx));
        }
        TrialRecord {
            number,
            state: "COMPLETE".to_string(),
            datetime_start: Some(datetime.to_string()),
            datetime_complete: Some(datetime.to_string()),
            params: params.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            param_distributions: HashMap::new(),
            values: values.into_iter().map(Some).collect(),
            user_attrs,
        }
    }

    #[test]
    fn test_param_detrend_removes_dominant_trend() {
        let n = 60;
        let qualities: Vec<f64> = (0..n).map(|i| {
            3.0 * (i as f64) + 10.0 + (i as f64 * 0.1).sin()
        }).collect();
        let params_list: Vec<HashMap<String, f64>> = (0..n).map(|i| {
            let mut p = HashMap::new();
            p.insert("x".to_string(), i as f64);
            p
        }).collect();

        let residuals = param_detrend(&qualities, &params_list);
        let res_range = residuals.iter().cloned().fold(f64::INFINITY, f64::min)..residuals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let orig_range = qualities.iter().cloned().fold(f64::INFINITY, f64::min)..qualities.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(res_range.end - res_range.start < (orig_range.end - orig_range.start) * 0.2,
            "residuals range should be <20% of original, got residual range {} vs original {}",
            res_range.end - res_range.start, orig_range.end - orig_range.start);
    }

    #[test]
    fn test_param_detrend_preserves_small_signal() {
        let n = 80;
        let param_vals: Vec<f64> = (0..n).map(|i| {
            let scrambled = ((i * 37 + 13) % n) as f64;
            100.0 + 900.0 * (scrambled / n as f64)
        }).collect();
        let signal: Vec<f64> = (0..n).map(|i| 3.0 * (2.0 * std::f64::consts::PI * i as f64 / 20.0 + 1.3).sin()).collect();
        let qualities: Vec<f64> = (0..n).map(|i| {
            0.3 * param_vals[i] + 10.0 + signal[i]
        }).collect();
        let params_list: Vec<HashMap<String, f64>> = (0..n).map(|i| {
            let mut p = HashMap::new();
            p.insert("light_intensity".to_string(), param_vals[i]);
            p
        }).collect();

        let residuals = param_detrend(&qualities, &params_list);
        let corr = super::pearson_correlation(&signal, &residuals);
        assert!(corr > 0.7, "residuals should correlate with independent coupling signal, got r={}", corr);
    }

    #[test]
    fn test_detection_with_synthetic_coupling() {
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let phase_offset = 1.3418_f64;
        let coupling = 0.9_f64;
        let n = 80;

        let wm_meta = serde_json::json!({
            "type": "sinusoidal",
            "param_name": "light_intensity",
            "amplitude": amplitude,
            "period": period,
            "phase_offset": phase_offset
        });

        let sender_trials: Vec<TrialRecord> = (0..n).map(|i| {
            let light = 500.0 + amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + phase_offset).sin();
            let energy = light * 0.3 + (i as f64 * 0.5) + 2.0 * (i as f64 * 0.07).sin();
            let growth = 0.5 + 0.003 * i as f64 + 0.05 * (i as f64 * 0.13).sin();
            let water = light * 0.06 + (i as f64 * 0.1) + 0.3 * (i as f64 * 0.09).sin();
            let ts = format!("2026-05-08 21:{:02}:{:02}", 10 + i / 60, (i % 60) * 1);
            make_trial(i as i32, &ts,
                vec![("light_intensity", light), ("co2_injection", 5.0 + (i as f64 * 0.1)), ("irrigation", 1.0)],
                vec![growth, energy, water],
                Some((wm_meta.to_string().as_str(), i as i32))
            )
        }).collect();

        let receiver_trials: Vec<TrialRecord> = (0..n).map(|i| {
            let sender_light = 500.0 + amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + phase_offset).sin();
            let own_light = 300.0 + 200.0 * ((i * 7 + 3) as f64 % n as f64 / n as f64 * 10.0).sin();
            let energy = own_light * 0.3 + coupling * sender_light * 0.05 + (i as f64 * 0.4) + 3.0 * (i as f64 * 0.11).sin();
            let growth = 0.5 + 0.002 * i as f64 + 0.04 * (i as f64 * 0.17).sin();
            let water = own_light * 0.06 + coupling * sender_light * 0.01 + (i as f64 * 0.08) + 0.2 * (i as f64 * 0.13).sin();
            let ts = format!("2026-05-08 21:{:02}:{:02}", 10 + i / 60, (i % 60) * 1);
            make_trial(i as i32, &ts,
                vec![("light_intensity", own_light), ("co2_injection", 3.0 + (i as f64 * 0.2)), ("irrigation", 1.5)],
                vec![growth, energy, water],
                Some((serde_json::json!({"type":"sinusoidal","param_name":"light_intensity","amplitude":75.0,"period":20,"phase_offset":2.45}).to_string().as_str(), i as i32))
            )
        }).collect();

        let wm_signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + phase_offset).sin()
        }).collect();

        for obj_idx in 0..3 {
            let obj_name = ["growth_rate", "energy_kwh", "water_liters"][obj_idx];
            let receiver_quality: Vec<f64> = receiver_trials.iter()
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .map(|t| t.values[obj_idx].unwrap())
                .collect();
            let receiver_params: Vec<HashMap<String, f64>> = receiver_trials.iter()
                .map(|t| t.params.clone())
                .collect();

            let detrended = param_detrend(&receiver_quality, &receiver_params);
            let corr = super::pearson_correlation(&wm_signal, &detrended);

            if obj_idx == 0 {
                assert!(corr.abs() < 0.4, "growth_rate should not correlate with sender watermark, got r={}", corr);
            } else {
                assert!(corr.abs() > 0.15, "{} should correlate with sender watermark, got r={}", obj_name, corr);
            }
        }
    }

    #[test]
    fn test_detection_no_coupling() {
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let sender_phase = 1.3418_f64;
        let receiver_phase = 2.4546_f64;
        let n = 80;

        let wm_signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin()
        }).collect();

        let receiver_trials: Vec<TrialRecord> = (0..n).map(|i| {
            let own_light = 300.0 + 200.0 * ((i * 7 + 3) as f64 % n as f64 / n as f64 * 10.0).sin()
                + amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin() * 0.01;
            let energy = own_light * 0.3 + (i as f64 * 0.4) + 3.0 * (i as f64 * 0.11).sin();
            let growth = 0.5 + 0.002 * i as f64 + 0.04 * (i as f64 * 0.17).sin();
            let water = own_light * 0.06 + (i as f64 * 0.08) + 0.2 * (i as f64 * 0.13).sin();
            let ts = format!("2026-05-08 21:{:02}:{:02}", 10 + i / 60, (i % 60) * 1);
            make_trial(i as i32, &ts,
                vec![("light_intensity", own_light), ("co2_injection", 3.0 + (i as f64 * 0.2)), ("irrigation", 1.5)],
                vec![growth, energy, water],
                Some((serde_json::json!({"type":"sinusoidal","param_name":"light_intensity","amplitude":75.0,"period":20,"phase_offset":receiver_phase}).to_string().as_str(), i as i32))
            )
        }).collect();

        for obj_idx in 0..3 {
            let obj_name = ["growth_rate", "energy_kwh", "water_liters"][obj_idx];
            let receiver_quality: Vec<f64> = receiver_trials.iter()
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .map(|t| t.values[obj_idx].unwrap())
                .collect();
            let receiver_params: Vec<HashMap<String, f64>> = receiver_trials.iter()
                .map(|t| t.params.clone())
                .collect();

            let detrended = param_detrend(&receiver_quality, &receiver_params);
            let corr = super::pearson_correlation(&wm_signal, &detrended);

            assert!(corr.abs() < 0.4, "{} should not correlate with sender when no coupling, got r={}", obj_name, corr);
        }
    }

    #[test]
    fn test_diagnostic_no_coupling_realistic() {
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let sender_phase = 1.3418_f64;
        let receiver_phase = 2.4546_f64;
        let n = 120;

        let wm_signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin()
        }).collect();

        let receiver_trials: Vec<TrialRecord> = (0..n).map(|i| {
            let base_light = 500.0 + 400.0 * (i as f64 / n as f64 - 0.5);
            let wm_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let own_light = base_light + wm_offset;
            let co2 = 5.0 + 3.0 * (i as f64 * 0.05).sin();
            let energy = own_light * 0.3 + co2 * 2.0 + (i as f64 * 0.3) + 5.0 * (i as f64 * 0.08).sin();
            let growth = 0.5 + 0.003 * i as f64 + 0.03 * (i as f64 * 0.12).sin() + 0.001 * own_light;
            let water = own_light * 0.06 + co2 * 0.5 + (i as f64 * 0.1) + 2.0 * (i as f64 * 0.09).sin();
            let ts = format!("2026-05-08 22:{:02}:{:02}", i / 60, i % 60);
            make_trial(i as i32, &ts,
                vec![("light_intensity", own_light), ("co2_injection", co2), ("irrigation", 1.5)],
                vec![growth, energy, water],
                Some((serde_json::json!({"type":"sinusoidal","param_name":"light_intensity","amplitude":amplitude,"period":period as i32,"phase_offset":receiver_phase}).to_string().as_str(), i as i32))
            )
        }).collect();

        let obj_names = ["growth_rate", "energy_kwh", "water_liters"];
        for obj_idx in 0..3 {
            let receiver_quality: Vec<f64> = receiver_trials.iter()
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .map(|t| t.values[obj_idx].unwrap())
                .collect();
            let receiver_params: Vec<HashMap<String, f64>> = receiver_trials.iter()
                .map(|t| t.params.clone())
                .collect();

            let residuals = param_detrend(&receiver_quality, &receiver_params);

            let raw_mean = receiver_quality.iter().sum::<f64>() / n as f64;
            let raw_std = (receiver_quality.iter().map(|v| (v - raw_mean).powi(2)).sum::<f64>() / n as f64).sqrt();
            let res_mean = residuals.iter().sum::<f64>() / n as f64;
            let res_std = (residuals.iter().map(|v| (v - res_mean).powi(2)).sum::<f64>() / n as f64).sqrt();

            let lag0 = super::pearson_correlation(&wm_signal, &residuals);
            let max_lag = (n / 3).max(1).min(20);
            let (best_pearson, pearson_lag) = super::best_cross_correlation(&wm_signal, &residuals, max_lag);
            let (best_mf, mf_lag) = super::lagged_matched_filter(&wm_signal, &residuals, max_lag);

            eprintln!("\n=== obj{} ({}) ===", obj_idx, obj_names[obj_idx]);
            eprintln!("raw quality: mean={:.2} std={:.2}", raw_mean, raw_std);
            eprintln!("residuals:   mean={:.4f} std={:.4f} (reduction: {:.1}%)", res_mean, res_std, (1.0 - res_std / raw_std) * 100.0);
            eprintln!("lag-0 corr:  {:.4f}", lag0);
            eprintln!("best pearson: {:.4f} at lag={}", best_pearson, pearson_lag);
            eprintln!("best MF:      {:.4f} at lag={}", best_mf, mf_lag);
            eprintln!("residuals[:20]: {:?}", &residuals[..20.min(residuals.len())]);

            let lag0_abs = lag0.abs();
            let pearson_abs = best_pearson.abs();
            let mf_abs = best_mf.abs();
            assert!(lag0_abs < 0.3, "obj{} lag-0 corr too high: {}", obj_idx, lag0_abs);
            assert!(pearson_abs < 0.4, "obj{} pearson too high: {} at lag {}", obj_idx, pearson_abs, pearson_lag);
            assert!(mf_abs < 0.4, "obj{} MF too high: {} at lag {}", obj_idx, mf_abs, mf_lag);
        }
    }
}
