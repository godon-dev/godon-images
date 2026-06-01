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
        let wm_period = if wm_type == "multi_frequency" {
            wm_meta.get("periods").and_then(|v| v.as_array())
                .and_then(|arr| arr.first().and_then(|p| as_f64(p)))
                .unwrap_or(10.0) as usize
        } else {
            wm_meta.get("period").and_then(|v| as_f64(v)).unwrap_or(10.0) as usize
        };
        // Extract all periods for FFT spectral detection
        let wm_all_periods: Vec<usize> = if wm_type == "multi_frequency" {
            wm_meta.get("periods").and_then(|v| v.as_array())
                .map(|arr| arr.iter()
                    .filter_map(|p| as_f64(p).map(|v| v as usize))
                    .collect())
                .unwrap_or_else(|| vec![wm_period])
        } else {
            vec![wm_period]
        };
        let wm_param_name = wm_meta.get("param_name").and_then(|v| v.as_str()).unwrap_or("light_intensity");
        let wm_amplitude_raw = wm_meta.get("amplitude").and_then(|v| as_f64(v))
            .or_else(|| wm_meta.get("total_amplitude").and_then(|v| as_f64(v)))
            .unwrap_or(0.1);

        // Convergence gating: skip exploration phase where optimizer noise
        // drowns the watermark signal (research shows SNR ~0.75 is breaking point).
        // Use a NON-watermarked parameter for convergence detection, because the
        // watermarked param never converges (the sinusoidal offset keeps it oscillating).
        // Pick the parameter with the strongest convergence transition (highest early/late std ratio).
        let conv_window = 10;
        let all_param_names: Vec<String> = sender_trials.iter()
            .filter(|t| t.state == "COMPLETE")
            .flat_map(|t| t.params.keys())
            .filter(|k| *k != wm_param_name)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .cloned()
            .collect();
        let conv_param_name = all_param_names.iter()
            .filter_map(|pname| {
                let values: Vec<f64> = sender_trials.iter()
                    .filter(|t| t.state == "COMPLETE")
                    .filter_map(|t| t.params.get(pname.as_str()).copied())
                    .collect();
                if values.len() < conv_window * 3 {
                    return None;
                }
                let early_std = {
                    let s = &values[..conv_window * 2];
                    let n = s.len() as f64;
                    let m = s.iter().sum::<f64>() / n;
                    (s.iter().map(|v| (v - m).powi(2)).sum::<f64>() / n).sqrt()
                };
                let late_std = {
                    let s = &values[values.len().saturating_sub(conv_window * 2)..];
                    let n = s.len() as f64;
                    let m = s.iter().sum::<f64>() / n;
                    (s.iter().map(|v| (v - m).powi(2)).sum::<f64>() / n).sqrt()
                };
                if late_std < 1e-12 {
                    return None;
                }
                Some((pname.as_str(), early_std / late_std))
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(name, _ratio)| name)
            .unwrap_or(wm_param_name);
        let cutoff_idx = convergence_cutoff(&sender_trials, conv_param_name, conv_window);
        // Compute param_std on POST-CONVERGENCE trials only.
        // Using all trials inflates the std with exploration noise, tanking the SNR.
        let post_cutoff: Vec<TrialRecord> = match cutoff_idx {
            Some(idx) if idx > 0 => sender_trials.iter().skip(idx).cloned().collect(),
            _ => sender_trials.clone(),
        };
        // Estimate optimizer noise from a NON-watermarked parameter.
        // Using the watermarked param inflates std because the sinusoidal
        // watermark itself contributes A/sqrt(2), capping SNR at ~1.41.
        // A non-watermarked param reflects the true optimizer residual noise.
        let noise_std = compute_param_std(&post_cutoff, conv_param_name, 4);
        let snr_estimate = noise_std.map(|s| if s > 1e-12 { wm_amplitude_raw / s } else { f64::MAX });

        info!(
            "Watermark detection: type={} period={} param={} amp={:.1} conv_param={} cutoff={:?} param_std={:?} snr_est={:?}",
            wm_type, wm_period, wm_param_name, wm_amplitude_raw, conv_param_name, cutoff_idx, noise_std, snr_estimate
        );

        let wm_signal: Vec<f64> = {
            let period = wm_meta.get("period").and_then(|v| as_f64(v)).unwrap_or(20.0);
            let amplitude = wm_meta.get("amplitude").and_then(|v| as_f64(v)).unwrap_or(0.1);
            let phase_offset = wm_meta.get("phase_offset").and_then(|v| as_f64(v)).unwrap_or(0.0);
            let start_idx = cutoff_idx.unwrap_or(0);
            wm_trials.iter()
                .skip(start_idx)
                .map(|t| {
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

        let rcv_wm: Option<(f64, f64, f64, String)> = receiver_trials.iter()
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
                let param_name = wm.get("param_name").and_then(|v| v.as_str()).unwrap_or("light_intensity").to_string();
                Some((amp, per, phase, param_name))
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

            let cleaned = if let Some((rcv_amp, rcv_per, rcv_phase, _)) = rcv_wm {
                let self_subtracted = subtract_self_modulation(
                    &qual_raw, &aligned_quality[..n_align],
                    rcv_per, rcv_amp, rcv_phase,
                );
                self_subtracted
            } else {
                qual_raw.clone()
            };

            let lockin = lock_in_detect(&cleaned, wm_period as f64, wm_amplitude, wm_phase_offset);

            // FFT spectral detection: robust to non-stationary multi-objective optimization.
            // Detects narrowband watermark peaks above the broadband optimizer noise floor.
            let fft = fft_detect(&cleaned, &wm_all_periods);

            // Permutation test for lock-in p-value
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

            // Permutation test for FFT: shuffle destroys periodic structure
            let mut fft_exceed_count = 0usize;
            for _ in 0..n_perm {
                let mut shuffled: Vec<f64> = cleaned.to_vec();
                shuffle_vec(&mut shuffled, &mut rng);
                let perm_fft = fft_detect(&shuffled, &wm_all_periods);
                if perm_fft.snr >= fft.snr {
                    fft_exceed_count += 1;
                }
            }
            let fft_p_value = (fft_exceed_count + 1) as f64 / (n_perm + 1) as f64;

            // Combined detection: use the better of lock-in and FFT
            // FFT is superior for non-converging multi-objective; lock-in for stationary signals
            let fft_detected = fft.n_significant >= 2 && fft_p_value < 0.05;
            let lockin_detected = if lockin.snr > 4.0 {
                lockin.magnitude > 0.05 && p_value < 0.05
            } else if lockin.snr > 2.0 {
                lockin.magnitude > 0.10 && p_value < 0.05
            } else {
                lockin.magnitude > 0.15 && p_value < 0.01
            };
            let detected = fft_detected || lockin_detected;
            let best_method = if fft_detected && (!lockin_detected || fft.snr > lockin.snr) {
                "fft_spectral"
            } else {
                "lock_in"
            };

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
                    "detected": lockin_detected,
                },
                "fft": {
                    "snr": round4(fft.snr),
                    "combined_power": round4(fft.combined_power),
                    "noise_floor": round4(fft.noise_floor),
                    "n_significant": fft.n_significant,
                    "per_freq": fft.per_freq.iter().map(|(p, pw)| serde_json::json!({"period": p, "power": round4(*pw)})).collect::<Vec<_>>(),
                    "detected": fft_detected,
                    "p_value": round4(fft_p_value),
                },
                "best_method": best_method,
                "best_magnitude": round4(if best_method == "fft_spectral" { fft.snr } else { lockin.magnitude }),
                "best_lag": (lockin.phase * wm_period as f64 / (2.0 * std::f64::consts::PI)).round() as i32,
                "p_value": round4(if best_method == "fft_spectral" { fft_p_value } else { p_value }),
                "permutations": n_perm,
                "residuals": cleaned.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
                "sender_signal": sig.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
                "raw_quality": qual_raw.iter().map(|v| round4(*v)).collect::<Vec<f64>>(),
            });

            if detected && !overall_detected {
                overall_detected = true;
                overall_best_corr = if best_method == "fft_spectral" { fft.snr } else { lockin.magnitude };
                overall_best_lag = (lockin.phase * wm_period as f64 / (2.0 * std::f64::consts::PI)).round() as i32;
                overall_p_value = if best_method == "fft_spectral" { fft_p_value } else { p_value };
            } else if !overall_detected {
                let corr = if best_method == "fft_spectral" { fft.snr } else { lockin.magnitude };
                if corr > overall_best_corr {
                    overall_best_corr = corr;
                    overall_best_lag = (lockin.phase * wm_period as f64 / (2.0 * std::f64::consts::PI)).round() as i32;
                    overall_p_value = if best_method == "fft_spectral" { fft_p_value } else { p_value };
                }
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
            "convergence_cutoff": cutoff_idx,
            "noise_std": noise_std.map(|v| round4(v)),
            "snr_estimate": snr_estimate.map(|v| round4(v)),
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

/// Compute the standard deviation of a single parameter across sender trials.
/// Returns None if the parameter is not found or there are too few values.
fn compute_param_std(sender_trials: &[TrialRecord], param_name: &str, min_trials: usize) -> Option<f64> {
    let values: Vec<f64> = sender_trials.iter()
        .filter(|t| t.state == "COMPLETE")
        .filter_map(|t| t.params.get(param_name).copied())
        .collect();
    if values.len() < min_trials {
        return None;
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    Some(variance.sqrt())
}

/// Determine how many initial trials to skip based on convergence analysis.
/// During the exploration phase, the optimizer's parameter variance is high,
/// which drowns the watermark signal. We detect convergence by computing a
/// rolling std of the watermark parameter and finding where it stabilises.
fn convergence_cutoff(sender_trials: &[TrialRecord], param_name: &str, window: usize) -> Option<usize> {
    let values: Vec<f64> = sender_trials.iter()
        .filter(|t| t.state == "COMPLETE")
        .filter_map(|t| t.params.get(param_name).copied())
        .collect();
    if values.len() < window * 2 {
        return None;
    }
    // Compute rolling std with the given window
    let rolling_std: Vec<f64> = (0..values.len().saturating_sub(window) + 1)
        .map(|start| {
            let slice = &values[start..start + window];
            let n = slice.len() as f64;
            let mean = slice.iter().sum::<f64>() / n;
            (slice.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt()
        })
        .collect();
    if rolling_std.len() < 4 {
        return None;
    }
    // Find the point where rolling std drops below the final (converged) std * 1.5
    let final_std = *rolling_std.last().unwrap_or(&0.0);
    if final_std < 1e-12 {
        return None;
    }
    let threshold = final_std * 1.5;
    for (i, &s) in rolling_std.iter().enumerate() {
        if s <= threshold {
            // Map back to trial index — the i-th rolling window starts at trial i
            return Some(i);
        }
    }
    None
}

struct LockInResult {
    magnitude: f64,
    phase: f64,
    snr: f64,
    i_component: f64,
    q_component: f64,
}

struct FftResult {
    /// Combined power at all watermark frequency bins, normalized
    combined_power: f64,
    /// Noise floor estimated from neighboring bins
    noise_floor: f64,
    /// SNR = combined_power / noise_floor
    snr: f64,
    /// Per-frequency bin results (period, power)
    per_freq: Vec<(usize, f64)>,
    /// Number of watermark frequencies that exceed 3x noise floor
    n_significant: usize,
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

/// FFT-based spectral detection for periodic watermarks.
///
/// Computes the power spectrum of the residuals via DFT (Goertzel algorithm
/// at specific frequency bins corresponding to the known watermark periods).
/// The optimizer's exploration is broadband noise — power spread across all
/// frequencies. The watermark is narrowband — concentrated at periods 17, 23,
/// 29, 37. FFT naturally separates these because noise per bin decreases as
/// 1/sqrt(N) while signal power stays concentrated.
///
/// Key advantage over lock-in: no assumption of stationarity or convergence.
/// Works with multi-objective Pareto optimization that never converges.
fn fft_detect(residuals: &[f64], periods: &[usize]) -> FftResult {
    let n = residuals.len();
    if n < 16 || periods.is_empty() {
        return FftResult {
            combined_power: 0.0, noise_floor: 0.0, snr: 0.0,
            per_freq: vec![], n_significant: 0,
        };
    }

    let two_pi = 2.0 * std::f64::consts::PI;

    // Apply Hann window to reduce spectral leakage
    let windowed: Vec<f64> = residuals.iter().enumerate().map(|(i, &v)| {
        let w = 0.5 * (1.0 - (two_pi * i as f64 / n as f64).cos());
        v * w
    }).collect();

    // Goertzel algorithm: compute DFT at specific frequency bins
    // Frequency bin k corresponds to period N/k (in samples)
    // We want period P, so k = N/P
    let mut per_freq: Vec<(usize, f64)> = Vec::new();
    for &period in periods {
        if period == 0 || period >= n { continue; }
        let k = n as f64 / period as f64;

        // Goertzel algorithm
        let coeff = 2.0 * (two_pi * k / n as f64).cos();
        let mut s0 = 0.0_f64;
        let mut s1 = 0.0_f64;
        let mut s2 = 0.0_f64;
        for &v in &windowed {
            s0 = v + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        // DFT value at bin k
        let real = s1 - s2 * (two_pi * k / n as f64).cos();
        let imag = s2 * (two_pi * k / n as f64).sin();
        let power = (real * real + imag * imag) / (n as f64);
        per_freq.push((period, power));
    }

    // Compute noise floor from bins at non-watermark frequencies
    // Use bins offset by ±1 and ±2 from each watermark bin
    let wm_bins: Vec<usize> = periods.iter()
        .filter(|&&p| p > 0 && p < n)
        .map(|&p| (n as f64 / p as f64).round() as usize)
        .collect();

    let mut noise_powers: Vec<f64> = Vec::new();
    for &wm_k in &wm_bins {
        for offset in &[2usize, 3, 4, 5] {
            for sign in &[1i32, -1] {
                let noise_k = (wm_k as i32 + sign * (*offset as i32)).max(1) as usize;
                if noise_k == 0 || noise_k >= n || wm_bins.contains(&noise_k) { continue; }

                let coeff = 2.0 * (two_pi * noise_k as f64 / n as f64).cos();
                let mut s0 = 0.0_f64;
                let mut s1 = 0.0_f64;
                let mut s2 = 0.0_f64;
                for &v in &windowed {
                    s0 = v + coeff * s1 - s2;
                    s2 = s1;
                    s1 = s0;
                }
                let real = s1 - s2 * (two_pi * noise_k as f64 / n as f64).cos();
                let imag = s2 * (two_pi * noise_k as f64 / n as f64).sin();
                let power = (real * real + imag * imag) / (n as f64);
                noise_powers.push(power);
            }
        }
    }

    let noise_floor = if noise_powers.len() >= 4 {
        // Use median of noise bins for robustness
        let mut sorted = noise_powers.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        sorted[sorted.len() / 2]
    } else {
        1e-12
    };

    let combined_power: f64 = per_freq.iter().map(|(_, p)| *p).sum();
    let snr = if noise_floor > 1e-12 { combined_power / noise_floor } else { 0.0 };
    let n_significant = per_freq.iter().filter(|(_, p)| *p > 3.0 * noise_floor).count();

    FftResult {
        combined_power,
        noise_floor,
        snr,
        per_freq,
        n_significant,
    }
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
    let basis: Vec<f64> = (0..n).map(|idx| {
        let trial_idx = indexed_quality.get(idx)
            .and_then(|(_, wm_idx)| *wm_idx)
            .unwrap_or(idx);
        (two_pi * trial_idx as f64 / period + receiver_phase).sin()
    }).collect();

    let mut s0 = 0.0_f64;
    let mut s1 = 0.0_f64;
    let mut s2 = 0.0_f64;
    let mut sb = 0.0_f64;
    let mut sb1 = 0.0_f64;
    let mut sbb = 0.0_f64;
    let mut sq = 0.0_f64;
    let mut sq1 = 0.0_f64;
    let mut sqb = 0.0_f64;
    for i in 0..n {
        let fi = i as f64;
        let q = quality[i];
        let b = basis[i];
        s0 += 1.0;
        s1 += fi;
        s2 += fi * fi;
        sb += b;
        sb1 += fi * b;
        sbb += b * b;
        sq += q;
        sq1 += q * fi;
        sqb += q * b;
    }

    let det_s = s0 * (s2 * sbb - sb1 * sb1)
        - s1 * (s1 * sbb - sb1 * sb)
        + sb * (s1 * sb1 - s2 * sb);
    let beta = if det_s.abs() > 1e-12 {
        let num = s0 * (s2 * sqb - sb1 * sq1)
            - s1 * (s1 * sqb - sb * sq1)
            + sq * (s1 * sb1 - s2 * sb);
        num / det_s
    } else {
        let sum_bb: f64 = sbb - sb * sb / s0;
        if sum_bb > 1e-12 { sqb / sbb } else { 0.0 }
    };

    eprintln!(
        "self-subtraction: beta={:.4} (estimated self_amp={:.2} vs watermark_amp={:.1})",
        beta, beta, amplitude
    );

    quality.iter().enumerate().map(|(idx, &q)| {
        q - beta * basis[idx]
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
    fn test_self_subtraction_removes_pure_self_modulation() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let receiver_phase = 2.45_f64;
        let sensitivity = 0.3_f64;
        let base_light = 500.0_f64;

        let quality: Vec<f64> = (0..n).map(|i| {
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            sensitivity * self_offset
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();

        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);

        let residual_energy: f64 = cleaned.iter().map(|v| v * v).sum();
        let orig_energy: f64 = quality.iter().map(|v| v * v).sum();
        let reduction = 1.0 - residual_energy / orig_energy;
        eprintln!("pure self-mod: orig_energy={:.2} residual_energy={:.4} reduction={:.1}%", orig_energy, residual_energy, reduction * 100.0);
        assert!(reduction > 0.95, "should remove >95% of pure self-modulation energy, got reduction={:.1}%", reduction * 100.0);
    }

    #[test]
    fn test_self_subtraction_preserves_independent_coupling() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let receiver_phase = 2.45_f64;
        let sender_phase = 1.34_f64;
        let sensitivity = 0.3_f64;
        let coupling_strength = 0.9_f64;
        let base_light = 500.0_f64;

        let coupling_signal: Vec<f64> = (0..n).map(|i| {
            coupling_strength * amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin() * 0.05
        }).collect();

        let quality: Vec<f64> = (0..n).map(|i| {
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            sensitivity * self_offset + coupling_signal[i]
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();
        let _params_list: Vec<HashMap<String, f64>> = (0..n).map(|i| {
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let mut p = HashMap::new();
            p.insert("light_intensity".to_string(), base_light + self_offset);
            p
        }).collect();

        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);

        let corr = pearson_correlation(&cleaned, &coupling_signal);
        eprintln!("coupling preservation: corr={:.4}", corr);
        assert!(corr > 0.8, "coupling signal should survive self-subtraction, got corr={}", corr);
    }

    #[test]
    fn test_self_subtraction_with_trend_and_coupling() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let receiver_phase = 2.45_f64;
        let sender_phase = 1.34_f64;
        let sensitivity = 0.3_f64;
        let coupling_strength = 0.9_f64;

        let coupling_signal: Vec<f64> = (0..n).map(|i| {
            coupling_strength * amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin() * 0.05
        }).collect();

        let quality: Vec<f64> = (0..n).map(|i| {
            let trend = 5.0 * i as f64 + 100.0 + 10.0 * (i as f64 * 0.08).sin();
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            trend + sensitivity * self_offset + coupling_signal[i]
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();
        let _params_list: Vec<HashMap<String, f64>> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let mut p = HashMap::new();
            p.insert("light_intensity".to_string(), base_light + self_offset);
            p
        }).collect();

        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);

        let lockin = lock_in_detect(&cleaned, period, amplitude, sender_phase);
        eprintln!("trend+coupling after self-sub: lockin mag={:.4} snr={:.2}", lockin.magnitude, lockin.snr);
        assert!(lockin.magnitude > 0.02, "coupling should be detectable after self-subtraction with trend, got mag={}", lockin.magnitude);
    }

    #[test]
    fn test_self_subtraction_no_coupling_should_not_create_signal() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let receiver_phase = 2.45_f64;
        let sender_phase = 1.34_f64;
        let sensitivity = 0.3_f64;

        let quality: Vec<f64> = (0..n).map(|i| {
            let trend = 5.0 * i as f64 + 100.0 + 10.0 * (i as f64 * 0.08).sin();
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            trend + sensitivity * self_offset
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();

        let lockin_before = lock_in_detect(&quality, period, amplitude, sender_phase);
        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);
        let lockin_after = lock_in_detect(&cleaned, period, amplitude, sender_phase);

        eprintln!("no-coupling: before={:.4} after={:.4}", lockin_before.magnitude, lockin_after.magnitude);
        assert!(lockin_after.magnitude <= lockin_before.magnitude * 1.5,
            "self-subtraction should not create a signal where none exists, before={:.4} after={:.4}",
            lockin_before.magnitude, lockin_after.magnitude);
    }

    #[test]
    fn test_self_subtraction_different_sensitivity_per_objective() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let receiver_phase = 2.45_f64;
        let sender_phase = 1.34_f64;

        let sensitivities = [0.001, 0.3, 0.06];
        let coupling_strengths = [0.0, 0.9, 0.9];
        let obj_names = ["growth", "energy", "water"];

        for obj_idx in 0..3 {
            let quality: Vec<f64> = (0..n).map(|i| {
                let trend = 5.0 * i as f64 + 100.0;
                let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
                let coupling = coupling_strengths[obj_idx] * 500.0 * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin() * 0.05;
                trend + sensitivities[obj_idx] * self_offset + coupling
            }).collect();
            let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();

            let lockin_before = lock_in_detect(&quality, period, amplitude, sender_phase);
            let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);
            let lockin_after = lock_in_detect(&cleaned, period, amplitude, sender_phase);

            eprintln!("  obj{} ({}): sens={} coupling={} | before={:.4} after={:.4}",
                obj_idx, obj_names[obj_idx], sensitivities[obj_idx], coupling_strengths[obj_idx],
                lockin_before.magnitude, lockin_after.magnitude);

            if coupling_strengths[obj_idx] == 0.0 {
                assert!(lockin_after.magnitude <= lockin_before.magnitude * 1.5,
                    "obj{} no coupling: self-sub should not create signal, before={:.4} after={:.4}",
                    obj_idx, lockin_before.magnitude, lockin_after.magnitude);
            }
        }
    }

    #[test]
    fn test_self_subtraction_high_self_leak_removes_false_positive() {
        let n = 80;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let sender_phase = 1.34_f64;
        let receiver_phase = sender_phase + 0.1;
        let sensitivity = 0.3_f64;

        let quality: Vec<f64> = (0..n).map(|i| {
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            sensitivity * self_offset + 3.0 * i as f64
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();

        let lockin_before = lock_in_detect(&quality, period, amplitude, sender_phase);
        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);
        let lockin_after = lock_in_detect(&cleaned, period, amplitude, sender_phase);

        eprintln!("high self-leak (Δφ=0.1): before={:.4} after={:.4}", lockin_before.magnitude, lockin_after.magnitude);
        assert!(lockin_after.magnitude < lockin_before.magnitude,
            "self-subtraction should reduce magnitude, before={:.4} after={:.4}",
            lockin_before.magnitude, lockin_after.magnitude);
    }

    #[test]
    fn test_self_subtraction_same_phase_sender_receiver() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let phase = 1.34_f64;
        let sensitivity = 0.3_f64;
        let coupling_strength = 0.9_f64;

        let coupling_signal: Vec<f64> = (0..n).map(|i| {
            coupling_strength * 500.0 * (2.0 * std::f64::consts::PI * i as f64 / period + phase).sin() * 0.05
        }).collect();

        let quality: Vec<f64> = (0..n).map(|i| {
            let trend = 5.0 * i as f64 + 100.0;
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + phase).sin();
            trend + sensitivity * self_offset + coupling_signal[i]
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();
        let _params_list: Vec<HashMap<String, f64>> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + phase).sin();
            let mut p = HashMap::new();
            p.insert("light_intensity".to_string(), base_light + self_offset);
            p
        }).collect();

        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, phase);

        let lockin = lock_in_detect(&cleaned, period, amplitude, phase);
        eprintln!("same-phase coupling after self-sub: mag={:.4}", lockin.magnitude);

        let corr = pearson_correlation(&cleaned, &coupling_signal);
        eprintln!("coupling correlation after same-phase self-sub: corr={:.4} (same-phase coupling is indistinguishable from self-mod)", corr);
        assert!(lockin.magnitude < 0.5, "same-phase coupling is removed by self-subtraction (expected limitation), got mag={}", lockin.magnitude);
    }

    #[test]
    fn test_self_subtraction_opposite_phase() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let receiver_phase = 1.34_f64;
        let sender_phase = receiver_phase + std::f64::consts::PI;
        let sensitivity = 0.3_f64;

        let quality: Vec<f64> = (0..n).map(|i| {
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            sensitivity * self_offset + 3.0 * i as f64
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();
        let _params_list: Vec<HashMap<String, f64>> = (0..n).map(|i| {
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let mut p = HashMap::new();
            p.insert("light_intensity".to_string(), 500.0 + self_offset);
            p
        }).collect();

        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);

        let lockin = lock_in_detect(&cleaned, period, amplitude, sender_phase);
        eprintln!("opposite-phase no-coupling after self-sub: mag={:.4}", lockin.magnitude);
        assert!(lockin.magnitude < 0.3, "opposite-phase should be clean after self-sub, got mag={}", lockin.magnitude);
    }

    #[test]
    fn test_self_subtraction_edge_cases() {
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let phase = 1.0_f64;

        let empty: Vec<f64> = vec![];
        let empty_indexed: Vec<(f64, Option<usize>)> = vec![];
        let result = subtract_self_modulation(&empty, &empty_indexed, period, amplitude, phase);
        assert!(result.is_empty());

        let short: Vec<f64> = vec![1.0, 2.0, 3.0];
        let short_indexed: Vec<(f64, Option<usize>)> = vec![(1.0, Some(0)), (2.0, Some(1)), (3.0, Some(2))];
        let result = subtract_self_modulation(&short, &short_indexed, period, amplitude, phase);
        assert_eq!(result, short);

        let data: Vec<f64> = (0..10).map(|i| i as f64).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..10).map(|i| (i as f64, Some(i))).collect();
        let result = subtract_self_modulation(&data, &indexed, 0.0, amplitude, phase);
        assert_eq!(result, data);

        let result = subtract_self_modulation(&data, &indexed, period, 0.0, phase);
        assert_eq!(result, data);
    }

    #[test]
    fn test_self_subtraction_noise_robustness() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let receiver_phase = 2.45_f64;
        let sender_phase = 1.34_f64;
        let sensitivity = 0.3_f64;

        let mut rng = fastrand::Rng::new();

        let quality: Vec<f64> = (0..n).map(|i| {
            let trend = 5.0 * i as f64 + 100.0;
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let noise = (rng.f64() - 0.5) * 20.0;
            trend + sensitivity * self_offset + noise
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();
        let _params_list: Vec<HashMap<String, f64>> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let mut p = HashMap::new();
            p.insert("light_intensity".to_string(), base_light + self_offset);
            p
        }).collect();

        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);
        let lockin = lock_in_detect(&cleaned, period, amplitude, sender_phase);

        eprintln!("noisy no-coupling after self-sub: mag={:.4}", lockin.magnitude);
        assert!(lockin.magnitude < 0.5, "noisy data without coupling should not false positive, got mag={}", lockin.magnitude);
    }

    #[test]
    fn test_self_subtraction_realistic_no_coupling() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let sender_phase = 1.34_f64;
        let receiver_phase = 2.45_f64;

        let quality: Vec<f64> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let wm_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let own_light = base_light + wm_offset;
            let co2 = 3.0 + 4.0 * (i as f64 / n as f64);
            own_light * 0.3 + co2 * 2.0 + (i as f64 * 0.3) + 5.0 * (i as f64 * 0.08).sin()
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();
        let _params_list: Vec<HashMap<String, f64>> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let wm_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let mut p = HashMap::new();
            p.insert("light_intensity".to_string(), base_light + wm_offset);
            p.insert("co2_injection".to_string(), 3.0 + 4.0 * (i as f64 / n as f64));
            p
        }).collect();

        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);
        let lockin = lock_in_detect(&cleaned, period, amplitude, sender_phase);

        eprintln!("realistic no-coupling: raw_std={:.2} clean_std={:.2} mag={:.4} snr={:.2}",
            quality.iter().map(|v| (v - quality.iter().sum::<f64>() / n as f64).powi(2)).sum::<f64>() / n as f64,
            cleaned.iter().map(|v| (v - cleaned.iter().sum::<f64>() / n as f64).powi(2)).sum::<f64>() / n as f64,
            lockin.magnitude, lockin.snr);
        assert!(lockin.magnitude < 0.3, "realistic no-coupling should not be detected, got mag={}", lockin.magnitude);
    }

    #[test]
    fn test_self_subtraction_realistic_with_coupling() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let sender_phase = 1.34_f64;
        let receiver_phase = 2.45_f64;
        let coupling = 0.9_f64;

        let sender_signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin()
        }).collect();

        let quality: Vec<f64> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let wm_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let own_light = base_light + wm_offset;
            let sender_light = 500.0 + sender_signal[i];
            let co2 = 3.0 + 4.0 * (i as f64 / n as f64);
            own_light * 0.3 + co2 * 2.0 + (i as f64 * 0.3) + 5.0 * (i as f64 * 0.08).sin()
                + coupling * sender_light * 0.05
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();
        let _params_list: Vec<HashMap<String, f64>> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let wm_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let mut p = HashMap::new();
            p.insert("light_intensity".to_string(), base_light + wm_offset);
            p.insert("co2_injection".to_string(), 3.0 + 4.0 * (i as f64 / n as f64));
            p
        }).collect();

        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);
        let lockin = lock_in_detect(&cleaned, period, amplitude, sender_phase);

        eprintln!("realistic coupling: mag={:.4} snr={:.2}", lockin.magnitude, lockin.snr);
        assert!(lockin.magnitude > 0.02,
            "realistic coupling should be detectable after self-subtraction, got mag={}", lockin.magnitude);
    }

    #[test]
    fn test_self_subtraction_magnitude_comparison() {
        let n = 120;
        let period = 20.0_f64;
        let amplitude = 75.0_f64;
        let sender_phase = 1.34_f64;
        let receiver_phase = 2.45_f64;
        let coupling = 0.9_f64;

        let quality: Vec<f64> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let wm_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let own_light = base_light + wm_offset;
            let sender_light = 500.0 + amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + sender_phase).sin();
            let co2 = 3.0 + 4.0 * (i as f64 / n as f64);
            own_light * 0.3 + co2 * 2.0 + (i as f64 * 0.3) + 5.0 * (i as f64 * 0.08).sin()
                + coupling * sender_light * 0.05
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();
        let _params_list: Vec<HashMap<String, f64>> = (0..n).map(|i| {
            let base_light = 100.0 + 800.0 * (i as f64 / n as f64);
            let wm_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period + receiver_phase).sin();
            let mut p = HashMap::new();
            p.insert("light_intensity".to_string(), base_light + wm_offset);
            p.insert("co2_injection".to_string(), 3.0 + 4.0 * (i as f64 / n as f64));
            p
        }).collect();

        let lockin_raw = lock_in_detect(&quality, period, amplitude, sender_phase);
        let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);
        let lockin_cleaned = lock_in_detect(&cleaned, period, amplitude, sender_phase);

        let self_sub_mag = lock_in_detect(&quality, period, amplitude, receiver_phase).magnitude;

        eprintln!("mag comparison: raw={:.4} self_sub_at_rcv_phase={:.4} cleaned_at_snd_phase={:.4}",
            lockin_raw.magnitude, self_sub_mag, lockin_cleaned.magnitude);
        eprintln!("  self-modulation accounts for {:.1}% of raw lock-in magnitude",
            (self_sub_mag / lockin_raw.magnitude.max(1e-12)) * 100.0);
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
            let quality: Vec<f64> = receiver_trials.iter()
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .map(|t| t.values[obj_idx].unwrap())
                .collect();
            let indexed: Vec<(f64, Option<usize>)> = receiver_trials.iter()
                .enumerate()
                .filter(|(_, t)| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .map(|(i, t)| (t.values[obj_idx].unwrap(), Some(i)))
                .collect();
            let _params_list: Vec<HashMap<String, f64>> = receiver_trials.iter()
                .filter(|t| t.values.get(obj_idx).map_or(false, |v| v.is_some_and(|f| f.is_finite())))
                .map(|t| t.params.clone())
                .collect();

            let cleaned = subtract_self_modulation(&quality, &indexed, period, amplitude, receiver_phase);

            let raw_std = (quality.iter().map(|v| (v - quality.iter().sum::<f64>() / n as f64).powi(2)).sum::<f64>() / n as f64).sqrt();
            let clean_std = (cleaned.iter().map(|v| (v - cleaned.iter().sum::<f64>() / n as f64).powi(2)).sum::<f64>() / n as f64).sqrt();

            let lockin = lock_in_detect(&cleaned, period, amplitude, sender_phase);

            let n_perm = 500usize;
            let mut rng = fastrand::Rng::new();
            let mut exceed_count = 0usize;
            for _ in 0..n_perm {
                let mut shuffled: Vec<f64> = cleaned.clone();
                shuffle_vec(&mut shuffled, &mut rng);
                let perm_lockin = lock_in_detect(&shuffled, period, amplitude, sender_phase);
                if perm_lockin.magnitude >= lockin.magnitude {
                    exceed_count += 1;
                }
            }
            let p_value = (exceed_count + 1) as f64 / (n_perm + 1) as f64;

            let detected = lockin.magnitude > 0.15 && p_value < 0.05;

            eprintln!("  obj{} ({}): raw_std={:.2} clean_std={:.2} reduction={:.1}% | lock_in mag={:.4} phase={:.2} snr={:.2} I={:.4} Q={:.4} p={:.4} detected={} expect={}",
                obj_idx, obj_names[obj_idx], raw_std, clean_std, (1.0 - clean_std / raw_std) * 100.0,
                lockin.magnitude, lockin.phase, lockin.snr, lockin.i_component, lockin.q_component, p_value,
                detected, expect_detection[obj_idx]);

            if !expect_detection[obj_idx] {
                assert!(!detected, "{} obj{} ({}) should NOT be detected: lock_in mag={:.4} snr={:.2} p={:.4}",
                    name, obj_idx, obj_names[obj_idx], lockin.magnitude, lockin.snr, p_value);
            }
        }
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
        run_lockin_diagnostic("no_coupling_60trials", 60, 20.0, 75.0, 1.34, 2.45, [0.0, 0.0, 0.0], "linear", [false, false, false]);
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
        run_lockin_diagnostic("no_coupling_period40", 200, 40.0, 75.0, 1.34, 2.45, [0.0, 0.0, 0.0], "linear", [false, false, false]);
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
    fn test_crossfreq_self_subtraction_preserves_different_freq_coupling() {
        let n = 238;
        let receiver_period = 17.0_f64;
        let sender_period = 23.0_f64;
        let amplitude = 75.0_f64;
        let receiver_phase = 2.45_f64;
        let sender_phase = 1.34_f64;
        let sensitivity = 0.3_f64;
        let coupling_strength = 0.9_f64;
        let coupling_transfer = 0.3;

        let coupling_signal: Vec<f64> = (0..n).map(|i| {
            coupling_strength * amplitude * (2.0 * std::f64::consts::PI * i as f64 / sender_period + sender_phase).sin() * coupling_transfer
        }).collect();

        let quality_with_coupling: Vec<f64> = (0..n).map(|i| {
            let trend = 100.0 + 50.0 * (1.0 - (-3.0 * i as f64 / n as f64).exp());
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / receiver_period + receiver_phase).sin();
            trend + sensitivity * self_offset + coupling_signal[i]
        }).collect();

        let quality_without_coupling: Vec<f64> = (0..n).map(|i| {
            let trend = 100.0 + 50.0 * (1.0 - (-3.0 * i as f64 / n as f64).exp());
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / receiver_period + receiver_phase).sin();
            trend + sensitivity * self_offset
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality_with_coupling[i], Some(i))).collect();
        let indexed_no: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality_without_coupling[i], Some(i))).collect();

        let cleaned_with = subtract_self_modulation(&quality_with_coupling, &indexed, receiver_period, amplitude, receiver_phase);
        let cleaned_without = subtract_self_modulation(&quality_without_coupling, &indexed_no, receiver_period, amplitude, receiver_phase);

        let lockin_with = lock_in_detect(&cleaned_with, sender_period, amplitude, sender_phase);
        let lockin_without = lock_in_detect(&cleaned_without, sender_period, amplitude, sender_phase);

        eprintln!("crossfreq with coupling: mag={:.4}, without: mag={:.4}", lockin_with.magnitude, lockin_without.magnitude);
        assert!(lockin_with.magnitude > 2.0 * lockin_without.magnitude,
            "coupling should make lock-in magnitude at least 2x the no-coupling baseline, got with={:.4} without={:.4}",
            lockin_with.magnitude, lockin_without.magnitude);
    }

    #[test]
    fn test_crossfreq_self_subtraction_no_false_positive() {
        let n = 17 * 14;
        let receiver_period = 17.0_f64;
        let sender_period = 23.0_f64;
        let amplitude = 75.0_f64;
        let receiver_phase = 2.45_f64;
        let sender_phase = 1.34_f64;
        let sensitivity = 0.3_f64;

        let quality: Vec<f64> = (0..n).map(|i| {
            let trend = 100.0 + 50.0 * (1.0 - (-3.0 * i as f64 / n as f64).exp());
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / receiver_period + receiver_phase).sin();
            trend + sensitivity * self_offset
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();

        let cleaned = subtract_self_modulation(&quality, &indexed, receiver_period, amplitude, receiver_phase);

        let lockin = lock_in_detect(&cleaned, sender_period, amplitude, sender_phase);
        eprintln!("crossfreq no-coupling: mag={:.4}", lockin.magnitude);
        assert!(lockin.magnitude < 0.3, "no coupling at different freq should not false positive, got mag={}", lockin.magnitude);
    }

    #[test]
    fn test_crossfreq_coupling_detected_cleanly() {
        let n = 23 * 10;
        let receiver_period = 17.0_f64;
        let sender_period = 23.0_f64;
        let amplitude = 75.0_f64;
        let receiver_phase = 2.45_f64;
        let sender_phase = 1.34_f64;
        let sensitivity = 0.3_f64;
        let coupling_strength = 0.9_f64;
        let coupling_transfer = 0.3;

        let coupling_signal: Vec<f64> = (0..n).map(|i| {
            coupling_strength * amplitude * (2.0 * std::f64::consts::PI * i as f64 / sender_period + sender_phase).sin() * coupling_transfer
        }).collect();

        let quality: Vec<f64> = (0..n).map(|i| {
            let trend = 100.0 + 50.0 * (1.0 - (-3.0 * i as f64 / n as f64).exp());
            let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / receiver_period + receiver_phase).sin();
            trend + sensitivity * self_offset + coupling_signal[i]
        }).collect();
        let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();

        let cleaned = subtract_self_modulation(&quality, &indexed, receiver_period, amplitude, receiver_phase);

        let lockin = lock_in_detect(&cleaned, sender_period, amplitude, sender_phase);
        eprintln!("crossfreq coupling detected: mag={:.4} snr={:.2}", lockin.magnitude, lockin.snr);
        assert!(lockin.magnitude > 0.1, "different-freq coupling should be clearly detected, got mag={}", lockin.magnitude);

        let n_perm = 500usize;
        let mut rng = fastrand::Rng::new();
        let mut exceed_count = 0usize;
        for _ in 0..n_perm {
            let mut shuffled: Vec<f64> = cleaned.clone();
            shuffle_vec(&mut shuffled, &mut rng);
            let perm_lockin = lock_in_detect(&shuffled, sender_period, amplitude, sender_phase);
            if perm_lockin.magnitude >= lockin.magnitude {
                exceed_count += 1;
            }
        }
        let p_value = (exceed_count + 1) as f64 / (n_perm + 1) as f64;
        eprintln!("crossfreq coupling p-value: {:.4}", p_value);
        assert!(p_value < 0.05, "coupling should be statistically significant, got p={}", p_value);
    }

    #[test]
    fn test_crossfreq_all_period_pairs() {
        let periods = [17.0_f64, 23.0, 29.0, 37.0];
        let amplitude = 75.0_f64;
        let n = 23 * 29;

        for (rx_period, tx_period) in periods.iter().flat_map(|rx| periods.iter().map(move |tx| (*rx, *tx))) {
            if rx_period == tx_period {
                continue;
            }

            let receiver_phase = 2.45_f64;
            let sender_phase = 1.34_f64;
            let coupling_strength = 0.9_f64;

            let quality: Vec<f64> = (0..n).map(|i| {
                let trend = 100.0 + 50.0 * (1.0 - (-3.0 * i as f64 / n as f64).exp());
                let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / rx_period + receiver_phase).sin();
                let coupling = coupling_strength * amplitude * (2.0 * std::f64::consts::PI * i as f64 / tx_period + sender_phase).sin() * 0.3;
                trend + 0.3 * self_offset + coupling
            }).collect();
            let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();

            let cleaned = subtract_self_modulation(&quality, &indexed, rx_period, amplitude, receiver_phase);
            let lockin = lock_in_detect(&cleaned, tx_period, amplitude, sender_phase);

            eprintln!("rx_period={} tx_period={}: mag={:.4}", rx_period, tx_period, lockin.magnitude);
            assert!(lockin.magnitude > 0.1,
                "coupling should be detected for rx={} tx={}, got mag={}", rx_period, tx_period, lockin.magnitude);
        }
    }

    #[test]
    fn test_crossfreq_no_coupling_all_period_pairs() {
        let periods = [17.0_f64, 23.0, 29.0, 37.0];
        let amplitude = 75.0_f64;
        let n = 23 * 29;

        for (rx_period, tx_period) in periods.iter().flat_map(|rx| periods.iter().map(move |tx| (*rx, *tx))) {
            if rx_period == tx_period {
                continue;
            }

            let receiver_phase = 2.45_f64;
            let sender_phase = 1.34_f64;

            let quality: Vec<f64> = (0..n).map(|i| {
                let trend = 100.0 + 50.0 * (1.0 - (-3.0 * i as f64 / n as f64).exp());
                let self_offset = amplitude * (2.0 * std::f64::consts::PI * i as f64 / rx_period + receiver_phase).sin();
                trend + 0.3 * self_offset
            }).collect();
            let indexed: Vec<(f64, Option<usize>)> = (0..n).map(|i| (quality[i], Some(i))).collect();

            let cleaned = subtract_self_modulation(&quality, &indexed, rx_period, amplitude, receiver_phase);
            let lockin = lock_in_detect(&cleaned, tx_period, amplitude, sender_phase);

            eprintln!("no-coupling rx_period={} tx_period={}: mag={:.4}", rx_period, tx_period, lockin.magnitude);
            assert!(lockin.magnitude < 0.3,
                "no-coupling should not false positive for rx={} tx={}, got mag={}", rx_period, tx_period, lockin.magnitude);
        }
    }

    #[test]
    fn test_fft_pure_sinusoid_detection() {
        // Generate a clean sinusoid at period 17 — FFT should find strong peak
        let n = 200;
        let period = 17_usize;
        let amplitude = 1.0_f64;
        let signal: Vec<f64> = (0..n).map(|i| {
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period as f64).sin()
        }).collect();

        let fft = super::fft_detect(&signal, &[period]);
        assert!(fft.snr > 5.0, "pure sinusoid FFT SNR should be >> 1, got {}", fft.snr);
        assert_eq!(fft.n_significant, 1, "should have 1 significant frequency");
    }

    #[test]
    fn test_fft_sinusoid_in_noise() {
        // Sinusoid buried in Gaussian noise — FFT should still detect via narrowband peak
        let n = 280;
        let period = 23_usize;
        let amplitude = 10.0_f64;
        let noise_std = 50.0_f64;
        // Deterministic "noise" from a simple LCG to avoid randomness in tests
        let signal: Vec<f64> = (0..n).map(|i| {
            let noise = noise_std * (((i as f64 * 1103515245.0 + 12345.0) % 2147483648.0) / 2147483648.0 - 0.5) * 2.0;
            amplitude * (2.0 * std::f64::consts::PI * i as f64 / period as f64).sin() + noise
        }).collect();

        let fft = super::fft_detect(&signal, &[period]);
        assert!(fft.snr > 2.0, "sinusoid in noise should have FFT SNR > 2, got {}", fft.snr);
    }

    #[test]
    fn test_fft_multi_frequency_detection() {
        // Multi-frequency watermark: periods 17 and 23
        let n = 280;
        let periods = [17_usize, 23];
        let amplitude = 5.0_f64;
        let signal: Vec<f64> = (0..n).map(|i| {
            let mut v = 0.0;
            for &p in &periods {
                v += (amplitude / periods.len() as f64) * (2.0 * std::f64::consts::PI * i as f64 / p as f64).sin();
            }
            v
        }).collect();

        let fft = super::fft_detect(&signal, &periods);
        assert!(fft.n_significant >= 2, "should detect both watermark frequencies, got {} significant", fft.n_significant);
        assert!(fft.snr > 3.0, "multi-frequency FFT SNR should be > 3, got {}", fft.snr);
    }

    #[test]
    fn test_fft_with_linear_trend() {
        // Sinusoid + strong linear trend — FFT should handle this because
        // the trend is low-frequency energy, not at periods 17-23
        let n = 280;
        let period = 17_usize;
        let amplitude = 5.0_f64;
        let signal: Vec<f64> = (0..n).map(|i| {
            let trend = 1000.0 * i as f64 / n as f64; // strong linear trend 0 -> 1000
            let wm = amplitude * (2.0 * std::f64::consts::PI * i as f64 / period as f64).sin();
            trend + wm
        }).collect();

        let fft = super::fft_detect(&signal, &[period]);
        // The trend is low-frequency, not at period 17. Watermark should still be detected.
        assert!(fft.snr > 2.0, "FFT should detect watermark despite trend, got SNR={}", fft.snr);
    }
}
