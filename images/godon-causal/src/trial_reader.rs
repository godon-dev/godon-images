use ledger_reader::{record_from_row, TrialRecord, FETCH_TRIALS_SQL};
pub use ledger_reader::TrialRecord as LedgerTrialRecord;
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio_postgres::Error;

// ─── DB Config & Reader ─────────────────────────────────────────────

#[derive(Debug, Clone)]
struct DbConfig {
    user: String,
    password: String,
    host: String,
    port: u16,
}

#[derive(Debug, Clone)]
pub struct TrialReader {
    config: DbConfig,
}

impl TrialReader {
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
        }
    }

    pub fn systemtender_db_name(systemtender_id: &str) -> String {
        format!("systemtender_{}", systemtender_id.replace('-', "_"))
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

    /// Connect to the archive DB (curve persistence + observation reads).
    pub async fn connect_archive(&self) -> Result<tokio_postgres::Client, Error> {
        self.connect("archive_db").await
    }

    // ─── Read receiver observations within a time window ────────────

    /// Groupless observation read for the wish doors: rows of ONE
    /// receiver, keyed by its globally-unique uuid, regardless of
    /// which inference group banked them. The wish speaks uuids, not
    /// rooms — the judge must not be blind to readings taken in a
    /// group the groupless wish never named (found live, Sep 19:
    /// judge read group 'default', world banked under
    /// 'bench-characterization').
    pub async fn read_receiver_observations_for(
        &self,
        receiver_id: &str,
        start_epoch: f64,
        end_epoch: f64,
    ) -> Result<std::collections::HashMap<String, Vec<f64>>, Error> {
        let client = self.connect("archive_db").await?;

        let rows = client
            .query(
                "SELECT CAST(objective_values AS TEXT), lease_phase FROM receiver_observations \
                 WHERE receiver_id = $3::varchar \
                 AND lease_phase IS NOT NULL \
                 AND EXTRACT(EPOCH FROM written_at) >= $1::double precision \
                 AND EXTRACT(EPOCH FROM written_at) <= $2::double precision \
                 ORDER BY written_at",
                &[&start_epoch, &end_epoch, &receiver_id],
            )
            .await?;

        let mut values: std::collections::HashMap<String, Vec<f64>> =
            std::collections::HashMap::new();
        for row in &rows {
            // Verdicts are taken at rest: a reading banked while the
            // sender's push excites the receiver measures the push, not
            // the wish. Only settled (non-push) readings are evidence
            // (found live, round 10: the judge sampled the push transient
            // at n=3, median 0.83 -> missed, 15s after the sender adopted).
            let phase: String = row.get(1);
            if !is_settled_phase(&phase) {
                continue;
            }
            let json_str: String = row.get(0);
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&json_str) {
                if let Some(map) = obj.as_object() {
                    for (ch, vv) in map {
                        if let Some(v) = vv.as_f64() {
                            values.entry(ch.clone()).or_default().push(v);
                        }
                    }
                }
            }
        }
        debug!(
            "read_receiver_observations_for: receiver={} window=[{:.0}, {:.0}] -> {} channels",
            receiver_id,
            start_epoch,
            end_epoch,
            values.len()
        );
        Ok(values)
    }

    /// Per-receiver, per-channel series: receiver_id -> channel -> ordered
    /// readings. The probe window holds rows from EVERY holding receiver;
    /// grouping by receiver keeps each listener's response its own — the
    /// shift of B must never mix into C's medians (multi-receiver mingling).
    pub async fn read_receiver_observations(
        &self,
        group_id: &str,
        sender_id: &str,
        start_epoch: f64,
        end_epoch: f64,
    ) -> Result<std::collections::HashMap<String, std::collections::HashMap<String, Vec<f64>>>, Error>
    {
        let client = self.connect("archive_db").await?;

        // Receiver rows only: same group, not the sender's own self-reads,
        // and phase-tagged (receiver HOLD trials carry the observed lease
        // phase; the sender's pause trials write untagged rows with
        // mode 'hold', which poisoned shift medians before this filter —
        // the run-23 +0.339 phantom on a dead param).
        let rows = client
            .query(
                "SELECT receiver_id, CAST(objective_values AS TEXT) FROM receiver_observations \
                 WHERE group_id = $3::varchar \
                 AND receiver_id != $4::varchar \
                 AND lease_phase IS NOT NULL \
                 AND EXTRACT(EPOCH FROM written_at) >= $1::double precision \
                 AND EXTRACT(EPOCH FROM written_at) <= $2::double precision \
                 ORDER BY written_at",
                &[&start_epoch, &end_epoch, &group_id, &sender_id],
            )
            .await?;

        // Per-receiver per-channel series. Every configured objective key
        // in the row becomes a channel — no hardcoded objective_0.
        let mut values: std::collections::HashMap<
            String,
            std::collections::HashMap<String, Vec<f64>>,
        > = std::collections::HashMap::new();
        for row in &rows {
            let receiver_id: String = row.get(0);
            let json_str: String = row.get(1);
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&json_str) {
                if let Some(map) = obj.as_object() {
                    let per_recv = values.entry(receiver_id).or_default();
                    for (ch, vv) in map {
                        if let Some(v) = vv.as_f64() {
                            per_recv.entry(ch.clone()).or_default().push(v);
                        }
                    }
                }
            }
        }

        debug!(
            "read_receiver_observations: group={} window=[{:.0}, {:.0}] -> {} receivers",
            group_id,
            start_epoch,
            end_epoch,
            values.len()
        );
        Ok(values)
    }

    /// The sender's own node readings in a window — self-rows the
    /// systemtender publishes during its push and pause trials
    /// (receiver_id = the walking sender). Per-channel series; these
    /// bank the self-curve and its baseline (pause window = the
    /// sender at neutral).
    pub async fn read_self_observations(
        &self,
        group_id: &str,
        sender_id: &str,
        start_epoch: f64,
        end_epoch: f64,
    ) -> Result<std::collections::HashMap<String, Vec<f64>>, Error> {
        let client = self.connect("archive_db").await?;

        let rows = client
            .query(
                "SELECT CAST(objective_values AS TEXT) FROM receiver_observations \
                 WHERE group_id = $3::varchar \
                 AND receiver_id = $4::varchar \
                 AND lease_phase IS NOT NULL \
                 AND EXTRACT(EPOCH FROM written_at) >= $1::double precision \
                 AND EXTRACT(EPOCH FROM written_at) <= $2::double precision \
                 ORDER BY written_at",
                &[&start_epoch, &end_epoch, &group_id, &sender_id],
            )
            .await?;

        let mut values: std::collections::HashMap<String, Vec<f64>> =
            std::collections::HashMap::new();
        for row in &rows {
            let json_str: String = row.get(0);
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&json_str) {
                if let Some(map) = obj.as_object() {
                    for (ch, vv) in map {
                        if let Some(v) = vv.as_f64() {
                            values.entry(ch.clone()).or_default().push(v);
                        }
                    }
                }
            }
        }
        Ok(values)
    }

    /// The group's standing dials — each systemtender's applied params,
    /// refreshed per trial in the heartbeat table. This IS the
    /// constellation, assembled from purely local publications.
    /// Returns {systemtender_id: params} as a JSON value.
    pub async fn read_standing_params(&self, group_id: &str) -> Result<serde_json::Value, Error> {
        let client = self.connect("archive_db").await?;

        let rows = client
            .query(
                "SELECT systemtender_id, CAST(params AS TEXT) \
                 FROM interference_active_systemtenders \
                 WHERE group_id = $1::varchar AND params IS NOT NULL \
                 AND last_seen > NOW() - INTERVAL '600 seconds'",
                &[&group_id],
            )
            .await?;

        let mut map = serde_json::Map::new();
        for row in &rows {
            let systemtender_id: String = row.get(0);
            let params_str: String = row.get(1);
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&params_str) {
                map.insert(systemtender_id, v);
            }
        }
        Ok(serde_json::Value::Object(map))
    }

    // ─── List all systemtender IDs ────────────────────────────────────────

    pub async fn list_systemtenders(&self) -> Result<Vec<String>, Error> {
        let client = self.connect("yugabyte").await?;
        let rows = client
            .query("SELECT datname FROM pg_database WHERE datname LIKE 'systemtender_%' ORDER BY datname", &[])
            .await?;

        let mut systemtenders = Vec::new();
        for row in &rows {
            let dbname: String = row.get(0);
            // Strip systemtender_ prefix to get UUID with dashes
            if let Some(uuid) = dbname.strip_prefix("systemtender_") {
                systemtenders.push(uuid.replace('_', "-"));
            }
        }
        Ok(systemtenders)
    }

    // ─── Read all trials for a systemtender ───────────────────────────────

    pub async fn read_trials(&self, systemtender_id: &str) -> Result<Vec<TrialRecord>, Error> {
        let db = Self::systemtender_db_name(systemtender_id);
        let client = self.connect(&db).await?;

        let rows = client.query(FETCH_TRIALS_SQL, &[]).await?;
        let trials: Vec<TrialRecord> = rows.iter().map(record_from_row).collect();

        info!(
            "Loaded {} trials for systemtender {}",
            trials.len(),
            systemtender_id
        );
        Ok(trials)
    }

    pub async fn read_probe_trials(&self, systemtender_id: &str) -> Result<ProbeTrials, Error> {
        let trials = self.read_trials(systemtender_id).await?;
        Ok(ProbeTrials::from_trials(systemtender_id, &trials))
    }
}


// ─── Classified Probe Trials ────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProbeTrials {
    pub systemtender_id: String,
    pub push_trials: Vec<ProbeTrial>,
    pub pause_trials: Vec<ProbeTrial>,
    pub hold_calib_trials: Vec<ProbeTrial>,
    pub receiver_hold_trials: Vec<ReceiverTrial>,
}

impl ProbeTrials {
    pub fn from_trials(systemtender_id: &str, trials: &[TrialRecord]) -> Self {
        let mut push_trials = Vec::new();
        let mut pause_trials = Vec::new();
        let mut hold_calib_trials = Vec::new();
        let mut receiver_hold_trials = Vec::new();

        for t in trials {
            if t.state != "COMPLETE" {
                continue;
            }

            let ts = t
                .datetime_start
                .as_ref()
                .and_then(|s| parse_timestamp_secs(s));

            let values: Vec<f64> = t.values.iter().filter_map(|v| *v).collect();
            if values.is_empty() {
                continue;
            }

            let observations = t.observations();
            let impulse_scale = t.attr_f64("impulse_scale").unwrap_or(1.0);

            let timestamp = match ts {
                Some(ts) => ts,
                None => continue,
            };

            // kind is written once by the worker (classify) — no more
            // string-matching attr literals to bucket a trial (a stamp
            // rename once starved this reader silently for two days).
            let params: HashMap<String, f64> = t
                .params
                .iter()
                .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
                .collect();

            match t.kind.as_str() {
                "push" => push_trials.push(ProbeTrial {
                    timestamp,
                    trial_number: t.number as i32,
                    params: params.clone(),
                    values: values.clone(),
                    observations: observations.clone(),
                    impulse_scale,
                }),
                "pause" => pause_trials.push(ProbeTrial {
                    timestamp,
                    trial_number: t.number as i32,
                    params: params.clone(),
                    values: values.clone(),
                    observations: observations.clone(),
                    impulse_scale,
                }),
                "hold_calib" => hold_calib_trials.push(ProbeTrial {
                    timestamp,
                    trial_number: t.number as i32,
                    params: params.clone(),
                    values: values.clone(),
                    observations: observations.clone(),
                    impulse_scale,
                }),
                "hold" => {
                    let lease_phase = t
                        .attr_str("lease_phase")
                        .or_else(|| t.attr_str("impulse_phase"))
                        .unwrap_or("")
                        .to_string();
                    receiver_hold_trials.push(ReceiverTrial {
                        timestamp,
                        trial_number: t.number as i32,
                        values: values.clone(),
                        observations,
                        phase: lease_phase,
                    });
                }
                _ => {}
            }
        }

        Self {
            systemtender_id: systemtender_id.to_string(),
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

/// Verdicts are taken at rest. `probe_push` is the sender's excitation
/// stroke - readings banked under it measure the stroke, not the wish.
/// Everything else (probe_pause, hold, ...) is settled. Default-include:
/// an unknown phase is evidence until proven excitation.
pub fn is_settled_phase(phase: &str) -> bool {
    phase != "probe_push"
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

#[cfg(test)]
mod settled_phase_tests {
    use super::*;

    #[test]
    fn push_is_the_only_excitation() {
        assert!(!is_settled_phase("probe_push"));
        assert!(is_settled_phase("probe_pause"));
        assert!(is_settled_phase("hold"));
        // unknown phases default to evidence: excitation must be named
        assert!(is_settled_phase("walk_probe"));
        assert!(is_settled_phase(""));
    }
}

#[cfg(test)]
mod kind_classification_tests {
    //! History: the re-walk rung renamed the coordinator's phase stamps
    //! (push -> probe_push); the reader classified by string-matching
    //! attr literals and starved silently for two days (runs with "0
    //! push, 0 pause, 160 receiver_hold"). The own-ledger rung moved
    //! classification into a `kind` column written once by the worker
    //! (classify()) — these tests pin that attrs, however renamed or
    //! absent, can no longer drive or starve the bucketing.

    use super::*;

    fn trial(kind: &str, phase_attr: Option<&str>) -> TrialRecord {
        let mut user_attrs = HashMap::new();
        if let Some(phase) = phase_attr {
            user_attrs.insert("impulse_phase".to_string(), serde_json::json!(phase));
        }
        TrialRecord {
            number: 1,
            kind: kind.to_string(),
            state: "COMPLETE".to_string(),
            datetime_start: Some("2026-09-18 20:00:00".to_string()),
            datetime_complete: None,
            params: HashMap::new(),
            values: vec![Some(0.5)],
            error: None,
            user_attrs,
        }
    }

    #[test]
    fn kind_classifies_regardless_of_attr_vocabulary() {
        for (kind, phase_attr) in [
            ("push", Some("push")),
            ("push", Some("probe_push")),
            ("push", None),
            ("push", Some("anything_renamed")),
        ] {
            let probe = ProbeTrials::from_trials("s", &[trial(kind, phase_attr)]);
            assert_eq!(
                probe.push_trials.len(), 1,
                "kind={kind} with attr={phase_attr:?} must classify as push"
            );
        }
    }

    #[test]
    fn receiver_hold_takes_phase_from_lease() {
        let probe = ProbeTrials::from_trials(
            "s",
            &[trial("hold", Some("probe_hold"))],
        );
        assert_eq!(probe.receiver_hold_trials.len(), 1);
        assert_eq!(probe.receiver_hold_trials[0].phase, "probe_hold");
    }

    #[test]
    fn unknown_kind_buckets_nowhere() {
        let probe = ProbeTrials::from_trials("s", &[trial("idle", None)]);
        assert!(probe.push_trials.is_empty());
        assert!(probe.pause_trials.is_empty());
        assert!(probe.hold_calib_trials.is_empty());
        assert!(probe.receiver_hold_trials.is_empty());
    }
}
