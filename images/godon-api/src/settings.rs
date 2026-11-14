//! The estate's settings namespace (settled 2026-11-13, the
//! holder's word): ONE live tree,
//! served by this API only. Storage is the platform's own variable
//! store — windmill variables at `f/settings/{leaf}`, one variable
//! per leaf, content = a self-describing envelope. Charts demoted to
//! the install-time SEED: the seeder writes these leaves at install
//! and nothing else ever seeds them.
//!
//! Two subtrees, one rule between them:
//! - `policy/` — numbers that change what measurements MEAN. Moves
//!   are GUARDED: refused while any tender is live (the mid-room
//!   forbid; rooms stamp the values they ran under, receipts cite
//!   them).
//! - `config/` — plumbing. Plain writes.
//!
//! Leaves are DECLARED in the registry below — the tree's grammar
//! lives in code; PUT to an unknown leaf is a 404, nothing free-form.
//! Every move is validated and logged (the loki stream carries the
//! SETTINGS MOVE line).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Config;
use crate::handlers::get_client;
use crate::types::ErrorResponse;
use crate::windmill_adapter::WindmillClient;

// ─── the registry (the tree's grammar) ──────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subtree {
    Policy,
    /// the plumbing subtree — plain writes. No members yet; the
    /// variant is the grammar (GET answers both subtrees, the guard
    /// rule names the pair).
    #[allow(dead_code)]
    Config,
}

impl Subtree {
    fn name(&self) -> &'static str {
        match self {
            Subtree::Policy => "policy",
            Subtree::Config => "config",
        }
    }
}

pub struct LeafDef {
    /// path under the settings root, e.g. "policy/convergence_fraction"
    pub path: &'static str,
    pub subtree: Subtree,
    pub description: &'static str,
    /// validation: finite f64 strictly inside (lo, hi)
    pub range: (f64, f64),
}

/// First member: `policy/convergence_fraction` — the #309 cure. The
/// declared precision as a DIMENSIONLESS FRACTION of each objective's
/// own measured scale; the engine freezes it at spawn (tender
/// startup) and holds it for the tender's whole life. The SEED value
/// (what a clean-slate reinstall plants) is the seeder's concern —
/// charts hands it to the seeder, never read again.
pub const LEAVES: &[LeafDef] = &[LeafDef {
    path: "policy/convergence_fraction",
    subtree: Subtree::Policy,
    description: "declared precision as a fraction of each objective's own measured scale (#309 honest units); frozen at spawn per tender, estate default moved between rooms",
    range: (0.0, 1.0),
}];

pub fn leaf(path: &str) -> Option<&'static LeafDef> {
    LEAVES.iter().find(|l| l.path == path)
}

pub fn variable_path(leaf_path: &str) -> String {
    format!("f/settings/{leaf_path}")
}

// ─── the envelope (what each variable holds) ────────────────────

/// The content of each settings variable: the value plus where it
/// came from and when it last moved — everything GET owes, with zero
/// extra platform surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub value: f64,
    /// seed (written by the seeder at install) | api (moved via PUT)
    pub origin: String,
    #[serde(rename = "movedAt")]
    pub moved_at: String,
    #[serde(rename = "movedBy")]
    pub moved_by: String,
}

impl Envelope {
    pub fn moved(value: f64) -> Self {
        Envelope {
            value,
            origin: "api".to_string(),
            moved_at: chrono::Utc::now().to_rfc3339(),
            moved_by: "api".to_string(),
        }
    }
}

// ─── adapter surface (the platform variable store) ──────────────

use base64::Engine as _;

impl WindmillClient {
    /// Read one leaf's envelope; None while the variable is absent
    /// (origin "unset" — an unseeded estate) or empty (deleted).
    ///
    /// The envelope rides the variable as BASE64 — a quote-free
    /// carrier. Receipt (2026-11-13, this build): wmill-1.659's
    /// get_variable_raw double-escapes any content containing quote
    /// characters (its from_str_patched wraps, escapes quotes but
    /// not backslashes, then slices the outer chars) — JSON content
    /// cannot round-trip; credentials never carried quotes, so the
    /// defect slept until now. Base64 rides the platform's own door
    /// with zero hand-rolling and no quotes to mangle.
    pub fn get_setting(&self, leaf_path: &str) -> anyhow::Result<Option<Envelope>> {
        let raw = match self.client.get_variable_raw(&variable_path(leaf_path)) {
            Ok(raw) => raw,
            // wmill-1.659 seam: a missing variable surfaces as
            // ApiError carrying the 404 text (the SDK flattens the
            // status into a string). Absence is the UNSET leaf, not
            // an error — everything else fails loud.
            Err(e @ wmill::SdkError::ApiError(_)) if e.to_string().contains("404") => {
                return Ok(None);
            }
            Err(e) => {
                return Err(anyhow::anyhow!("settings read failed at {leaf_path}: {e}"));
            }
        };
        if raw.trim().is_empty() {
            return Ok(None);
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(raw.trim().as_bytes())
            .map_err(|e| anyhow::anyhow!("settings carrier is not base64 at {leaf_path}: {e}"))?;
        serde_json::from_slice(&decoded)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("settings envelope unreadable at {leaf_path}: {e}"))
    }

    pub fn set_setting(&self, leaf_path: &str, envelope: &Envelope) -> anyhow::Result<()> {
        let carrier = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_string(envelope).unwrap_or_default());
        self.client
            .set_variable(carrier, &variable_path(leaf_path), false)
            .map_err(|e| anyhow::anyhow!("settings write failed at {leaf_path}: {e}"))?;
        Ok(())
    }
}

// ─── the policy guard ───────────────────────────────────────────

/// A tender is LIVE while creating/active/running (case-insensitive:
/// the controller's own stubs show both "active" and "ACTIVE").
/// Parked, stopped, finished and failed tenders hold their frozen
/// numbers — the estate default moves BETWEEN rooms by design.
fn is_live_status(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "creating" | "active" | "running"
    )
}

fn live_tenders(client: &WindmillClient) -> anyhow::Result<Vec<String>> {
    Ok(client
        .list_systemtenders()?
        .into_iter()
        .filter(|s| is_live_status(&s.status))
        .map(|s| s.name)
        .collect())
}

// ─── doors ──────────────────────────────────────────────────────

fn leaf_json(def: &LeafDef, envelope: Option<&Envelope>) -> Value {
    let mut v = match envelope {
        Some(e) => serde_json::to_value(e).unwrap_or_else(|_| json!({})),
        None => json!({"origin": "unset"}),
    };
    if let Some(obj) = v.as_object_mut() {
        obj.insert("path".into(), json!(def.path));
        obj.insert("description".into(), json!(def.description));
    }
    v
}

/// GET /settings — the whole tree: both subtrees, every leaf with its
/// envelope. A windmill read failure fails the read loudly (the
/// namespace never silently sleeps).
pub async fn get_tree(
    State(_config): State<Config>,
) -> Result<Json<Value>, (StatusCode, Json<ErrorResponse>)> {
    let client = get_client()?;
    let leaves = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Value>> {
        LEAVES
            .iter()
            .map(|def| {
                let envelope = client.get_setting(def.path)?;
                Ok(leaf_json(def, envelope.as_ref()))
            })
            .collect()
    })
    .await
    .map_err(join_err)?
    .map_err(settings_read_err)?;

    let mut tree = json!({"policy": {}, "config": {}});
    for (def, value) in LEAVES.iter().zip(leaves) {
        let subtree = tree
            .get_mut(def.subtree.name())
            .and_then(|s| s.as_object_mut())
            .expect("both subtrees exist above");
        subtree.insert(
            def.path.rsplit('/').next().unwrap_or(def.path).to_string(),
            value,
        );
    }
    Ok(Json(tree))
}

/// GET /settings/{*path} — one leaf (the engine's spawn-time read).
pub async fn get_leaf(
    State(_config): State<Config>,
    Path(path): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<ErrorResponse>)> {
    let Some(def) = leaf(&path) else {
        return Err(unknown_leaf(&path));
    };
    let client = get_client()?;
    let envelope = tokio::task::spawn_blocking(move || client.get_setting(def.path))
        .await
        .map_err(join_err)?
        .map_err(settings_read_err)?;
    Ok(Json(leaf_json(def, envelope.as_ref())))
}

#[derive(Debug, Deserialize)]
pub struct SettingPut {
    pub value: f64,
}

/// PUT /settings/{*path} — move ONE leaf. Validated against the
/// registry; `policy/` moves are refused (409) while any tender is
/// live; every move is logged.
pub async fn put_leaf(
    State(_config): State<Config>,
    Path(path): Path<String>,
    Json(payload): Json<SettingPut>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<ErrorResponse>)> {
    let Some(def) = leaf(&path) else {
        return Err(unknown_leaf(&path));
    };
    let (lo, hi) = def.range;
    if !payload.value.is_finite() || payload.value <= lo || payload.value >= hi {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse::new(
                format!(
                    "value {} outside the leaf's range ({}, {}) — {}",
                    payload.value, lo, hi, def.path
                ),
                "SETTING_VALUE_OUT_OF_RANGE",
            )),
        ));
    }

    let client = get_client()?;
    let result = tokio::task::spawn_blocking(move || {
        // the mid-room forbid: policy moves wait for quiet
        if def.subtree == Subtree::Policy {
            let live = live_tenders(&client)?;
            if !live.is_empty() {
                return Ok(Err((
                    StatusCode::CONFLICT,
                    Json(ErrorResponse::with_details(
                        format!(
                            "policy move refused while tenders are live: {} — the estate default moves BETWEEN rooms (mid-room forbid, settlement 2026-11-13)",
                            live.join(", ")
                        ),
                        "POLICY_MOVE_MID_ROOM",
                        json!({"live": live}),
                    )),
                )));
            }
        }
        let previous = client.get_setting(def.path)?;
        let previous_origin = previous.as_ref().map(|e| e.origin.clone());
        let envelope = Envelope::moved(payload.value);
        client.set_setting(def.path, &envelope)?;
        anyhow::Ok(Ok((envelope, previous_origin)))
    })
    .await
    .map_err(join_err)?
    .map_err(settings_write_err)?;

    let (envelope, previous_origin) = result?;
    log::info!(
        "SETTINGS MOVE: {} -> {} ({}; origin {} -> api)",
        def.path,
        payload.value,
        def.subtree.name(),
        previous_origin.as_deref().unwrap_or("unset")
    );
    let mut body = serde_json::to_value(&envelope).unwrap_or_else(|_| json!({}));
    if let Some(obj) = body.as_object_mut() {
        obj.insert("path".into(), json!(def.path));
        obj.insert("description".into(), json!(def.description));
    }
    Ok((StatusCode::OK, Json(body)))
}

fn unknown_leaf(path: &str) -> (StatusCode, Json<ErrorResponse>) {
    let known: Vec<&str> = LEAVES.iter().map(|l| l.path).collect();
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse::with_details(
            format!("unknown settings leaf: {path}"),
            "SETTING_UNKNOWN",
            json!({ "known": known }),
        )),
    )
}

fn join_err(e: tokio::task::JoinError) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(
            format!("Settings task join error: {e}"),
            "INTERNAL_SERVER_ERROR",
        )),
    )
}

fn settings_read_err(e: anyhow::Error) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(
            format!("Failed to read settings: {e}"),
            "INTERNAL_SERVER_ERROR",
        )),
    )
}

fn settings_write_err(e: anyhow::Error) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(
            format!("Failed to move setting: {e}"),
            "INTERNAL_SERVER_ERROR",
        )),
    )
}

// ─── tests (one env window, sequential scenarios — env is global) ───

#[cfg(test)]
mod settings_tests {
    use super::*;
    use crate::windmill_adapter::tests_support::FakeWindmill;

    const ENVELOPE: &str =
        r#"{"value":0.02,"origin":"seed","movedAt":"2026-11-13T00:00:00Z","movedBy":"seeder"}"#;

    /// The wire body windmill serves for a string variable: the
    /// content JSON-quoted. Content itself is the base64 carrier.
    fn quoted(s: &str) -> String {
        serde_json::to_string(&serde_json::json!(s)).unwrap()
    }

    fn b64(s: &str) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    /// Wire body for envelope content: JSON-quoted base64 carrier.
    fn envelope_wire(envelope_json: &str) -> String {
        quoted(&b64(envelope_json))
    }

    /// The variable-metadata object set_variable's existence check
    /// reads before it updates.
    fn variable_meta() -> String {
        r#"{"path":"f/settings/policy/convergence_fraction","value":"x","is_secret":false,"description":"","created_at":"2026-11-13T00:00:00.000Z","updated_at":"2026-11-13T00:00:00.000Z","schema":null,"account":null,"is_oauth":false,"expires_at":null}"#.into()
    }

    fn env_at(f: &FakeWindmill) {
        std::env::set_var("WINDMILL_BASE_URL", f.base_url());
        std::env::set_var("BASE_INTERNAL_URL", f.base_url());
        std::env::set_var("WM_TOKEN", "test-token");
        std::env::set_var("WM_WORKSPACE", "ws");
        std::env::set_var("WINDMILL_FOLDER", "controller");
    }

    /// The controller script's RESULT: {"data": [summary rows]}.
    fn tender_list(statuses: &[(&str, &str)]) -> String {
        let rows: Vec<String> = statuses
            .iter()
            .map(|(id, st)| {
                format!(r#"{{"id":"{id}","name":"{id}","status":"{st}","createdAt":"t"}}"#)
            })
            .collect();
        format!(r#"{{"data":[{}]}}"#, rows.join(","))
    }

    fn job() -> (u16, String) {
        (200, "123e4567-e89b-12d3-a456-426614174000".into())
    }

    fn done(result: &str) -> (u16, String) {
        (
            200,
            format!(r#"{{"completed":true,"success":true,"result":{}}}"#, result),
        )
    }

    /// One controller-script call on the wire: enqueue (job id) + the
    /// completed job carrying the script's result.
    fn script_run(result: &str) -> Vec<(u16, String)> {
        vec![job(), done(result)]
    }

    #[test]
    fn the_settings_doors_speak_their_contracts() {
        let _env = crate::windmill_adapter::tests_support::ENV_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async { the_settings_doors_scenarios().await });
    }

    async fn the_settings_doors_scenarios() {
        // GET tree: one variable read per leaf
        let f = FakeWindmill::start(vec![(200, envelope_wire(ENVELOPE))]);
        env_at(&f);
        let tree = get_tree(State(Config { port: 8080 })).await.unwrap().0;
        assert_eq!(tree["policy"]["convergence_fraction"]["value"], json!(0.02));
        assert_eq!(
            tree["policy"]["convergence_fraction"]["origin"],
            json!("seed")
        );
        assert_eq!(tree["config"], json!({}));
        assert!(
            f.recorded().iter().any(|(_, p)| p.contains("settings")),
            "the leaf read rides the platform variable store: {:?}",
            f.recorded()
        );

        // GET leaf: unset estate (the variable absent, 404) answers
        // origin unset, no value
        let f = FakeWindmill::start(vec![(404, String::new())]);
        env_at(&f);
        let leaf_body = get_leaf(
            State(Config { port: 8080 }),
            Path("policy/convergence_fraction".into()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(leaf_body["origin"], json!("unset"));
        assert!(leaf_body.get("value").is_none());

        // GET unknown leaf: 404 with the known list
        let f = FakeWindmill::start(vec![]);
        env_at(&f);
        let err = get_leaf(State(Config { port: 8080 }), Path("policy/nonsense".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert!(
            f.recorded().is_empty(),
            "unknown leaves never touch the platform"
        );

        // PUT validation: outside (0,1) refused at the door, zero wire
        let f = FakeWindmill::start(vec![]);
        env_at(&f);
        let err = put_leaf(
            State(Config { port: 8080 }),
            Path("policy/convergence_fraction".into()),
            Json(SettingPut { value: 1.5 }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(f.recorded().is_empty());

        // PUT policy move while a tender is live: 409, names the tender
        let f = FakeWindmill::start(script_run(&tender_list(&[("st-1", "ACTIVE")])));
        env_at(&f);
        let err = put_leaf(
            State(Config { port: 8080 }),
            Path("policy/convergence_fraction".into()),
            Json(SettingPut { value: 0.03 }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1 .0.code, "POLICY_MOVE_MID_ROOM");
        assert!(err.1 .0.message.contains("st-1"));

        // PUT policy move on a quiet estate: guard reads tenders, the
        // current envelope, the variable's existence, then writes —
        // parked/finished/failed are NOT live
        let f = FakeWindmill::start(vec![
            job(),
            done(&tender_list(&[("st-1", "finished"), ("st-2", "stopped")])),
            (200, envelope_wire(ENVELOPE)),
            (200, variable_meta()),
            (200, "\"ok\"".into()),
        ]);
        env_at(&f);
        let (status, body) = put_leaf(
            State(Config { port: 8080 }),
            Path("policy/convergence_fraction".into()),
            Json(SettingPut { value: 0.03 }),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["value"], json!(0.03));
        assert_eq!(body["origin"], json!("api"));
        // the write rides the platform's variable create/update door;
        // the leaf's path and the b64 carrier travel in the body
        let writes: Vec<Envelope> = f
            .bodies("variables/create")
            .into_iter()
            .chain(f.bodies("variables/update"))
            .filter_map(|b| {
                let path_is_our_leaf = b
                    .get("path")
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| p.contains("f/settings/"));
                let carrier = b.get("value").and_then(|v| v.as_str())?;
                if !path_is_our_leaf {
                    return None;
                }
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(carrier)
                    .ok()
                    .and_then(|d| serde_json::from_slice::<Envelope>(&d).ok())
            })
            .filter(|e: &Envelope| e.origin == "api")
            .collect();
        assert_eq!(writes.len(), 1, "exactly one leaf write");
        assert_eq!(writes[0].value, 0.03);

        std::env::remove_var("WINDMILL_BASE_URL");
        std::env::remove_var("BASE_INTERNAL_URL");
    }

    #[test]
    fn live_statuses_match_the_controller_vocabulary() {
        assert!(is_live_status("creating"));
        assert!(is_live_status("active"));
        assert!(is_live_status("ACTIVE"));
        assert!(is_live_status("running"));
        assert!(!is_live_status("finished"));
        assert!(!is_live_status("stopped"));
        assert!(!is_live_status("create-failed"));
        assert!(!is_live_status("deletion-failed"));
    }
}
