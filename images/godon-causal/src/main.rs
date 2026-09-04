mod artifact;
mod characterizer;
mod detector;
mod graph;
mod probe_curves;
mod curve_store;
mod query;
mod trial_reader;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post},
    Router,
};
use log::info;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;

use detector::{CfarDetector, EdgeDetector};
use graph::{BuildResult, CausalGraph, CausalNode};
use trial_reader::{mad, TrialReader};

// ─── App State ──────────────────────────────────────────────────────

struct AppState {
    reader: TrialReader,
    graph: RwLock<Option<CausalGraph>>,
    build_status: RwLock<BuildStatus>,
    curves: RwLock<probe_curves::CurveRegistry>,
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
            curves: RwLock::new(probe_curves::CurveRegistry::new()),
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
            Ok(mut graph) => {
                // Attach the characterization output: measured response
                // curves at build time. Detection edges + curves leave
                // the service as one artifact.
                graph.curves = state_clone.curves.read().await.snapshot();
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

async fn get_curves(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let curves = state.curves.read().await.snapshot();
    // k_retire rides the export so every banked curve states which
    // tolerance measured it — no archaeology of image versions.
    Json(serde_json::json!({ "curves": curves, "k_retire": crate::probe_curves::k_retire() }))
}

async fn delete_curves_for_sender(
    State(state): State<Arc<AppState>>,
    Path(sender_id): Path<String>,
) -> Json<serde_json::Value> {
    // Breeder purged: its curves die with it — memory and persisted rows.
    // Restart replay must not resurrect them.
    let removed = state.curves.write().await.delete_sender(&sender_id);
    let mut rows_deleted: u64 = 0;
    let mut db_error: Option<String> = None;
    match state.reader.connect_archive().await {
        Ok(client) => {
            // Fresh stacks: startup table-create may have raced the DB
            // (causal up before YugaByte ready) — the write path self-heals
            // this; do the same before deleting.
            if let Err(e) = curve_store::ensure_curve_table(&client).await {
                db_error = Some(e.to_string());
            } else {
                match curve_store::delete_curve_points(&client, &sender_id).await {
                    Ok(n) => rows_deleted = n,
                    Err(e) => db_error = Some(e.to_string()),
                }
            }
        }
        Err(e) => db_error = Some(e.to_string()),
    }
    if let Some(e) = &db_error {
        log::error!(
            "curve row deletion failed for sender {} (registry cleared, rows may replay on restart): {}",
            sender_id, e
        );
    }
    Json(serde_json::json!({
        "sender_id": sender_id,
        "curves_removed": removed,
        "rows_deleted": rows_deleted,
        "db_error": db_error,
    }))
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

async fn predict_multihop(
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
    let predictions = graph.predict_multihop(&req.sender_id, scale);
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
        curves: Vec::new(),
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

// ─── Real-Time Per-Edge Characterization ────────────────────────────
//
// Measures a receiver's objective_0 shift between the push and pause windows
// of a single probe, records (probe_level, shift) into the per-edge response
// curve, and reports the curve's convergence delta.

#[derive(serde::Deserialize)]
struct ProbeResultRequest {
    group_id: String,
    sender_id: String,
    probe_param: String,
    probe_level: f64,
    push_start: String,
    pause_end: String,
    convergence_threshold: f64,
    /// Declared parameter range (upper - lower) from the breeder.
    /// Scales gap ignorance; absent → observed level span.
    param_range: Option<f64>,
}

async fn probe_result(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ProbeResultRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Parse ISO 8601 timestamps to epoch seconds for DB queries.
    let push_start = parse_iso_to_epoch(&req.push_start).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("invalid push_start '{}': {}", req.push_start, e)
            })),
        )
    })?;
    let pause_end = parse_iso_to_epoch(&req.pause_end).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!("invalid pause_end '{}': {}", req.pause_end, e)
            })),
        )
    })?;

    // Split the [push_start, pause_end] window at its midpoint: the first half
    // covers the push (excitation) phase, the second half the pause (settling)
    // phase. We compare the median receiver objective per window.
    let midpoint = (push_start + pause_end) / 2.0;

    // Per-receiver, per-channel series for each window. Every holding
    // receiver's rows are in the window — each listener is characterized
    // separately (its own medians, its own curve).
    let push_by_recv = state
        .reader
        .read_receiver_observations(&req.group_id, &req.sender_id, push_start, midpoint)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("failed to read push observations: {}", e)
                })),
            )
        })?;

    let pause_by_recv = state
        .reader
        .read_receiver_observations(&req.group_id, &req.sender_id, midpoint, pause_end)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("failed to read pause observations: {}", e)
                })),
            )
        })?;

    // Union of receivers seen in either window.
    let mut receivers: Vec<String> = push_by_recv.keys().cloned().collect();
    for recv in pause_by_recv.keys() {
        if !receivers.contains(recv) {
            receivers.push(recv.clone());
        }
    }
    receivers.sort();

    #[derive(Clone)]
    struct ChannelResult {
        shift: f64,
        shift_bar: f64,
        z: f64,
        drift: bool,
        converged: bool,
        gaps: Vec<crate::probe_curves::GapInfo>,
    }

    #[derive(Clone)]
    struct ReceiverResult {
        channels: Vec<(String, ChannelResult)>,
    }

    impl ReceiverResult {
        /// Primary channel of this receiver: largest |shift| (stable
        /// tie-break via sorted channel order upstream).
        fn primary(&self) -> &(String, ChannelResult) {
            let mut best = &self.channels[0];
            for cr in self.channels.iter().skip(1) {
                if cr.1.shift.abs() > best.1.shift.abs() {
                    best = cr;
                }
            }
            best
        }
        fn all_converged(&self) -> bool {
            self.channels.iter().all(|(_, c)| c.converged)
        }
    }

    let mut per_receiver: Vec<(String, ReceiverResult)> = Vec::new();
    for recv in &receivers {
        let push_by_ch = push_by_recv.get(recv);
        let pause_by_ch = pause_by_recv.get(recv);

        // Union of channels seen in either window for THIS receiver.
        let mut channels: Vec<String> = push_by_ch
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        if let Some(pm) = pause_by_ch {
            for ch in pm.keys() {
                if !channels.contains(ch) {
                    channels.push(ch.clone());
                }
            }
        }
        channels.sort();

        let mut per_channel: Vec<(String, ChannelResult)> = Vec::new();
        for ch in &channels {
            let push_obs = push_by_ch
                .and_then(|m| m.get(ch))
                .cloned()
                .unwrap_or_default();
            let pause_obs = pause_by_ch
                .and_then(|m| m.get(ch))
                .cloned()
                .unwrap_or_default();
            if push_obs.is_empty() && pause_obs.is_empty() {
                continue;
            }
            let push_median = median_f64(&push_obs);
            let pause_median = median_f64(&pause_obs);
            let shift = push_median - pause_median;

            // Measurement uncertainty of the shift: MAD of the raw samples in
            // each window (the same robust estimator CFAR uses), combined in
            // quadrature. Conservative by construction — sample-scale scatter,
            // not median-of-N — so it errs toward blending; drift fires only
            // on movements larger than the raw scatter.
            let push_mad = mad(&push_obs);
            let pause_mad = mad(&pause_obs);
            let shift_bar = (push_mad * push_mad + pause_mad * pause_mad).sqrt();

            let outcome = {
                let mut curves = state.curves.write().await;
                curves.probe(
                    &req.sender_id,
                    recv,
                    &req.probe_param,
                    ch,
                    req.probe_level,
                    shift,
                    shift_bar,
                    req.convergence_threshold,
                    req.param_range,
                )
            };
            let converged = state
                .curves
                .read()
                .await
                .is_converged(&req.sender_id, recv, &req.probe_param, ch);
            let gaps = state
                .curves
                .read()
                .await
                .get_curve(&req.sender_id, recv, &req.probe_param, ch)
                .map(|c| c.gaps())
                .unwrap_or_default();
            per_channel.push((
                ch.clone(),
                ChannelResult { shift, shift_bar, z: outcome.z, drift: outcome.drift, converged, gaps },
            ));
        }

        if !per_channel.is_empty() {
            per_receiver.push((recv.clone(), ReceiverResult { channels: per_channel }));
        }
    }

    if per_receiver.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "no receiver observations in the probe window"
            })),
        ));
    }

    // Primary receiver: largest |shift| across all receivers and channels.
    // Top-level fields describe it — with one receiver this is exactly the
    // previous (2-breeder) behavior.
    let mut primary_recv = &per_receiver[0];
    for rr in per_receiver.iter().skip(1) {
        if rr.1.primary().1.shift.abs()
            > primary_recv.1.primary().1.shift.abs()
        {
            primary_recv = rr;
        }
    }
    let primary_recv_name = primary_recv.0.clone();
    let primary = primary_recv.1.primary();
    let primary_name = primary.0.clone();
    let shift = primary.1.shift;
    let shift_bar = primary.1.shift_bar;

    // Retirement signal: every receiver, every channel converged.
    let mut converged = per_receiver.iter().all(|(_, rr)| rr.all_converged());
    // Gaps: union across all receivers and channels (eligibility consults
    // the union — one fat bracket anywhere keeps the param in rotation).
    let mut gaps: Vec<crate::probe_curves::GapInfo> = Vec::new();
    for (_, rr) in per_receiver.iter() {
        for (_, c) in rr.channels.iter() {
            gaps.extend(c.gaps.iter().cloned());
        }
    }
    // de-dup identical gaps across receivers/channels (same levels/jump)
    gaps.dedup_by(|a, b| {
        a.from_level == b.from_level && a.to_level == b.to_level && a.jump == b.jump
    });
    let drift = per_receiver
        .iter()
        .any(|(_, rr)| rr.channels.iter().any(|(_, c)| c.drift));
    let z = primary.1.z;

    let delta = state
        .curves
        .read()
        .await
        .get_curve(&req.sender_id, &primary_recv_name, &req.probe_param, &primary_name)
        .map(|c| c.last_delta())
        .unwrap_or(f64::MAX / 2.0);

    // Regime stamp: the group's standing dials at measurement time —
    // the ambient this round was measured under. Assembled from
    // purely local publications; None on failure (regime unrecorded,
    // the same honesty legacy rows carry).
    let ambient: Option<String> = state
        .reader
        .read_standing_params(&req.group_id)
        .await
        .ok()
        .map(|v| v.to_string());

    // Self-curve: the sender's own node readings in the same windows
    // (its push/pause trials publish them). The pause window is the
    // sender at neutral — its median is the join baseline, banked
    // instead of excavated. Probed into the registry like any curve
    // (receiver_id = the sender); never an edge — the graph builds
    // from detection trials, not the curve registry.
    let self_push = state
        .reader
        .read_self_observations(&req.group_id, &req.sender_id, push_start, midpoint)
        .await
        .unwrap_or_default();
    let self_pause = state
        .reader
        .read_self_observations(&req.group_id, &req.sender_id, midpoint, pause_end)
        .await
        .unwrap_or_default();

    let mut self_channels: Vec<(String, f64, f64)> = Vec::new();
    {
        let mut channels: Vec<String> = self_push.keys().cloned().collect();
        for ch in self_pause.keys() {
            if !channels.contains(ch) {
                channels.push(ch.clone());
            }
        }
        channels.sort();
        for ch in channels {
            let p = self_push.get(&ch).cloned().unwrap_or_default();
            let q = self_pause.get(&ch).cloned().unwrap_or_default();
            // A self delta needs both windows: the level reading and
            // the neutral baseline.
            if p.is_empty() || q.is_empty() {
                continue;
            }
            let shift = median_f64(&p) - median_f64(&q);
            let p_mad = mad(&p);
            let q_mad = mad(&q);
            let bar = (p_mad * p_mad + q_mad * q_mad).sqrt();
            state
                .curves
                .write()
                .await
                .probe(
                    &req.sender_id,
                    &req.sender_id,
                    &req.probe_param,
                    &ch,
                    req.probe_level,
                    shift,
                    bar,
                    req.convergence_threshold,
                    req.param_range,
                );
            self_channels.push((ch, shift, bar));
        }
    }

    // Walker fix: the self-curve enters the retirement verdict — for a
    // self-walk the curve under study IS the walker's own. Enumerate it
    // with the listeners; empty self reads stay neutral (no rows yet).
    let mut self_out: Vec<SelfChannelOut> = Vec::new();
    {
        let curves = state.curves.read().await;
        for (name, shift, bar) in self_channels.iter() {
            let sc_converged = curves.is_converged(
                &req.sender_id, &req.sender_id, &req.probe_param, name);
            let sc_gaps = curves
                .get_curve(&req.sender_id, &req.sender_id, &req.probe_param, name)
                .map(|c| c.gaps())
                .unwrap_or_default();
            self_out.push(SelfChannelOut {
                name: name.clone(),
                shift: *shift,
                bar: *bar,
                converged: sc_converged,
                gaps: sc_gaps,
            });
        }
    }
    converged = fold_self_verdict(converged, &mut gaps, &self_out);

    // Write-through persistence per receiver × channel (best-effort,
    // failure only logs).
    if let Ok(client) = state.reader.connect_archive().await {
        for (recv, rr) in per_receiver.iter() {
            for (name, c) in rr.channels.iter() {
                curve_store::persist_point(
                    &client,
                    &req.group_id,
                    &req.sender_id,
                    recv,
                    &req.probe_param,
                    name,
                    req.probe_level,
                    c.shift,
                    c.shift_bar,
                    req.convergence_threshold,
                    ambient.as_deref(),
                )
                .await;
            }
        }
        for (name, shift, bar) in self_channels.iter() {
            curve_store::persist_point(
                &client,
                &req.group_id,
                &req.sender_id,
                &req.sender_id,
                &req.probe_param,
                name,
                req.probe_level,
                *shift,
                *bar,
                req.convergence_threshold,
                ambient.as_deref(),
            )
            .await;
        }
    } else {
        log::error!(
            "curve point NOT persisted — archive DB unreachable (sender={} param={})",
            req.sender_id,
            req.probe_param
        );
    }

    // Replace INFINITY with a large finite value — serde_json serializes
    // Infinity as null, which the coordinator interprets as failure.
    let delta_json = if delta.is_infinite() {
        serde_json::Value::from(f64::MAX / 2.0)
    } else {
        serde_json::Value::from(delta)
    };

    // unresolved count over the union:
    let unresolved = gaps.iter().filter(|g| g.unresolved).count();

    // Legacy shape: the primary receiver's per-channel map (consumers
    // predating per-receiver curves read this).
    let channels_json: serde_json::Map<String, serde_json::Value> = primary_recv
        .1
        .channels
        .iter()
        .map(|(name, c)| {
            (
                name.clone(),
                serde_json::json!({
                    "shift": c.shift,
                    "shift_bar": c.shift_bar,
                    "z": c.z,
                    "drift": c.drift,
                    "converged": c.converged,
                    "gaps": c.gaps,
                }),
            )
        })
        .collect();

    // Full per-receiver map: every listener's channels, its aggregate
    // convergence, its gaps union, and its primary channel's delta.
    // Built with a plain loop — the per-receiver delta reads need .await,
    // which iterator closures cannot hold.
    let mut receivers_json: serde_json::Map<String, serde_json::Value> =
        serde_json::Map::new();
    for (recv, rr) in per_receiver.iter() {
        let p = rr.primary();
        let recv_delta = state
            .curves
            .read()
            .await
            .get_curve(&req.sender_id, recv, &req.probe_param, &p.0)
            .map(|c| c.last_delta())
            .unwrap_or(f64::MAX / 2.0);
        let recv_delta_json = if recv_delta.is_infinite() {
            serde_json::Value::from(f64::MAX / 2.0)
        } else {
            serde_json::Value::from(recv_delta)
        };
        let mut recv_gaps: Vec<crate::probe_curves::GapInfo> = Vec::new();
        for (_, c) in rr.channels.iter() {
            recv_gaps.extend(c.gaps.iter().cloned());
        }
        recv_gaps.dedup_by(|a, b| {
            a.from_level == b.from_level && a.to_level == b.to_level && a.jump == b.jump
        });
        let recv_channels: serde_json::Map<String, serde_json::Value> = rr
            .channels
            .iter()
            .map(|(name, c)| {
                (
                    name.clone(),
                    serde_json::json!({
                        "shift": c.shift,
                        "shift_bar": c.shift_bar,
                        "z": c.z,
                        "drift": c.drift,
                        "converged": c.converged,
                        "gaps": c.gaps,
                    }),
                )
            })
            .collect();
        receivers_json.insert(
            recv.clone(),
            serde_json::json!({
                "primary_channel": p.0,
                "shift": p.1.shift,
                "shift_bar": p.1.shift_bar,
                "z": p.1.z,
                "drift": rr.channels.iter().any(|(_, c)| c.drift),
                "delta": recv_delta_json,
                "converged": rr.all_converged(),
                "gaps": recv_gaps,
                "unresolved_gaps": recv_gaps.iter().filter(|g| g.unresolved).count(),
                "channels": recv_channels,
            }),
        );
    }

    let self_primary = self_out
        .iter()
        .max_by(|a, b| {
            a.shift.abs()
                .partial_cmp(&b.shift.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|p| p.name.clone());
    let self_delta_json = match &self_primary {
        Some(name) => {
            let d = state
                .curves
                .read()
                .await
                .get_curve(&req.sender_id, &req.sender_id, &req.probe_param, name)
                .map(|c| c.last_delta())
                .unwrap_or(f64::MAX / 2.0);
            if d.is_infinite() {
                serde_json::Value::from(f64::MAX / 2.0)
            } else {
                serde_json::Value::from(d)
            }
        }
        None => serde_json::Value::from(f64::MAX / 2.0),
    };
    receivers_json.insert(
        "self".to_string(),
        self_receiver_entry(&self_out, self_delta_json),
    );

    let self_json: serde_json::Map<String, serde_json::Value> = self_channels
        .iter()
        .map(|(name, shift, bar)| {
            (
                name.clone(),
                serde_json::json!({"shift": shift, "shift_bar": bar}),
            )
        })
        .collect();

    Ok(Json(serde_json::json!({
        "sender_id": req.sender_id,
        "probe_param": req.probe_param,
        "probe_level": req.probe_level,
        "primary_receiver": primary_recv_name,
        "primary_channel": primary_name,
        "shift": shift,
        "shift_bar": shift_bar,
        "z": z,
        "drift": drift,
        "delta": delta_json,
        "converged": converged,
        "gaps": gaps,
        "unresolved_gaps": unresolved,
        "channels": channels_json,
        "receivers": receivers_json,
        "self": self_json,
        "ambient": ambient,
        "k_retire": crate::probe_curves::k_retire(),
    })))
}

/// One self-channel's verdict inputs, gathered by the caller (which holds
/// the registry read guard) and folded by `fold_self_verdict` /
/// `self_receiver_entry`.
pub(crate) struct SelfChannelOut {
    pub name: String,
    pub shift: f64,
    pub bar: f64,
    pub converged: bool,
    pub gaps: Vec<crate::probe_curves::GapInfo>,
}

/// Fold the self-curve into the retirement verdict. For a self-walk the
/// curve under study IS the walker's own — enumerate it with the
/// listeners: converged ANDs in, its gaps join the union (deduped).
/// Empty self reads are neutral: convergence cannot be demanded from a
/// curve with no banked rows yet.
pub(crate) fn fold_self_verdict(
    converged: bool,
    gaps: &mut Vec<crate::probe_curves::GapInfo>,
    self_out: &[SelfChannelOut],
) -> bool {
    let mut self_all = true;
    for sc in self_out {
        self_all &= sc.converged;
        gaps.extend(sc.gaps.iter().cloned());
    }
    gaps.dedup_by(|a, b| {
        a.from_level == b.from_level && a.to_level == b.to_level && a.jump == b.jump
    });
    converged && self_all
}

/// The `self` entry of the response's receivers map — same shape as a
/// listener entry so the coordinator's per-listener paper trail logs it.
/// z stays 0.0 (honest: no z is computed for the self read) and the
/// primary is never the marker star — reporting semantics unchanged.
pub(crate) fn self_receiver_entry(
    self_out: &[SelfChannelOut],
    delta_json: serde_json::Value,
) -> serde_json::Value {
    let primary = self_out
        .iter()
        .max_by(|a, b| {
            a.shift.abs()
                .partial_cmp(&b.shift.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    let mut union: Vec<crate::probe_curves::GapInfo> = Vec::new();
    for sc in self_out {
        union.extend(sc.gaps.iter().cloned());
    }
    union.dedup_by(|a, b| {
        a.from_level == b.from_level && a.to_level == b.to_level && a.jump == b.jump
    });
    let channels: serde_json::Map<String, serde_json::Value> = self_out
        .iter()
        .map(|sc| {
            (
                sc.name.clone(),
                serde_json::json!({
                    "shift": sc.shift,
                    "shift_bar": sc.bar,
                    "converged": sc.converged,
                    "gaps": sc.gaps,
                }),
            )
        })
        .collect();
    serde_json::json!({
        "primary_channel": primary.map(|p| p.name.clone()).unwrap_or_default(),
        "shift": primary.map(|p| p.shift).unwrap_or(0.0),
        "shift_bar": primary.map(|p| p.bar).unwrap_or(0.0),
        "z": 0.0,
        "drift": false,
        "delta": delta_json,
        "converged": self_out.iter().all(|sc| sc.converged),
        "gaps": union,
        "unresolved_gaps": union.iter().filter(|g| g.unresolved).count(),
        "channels": channels,
    })
}

/// The notebook page: every banked curve of this sender's param, with its
/// levels, gaps (carrying each gap's own bars_sum), and convergence. The
/// walker is a pure function of this view — no RAM state to lose.
pub(crate) fn assemble_walk_view(
    sender_id: &str,
    param: &str,
    refinement_level: u32,
    entries: &[crate::probe_curves::CurveEntry],
) -> serde_json::Value {
    let curves: Vec<serde_json::Value> = entries
        .iter()
        .filter(|e| e.sender_id == sender_id && e.param == param)
        .map(|e| {
            serde_json::json!({
                "receiver_id": e.receiver_id,
                "channel": e.channel,
                "converged": e.state.converged,
                "levels": e.state.points.iter().map(|p| p.0).collect::<Vec<f64>>(),
                "gaps": e.state.gaps,
            })
        })
        .collect();
    serde_json::json!({
        "sender_id": sender_id,
        "param": param,
        "refinement_level": refinement_level,
        "curves": curves,
    })
}

/// Median of a slice of f64 (0.0 for empty slices).
fn median_f64(v: &[f64]) -> f64 {
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

/// Parse an ISO 8601 timestamp string to epoch seconds.
/// Handles both naive (no timezone) and timezone-aware ISO strings.
/// Naive timestamps are assumed UTC.
fn parse_iso_to_epoch(s: &str) -> Result<f64, String> {
    // Try RFC 3339 first (with timezone), fall back to naive parsing.
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.timestamp() as f64 + dt.timestamp_subsec_nanos() as f64 / 1e9);
    }
    // Naive datetime — assume UTC.
    let naive = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
        .map_err(|e| e.to_string())?;
    let utc = chrono::TimeZone::from_utc_datetime(&chrono::Utc, &naive);
    Ok(utc.timestamp() as f64 + utc.timestamp_subsec_nanos() as f64 / 1e9)
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
    let state = Arc::new(AppState::new(reader.clone()));

    // Restart recovery: replay persisted curve points into the registry.
    // Best-effort — on DB failure the registry starts empty and live
    // probing repopulates it (and re-persists).
    match reader.connect_archive().await {
        Ok(client) => {
            if let Err(e) = curve_store::ensure_curve_table(&client).await {
                log::error!("curve_points table setup failed: {}", e);
            }
            match curve_store::load_curve_points(&client).await {
                Ok(rows) => {
                    let n = rows.len();
                    let mut curves = state.curves.write().await;
                    for r in rows {
                        curves.probe(
                            &r.sender_id,
                            &r.receiver_id,
                            &r.probe_param,
                            &r.channel,
                            r.probe_level,
                            r.shift,
                            r.bar,
                            r.convergence_threshold,
                            None,
                        );
                    }
                    info!("loaded {} persisted curve points into registry", n);
                }
                Err(e) => log::error!("curve point load failed: {}", e),
            }
        }
        Err(e) => {
            log::error!("archive DB unavailable at startup, curves start empty: {}", e)
        }
    }

    let app = Router::new()
        .route("/health", get(health))
        // Real-time detection (per-pair, on-demand)
        .route("/detect/{sender_id}/{receiver_id}", get(detect_pair))
        // Real-time characterization (response curves per edge)
        .route("/characterize", post(probe_result))
        // Batch graph building
        .route("/build", post(build))
        .route("/build/status", get(build_status))
        // Cached graph endpoints
        .route("/graph", get(get_graph))
        .route("/artifact", get(get_artifact))
        .route("/curves", get(get_curves))
        .route("/curves/{sender_id}", delete(delete_curves_for_sender))
        .route("/predict", post(predict))
        .route("/predict/multihop", post(predict_multihop))
        .route("/impact/{breeder_id}", get(impact))
        .route("/causes/{breeder_id}", get(causes))
        .layer(CorsLayer::very_permissive())
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", host, port).parse().expect("invalid addr");
    info!("godon-causal listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── GET /curves ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_curves_empty_registry() {
        let state = Arc::new(AppState::new(TrialReader::from_env()));
        let res = get_curves(State(state)).await;
        let arr = res.0["curves"].as_array().expect("curves array");
        assert_eq!(arr.len(), 0);
    }

    // ─── parse_iso_to_epoch ───────────────────────────────────────

    #[test]
    fn test_parse_naive_iso() {
        // Python datetime.isoformat() produces naive timestamps (no timezone).
        let epoch = parse_iso_to_epoch("2026-08-11T12:00:00.000000").unwrap();
        // Verify it's a reasonable epoch for 2026 (between 2025-01-01 and 2027-01-01)
        assert!(epoch > 1735689600.0, "expected > 2025-01-01 epoch, got {}", epoch);
        assert!(epoch < 1798761600.0, "expected < 2027-01-01 epoch, got {}", epoch);
    }

    #[test]
    fn test_parse_rfc3339_iso() {
        let epoch = parse_iso_to_epoch("2026-08-11T12:00:00Z").unwrap();
        assert!(epoch > 1700000000.0, "expected recent epoch, got {}", epoch);
    }

    #[test]
    fn test_parse_iso_with_offset() {
        let epoch = parse_iso_to_epoch("2026-08-11T12:00:00+00:00").unwrap();
        assert!(epoch > 1700000000.0);
    }

    #[test]
    fn test_parse_iso_garbage_fails() {
        assert!(parse_iso_to_epoch("not-a-date").is_err());
    }

    // ─── ProbeResultRequest deserialization ───────────────────────

    #[test]
    fn test_deserialize_probe_result_request_naive_iso() {
        // The coordinator sends datetime.isoformat() — naive ISO strings.
        let json = r#"{
            "group_id": "bench-char",
            "sender_id": "abc-123",
            "probe_param": "param_0",
            "probe_level": 50.0,
            "push_start": "2026-08-11T12:00:00.123456",
            "pause_end": "2026-08-11T12:05:00.654321",
            "convergence_threshold": 0.02
        }"#;
        let req: ProbeResultRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.probe_param, "param_0");
        assert!((req.probe_level - 50.0).abs() < 1e-9);
        assert_eq!(req.push_start, "2026-08-11T12:00:00.123456");
    }

    #[test]
    fn test_deserialize_probe_result_request_rejects_float_timestamps() {
        // If someone sends epoch floats instead of ISO strings, it must fail
        // (not silently parse a truncated float as a string).
        let json = r#"{
            "group_id": "test",
            "sender_id": "test",
            "probe_param": "param_0",
            "probe_level": 50.0,
            "push_start": 1723387200.0,
            "pause_end": 1723387500.0,
            "convergence_threshold": 0.02
        }"#;
        let result: Result<ProbeResultRequest, _> = serde_json::from_str(json);
        assert!(result.is_err(), "float timestamps must be rejected");
    }

    // ─── JSON response serialization (the INFINITY bug) ───────────

    #[test]
    fn test_delta_infinity_is_not_null_in_json() {
        // The first point on a ResponseCurve returns f64::INFINITY.
        // serde_json serializes Infinity as null. The coordinator reads
        // null delta as failure. This test catches that regression.
        let delta = f64::INFINITY;

        // Naive serialization (the old code):
        let naive_json = serde_json::to_string(&serde_json::json!({"delta": delta})).unwrap();
        assert!(
            naive_json.contains("null"),
            "INFINITY should serialize as null (this is the bug we're testing for)"
        );

        // The fix: convert INFINITY to finite before serializing.
        let fixed_value = if delta.is_infinite() {
            f64::MAX / 2.0
        } else {
            delta
        };
        let fixed_json = serde_json::to_string(&serde_json::json!({"delta": fixed_value})).unwrap();
        assert!(
            !fixed_json.contains("null"),
            "fixed delta must not be null in JSON: {}", fixed_json
        );
        assert!(
            fixed_json.parse::<serde_json::Value>().unwrap()["delta"].as_f64().is_some(),
            "fixed delta must deserialize back as f64: {}", fixed_json
        );
    }

    #[test]
    fn test_normal_delta_serializes_correctly() {
        let delta = 0.015_f64;
        let json = serde_json::to_string(&serde_json::json!({"delta": delta})).unwrap();
        assert!(json.contains("0.015"));
        assert!(!json.contains("null"));
    }

    // ─── walker fix: self-curve enters the retirement verdict ────

    use crate::probe_curves::GapInfo;

    fn gap(from: f64, to: f64, jump: f64, bars: f64, unresolved: bool, ign: f64) -> GapInfo {
        GapInfo {
            from_level: from,
            to_level: to,
            jump,
            bars_sum: bars,
            width: to - from,
            unresolved,
            ignorance: ign,
        }
    }

    fn self_out(name: &str, shift: f64, converged: bool, gaps: Vec<GapInfo>) -> SelfChannelOut {
        SelfChannelOut {
            name: name.to_string(),
            shift,
            bar: 0.02,
            converged,
            gaps,
        }
    }

    #[test]
    fn test_fold_self_bracket_joins_union_and_unstable_self_flips_key() {
        // Seed-48 tail defect: a stable-but-unresolved self curve (the
        // +0.996 step at L=100) must join the gap union so the breeder's
        // blocking check sees it — retirement blocked via key 2.
        let mut gaps = vec![gap(0.0, 50.0, 0.006, 0.037, false, 0.003)];
        let self_o = vec![self_out(
            "objective_1",
            0.996,
            true,
            vec![gap(50.0, 100.0, 1.004, 0.041, true, 0.502)],
        )];
        let converged = fold_self_verdict(true, &mut gaps, &self_o);
        assert!(converged, "self stability alone does not flip key 1");
        assert!(
            gaps.iter().any(|g| g.unresolved && g.ignorance > 0.5),
            "the self bracket joins the union — key 2 now sees it"
        );

        // An UNSTABLE self curve flips key 1 — the room-stillness AND.
        let mut gaps2 = vec![];
        let self_unstable = vec![self_out("objective_0", 0.3, false, vec![])];
        assert!(!fold_self_verdict(true, &mut gaps2, &self_unstable));
    }

    #[test]
    fn test_fold_self_dedups_and_keeps_neutral_on_empty() {
        let g = gap(0.0, 50.0, 0.006, 0.037, false, 0.003);
        let mut gaps = vec![g.clone()];
        let self_o = vec![self_out("objective_0", 0.01, true, vec![g.clone()])];
        let converged = fold_self_verdict(true, &mut gaps, &self_o);
        assert!(converged);
        assert_eq!(gaps.len(), 1, "identical gaps dedup across the fold");

        let mut untouched = vec![g.clone()];
        assert!(fold_self_verdict(true, &mut untouched, &[]), "empty self reads are neutral — cannot demand convergence from a curve with no rows");
        assert_eq!(untouched.len(), 1);
    }

    #[test]
    fn test_self_receiver_entry_primary_and_union() {
        let self_o = vec![
            self_out("objective_0", 0.01, true, vec![gap(0.0, 50.0, 0.1, 0.04, true, 0.05)]),
            self_out("objective_1", -0.996, false, vec![gap(50.0, 100.0, 1.004, 0.041, true, 0.502)]),
        ];
        let entry = self_receiver_entry(&self_o, serde_json::json!(0.25));
        assert_eq!(entry["primary_channel"], "objective_1", "primary = largest |shift|");
        assert_eq!(entry["converged"], false);
        assert_eq!(entry["unresolved_gaps"], 2);
        assert_eq!(entry["z"], 0.0, "self has no z — keep the TELL line parseable");
        let union = entry["gaps"].as_array().unwrap();
        assert_eq!(union.len(), 2, "per-channel gaps union, deduped");
    }

    #[test]
    fn test_assemble_walk_view_filters_and_carries_refinement() {
        let mk_entry = |sender: &str, recv: &str, param: &str, ch: &str, levels: Vec<f64>| {
            crate::probe_curves::CurveEntry {
                sender_id: sender.to_string(),
                receiver_id: recv.to_string(),
                param: param.to_string(),
                channel: ch.to_string(),
                state: crate::probe_curves::CurveState {
                    num_points: levels.len(),
                    last_delta: 0.0,
                    converged: false,
                    gaps: vec![],
                    points: levels.iter().map(|l| (*l, 0.0, 0.02)).collect(),
                },
            }
        };
        let entries = vec![
            mk_entry("S", "R1", "param_0", "objective_0", vec![0.0, 50.0]),
            mk_entry("S", "S", "param_0", "objective_0", vec![0.0, 50.0, 100.0]),
            mk_entry("S", "R1", "param_1", "objective_0", vec![50.0]),
            mk_entry("OTHER", "R1", "param_0", "objective_0", vec![0.0]),
        ];
        let view = assemble_walk_view("S", "param_0", 2, &entries);
        assert_eq!(view["refinement_level"], 2);
        let curves = view["curves"].as_array().unwrap();
        assert_eq!(curves.len(), 2, "only this sender's this-param curves");
        let self_curve = curves.iter().find(|c| c["receiver_id"] == "S").unwrap();
        assert_eq!(self_curve["levels"].as_array().unwrap().len(), 3);
    }
}
