//! The estate's trial ledger read surface — one decode, both readers.
//!
//! The worker (python, `engine/trial_ledger.py`) writes the tender's
//! `trials` and `ledger_state` tables. This crate is the ONE rust
//! implementation of reading them: the row type, the base query, the
//! attrs decode, and the golden-fixture loader. Observer and causal
//! consume it and keep their own projections above it.
//!
//! The contract is the golden fixture (`ledger_golden.json`, produced
//! by the writer module against real postgres): both this crate's
//! tests and the writer's tests hold the same artifact, so writer and
//! readers agree or CI is red somewhere.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio_postgres::Row;

/// One trial, shaped exactly as the study-era readers exposed it —
/// `values` is a vec (single objective maps to one element), attrs
/// decode to the same JSON values the stamps carried.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialRecord {
    pub number: i32,
    pub kind: String,
    #[serde(default)]
    pub params: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub values: Vec<Option<f64>>,
    pub state: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub user_attrs: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub datetime_start: Option<String>,
    #[serde(default)]
    pub datetime_complete: Option<String>,
}

impl TrialRecord {
    pub fn value(&self) -> Option<f64> {
        self.values.iter().flatten().next().copied()
    }

    pub fn attr_str(&self, key: &str) -> Option<&str> {
        self.user_attrs.get(key).and_then(|v| v.as_str())
    }

    pub fn attr_f64(&self, key: &str) -> Option<f64> {
        self.user_attrs.get(key).and_then(|v| v.as_f64())
    }

    /// The observations stamp: pre-serialized JSON (a string holding a
    /// list), decoded here once — never again per reader.
    pub fn observations(&self) -> Vec<f64> {
        let raw = match self.attr_str("observations") {
            Some(s) => s,
            None => return Vec::new(),
        };
        match serde_json::from_str::<serde_json::Value>(raw) {
            Ok(serde_json::Value::Array(items)) => items
                .into_iter()
                .filter_map(|v| v.as_f64())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// The effectuation params stamp — what the trial actually applied.
    pub fn effectuation_params(&self) -> HashMap<String, serde_json::Value> {
        match self.attr_str("effectuation_params") {
            Some(s) => serde_json::from_str(s).unwrap_or_default(),
            None => HashMap::new(),
        }
    }
}

pub fn record_from_row(row: &Row) -> TrialRecord {
    let value: Option<f64> = row.get("value");
    let kind: String = row.get("kind");
    let state: String = row.get("state");
    let number: i32 = row.get("number");
    let error: Option<String> = row.try_get("error").ok().flatten();
    let params_json: serde_json::Value = row.get("params");
    let attrs_json: serde_json::Value = row.get("attrs");
    let created: Option<String> = row.try_get("created_ts").ok().flatten();
    let finished: Option<String> = row.try_get("finished_ts").ok().flatten();

    TrialRecord {
        number,
        kind,
        params: decode_map(params_json),
        values: value.map(|v| vec![Some(v)]).unwrap_or_default(),
        state,
        error,
        user_attrs: decode_map(attrs_json),
        datetime_start: created,
        datetime_complete: finished,
    }
}

fn decode_map(v: serde_json::Value) -> HashMap<String, serde_json::Value> {
    match v {
        serde_json::Value::Object(m) => m.into_iter().collect(),
        // canonical fixture form: a string holding the JSON object
        serde_json::Value::String(s) => match serde_json::from_str::<serde_json::Value>(&s) {
            Ok(serde_json::Value::Object(m)) => m.into_iter().collect(),
            _ => HashMap::new(),
        },
        _ => HashMap::new(),
    }
}

/// The base query: everything the readers need, in number order. One
/// SELECT replaces the study-era join across five tables.
pub const FETCH_TRIALS_SQL: &str =
    "SELECT number, kind, params, value, state, error, attrs, \
     CAST(created_ts AS TEXT) AS created_ts, \
     CAST(finished_ts AS TEXT) AS finished_ts \
     FROM trials ORDER BY number";

pub const FETCH_TRIALS_PAGED_SQL: &str =
    "SELECT number, kind, params, value, state, error, attrs, \
     CAST(created_ts AS TEXT) AS created_ts, \
     CAST(finished_ts AS TEXT) AS finished_ts \
     FROM trials ORDER BY number OFFSET $1 LIMIT $2";

pub const COUNT_TRIALS_SQL: &str = "SELECT COUNT(*) FROM trials";

pub const FETCH_STATE_SQL: &str =
    "SELECT key, value_json FROM ledger_state ORDER BY key";

/// Seconds since the last trial landed — the stall signal. The
/// study-era two-hour silent stall was invisible for exactly the lack
/// of this number.
pub const LAST_TRIAL_AGE_SQL: &str =
    "SELECT EXTRACT(EPOCH FROM (now() - MAX(created_ts))) FROM trials";

/// The ledger state row, decoded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateEntry {
    pub key: String,
    pub value_json: serde_json::Value,
}

pub fn state_from_row(row: &Row) -> StateEntry {
    StateEntry {
        key: row.get("key"),
        value_json: row.get("value_json"),
    }
}

/// The fixture loader: the golden artifact carries the rows in the
/// canonical writer form; this normalizes them into records.
#[derive(Debug, Deserialize)]
pub struct GoldenFixture {
    pub trials: Vec<GoldenTrial>,
    pub ledger_state: Vec<GoldenState>,
}

#[derive(Debug, Deserialize)]
pub struct GoldenTrial {
    pub number: i32,
    pub kind: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub value: Option<f64>,
    pub state: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub attrs: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct GoldenState {
    pub key: String,
    pub value_json: serde_json::Value,
}

impl GoldenFixture {
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn records(&self) -> Vec<TrialRecord> {
        self.trials
            .iter()
            .map(|t| TrialRecord {
                number: t.number,
                kind: t.kind.clone(),
                params: decode_map(t.params.clone()),
                values: t.value.map(|v| vec![Some(v)]).unwrap_or_default(),
                state: t.state.clone(),
                error: t.error.clone(),
                user_attrs: decode_map(t.attrs.clone()),
                datetime_start: None,
                datetime_complete: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/ledger_golden.json");

    #[test]
    fn golden_fixture_decodes() {
        let fixture = GoldenFixture::parse(FIXTURE).expect("fixture parses");
        let records = fixture.records();
        assert_eq!(records.len(), 2);

        let first = &records[0];
        assert_eq!(first.number, 1);
        assert_eq!(first.state, "COMPLETE");
        assert_eq!(first.kind, "push");
        assert_eq!(first.value(), Some(0.512));
        assert_eq!(first.attr_str("detection_mode"), Some("impulse"));
        assert_eq!(first.attr_f64("impulse_scale"), Some(1.5));
        // pre-serialized JSON stamps decode through the one accessor
        assert_eq!(first.observations(), vec![0.51, 0.49]);
        assert_eq!(
            first.effectuation_params().get("param_0"),
            Some(&serde_json::json!(75.0))
        );

        let second = &records[1];
        assert_eq!(second.state, "FAIL");
        assert_eq!(second.kind, "hold");
        assert_eq!(
            second.error.as_deref(),
            Some("park without neutral params")
        );
    }

    #[test]
    fn classification_by_kind_matches_the_pinned_contract() {
        let fixture = GoldenFixture::parse(FIXTURE).unwrap();
        let records = fixture.records();
        // probe_push stamped attrs -> kind 'push' (the rename class cure)
        assert_eq!(records[0].kind, "push");
        // detection_mode hold, coord_state idle -> receiver hold
        assert_eq!(records[1].kind, "hold");
    }
}
