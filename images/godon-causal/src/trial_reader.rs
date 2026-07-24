use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio_postgres::{Error, Row};

// ─── DB Config & Reader ─────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DbConfig {
    user: String,
    password: String,
    host: String,
    port: u16,
}

pub struct TrialReader {
    config: DbConfig,
}

impl TrialReader {
    pub fn from_env() -> Self {
        Self {
            config: DbConfig {
                user: std::env::var("GODON_ARCHIVE_DB_USER")
                    .unwrap_or_else(|_| "yugabyte".into()),
                password: std::env::var("GODON_ARCHIVE_DB_PASSWORD")
                    .unwrap_or_else(|_| "yugabyte".into()),
                host: std::env::var("GODON_ARCHIVE_DB_SERVICE_HOST")
                    .unwrap_or_else(|_| "yb-tserver-0".into()),
                port: std::env::var("GODON_ARCHIVE_DB_SERVICE_PORT")
                    .ok()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(5433),
            },
        }
    }

    pub fn breeder_db_name(breeder_id: &str) -> String {
        format!("breeder_{}", breeder_id.replace('-', "_"))
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

    pub async fn health_check(&self) -> bool {
        match self.connect("yugabyte").await {
            Ok(_) => true,
            Err(e) => {
                debug!("DB health check failed: {}", e);
                false
            }
        }
    }

    // ─── List all breeder IDs ────────────────────────────────────────

    pub async fn list_breeders(&self) -> Result<Vec<String>, Error> {
        let client = self.connect("yugabyte").await?;
        let rows = client
            .query("SELECT study_name FROM studies ORDER BY study_name", &[])
            .await?;

        // Study names look like "uuid_study" — extract the UUID part
        let mut breeders = Vec::new();
        for row in &rows {
            let study_name: String = row.get(0);
            // Strip _study suffix to get breeder UUID
            if let Some(uuid) = study_name.strip_suffix("_study") {
                breeders.push(uuid.to_string());
            }
        }
        Ok(breeders)
    }

    // ─── Read all trials for a breeder ───────────────────────────────

    pub async fn read_trials(&self, breeder_id: &str) -> Result<Vec<TrialRecord>, Error> {
        let db = Self::breeder_db_name(breeder_id);
        let client = self.connect(&db).await?;

        let study_name = format!("{}_study", breeder_id);

        let trial_rows = client
            .query(
                "SELECT t.trial_id, t.number, CAST(t.state AS TEXT), \
                 CAST(t.datetime_start AS TEXT), CAST(t.datetime_complete AS TEXT) \
                 FROM trials t \
                 JOIN studies s ON t.study_id = s.study_id \
                 WHERE s.study_name = $1 \
                 ORDER BY t.number",
                &[&study_name],
            )
            .await?;

        let mut trials = Vec::new();
        for row in &trial_rows {
            let trial_id: i32 = row.get(0);
            let record = self.build_trial_record(&client, trial_id, row).await?;
            trials.push(record);
        }

        info!("Loaded {} trials for breeder {}", trials.len(), breeder_id);
        Ok(trials)
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
                "SELECT param_name, param_value FROM trial_params WHERE trial_id = $1",
                &[&trial_id],
            )
            .await?;

        let mut params = HashMap::new();
        for pr in &param_rows {
            let name: String = pr.get(0);
            let value: f64 = pr.get(1);
            params.insert(name, value);
        }

        let value_rows = client
            .query(
                "SELECT objective, value, CAST(value_type AS TEXT) FROM trial_values \
                 WHERE trial_id = $1 ORDER BY objective",
                &[&trial_id],
            )
            .await?;

        let mut values = Vec::new();
        if !value_rows.is_empty() {
            let max_obj = value_rows
                .iter()
                .map(|r: &Row| r.get::<_, i32>(0))
                .max()
                .unwrap_or(0);
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
            values,
            user_attrs,
        })
    }

    // ─── Read classified probe trials ────────────────────────────────

    pub async fn read_probe_trials(&self, breeder_id: &str) -> Result<ProbeTrials, Error> {
        let trials = self.read_trials(breeder_id).await?;
        Ok(ProbeTrials::from_trials(breeder_id, &trials))
    }
}

// ─── Trial Record ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialRecord {
    pub number: i32,
    pub state: String,
    pub datetime_start: Option<String>,
    pub datetime_complete: Option<String>,
    pub params: HashMap<String, f64>,
    pub values: Vec<Option<f64>>,
    pub user_attrs: HashMap<String, serde_json::Value>,
}

// ─── Classified Probe Trials ────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProbeTrials {
    pub breeder_id: String,
    pub push_trials: Vec<ProbeTrial>,
    pub pause_trials: Vec<ProbeTrial>,
    pub hold_calib_trials: Vec<ProbeTrial>,
    pub receiver_hold_trials: Vec<ReceiverTrial>,
}

impl ProbeTrials {
    pub fn from_trials(breeder_id: &str, trials: &[TrialRecord]) -> Self {
        let mut push_trials = Vec::new();
        let mut pause_trials = Vec::new();
        let mut hold_calib_trials = Vec::new();
        let mut receiver_hold_trials = Vec::new();

        for t in trials {
            if t.state != "COMPLETE" {
                continue;
            }

            let phase = t
                .user_attrs
                .get("impulse_phase")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let detection_mode = t
                .user_attrs
                .get("detection_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let coord_state = t
                .user_attrs
                .get("coord_state")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let ts = t
                .datetime_start
                .as_ref()
                .and_then(|s| parse_timestamp_secs(s));

            let values: Vec<f64> = t.values.iter().filter_map(|v| *v).collect();
            if values.is_empty() {
                continue;
            }

            let observations = extract_observations(&t.user_attrs);
            let impulse_scale = t
                .user_attrs
                .get("impulse_scale")
                .and_then(|v| v.as_f64())
                .unwrap_or(1.0);

            let timestamp = match ts {
                Some(ts) => ts,
                None => continue,
            };

            match phase {
                "push" => push_trials.push(ProbeTrial {
                    timestamp,
                    trial_number: t.number,
                    params: t.params.clone(),
                    values: values.clone(),
                    observations: observations.clone(),
                    impulse_scale,
                }),
                "pause" => pause_trials.push(ProbeTrial {
                    timestamp,
                    trial_number: t.number,
                    params: t.params.clone(),
                    values: values.clone(),
                    observations: observations.clone(),
                    impulse_scale,
                }),
                "hold_calib" => hold_calib_trials.push(ProbeTrial {
                    timestamp,
                    trial_number: t.number,
                    params: t.params.clone(),
                    values: values.clone(),
                    observations: observations.clone(),
                    impulse_scale,
                }),
                _ => {}
            }

            // Receiver hold: detection_mode == "hold" AND not sender's own hold_calib
            if detection_mode == "hold" && coord_state != "hold_calib" {
                let lease_phase = t
                    .user_attrs
                    .get("lease_phase")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        t.user_attrs
                            .get("impulse_phase")
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or("")
                    .to_string();

                receiver_hold_trials.push(ReceiverTrial {
                    timestamp,
                    trial_number: t.number,
                    values: values.clone(),
                    observations,
                    phase: lease_phase,
                });
            }
        }

        Self {
            breeder_id: breeder_id.to_string(),
            push_trials,
            pause_trials,
            hold_calib_trials,
            receiver_hold_trials,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProbeTrial {
    pub timestamp: f64,
    pub trial_number: i32,
    pub params: HashMap<String, f64>,
    pub values: Vec<f64>,
    pub observations: Vec<f64>,
    pub impulse_scale: f64,
}

#[derive(Debug, Clone)]
pub struct ReceiverTrial {
    pub timestamp: f64,
    pub trial_number: i32,
    pub values: Vec<f64>,
    pub observations: Vec<f64>,
    pub phase: String,
}

// ─── Helpers (ported from observer) ─────────────────────────────────

fn extract_observations(user_attrs: &HashMap<String, serde_json::Value>) -> Vec<f64> {
    let raw = match user_attrs.get("observations") {
        Some(v) => v,
        None => return Vec::new(),
    };

    // observations may be a JSON string or already-parsed object
    let parsed: serde_json::Value = if let Some(s) = raw.as_str() {
        serde_json::from_str(s).unwrap_or(serde_json::Value::Null)
    } else {
        raw.clone()
    };

    let mut result = Vec::new();
    if let Some(obj) = parsed.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for key in keys {
            if let Some(v) = obj.get(key).and_then(|vv| vv.as_f64()) {
                result.push(v);
            }
        }
    }
    result
}

pub fn parse_timestamp_secs(ts: &str) -> Option<f64> {
    let normalized = ts.trim().replace(' ', "T");
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

    let time_part = parts[1];
    let (time_str, tz_offset_secs) = if let Some(pos) = time_part.find(|c| c == '+' || c == '-') {
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

    let days_from_2000 = (year - 2000.0) * 365.25;
    let month_days = [
        0.0, 31.0, 59.0, 90.0, 120.0, 151.0, 181.0, 212.0, 243.0, 273.0, 304.0, 334.0,
    ];
    let day_of_year = month_days
        .get((month as usize).saturating_sub(1).min(11))
        .copied()
        .unwrap_or(0.0)
        + day;
    let epoch = (days_from_2000 + day_of_year) * 86400.0
        + hour * 3600.0
        + minute * 60.0
        + second
        + tz_offset_secs.unwrap_or(0.0);
    Some(epoch)
}

fn parse_tz_offset(s: &str) -> Option<f64> {
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

pub fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut sorted = v.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

pub fn mad(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let m = median(v);
    let deviations: Vec<f64> = v.iter().map(|x| (x - m).abs()).collect();
    median(&deviations) * 1.4826
}
