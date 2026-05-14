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

            let wm_amplitude = wm_meta.get("amplitude").and_then(|v| as_f64(v)).unwrap_or(0.1);
            let wm_phase_offset = wm_meta.get("phase_offset").and_then(|v| as_f64(v)).unwrap_or(0.0);
            let lockin = lock_in_detect(residuals, wm_period as f64, wm_amplitude, wm_phase_offset);

            let n_perm = 5000usize;
            let mut rng = fastrand::Rng::new();
            let mut exceed_count = 0usize;
            for _ in 0..n_perm {
                let mut shuffled: Vec<f64> = residuals.to_vec();
                shuffle_vec(&mut shuffled, &mut rng);
                let perm_lockin = lock_in_detect(&shuffled, wm_period as f64, wm_amplitude, wm_phase_offset);
                if perm_lockin.magnitude >= lockin.magnitude {
                    exceed_count += 1;
                }
            }
            let p_value = (exceed_count + 1) as f64 / (n_perm + 1) as f64;

            let detected = lockin.magnitude > 0.15 && p_value < 0.05;

            let obj_result = serde_json::json!({
                "objective_index": obj_idx,
                "detected": detected,
                "aligned_trials": n_align,
                "receiver_trials": receiver_quality.len(),
                "lock_in": {
                    "magnitude": round4(lockin.magnitude),
                    "phase": round4(lockin.phase),
                    "snr": round4(lockin.snr),
                    "i_component": round4(lockin.i_component),
                    "q_component": round4(lockin.q_component),
                    "detected": detected,
                },
                "best_method": "lock_in",
                "best_magnitude": round4(lockin.magnitude),
                "best_lag": (lockin.phase * wm_period as f64 / (2.0 * std::f64::consts::PI)).round() as i32,
                "p_value": round4(p_value),
                "permutations": n_perm,
                "residuals": residuals.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
                "sender_signal": sig.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
                "raw_quality": qual.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
            });

            if detected && !overall_detected {
                overall_detected = true;
                overall_best_corr = lockin.magnitude;
                overall_best_lag = (lockin.phase * wm_period as f64 / (2.0 * std::f64::consts::PI)).round() as i32;
                overall_p_value = p_value;
            } else if !overall_detected && lockin.magnitude > overall_best_corr {
                overall_best_corr = lockin.magnitude;
                overall_best_lag = (lockin.phase * wm_period as f64 / (2.0 * std::f64::consts::PI)).round() as i32;
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

        let result = serde_json::json!({
            "detected": overall_detected,
            "sender_id": sender_id,
            "receiver_id": receiver_id,
            "watermark_type": wm_type,
            "watermark_trials": wm_signal.len(),
            "best_method": "lock_in",
            "best_magnitude": round4(overall_best_corr),
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

struct LockInResult {
    magnitude: f64,
    phase: f64,
    snr: f64,
    i_component: f64,
    q_component: f64,
}

fn lock_in_detect(
    residuals: &[f64],
    period: f64,
    amplitude: f64,
    sender_phase: f64,
) -> LockInResult {
    let n = residuals.len();
    if n < 4 || period <= 0.0 || amplitude <= 0.0 {
        return LockInResult { magnitude: 0.0, phase: 0.0, snr: 0.0, i_component: 0.0, q_component: 0.0 };
    }

    let res_mean = residuals.iter().sum::<f64>() / n as f64;
    let res_std = (residuals.iter().map(|v| (v - res_mean).powi(2)).sum::<f64>() / n as f64).sqrt();
    if res_std < 1e-12 {
        return LockInResult { magnitude: 0.0, phase: 0.0, snr: 0.0, i_component: 0.0, q_component: 0.0 };
    }

    let two_pi = 2.0 * std::f64::consts::PI;
    let mut i_sum = 0.0_f64;
    let mut q_sum = 0.0_f64;
    for (idx, &r) in residuals.iter().enumerate() {
        let phase = two_pi * idx as f64 / period + sender_phase;
        i_sum += r * phase.cos();
        q_sum += r * phase.sin();
    }
    i_sum /= (n as f64) * amplitude / 2.0;
    q_sum /= (n as f64) * amplitude / 2.0;

    let magnitude = (i_sum * i_sum + q_sum * q_sum).sqrt();
    let phase = q_sum.atan2(i_sum);

    let mut noise_power = 0.0_f64;
    let n_noise_freqs = 4;
    for offset in 1..=n_noise_freqs {
        let freq_offset = offset as f64;
        let test_period = period * period / (period + freq_offset);
        let mut ni = 0.0_f64;
        let mut nq = 0.0_f64;
        for (idx, &r) in residuals.iter().enumerate() {
            let p = two_pi * idx as f64 / test_period;
            ni += r * p.cos();
            nq += r * p.sin();
        }
        ni /= (n as f64) * amplitude / 2.0;
        nq /= (n as f64) * amplitude / 2.0;
        noise_power += ni * ni + nq * nq;
    }
    noise_power /= n_noise_freqs as f64;
    let snr = if noise_power > 1e-12 { magnitude / noise_power.sqrt() } else { 0.0 };

    LockInResult { magnitude, phase, snr, i_component: i_sum, q_component: q_sum }
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
        let sender_phase = 1.3418_f64;
        let receiver_phase = 2.45_f64;
        let coupling = 0.9_f64;
        let n = 80;

        let receiver_trials: Vec<TrialRecord> = (0..n).map(|i| {
            let sender_light = 500.0 + amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin();
            let own_light = 300.0 + 200.0 * ((i * 7 + 3) as f64 % n as f64 / n as f64 * 10.0).sin()
                + amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let energy = own_light * 0.3 + coupling * sender_light * 0.05 + (i as f64 * 0.4) + 3.0 * (i as f64 * 0.11).sin();
            let growth = 0.5 + 0.002 * i as f64 + 0.04 * (i as f64 * 0.17).sin();
            let water = own_light * 0.06 + coupling * sender_light * 0.01 + (i as f64 * 0.08) + 0.2 * (i as f64 * 0.13).sin();
            let ts = format!("2026-05-08 21:{:02}:{:02}", 10 + i / 60, (i % 60) * 1);
            make_trial(i as i32, &ts,
                vec![("light_intensity", own_light), ("co2_injection", 3.0 + (i as f64 * 0.2)), ("irrigation", 1.5)],
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

            let detrended = param_detrend(&receiver_quality, &receiver_params);
            let lockin = super::lock_in_detect(&detrended, period, amplitude, sender_phase);

            eprintln!("  obj{} ({}): lock_in mag={:.4} snr={:.2}", obj_idx, obj_names[obj_idx], lockin.magnitude, lockin.snr);

            if obj_idx == 0 {
                assert!(lockin.magnitude < 0.5, "growth_rate should not show strong coupling, got mag={:.4} snr={:.2}", lockin.magnitude, lockin.snr);
            } else if obj_idx == 1 {
                assert!(lockin.magnitude > 0.05, "{} should show coupling via lock-in, got mag={:.4}", obj_names[obj_idx], lockin.magnitude);
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

        let receiver_trials: Vec<TrialRecord> = (0..n).map(|i| {
            let own_light = 300.0 + 200.0 * ((i * 7 + 3) as f64 % n as f64 / n as f64 * 10.0).sin()
                + amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let energy = own_light * 0.3 + (i as f64 * 0.4) + 3.0 * (i as f64 * 0.11).sin();
            let growth = 0.5 + 0.002 * i as f64 + 0.04 * (i as f64 * 0.17).sin();
            let water = own_light * 0.06 + (i as f64 * 0.08) + 0.2 * (i as f64 * 0.13).sin();
            let ts = format!("2026-05-08 21:{:02}:{:02}", 10 + i / 60, (i % 60) * 1);
            make_trial(i as i32, &ts,
                vec![("light_intensity", own_light), ("co2_injection", 3.0 + (i as f64 * 0.2)), ("irrigation", 1.5)],
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

            let detrended = param_detrend(&receiver_quality, &receiver_params);
            let lockin = super::lock_in_detect(&detrended, period, amplitude, sender_phase);

            eprintln!("  obj{} ({}): lock_in mag={:.4} snr={:.2}", obj_idx, obj_names[obj_idx], lockin.magnitude, lockin.snr);
            assert!(lockin.magnitude < 0.3, "{} should not show coupling, got mag={:.4}", obj_names[obj_idx], lockin.magnitude);
        }
    }

    fn generate_receiver_trials(
        n: usize,
        period: f64,
        amplitude: f64,
        sender_phase: f64,
        receiver_phase: f64,
        coupling_factors: [f64; 3],
        param_pattern: &str,
    ) -> Vec<TrialRecord> {
        (0..n).map(|i| {
            let base_light = match param_pattern {
                "linear" => 100.0 + 800.0 * (i as f64 / n as f64),
                "random_walk" => 500.0 + 300.0 * (i as f64 * 0.03).sin() + 100.0 * (i as f64 * 0.07).sin(),
                "step" => if i < n / 3 { 200.0 } else if i < 2 * n / 3 { 500.0 } else { 800.0 },
                "scrambled" => 100.0 + 800.0 * (((i * 37 + 13) % n) as f64 / n as f64),
                _ => 500.0,
            };
            let wm_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let own_light = base_light + wm_offset;
            let sender_light = 500.0 + amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin();
            let co2 = 3.0 + 4.0 * (i as f64 / n as f64) + 1.0 * (i as f64 * 0.05).sin();
            let irr = 1.0 + 0.5 * (i as f64 * 0.03).sin();

            let growth = 0.5
                + 0.001 * own_light
                + 0.003 * i as f64
                + 0.04 * (i as f64 * 0.12).sin()
                + coupling_factors[0] * sender_light * 0.0001;
            let energy = own_light * 0.3
                + co2 * 2.0
                + (i as f64 * 0.3)
                + 5.0 * (i as f64 * 0.08).sin()
                + coupling_factors[1] * sender_light * 0.05;
            let water = own_light * 0.06
                + co2 * 0.5
                + (i as f64 * 0.1)
                + 2.0 * (i as f64 * 0.09).sin()
                + coupling_factors[2] * sender_light * 0.01;

            let ts = format!("2026-05-08 22:{:02}:{:02}", i / 60, i % 60);
            make_trial(i as i32, &ts,
                vec![("light_intensity", own_light), ("co2_injection", co2), ("irrigation", irr)],
                vec![growth, energy, water],
                Some((serde_json::json!({"type":"sinusoidal","param_name":"light_intensity","amplitude":amplitude,"period":period as i32,"phase_offset":receiver_phase}).to_string().as_str(), i as i32))
            )
        }).collect()
    }

    fn run_lockin_diagnostic(
        name: &str,
        n: usize,
        period: f64,
        amplitude: f64,
        sender_phase: f64,
        receiver_phase: f64,
        coupling_factors: [f64; 3],
        param_pattern: &str,
        expect_detection: [bool; 3],
    ) {
        let receiver_trials = generate_receiver_trials(n, period, amplitude, sender_phase, receiver_phase, coupling_factors, param_pattern);

        let obj_names = ["growth_rate", "energy_kwh", "water_liters"];
        eprintln!("\n======== {} (n={}, period={}, amp={}, coupling=[{},{},{}], pattern={}) ========",
            name, n, period, amplitude, coupling_factors[0], coupling_factors[1], coupling_factors[2], param_pattern);

        for obj_idx in 0..3 {
            let receiver_quality: Vec<f64> = receiver_trials.iter()
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .map(|t| t.values[obj_idx].unwrap())
                .collect();
            let receiver_params: Vec<HashMap<String, f64>> = receiver_trials.iter()
                .map(|t| t.params.clone())
                .collect();

            let residuals = param_detrend(&receiver_quality, &receiver_params);

            let res_mean = residuals.iter().sum::<f64>() / n as f64;
            let res_std = (residuals.iter().map(|v| (v - res_mean).powi(2)).sum::<f64>() / n as f64).sqrt();
            let raw_std = (receiver_quality.iter().map(|v| (v - receiver_quality.iter().sum::<f64>() / n as f64).powi(2)).sum::<f64>() / n as f64).sqrt();

            let lockin = super::lock_in_detect(&residuals, period, amplitude, sender_phase);

            let n_perm = 500usize;
            let mut rng = fastrand::Rng::new();
            let mut exceed_count = 0usize;
            for _ in 0..n_perm {
                let mut shuffled: Vec<f64> = residuals.clone();
                shuffle_vec(&mut shuffled, &mut rng);
                let perm_lockin = super::lock_in_detect(&shuffled, period, amplitude, sender_phase);
                if perm_lockin.magnitude >= lockin.magnitude {
                    exceed_count += 1;
                }
            }
            let p_value = (exceed_count + 1) as f64 / (n_perm + 1) as f64;

            let detected = lockin.magnitude > 0.15 && p_value < 0.05;

            eprintln!("  obj{} ({}): raw_std={:.2} res_std={:.4} reduction={:.1}% | lock_in mag={:.4} phase={:.2} snr={:.2} I={:.4} Q={:.4} p={:.4} detected={} expect={}",
                obj_idx, obj_names[obj_idx], raw_std, res_std, (1.0 - res_std / raw_std) * 100.0,
                lockin.magnitude, lockin.phase, lockin.snr, lockin.i_component, lockin.q_component, p_value,
                detected, expect_detection[obj_idx]);

            if !expect_detection[obj_idx] {
                assert!(!detected, "{} obj{} ({}) should NOT be detected: lock_in mag={:.4} snr={:.2} p={:.4}",
                    name, obj_idx, obj_names[obj_idx], lockin.magnitude, lockin.snr, p_value);
            }
        }
    }

    #[test]
    fn test_lock_in_pure_sinusoid_recovery() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 1.0_f64;
        let phase = 1.34_f64;
        let signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + phase).sin()
        }).collect();

        let lockin = super::lock_in_detect(&signal, period, amplitude, phase);
        assert!(lockin.magnitude > 0.9, "pure sinusoid should give magnitude ~1.0, got {}", lockin.magnitude);
        assert!(lockin.q_component > 0.9, "Q component should be ~1.0 for sine signal, got {}", lockin.q_component);
        assert!(lockin.i_component.abs() < 0.1, "I component should be ~0 for sine signal, got {}", lockin.i_component);
    }

    #[test]
    fn test_lock_in_delayed_sinusoid() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 1.0_f64;
        let phase = 1.34_f64;
        let signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + phase).sin()
        }).collect();

        let delay_samples = 5;
        let delayed: Vec<f64> = (0..n).map(|i| {
            if i < delay_samples { 0.0 } else { signal[i - delay_samples] }
        }).collect();

        let lockin = super::lock_in_detect(&delayed, period, amplitude, phase);
        assert!(lockin.magnitude > 0.7, "delayed sinusoid should still be detected, got mag={}", lockin.magnitude);
    }

    #[test]
    fn test_lock_in_noise_rejection() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 1.0_f64;
        let phase = 1.34_f64;

        let mut rng = fastrand::Rng::new();
        let noise: Vec<f64> = (0..n).map(|_| rng.f64() * 2.0 - 1.0).collect();

        let lockin = super::lock_in_detect(&noise, period, amplitude, phase);
        assert!(lockin.magnitude < 0.3, "pure noise should give low magnitude, got {}", lockin.magnitude);
    }

    #[test]
    fn test_lock_in_slow_drift_rejection() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 1.0_f64;
        let phase = 1.34_f64;

        let drift: Vec<f64> = (0..n).map(|i| {
            10.0 * (i as f64 / n as f64) + 3.0 * (i as f64 * 0.02).sin() + 2.0 * (i as f64 * 0.05).sin()
        }).collect();

        let lockin = super::lock_in_detect(&drift, period, amplitude, phase);
        assert!(lockin.magnitude < 1.0, "slow drift should give low magnitude, got {}", lockin.magnitude);
    }

    #[test]
    fn test_lock_in_different_phase_detection() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 1.0_f64;
        let sender_phase = 1.34_f64;
        let other_phase = 4.56_f64;

        let other_signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + other_phase).sin()
        }).collect();

        let lockin = super::lock_in_detect(&other_signal, period, amplitude, sender_phase);
        assert!(lockin.magnitude > 0.8, "same-freq different-phase sinusoid should still have high magnitude, got mag={}", lockin.magnitude);
        assert!(lockin.i_component.abs() < 0.3, "I component should be low (phase mismatch), got I={}", lockin.i_component);
        assert!(lockin.q_component.abs() > 0.3, "Q component should carry the energy, got Q={}", lockin.q_component);
    }

    #[test]
    fn test_lock_in_coupled_sinusoid_in_noise() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 1.0_f64;
        let phase = 1.34_f64;

        let coupling_signal: Vec<f64> = (0..n).map(|i| {
            0.3 * amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + phase).sin()
        }).collect();
        let mut rng = fastrand::Rng::new();
        let noise: Vec<f64> = (0..n).map(|_| rng.f64() * 0.5 - 0.25).collect();
        let mixed: Vec<f64> = (0..n).map(|i| coupling_signal[i] + noise[i]).collect();

        let lockin = super::lock_in_detect(&mixed, period, amplitude, phase);
        assert!(lockin.magnitude > 0.1, "weak coupled signal in noise should be detected, got mag={}", lockin.magnitude);
    }

    #[test]
    fn test_lock_in_few_cycles() {
        let n = 30;
        let period = 20.0_f64;
        let amplitude = 1.0_f64;
        let phase = 1.34_f64;
        let signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + phase).sin()
        }).collect();

        let lockin = super::lock_in_detect(&signal, period, amplitude, phase);
        assert!(lockin.magnitude > 0.5, "even 1.5 cycles should give reasonable magnitude, got {}", lockin.magnitude);
    }

    #[test]
    fn test_lock_in_empty_input() {
        let lockin = super::lock_in_detect(&[], 20.0, 1.0, 0.0);
        assert_eq!(lockin.magnitude, 0.0);
        let lockin = super::lock_in_detect(&[1.0, 2.0], 20.0, 0.0, 0.0);
        assert_eq!(lockin.magnitude, 0.0);
    }

    #[test]
    fn test_lock_in_nonlinear_distortion() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 1.0_f64;
        let phase = 1.34_f64;
        let signal: Vec<f64> = (0..n).map(|i| {
            let raw = (2.0 * std::f64::consts::PI * i as f64 / period + phase).sin();
            amplitude * raw.max(-0.5).min(0.5)
        }).collect();

        let lockin = super::lock_in_detect(&signal, period, amplitude, phase);
        assert!(lockin.magnitude > 0.3, "clipped sinusoid still has fundamental energy, got mag={}", lockin.magnitude);
    }

    #[test]
    fn test_diag_no_coupling_linear_exploration() {
        run_lockin_diagnostic("no_coupling_linear", 120, 20.0, 75.0, 1.34, 2.45, [0.0, 0.0, 0.0], "linear", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_random_walk() {
        run_lockin_diagnostic("no_coupling_random_walk", 120, 20.0, 75.0, 1.34, 2.45, [0.0, 0.0, 0.0], "random_walk", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_step_exploration() {
        run_lockin_diagnostic("no_coupling_step", 120, 20.0, 75.0, 1.34, 2.45, [0.0, 0.0, 0.0], "step", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_scrambled() {
        run_lockin_diagnostic("no_coupling_scrambled", 120, 20.0, 75.0, 1.34, 2.45, [0.0, 0.0, 0.0], "scrambled", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_short_trials() {
        run_lockin_diagnostic("no_coupling_40trials", 40, 20.0, 75.0, 1.34, 2.45, [0.0, 0.0, 0.0], "linear", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_long_trials() {
        run_lockin_diagnostic("no_coupling_200trials", 200, 20.0, 75.0, 1.34, 2.45, [0.0, 0.0, 0.0], "linear", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_small_amplitude() {
        run_lockin_diagnostic("no_coupling_small_amp", 120, 20.0, 20.0, 1.34, 2.45, [0.0, 0.0, 0.0], "linear", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_large_amplitude() {
        run_lockin_diagnostic("no_coupling_large_amp", 120, 20.0, 200.0, 1.34, 2.45, [0.0, 0.0, 0.0], "linear", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_short_period() {
        run_lockin_diagnostic("no_coupling_period10", 120, 10.0, 75.0, 1.34, 2.45, [0.0, 0.0, 0.0], "linear", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_long_period() {
        run_lockin_diagnostic("no_coupling_period40", 120, 40.0, 75.0, 1.34, 2.45, [0.0, 0.0, 0.0], "linear", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_same_phase() {
        run_lockin_diagnostic("no_coupling_same_phase", 120, 20.0, 75.0, 1.34, 1.34, [0.0, 0.0, 0.0], "linear", [false, false, false]);
    }

    #[test]
    fn test_diag_no_coupling_opposite_phase() {
        run_lockin_diagnostic("no_coupling_opposite_phase", 120, 20.0, 75.0, 1.34, 1.34 + std::f64::consts::PI, [0.0, 0.0, 0.0], "linear", [false, false, false]);
    }

    #[test]
    fn test_diag_strong_coupling_linear() {
        run_lockin_diagnostic("strong_coupling_linear", 120, 20.0, 75.0, 1.34, 2.45, [0.0, 0.9, 0.9], "linear", [false, true, true]);
    }

    #[test]
    fn test_diag_strong_coupling_random_walk() {
        run_lockin_diagnostic("strong_coupling_random_walk", 120, 20.0, 75.0, 1.34, 2.45, [0.0, 0.9, 0.9], "random_walk", [false, true, true]);
    }

    #[test]
    fn test_diag_strong_coupling_scrambled() {
        run_lockin_diagnostic("strong_coupling_scrambled", 120, 20.0, 75.0, 1.34, 2.45, [0.0, 0.9, 0.9], "scrambled", [false, true, true]);
    }

    #[test]
    fn test_diag_moderate_coupling() {
        run_lockin_diagnostic("moderate_coupling", 120, 20.0, 75.0, 1.34, 2.45, [0.0, 0.5, 0.5], "linear", [false, true, true]);
    }

    #[test]
    fn test_diag_weak_coupling() {
        run_lockin_diagnostic("weak_coupling", 120, 20.0, 75.0, 1.34, 2.45, [0.0, 0.2, 0.2], "linear", [false, false, false]);
    }

    #[test]
    fn test_diag_strong_coupling_short_trials() {
        run_lockin_diagnostic("strong_coupling_60trials", 60, 20.0, 75.0, 1.34, 2.45, [0.0, 0.9, 0.9], "linear", [false, true, true]);
    }

    #[test]
    fn test_diag_strong_coupling_long_trials() {
        run_lockin_diagnostic("strong_coupling_200trials", 200, 20.0, 75.0, 1.34, 2.45, [0.0, 0.9, 0.9], "linear", [false, true, true]);
    }

    #[test]
    fn test_diag_coupling_only_one_objective() {
        run_lockin_diagnostic("coupling_one_obj", 120, 20.0, 75.0, 1.34, 2.45, [0.0, 0.9, 0.0], "linear", [false, true, false]);
    }

    #[test]
    fn test_moving_median_preserves_sinusoid() {
        let n = 120;
        let period = 20.0_f64;
        let signal: Vec<f64> = (0..n).map(|i| {
            2.0 * (2.0 * std::f64::consts::PI * i as f64 / period).sin()
        }).collect();

        let window = (period as usize * 2).max(4).min(n);
        let detrended = moving_median_detrend(&signal, window);

        let survival = pearson_correlation(&signal, &detrended);
        eprintln!("sinusoid survival after moving median (window={}): correlation={:.4}", window, survival);
        assert!(survival > 0.5, "period-20 sinusoid should partially survive moving median with window 40, got corr={}", survival);
    }

    #[test]
    fn test_moving_median_removes_linear_trend() {
        let n = 120;
        let trend: Vec<f64> = (0..n).map(|i| 5.0 * i as f64 + 100.0).collect();

        let window = 40;
        let detrended = moving_median_detrend(&trend, window);

        let range = detrended.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - detrended.iter().cloned().fold(f64::INFINITY, f64::min);
        eprintln!("linear trend residual range after moving median: {:.4}", range);
        assert!(range < 100.0, "linear trend should be mostly removed, got range={}", range);
    }

    #[test]
    fn test_moving_median_removes_slow_drift() {
        let n = 120;
        let drift: Vec<f64> = (0..n).map(|i| {
            50.0 * (i as f64 / n as f64).sqrt() + 20.0 * (i as f64 * 0.015).sin()
        }).collect();

        let window = 40;
        let detrended = moving_median_detrend(&drift, window);

        let drift_range = drift.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - drift.iter().cloned().fold(f64::INFINITY, f64::min);
        let res_range = detrended.iter().cloned().fold(f64::NEG_INFINITY, f64::max) - detrended.iter().cloned().fold(f64::INFINITY, f64::min);
        eprintln!("drift range: {:.1} -> residual range: {:.4}", drift_range, res_range);
        assert!(res_range < drift_range * 0.3, "slow drift should be reduced, residual range={} vs drift range={}", res_range, drift_range);
    }

    #[test]
    fn test_double_detrend_trend_plus_sinusoid() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 2.0_f64;
        let trend: Vec<f64> = (0..n).map(|i| 5.0 * i as f64 + 100.0).collect();
        let sinusoid: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period).sin()
        }).collect();
        let combined: Vec<f64> = (0..n).map(|i| trend[i] + sinusoid[i]).collect();

        let window = 40;
        let after_mm = moving_median_detrend(&combined, window);

        let survival = pearson_correlation(&sinusoid, &after_mm);
        eprintln!("sinusoid survival through moving median on (trend+sinusoid): {:.4}", survival);
        assert!(survival > 0.5, "sinusoid should survive moving median detrending of trend+sinusoid, got corr={}", survival);
    }

    #[test]
    fn test_double_detrend_realistic_no_coupling() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let sender_phase = 1.34_f64;
        let receiver_phase = 2.45_f64;

        let receiver_trials: Vec<TrialRecord> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let wm_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let own_light = base_light + wm_offset;
            let co2 = 3.0 + 4.0 * (i as f64 / n as f64);
            let energy = own_light * 0.3 + co2 * 2.0 + (i as f64 * 0.3) + 5.0 * (i as f64 * 0.08).sin();
            let growth = 0.5 + 0.001 * own_light + 0.003 * i as f64 + 0.04 * (i as f64 * 0.12).sin();
            let water = own_light * 0.06 + co2 * 0.5 + (i as f64 * 0.1) + 2.0 * (i as f64 * 0.09).sin();
            let ts = format!("2026-05-14 12:{:02}:{:02}", i / 60, i % 60);
            make_trial(i as i32, &ts,
                vec![("light_intensity", own_light), ("co2_injection", co2)],
                vec![growth, energy, water],
                Some((serde_json::json!({"type":"sinusoidal","param_name":"light_intensity","amplitude":amplitude,"period":period as i32,"phase_offset":receiver_phase}).to_string().as_str(), i as i32))
            )
        }).collect();

        let sender_signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin()
        }).collect();

        let obj_names = ["growth_rate", "energy_kwh", "water_liters"];
        eprintln!("\n======== NO COUPLING (double detrend) ========");
        let mut no_coupling_max_mag = 0.0_f64;
        for obj_idx in 0..3 {
            let quality: Vec<f64> = receiver_trials.iter()
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .map(|t| t.values[obj_idx].unwrap())
                .collect();
            let params: Vec<HashMap<String, f64>> = receiver_trials.iter().map(|t| t.params.clone()).collect();

            let after_knn = param_detrend(&quality, &params);
            let knn_corr = pearson_correlation(&sender_signal, &after_knn);

            let window = (period as usize * 2).max(4).min(after_knn.len());
            let after_double = moving_median_detrend(&after_knn, window);
            let double_corr = pearson_correlation(&sender_signal, &after_double);

            let lockin_knn = lock_in_detect(&after_knn, period, amplitude, sender_phase);
            let lockin_double = lock_in_detect(&after_double, period, amplitude, sender_phase);

            no_coupling_max_mag = no_coupling_max_mag.max(lockin_double.magnitude);

            eprintln!("  obj{} ({}): knn_corr={:.4} double_corr={:.4} | knn_mag={:.4} knn_snr={:.2} | double_mag={:.4} double_snr={:.2}",
                obj_idx, obj_names[obj_idx], knn_corr, double_corr, lockin_knn.magnitude, lockin_knn.snr, lockin_double.magnitude, lockin_double.snr);
        }
        assert!(no_coupling_max_mag < 0.5, "no-coupling double detrend max magnitude should be low, got {}", no_coupling_max_mag);
    }

    #[test]
    fn test_double_detrend_realistic_with_coupling() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let sender_phase = 1.34_f64;
        let receiver_phase = 2.45_f64;
        let coupling = 0.9_f64;

        let receiver_trials: Vec<TrialRecord> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let wm_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let own_light = base_light + wm_offset;
            let sender_light = 500.0 + amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin();
            let co2 = 3.0 + 4.0 * (i as f64 / n as f64);
            let energy = own_light * 0.3 + co2 * 2.0 + (i as f64 * 0.3) + 5.0 * (i as f64 * 0.08).sin()
                + coupling * sender_light * 0.05;
            let growth = 0.5 + 0.001 * own_light + 0.003 * i as f64 + 0.04 * (i as f64 * 0.12).sin();
            let water = own_light * 0.06 + co2 * 0.5 + (i as f64 * 0.1) + 2.0 * (i as f64 * 0.09).sin()
                + coupling * sender_light * 0.01;
            let ts = format!("2026-05-14 12:{:02}:{:02}", i / 60, i % 60);
            make_trial(i as i32, &ts,
                vec![("light_intensity", own_light), ("co2_injection", co2)],
                vec![growth, energy, water],
                Some((serde_json::json!({"type":"sinusoidal","param_name":"light_intensity","amplitude":amplitude,"period":period as i32,"phase_offset":receiver_phase}).to_string().as_str(), i as i32))
            )
        }).collect();

        let sender_signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin()
        }).collect();

        let obj_names = ["growth_rate", "energy_kwh", "water_liters"];
        eprintln!("\n======== WITH COUPLING 0.9 (double detrend) ========");
        let mut coupling_max_mag = 0.0_f64;
        for obj_idx in 0..3 {
            let quality: Vec<f64> = receiver_trials.iter()
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .map(|t| t.values[obj_idx].unwrap())
                .collect();
            let params: Vec<HashMap<String, f64>> = receiver_trials.iter().map(|t| t.params.clone()).collect();

            let after_knn = param_detrend(&quality, &params);
            let knn_corr = pearson_correlation(&sender_signal, &after_knn);

            let window = (period as usize * 2).max(4).min(after_knn.len());
            let after_double = moving_median_detrend(&after_knn, window);
            let double_corr = pearson_correlation(&sender_signal, &after_double);

            let lockin_knn = lock_in_detect(&after_knn, period, amplitude, sender_phase);
            let lockin_double = lock_in_detect(&after_double, period, amplitude, sender_phase);

            if obj_idx == 1 { coupling_max_mag = lockin_double.magnitude; }

            eprintln!("  obj{} ({}): knn_corr={:.4} double_corr={:.4} | knn_mag={:.4} knn_snr={:.2} | double_mag={:.4} double_snr={:.2}",
                obj_idx, obj_names[obj_idx], knn_corr, double_corr, lockin_knn.magnitude, lockin_knn.snr, lockin_double.magnitude, lockin_double.snr);
        }
        assert!(coupling_max_mag > 0.05, "coupling energy_kwh should survive double detrend, got mag={}", coupling_max_mag);
    }
}
