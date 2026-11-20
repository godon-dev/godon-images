// godon-bench-generic — Configurable Synthetic Coupling Bench
//
// A benchmark target for the godon optimization engine that provides
// GROUND TRUTH coupling. The coupling topology, strengths, nonlinearity,
// and noise are all configurable. This allows systematic validation of
// the causal scanner: you plant the coupling, the scanner recovers it,
// you compare.
//
// Multiple virtual nodes run inside one container. Each node is exposed
// as a route prefix: /node-1/apply, /node-2/metrics, etc.
//
// CONFIGURATION
//
// Mount a topology.yaml config file:
//   volumes:
//     - ./topology.yaml:/config/topology.yaml
//   environment:
//     CONFIG_PATH: /config/topology.yaml
//
// ENDPOINTS (per node)
//
//   POST /{node_id}/apply          Apply params, compute objectives
//   GET  /{node_id}/metrics/json    Current objectives as JSON
//   GET  /{node_id}/status          Current objectives (for coupling fetch)
//   POST /{node_id}/reset           Reset node to initial state
//
// GLOBAL ENDPOINTS
//
//   GET  /health                    Liveness + node list
//   GET  /config                    Return the loaded topology (ground truth)
//
// DRIFT ENDPOINTS (per node)
//
//   POST /{node_id}/drift/start     Cue a live readout drift NOW:
//                                   body {rate_per_second, cap?, name?}.
//                                   200 first cue, 409 if a door drift is
//                                   already running on the node. The ramp
//                                   runs on WALL TIME — the world's speed
//                                   is not an artifact of trial cadence.
//   GET  /{node_id}/drift/state     Running drifts (door + declared),
//                                   cue stamps in BOTH clocks (uptime
//                                   seconds + tick), current offsets.

mod sim;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use log::info;
use serde::{Deserialize, Serialize};
use sim::{BenchConfig, SharedSimulator, Simulator};
use std::net::SocketAddr;
use std::sync::Arc;

// ─── Request/Response Types ─────────────────────────────────────────

/// POST /{node_id}/apply — accepts named params as a flat dict.
/// The systemtender sends {"param_0": 50.0, "param_1": 50.0, ...}.
/// We convert to array internally based on config.
#[derive(Debug, Deserialize)]
struct ApplyRequest {
    #[serde(flatten)]
    params: std::collections::HashMap<String, f64>,
}

/// GET /{node_id}/metrics/json — returns named scalar objectives.
/// objective_0, objective_1, etc. — built dynamically from config.
/// The systemtender reads via `key: objective_N` (scalar, not array).
fn build_metrics_response(node_id: &str, tick: u64, objectives: &[f64]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "node_id".to_string(),
        serde_json::Value::String(node_id.to_string()),
    );
    map.insert(
        "tick".to_string(),
        serde_json::Value::Number(serde_json::Number::from(tick)),
    );
    for (i, val) in objectives.iter().enumerate() {
        if let Some(n) = serde_json::Number::from_f64(*val) {
            map.insert(format!("objective_{}", i), serde_json::Value::Number(n));
        }
    }
    serde_json::Value::Object(map)
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: String,
    nodes: Vec<String>,
    edges: usize,
}

// ─── App State ──────────────────────────────────────────────────────

struct AppState {
    sim: SharedSimulator,
    config: BenchConfig,
}

// ─── Handlers ───────────────────────────────────────────────────────

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let sim = state.sim.lock().unwrap();
    Json(HealthResponse {
        status: "ok".to_string(),
        nodes: sim.list_nodes(),
        edges: state.config.edges.len(),
    })
}

async fn get_config(State(state): State<Arc<AppState>>) -> Json<BenchConfig> {
    Json(state.config.clone())
}

async fn apply(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
    Json(req): Json<ApplyRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    // Convert named params dict to array based on config
    let Some(node_config) = state.config.nodes.iter().find(|n| n.id == node_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("unknown node: {node_id}") })),
        );
    };

    let mut param_array = vec![node_config.param_lower; node_config.params];
    for (key, val) in &req.params {
        // Try to parse param_N format
        if let Some(idx_str) = key.strip_prefix("param_") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if idx < param_array.len() {
                    param_array[idx] = *val;
                }
            }
        }
    }

    let mut sim = state.sim.lock().unwrap();
    let objectives = sim.apply(&node_id, &param_array);
    let tick = sim.nodes.get(&node_id).map(|n| n.tick).unwrap_or(0);
    info!(
        "Node {} applied {} params -> objectives: {:?}",
        node_id,
        param_array.len(),
        objectives
    );
    (
        StatusCode::OK,
        Json(build_metrics_response(&node_id, tick, &objectives)),
    )
}

async fn metrics_json(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
) -> Json<serde_json::Value> {
    let mut sim = state.sim.lock().unwrap();
    let objectives = sim.get_status(&node_id).unwrap_or_default();
    let tick = sim.nodes.get(&node_id).map(|n| n.tick).unwrap_or(0);
    Json(build_metrics_response(&node_id, tick, &objectives))
}

async fn status(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
) -> Json<serde_json::Value> {
    let mut sim = state.sim.lock().unwrap();
    let objectives = sim.get_status(&node_id).unwrap_or_default();
    let tick = sim.nodes.get(&node_id).map(|n| n.tick).unwrap_or(0);
    Json(build_metrics_response(&node_id, tick, &objectives))
}

/// GET /{node_id}/truth — the ground-truth read. Noise-free objectives
/// at the current tick (readout bias included), the node's effective
/// shape, and every edge's effective strength. This is the answer key
/// validation runs check the engine's verdicts against. Read-only: no
/// tick advances, no noise is drawn.
async fn truth(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let sim = state.sim.lock().unwrap();
    match sim.truth(&node_id) {
        Some(t) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "node_id": t.node_id,
                "total_ticks": t.total_ticks,
                "uptime_secs": t.uptime_secs,
                "objectives": t.objectives,
                "effective_weights": t.effective_weights,
                "base_from": t.base_from,
                "base_to": t.base_to,
                "base_progress": t.base_progress,
                "offset_effective": t.offset_effective,
                "noise_effective": t.noise_effective,
                "exogenous_effective": t.exogenous_effective,
                "edges": t.edges.iter().map(|e| serde_json::json!({
                    "from": e.from,
                    "to": e.to,
                    "to_channel": e.to_channel,
                    "strength_effective": e.strength_effective,
                })).collect::<Vec<_>>(),
            })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("unknown node: {node_id}")
            })),
        ),
    }
}

async fn reset(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
) -> Json<serde_json::Value> {
    let mut sim = state.sim.lock().unwrap();
    if let Some(config) = sim.nodes.get(&node_id).map(|n| n.config.clone()) {
        sim.nodes
            .insert(node_id.clone(), sim::NodeState::new(config));
        info!("Reset node {}", node_id);
        Json(serde_json::json!({"status": "ok", "node": node_id}))
    } else {
        Json(serde_json::json!({"error": "unknown node"}))
    }
}

/// POST /{node_id}/drift/start — cue a live readout drift NOW. The
/// stage door for story-cued slides (walk finished, hold landed —
/// engine state the bench must never observe itself). One door drift
/// per node: the second knock is a 409.
#[derive(Debug, Deserialize)]
struct DriftStartRequest {
    rate_per_second: f64,
    #[serde(default)]
    cap: Option<f64>,
    #[serde(default)]
    name: Option<String>,
}

async fn drift_start(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
    Json(req): Json<DriftStartRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if !req.rate_per_second.is_finite() || req.rate_per_second <= 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "rate_per_second must be a positive finite number"
            })),
        );
    }
    let mut sim = state.sim.lock().unwrap();
    match sim.drift_start(&node_id, req.rate_per_second, req.cap, req.name) {
        Some(Ok(live)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "started",
                "node": node_id,
                "name": live.name,
                "rate_per_second": live.rate_per_second,
                "cap": live.cap,
                "cued_uptime_secs": live.cued_uptime_secs,
                "cued_tick": live.cued_tick,
            })),
        ),
        Some(Err(())) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "status": "already_running",
                "node": node_id,
                "error": "a door drift is already running on this node"
            })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("unknown node: {node_id}")})),
        ),
    }
}

/// GET /{node_id}/drift/state — every running drift (door + declared)
/// with its cue stamps in both clocks and its current offset.
#[derive(Debug, Deserialize)]
struct ExoStartRequest {
    kind: String,
    amplitude: f64,
    #[serde(default)]
    objective: usize,
    #[serde(default)]
    travel_secs: f64,
    #[serde(default)]
    name: Option<String>,
}

/// POST /{node_id}/exogenous/start — cue an outside push NOW (the
/// officer's "load spike, now"). kind: "step" (travel_secs = ramp
/// window, then holds) or "sine" (travel_secs = period, bounded
/// ±amplitude). Runs on wall time from the cue; composes additively
/// with declared pushes and other cued pushes.
async fn exogenous_start(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
    Json(req): Json<ExoStartRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    if req.kind != "step" && req.kind != "sine" {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "kind must be \"step\" or \"sine\""
            })),
        );
    }
    if req.kind == "sine" && req.travel_secs <= 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "a sine push needs travel_secs > 0 (its period) — windowless it reads as exactly 0"
            })),
        );
    }
    if !req.amplitude.is_finite() || req.amplitude == 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "amplitude must be a finite non-zero number"
            })),
        );
    }
    if !req.travel_secs.is_finite() || req.travel_secs < 0.0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "travel_secs must be a finite number >= 0"
            })),
        );
    }
    let mut sim = state.sim.lock().unwrap();
    match sim.exogenous_start(
        &node_id,
        &req.kind,
        req.amplitude,
        req.objective,
        req.travel_secs,
        req.name,
    ) {
        Some(Ok(live)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "started",
                "node": node_id,
                "name": live.name,
                "kind": live.kind,
                "amplitude": live.amplitude,
                "objective": live.objective,
                "travel_secs": live.travel_secs,
                "cued_uptime_secs": live.cued_uptime_secs
            })),
        ),
        Some(Err(())) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "objective index out of range for this node"
            })),
        ),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "unknown node" })),
        ),
    }
}

async fn drift_state(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let sim = state.sim.lock().unwrap();
    let Some(node) = sim.nodes.get(&node_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("unknown node: {node_id}")})),
        );
    };
    let now = sim.uptime_secs();
    let running: Vec<serde_json::Value> = node
        .live_drifts
        .iter()
        .map(|d| {
            serde_json::json!({
                "name": d.name,
                "origin": d.origin,
                "rate_per_second": d.rate_per_second,
                "cap": d.cap,
                "cued_uptime_secs": d.cued_uptime_secs,
                "cued_tick": d.cued_tick,
                "current_offset": sim::live_offset_at(std::slice::from_ref(d), now),
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "node": node_id,
            "uptime_secs": now,
            "total_ticks": sim.total_ticks,
            "running": running,
        })),
    )
}

// ─── Main ───────────────────────────────────────────────────────────

/// The router as behaviour, not plumbing: built once here, served by
/// main, driven by the wire tests — request in, world moved, JSON out.
fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/config", get(get_config))
        .route("/{node_id}/apply", post(apply))
        .route("/{node_id}/metrics/json", get(metrics_json))
        .route("/{node_id}/status", get(status))
        .route("/{node_id}/truth", get(truth))
        .route("/{node_id}/reset", post(reset))
        .route("/{node_id}/drift/start", post(drift_start))
        .route("/{node_id}/drift/state", get(drift_state))
        .route("/{node_id}/exogenous/start", post(exogenous_start))
        .with_state(state)
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "topology.yaml".to_string());

    let config_str = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read config from {}: {}", config_path, e);
            eprintln!("Using default 2-node linear config");
            serde_yaml::to_string(&default_config()).unwrap()
        }
    };

    let config: BenchConfig = match serde_yaml::from_str(&config_str) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to parse config: {}", e);
            default_config()
        }
    };

    info!(
        "Starting generic bench: {} nodes, {} edges, gaussian={}, colored={}, drift={}",
        config.nodes.len(),
        config.edges.len(),
        config.noise.gaussian_sigma,
        config.noise.colored_sigma,
        config.noise.drift_rate
    );

    let sim = Arc::new(std::sync::Mutex::new(Simulator::from_config(
        config.clone(),
    )));

    let state = Arc::new(AppState {
        sim,
        config: config.clone(),
    });

    let app = build_router(state);

    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "8090".to_string())
        .parse()
        .unwrap();

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("Listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

fn default_config() -> BenchConfig {
    BenchConfig {
        nodes: vec![
            NodeConfig {
                id: "node-1".to_string(),
                params: 3,
                objectives: 2,
                base: "linear".to_string(),
                intake: "post".to_string(),
                weights: vec![vec![0.3, 0.5, 0.2], vec![0.1, 0.4, 0.5]],
                interactions: vec![],
                param_lower: 0.0,
                param_upper: 100.0,
                exogenous: vec![],
                morphs: vec![],
                offset_drift_rate: 0.0,
                offset_drift_cap: None,
                offset_drift_start_tick: None,
                drifts: vec![],
            },
            NodeConfig {
                id: "node-2".to_string(),
                params: 3,
                objectives: 2,
                base: "linear".to_string(),
                intake: "post".to_string(),
                weights: vec![vec![0.4, 0.3, 0.3], vec![0.2, 0.6, 0.2]],
                interactions: vec![],
                param_lower: 0.0,
                param_upper: 100.0,
                exogenous: vec![],
                morphs: vec![],
                offset_drift_rate: 0.0,
                offset_drift_cap: None,
                offset_drift_start_tick: None,
                drifts: vec![],
            },
        ],
        edges: vec![SimEdgeConfig {
            from: "node-1".to_string(),
            from_channel: 0,
            to: "node-2".to_string(),
            to_channel: 0,
            strength: 0.7,
            drift_rate: 0.0,
            target_strength: None,
            at_tick: None,
            at_secs: None,
            travel_ticks: None,
            travel_secs: None,
            clock: None,
        }],
        noise: SimNoiseConfig {
            gaussian_sigma: 0.02,
            colored_sigma: 0.0,
            drift_rate: 0.0,
            sigma: None,
            noise_type: None,
            target_gaussian_sigma: None,
            sigma_at_tick: None,
            sigma_at_secs: None,
            sigma_travel_ticks: None,
            sigma_travel_secs: None,
            sigma_clock: None,
        },
    }
}

use sim::{EdgeConfig as SimEdgeConfig, NodeConfig, NoiseConfig as SimNoiseConfig};

// ── the wire tests: behaviour, not plumbing ──────────────────────────
// The estate acceptance cell (2026-11-20) caught a bug this crate could
// not see: TruthReport's four new fields existed on the struct and
// nowhere on the wire, because the handler hand-built its JSON. These
// tests drive the REAL router with REAL requests so a field, a shape,
// or a status code can never again diverge between struct and HTTP.

#[cfg(test)]
mod wire_tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state() -> Arc<AppState> {
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]],
                 "edges_note": null,
                 "exogenous": [{"kind": "step", "amplitude": 0.25,
                                 "at_tick": 0, "travel_ticks": 3}]},
                {"id": "b", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[0.0]]}
            ],
            "edges": [{"from": "a", "from_channel": 0, "to": "b", "to_channel": 0,
                        "strength": 0.3, "target_strength": -0.3,
                        "at_tick": 5, "travel_ticks": 5}],
            "noise": {"gaussian_sigma": 0.02, "target_gaussian_sigma": 0.05,
                       "sigma_at_tick": 2, "sigma_travel_ticks": 4}
        }))
        .unwrap();
        let sim = Arc::new(std::sync::Mutex::new(Simulator::from_config(
            config.clone(),
        )));
        Arc::new(AppState { sim, config })
    }

    async fn call(
        app: &axum::Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut b = Request::builder().method(method).uri(uri);
        let req = match body {
            Some(json) => {
                b = b.header("content-type", "application/json");
                b.body(Body::from(json.to_string())).unwrap()
            }
            None => b.body(Body::empty()).unwrap(),
        };
        let resp = app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, v)
    }

    #[tokio::test]
    async fn wire_truth_carries_every_reported_field() {
        let app = build_router(test_state());
        let (code, t) = call(&app, "GET", "/a/truth", None).await;
        assert_eq!(code, StatusCode::OK);
        for key in [
            "node_id",
            "total_ticks",
            "uptime_secs",
            "objectives",
            "effective_weights",
            "base_from",
            "base_to",
            "base_progress",
            "offset_effective",
            "noise_effective",
            "exogenous_effective",
            "edges",
        ] {
            assert!(
                t.get(key).is_some(),
                "truth wire is missing '{key}' — the exact class the estate cell caught"
            );
        }
        let e = &t["edges"][0];
        assert_eq!(e["from"], "a");
        assert!((e["strength_effective"].as_f64().unwrap() - 0.3).abs() < 1e-9);
    }

    #[tokio::test]
    async fn wire_apply_takes_named_params_and_moves_the_world() {
        let app = build_router(test_state());
        let (code, _) = call(&app, "POST", "/a/apply", Some(r#"{"param_0":100.0}"#)).await;
        assert_eq!(code, StatusCode::OK);
        let (_, t) = call(&app, "GET", "/a/truth", None).await;
        // base linear 1.0 + settled exogenous 0.25 (travel 3, 1 tick in: 1/3 ramp)
        let obj = t["objectives"][0].as_f64().unwrap();
        assert!(obj > 0.9 && obj < 1.2, "world moved: {obj}");
    }

    #[tokio::test]
    async fn wire_apply_rejects_a_bare_array_loudly() {
        // the exact silent-400 shape that stalled the estate cell
        let app = build_router(test_state());
        let (code, _) = call(&app, "POST", "/a/apply", Some(r#"[100.0]"#)).await;
        assert_ne!(code, StatusCode::OK, "a bare array must not be accepted");
    }

    #[tokio::test]
    async fn wire_exogenous_door_cues_and_refuses_junk() {
        let app = build_router(test_state());
        let (code, v) = call(
            &app,
            "POST",
            "/b/exogenous/start",
            Some(r#"{"kind":"step","amplitude":0.15,"objective":0,"travel_secs":0.0}"#),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "door cue accepted: {v}");
        let (_, t) = call(&app, "GET", "/b/truth", None).await;
        assert!(
            (t["exogenous_effective"][0].as_f64().unwrap() - 0.15).abs() < 1e-9,
            "instant step visible on the wire"
        );
        let (bad, _) = call(
            &app,
            "POST",
            "/b/exogenous/start",
            Some(r#"{"kind":"sine","amplitude":0.3}"#),
        )
        .await;
        assert_eq!(
            bad,
            StatusCode::BAD_REQUEST,
            "windowless sine refused at the door"
        );
        let (junk, _) = call(
            &app,
            "POST",
            "/b/exogenous/start",
            Some(r#"{"kind":"wobble","amplitude":0.3}"#),
        )
        .await;
        assert_eq!(junk, StatusCode::BAD_REQUEST, "unknown kind refused");
    }

    #[tokio::test]
    async fn wire_drift_door_and_state() {
        let app = build_router(test_state());
        let (code, v) = call(
            &app,
            "POST",
            "/a/drift/start",
            Some(r#"{"rate_per_second":0.01,"cap":0.5}"#),
        )
        .await;
        assert_eq!(code, StatusCode::OK, "drift cue accepted: {v}");
        let (code2, s) = call(&app, "GET", "/a/drift/state", None).await;
        assert_eq!(code2, StatusCode::OK);
        assert!(
            s.to_string().contains("rate_per_second"),
            "state lists the drift"
        );
    }

    #[tokio::test]
    async fn wire_unknown_node_is_a_404() {
        let app = build_router(test_state());
        let (code, _) = call(&app, "GET", "/nope/truth", None).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
    }
}
