mod artifact;
mod characterizer;
mod detector;
mod graph;
mod query;
mod trial_reader;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use log::info;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;

use detector::{CfarDetector, EdgeDetector};
use graph::{BuildResult, CausalGraph, CausalNode};
use trial_reader::TrialReader;

// ─── App State ──────────────────────────────────────────────────────

struct AppState {
    reader: TrialReader,
    graph: RwLock<Option<CausalGraph>>,
    build_status: RwLock<BuildStatus>,
}

#[derive(Clone, serde::Serialize)]
enum BuildStatus {
    Idle,
    Building,
    Done {
        at: String,
        edges: usize,
        duration_secs: f64,
    },
    Error {
        at: String,
        message: String,
    },
}

impl Default for BuildStatus {
    fn default() -> Self {
        Self::Idle
    }
}

impl AppState {
    fn new(reader: TrialReader) -> Self {
        Self {
            reader,
            graph: RwLock::new(None),
            build_status: RwLock::new(BuildStatus::default()),
        }
    }
}

// ─── Handlers ───────────────────────────────────────────────────────

async fn health(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let graph_built = state.graph.read().await.is_some();
    let db_ok = state.reader.health_check().await;
    Json(serde_json::json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "graph_built": graph_built,
        "db_reachable": db_ok,
    }))
}

#[derive(serde::Deserialize)]
struct BuildRequest {
    detection_confidence: Option<f64>,
}

async fn build(
    State(state): State<Arc<AppState>>,
    Json(req): Json<BuildRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Check if already building
    {
        let status = state.build_status.read().await;
        if matches!(*status, BuildStatus::Building) {
            return Ok(Json(serde_json::json!({
                "status": "already_building"
            })));
        }
    }

    *state.build_status.write().await = BuildStatus::Building;

    let confidence = req.detection_confidence.unwrap_or(
        std::env::var("GODON_DETECTION_CONFIDENCE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.95),
    );

    let state_clone = Arc::clone(&state);

    // Spawn background task
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        info!("Starting graph build (confidence={})", confidence);

        let result = build_graph_inner(&state_clone.reader, confidence).await;

        let duration = start.elapsed().as_secs_f64();

        match result {
            Ok(graph) => {
                let edges = graph.edges_detected;
                info!(
                    "Graph build complete: {} edges detected in {:.1}s",
                    edges, duration
                );
                *state_clone.graph.write().await = Some(graph);
                *state_clone.build_status.write().await = BuildStatus::Done {
                    at: chrono::Utc::now().to_rfc3339(),
                    edges,
                    duration_secs: duration,
                };
            }
            Err(e) => {
                log::error!("Graph build failed: {}", e);
                *state_clone.build_status.write().await = BuildStatus::Error {
                    at: chrono::Utc::now().to_rfc3339(),
                    message: e.to_string(),
                };
            }
        }
    });

    Ok(Json(serde_json::json!({
        "status": "building",
        "detection_confidence": confidence,
    })))
}

async fn build_status(State(state): State<Arc<AppState>>) -> Json<BuildStatus> {
    let status = state.build_status.read().await;
    Json(status.clone())
}

async fn get_graph(
    State(state): State<Arc<AppState>>,
) -> Result<Json<CausalGraph>, (StatusCode, Json<serde_json::Value>)> {
    let guard = state.graph.read().await;
    match guard.as_ref() {
        Some(graph) => Ok(Json(graph.clone())),
        None => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "no graph built yet",
                "hint": "POST /build to construct the causal graph"
            })),
        )),
    }
}

async fn get_artifact(
    State(state): State<Arc<AppState>>,
) -> Result<String, (StatusCode, String)> {
    let guard = state.graph.read().await;
    match guard.as_ref() {
        Some(graph) => {
            let json = artifact::export_artifact(graph).map_err(|e| {
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?;
            Ok(json)
        }
        None => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "no graph built yet — POST /build".to_string(),
        )),
    }
}

#[derive(serde::Deserialize)]
struct PredictRequest {
    sender_id: String,
    impulse_scale: Option<f64>,
}

async fn predict(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PredictRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let guard = state.graph.read().await;
    let graph = match guard.as_ref() {
        Some(g) => g,
        None => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "no graph built"})),
            ))
        }
    };

    let scale = req.impulse_scale.unwrap_or(1.0);
    let predictions = graph.predict(&req.sender_id, scale);
    Ok(Json(serde_json::json!({"predictions": predictions})))
}

async fn impact(
    State(state): State<Arc<AppState>>,
    Path(breeder_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let guard = state.graph.read().await;
    let graph = match guard.as_ref() {
        Some(g) => g,
        None => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "no graph built"})),
            ))
        }
    };

    let edges = graph.edges_from(&breeder_id);
    Ok(Json(serde_json::json!({
        "breeder_id": breeder_id,
        "edges": edges,
        "count": edges.len(),
    })))
}

async fn causes(
    State(state): State<Arc<AppState>>,
    Path(breeder_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let guard = state.graph.read().await;
    let graph = match guard.as_ref() {
        Some(g) => g,
        None => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "no graph built"})),
            ))
        }
    };

    let edges = graph.edges_into(&breeder_id);
    Ok(Json(serde_json::json!({
        "breeder_id": breeder_id,
        "edges": edges,
        "count": edges.len(),
    })))
}

async fn summary(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let guard = state.graph.read().await;
    let graph = match guard.as_ref() {
        Some(g) => g,
        None => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "no graph built"})),
            ))
        }
    };

    Ok(Json(serde_json::json!(graph.summary())))
}

// ─── Graph Building ─────────────────────────────────────────────────

async fn build_graph_inner(
    reader: &TrialReader,
    confidence: f64,
) -> Result<CausalGraph, Box<dyn std::error::Error + Send + Sync>> {
    let detector = CfarDetector::new(confidence);

    let breeders = reader.list_breeders().await?;
    info!("Found {} breeders", breeders.len());

    // Load probe trials for all breeders
    let mut all_trials: std::collections::HashMap<String, trial_reader::ProbeTrials> =
        std::collections::HashMap::new();

    for breeder_id in &breeders {
        match reader.read_probe_trials(breeder_id).await {
            Ok(probe) => {
                info!(
                    "Breeder {}: {} push, {} pause, {} hold_calib, {} receiver_hold",
                    breeder_id,
                    probe.push_trials.len(),
                    probe.pause_trials.len(),
                    probe.hold_calib_trials.len(),
                    probe.receiver_hold_trials.len()
                );
                all_trials.insert(breeder_id.clone(), probe);
            }
            Err(e) => {
                info!("Skipping breeder {} (read error: {})", breeder_id, e);
            }
        }
    }

    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut pairs_evaluated = 0usize;

    for sender_id in &breeders {
        for receiver_id in &breeders {
            if sender_id == receiver_id {
                continue;
            }

            let sender = match all_trials.get(sender_id) {
                Some(s) => s,
                None => continue,
            };
            let receiver = match all_trials.get(receiver_id) {
                Some(r) => r,
                None => continue,
            };

            // Skip if sender never probed
            if sender.push_trials.is_empty() {
                continue;
            }

            pairs_evaluated += 1;

            let detections = detector.detect(sender, receiver);

            for d in &detections {
                if d.detected {
                    let edge = characterizer::characterize(d, sender);
                    edges.push(edge);
                }
            }

            // Also store non-detected edges for completeness
            for d in &detections {
                if !d.detected {
                    let edge = characterizer::characterize(d, sender);
                    edges.push(edge);
                }
            }
        }
    }

    // Build nodes
    for breeder_id in &breeders {
        nodes.push(CausalNode {
            id: breeder_id.clone(),
            label: breeder_id.clone(),
            objectives: Vec::new(),
            observations: Vec::new(),
        });
    }

    let edges_detected = edges.iter().filter(|e| e.detected).count();

    let graph = CausalGraph {
        nodes,
        edges,
        built_at: chrono::Utc::now().to_rfc3339(),
        detector: detector.name().to_string(),
        detector_params: detector.params(),
        breeders_scanned: breeders.len(),
        pairs_evaluated,
        edges_detected,
    };

    Ok(graph)
}

// ─── Main ───────────────────────────────────────────────────────────

// ─── Real-Time Per-Pair Detection ───────────────────────────────────
//
// Reads trials for one sender/receiver pair, runs CFAR, returns result
// immediately. Does NOT touch the graph cache. This is the real-time
// endpoint the observer dashboard calls for "are they coupled right now?"

async fn detect_pair(
    State(state): State<Arc<AppState>>,
    axum::extract::Path((sender_id, receiver_id)): axum::extract::Path<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let confidence = std::env::var("GODON_DETECTION_CONFIDENCE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.95);

    let detector = CfarDetector::new(confidence);

    let sender = state
        .reader
        .read_probe_trials(&sender_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("failed to read sender trials: {}", e)
                })),
            )
        })?;

    let receiver = state
        .reader
        .read_probe_trials(&receiver_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("failed to read receiver trials: {}", e)
                })),
            )
        })?;

    if sender.push_trials.is_empty() {
        return Ok(Json(serde_json::json!({
            "detected": false,
            "reason": "no push trials from sender",
            "method": "cfar_block_step",
            "sender_id": sender_id,
            "receiver_id": receiver_id,
        })));
    }

    let detections = detector.detect(&sender, &receiver);

    let any_detected = detections.iter().any(|d| d.detected);

    Ok(Json(serde_json::json!({
        "detected": any_detected,
        "method": "cfar_block_step",
        "sender_id": sender_id,
        "receiver_id": receiver_id,
        "push_trials": sender.push_trials.len(),
        "pause_trials": sender.pause_trials.len(),
        "receiver_hold_trials": receiver.receiver_hold_trials.len(),
        "per_objective": detections,
    })))
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8091);

    let reader = TrialReader::from_env();
    let state = Arc::new(AppState::new(reader));

    let app = Router::new()
        .route("/health", get(health))
        // Real-time detection (per-pair, on-demand)
        .route("/detect/{sender_id}/{receiver_id}", get(detect_pair))
        // Batch graph building
        .route("/build", post(build))
        .route("/build/status", get(build_status))
        // Cached graph endpoints
        .route("/graph", get(get_graph))
        .route("/artifact", get(get_artifact))
        .route("/predict", post(predict))
        .route("/impact/{breeder_id}", get(impact))
        .route("/causes/{breeder_id}", get(causes))
        .route("/summary", get(summary))
        .layer(CorsLayer::very_permissive())
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", host, port).parse().expect("invalid addr");
    info!("godon-causal listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
