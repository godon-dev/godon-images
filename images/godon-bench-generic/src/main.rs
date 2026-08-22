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

mod sim;

use axum::{
    extract::{Path, State},
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
/// The breeder sends {"param_0": 50.0, "param_1": 50.0, ...}.
/// We convert to array internally based on config.
#[derive(Debug, Deserialize)]
struct ApplyRequest {
    #[serde(flatten)]
    params: std::collections::HashMap<String, f64>,
}

/// GET /{node_id}/metrics/json — returns named scalar objectives.
/// objective_0, objective_1, etc. — built dynamically from config.
/// The breeder reads via `key: objective_N` (scalar, not array).

fn build_metrics_response(
    node_id: &str,
    tick: u64,
    objectives: &[f64],
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("node_id".to_string(), serde_json::Value::String(node_id.to_string()));
    map.insert("tick".to_string(), serde_json::Value::Number(serde_json::Number::from(tick)));
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
) -> Json<serde_json::Value> {
    // Convert named params dict to array based on config
    let node_config = state.config.nodes.iter()
        .find(|n| n.id == node_id)
        .unwrap_or_else(|| panic!("unknown node: {}", node_id));

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
    info!("Node {} applied {} params -> objectives: {:?}", node_id, param_array.len(), objectives);
    Json(build_metrics_response(&node_id, tick, &objectives))
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

async fn reset(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
) -> Json<serde_json::Value> {
    let mut sim = state.sim.lock().unwrap();
    if let Some(config) = sim.nodes.get(&node_id).map(|n| n.config.clone()) {
        sim.nodes.insert(node_id.clone(), sim::NodeState::new(config));
        info!("Reset node {}", node_id);
        Json(serde_json::json!({"status": "ok", "node": node_id}))
    } else {
        Json(serde_json::json!({"error": "unknown node"}))
    }
}

// ─── Main ───────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    env_logger::init();

    let config_path = std::env::var("CONFIG_PATH")
        .unwrap_or_else(|_| "topology.yaml".to_string());

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

    let sim = Arc::new(std::sync::Mutex::new(Simulator::from_config(config.clone())));

    let state = Arc::new(AppState {
        sim,
        config: config.clone(),
    });

    // Build router: global endpoints + per-node endpoints
    let app = Router::new()
        .route("/health", get(health))
        .route("/config", get(get_config))
        .route("/{node_id}/apply", post(apply))
        .route("/{node_id}/metrics/json", get(metrics_json))
        .route("/{node_id}/status", get(status))
        .route("/{node_id}/reset", post(reset))
        .with_state(state);

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
                weights: vec![vec![0.3, 0.5, 0.2], vec![0.1, 0.4, 0.5]],
                interactions: vec![],
                param_lower: 0.0,
                param_upper: 100.0,
            },
            NodeConfig {
                id: "node-2".to_string(),
                params: 3,
                objectives: 2,
                base: "linear".to_string(),
                weights: vec![vec![0.4, 0.3, 0.3], vec![0.2, 0.6, 0.2]],
                interactions: vec![],
                param_lower: 0.0,
                param_upper: 100.0,
            },
        ],
        edges: vec![SimEdgeConfig {
            from: "node-1".to_string(),
            from_channel: 0,
            to: "node-2".to_string(),
            to_channel: 0,
            strength: 0.7,
            drift_rate: 0.0,
        }],
        noise: SimNoiseConfig {
            gaussian_sigma: 0.02,
            colored_sigma: 0.0,
            drift_rate: 0.0,
            sigma: None,
            noise_type: None,
        },
    }
}

use sim::{EdgeConfig as SimEdgeConfig, InteractionConfig as SimInteractionConfig, NoiseConfig as SimNoiseConfig, NodeConfig};
