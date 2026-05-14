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

        let rcv_wm: Option<(f64, f64, f64)> = receiver_trials.iter()
            .filter(|t| t.user_attrs.contains_key("watermark"))
            .filter_map(|t| {
                let raw = &t.user_attrs["watermark"];
                let wm: serde_json::Value = if raw.is_string() {
                    serde_json::from_str(raw.as_str().unwrap_or("{}")).ok()?
                } else {
                    raw.clone()
                };
                let amp = wm.get("amplitude").and_then(|v| as_f64(v))?;
                let per = wm.get("period").and_then(|v| as_f64(v))?;
                let phase = wm.get("phase_offset").and_then(|v| as_f64(v)).unwrap_or(0.0);
                Some((amp, per, phase))
            })
            .next();

        let wm_amplitude = wm_meta.get("amplitude").and_then(|v| as_f64(v)).unwrap_or(0.1);
        let wm_phase_offset = wm_meta.get("phase_offset").and_then(|v| as_f64(v)).unwrap_or(0.0);

        for obj_idx in 0..n_obj {
            let receiver_quality: Vec<(String, f64, Option<usize>)> = receiver_trials.iter()
                .filter(|t| t.state == "COMPLETE")
                .filter(|t| t.datetime_start.is_some())
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .filter_map(|t| {
                    let ts = t.datetime_start.clone()?;
                    let val = t.values.get(obj_idx).and_then(|v| *v)?;
                    let wm_idx = t.user_attrs.get("watermark_trial_idx")
                        .and_then(|v| as_f64(v)).map(|v| v as usize);
                    Some((ts, val, wm_idx))
                })
                .collect();

            if receiver_quality.len() < 4 { continue; }

            let rcv_start = receiver_quality.first().map(|(ts, _, _)| ts.as_str()).unwrap_or("");
            let rcv_end = receiver_quality.last().map(|(ts, _, _)| ts.as_str()).unwrap_or("");
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

            let aligned_quality: Vec<(f64, Option<usize>)> = receiver_quality.iter()
                .filter(|(ts, _, _)| ts.as_str() >= overlap_start && ts.as_str() <= overlap_end)
                .map(|(_, v, idx)| (*v, *idx))
                .collect();

            let n_align = aligned_signal.len().min(aligned_quality.len());
            if n_align < 4 { continue; }

            let sig = &aligned_signal[..n_align];
            let qual_raw: Vec<f64> = aligned_quality[..n_align].iter().map(|(v, _)| *v).collect();

            let cleaned = if let Some((rcv_amp, rcv_per, rcv_phase)) = rcv_wm {
                let self_subtracted = subtract_self_modulation(
                    &qual_raw, &aligned_quality[..n_align],
                    rcv_per, rcv_amp, rcv_phase,
                );
                self_subtracted
            } else {
                qual_raw.clone()
            };

            let lockin = lock_in_detect(&cleaned, wm_period as f64, wm_amplitude, wm_phase_offset);

            let n_perm = 5000usize;
            let mut rng = fastrand::Rng::new();
            let mut exceed_count = 0usize;
            for _ in 0..n_perm {
                let mut shuffled: Vec<f64> = cleaned.to_vec();
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
                "residuals": cleaned.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
                "sender_signal": sig.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
                "raw_quality": qual_raw.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
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

fn subtract_self_modulation(
    quality: &[f64],
    indexed_quality: &[(f64, Option<usize>)],
    period: f64,
    amplitude: f64,
    receiver_phase: f64,
) -> Vec<f64> {
    let n = quality.len();
    if n < 4 || period <= 0.0 || amplitude <= 0.0 {
        return quality.to_vec();
    }

    let two_pi = 2.0 * std::f64::consts::PI;
    let mut i_sum = 0.0_f64;
    let mut q_sum = 0.0_f64;
    for (idx, &q) in quality.iter().enumerate() {
        let trial_idx = indexed_quality.get(idx)
            .and_then(|(_, wm_idx)| *wm_idx)
            .unwrap_or(idx);
        let phase = two_pi * trial_idx as f64 / period + receiver_phase;
        i_sum += q * phase.cos();
        q_sum += q * phase.sin();
    }
    i_sum /= (n as f64) * amplitude / 2.0;
    q_sum /= (n as f64) * amplitude / 2.0;

    let self_i = i_sum;
    let self_q = q_sum;
    let self_mag = (self_i * self_i + self_q * self_q).sqrt();

    eprintln!(
        "self-modulation: mag={:.4} I={:.4} Q={:.4}",
        self_mag, self_i, self_q
    );

    let norm_factor = (n as f64) * amplitude / 2.0;
    quality.iter().enumerate().map(|(idx, &q)| {
        let trial_idx = indexed_quality.get(idx)
            .and_then(|(_, wm_idx)| *wm_idx)
            .unwrap_or(idx);
        let phase = two_pi * trial_idx as f64 / period + receiver_phase;
        let self_component = norm_factor * (self_i * phase.cos() + self_q * phase.sin());
        q - self_component
    }).collect()
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
}
