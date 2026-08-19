use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Channel Identification ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", content = "value")]
pub enum ChannelId {
    Objective(usize),
    Observation(String),
}

impl std::fmt::Display for ChannelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelId::Objective(i) => write!(f, "objective[{}]", i),
            ChannelId::Observation(name) => write!(f, "observation[{}]", name),
        }
    }
}

// ─── Response Function ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResponseFunction {
    StepResponse {
        sensitivity: f64,
        baseline: f64,
        recovery_fraction: f64,
    },
    /// Measured multi-level response curve: (level, shift) pairs.
    /// Source of truth for nonlinear composition; StepResponse stays
    /// the fallback when no curve was accumulated for an edge.
    CurveResponse {
        points: Vec<(f64, f64)>,
        converged: bool,
    },
}

impl ResponseFunction {
    pub fn predict_shift(&self, impulse_scale: f64) -> f64 {
        match self {
            ResponseFunction::StepResponse { sensitivity, .. } => sensitivity * impulse_scale,
            // Scalar contract preserved via linearization: least-squares
            // slope of the measured curve times the push magnitude.
            ResponseFunction::CurveResponse { points, .. } => {
                linear_regression_slope(points) * impulse_scale
            }
        }
    }

    /// Evaluate the response at an absolute parameter level.
    /// CurveResponse interpolates on the measured curve (clamped at the
    /// measured range); StepResponse falls back to the linear model.
    pub fn eval_level(&self, level: f64) -> f64 {
        match self {
            ResponseFunction::StepResponse { sensitivity, .. } => sensitivity * level,
            ResponseFunction::CurveResponse { points, .. } => interp_curve(level, points),
        }
    }
}

/// Least-squares slope through (x, y) points; 0.0 for fewer than 2 points.
fn linear_regression_slope(points: &[(f64, f64)]) -> f64 {
    if points.len() < 2 {
        return 0.0;
    }
    let n = points.len() as f64;
    let sx: f64 = points.iter().map(|p| p.0).sum();
    let sy: f64 = points.iter().map(|p| p.1).sum();
    let sxx: f64 = points.iter().map(|p| p.0 * p.0).sum();
    let sxy: f64 = points.iter().map(|p| p.0 * p.1).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < 1e-12 {
        return 0.0;
    }
    (n * sxy - sx * sy) / denom
}

/// Linear interpolation on a curve, clamped outside the measured range.
fn interp_curve(level: f64, points: &[(f64, f64)]) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    let sorted: Vec<(f64, f64)> = {
        let mut v = points.to_vec();
        v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        v
    };
    if level <= sorted[0].0 {
        return sorted[0].1;
    }
    if level >= sorted[sorted.len() - 1].0 {
        return sorted[sorted.len() - 1].1;
    }
    for i in 0..sorted.len() - 1 {
        let (x0, y0) = sorted[i];
        let (x1, y1) = sorted[i + 1];
        if x0 <= level && level <= x1 {
            if (x1 - x0).abs() < 1e-12 {
                return y0;
            }
            let t = (level - x0) / (x1 - x0);
            return y0 + t * (y1 - y0);
        }
    }
    sorted[sorted.len() - 1].1
}

// ─── Characterized Edge ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterizedEdge {
    pub sender_id: String,
    pub receiver_id: String,
    pub channel: ChannelId,

    // Detection
    pub detected: bool,
    pub confidence: f64,
    pub method: String,

    // Response
    pub response: ResponseFunction,
    pub noise_floor: f64,
    pub impulse_scale: f64,

    // Raw measurements
    pub rising_edge: f64,
    pub falling_edge: f64,
    pub baseline_median: f64,
    pub push_median: f64,
    pub pause_median: f64,

    // Sample counts
    pub n_push_samples: usize,
    pub n_pause_samples: usize,
    pub n_baseline_samples: usize,

    // Metadata
    #[serde(default)]
    pub characterized_at: String,
    pub rounds_total: usize,
}

// ─── Node ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalNode {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub objectives: Vec<String>,
    #[serde(default)]
    pub observations: Vec<String>,
}

// ─── Graph ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalGraph {
    pub nodes: Vec<CausalNode>,
    pub edges: Vec<CharacterizedEdge>,
    /// Measured response curves at build time, per (sender, param).
    /// Carries the characterization output into the exported artifact.
    #[serde(default)]
    pub curves: Vec<crate::probe_curves::CurveEntry>,
    #[serde(default)]
    pub built_at: String,
    #[serde(default)]
    pub detector: String,
    #[serde(default)]
    pub detector_params: serde_json::Value,
    #[serde(default)]
    pub breeders_scanned: usize,
    #[serde(default)]
    pub pairs_evaluated: usize,
    #[serde(default)]
    pub edges_detected: usize,
}

impl Default for CausalGraph {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            curves: Vec::new(),
            built_at: String::new(),
            detector: String::new(),
            detector_params: serde_json::Value::Null,
            breeders_scanned: 0,
            pairs_evaluated: 0,
            edges_detected: 0,
        }
    }
}

impl CausalGraph {
    pub fn edges_from(&self, node_id: &str) -> Vec<&CharacterizedEdge> {
        self.edges
            .iter()
            .filter(|e| e.sender_id == node_id)
            .collect()
    }

    pub fn edges_into(&self, node_id: &str) -> Vec<&CharacterizedEdge> {
        self.edges
            .iter()
            .filter(|e| e.receiver_id == node_id)
            .collect()
    }

    pub fn edges_between(&self, a: &str, b: &str) -> Vec<&CharacterizedEdge> {
        self.edges
            .iter()
            .filter(|e| (e.sender_id == a && e.receiver_id == b) || (e.sender_id == b && e.receiver_id == a))
            .collect()
    }

    pub fn detected_edges(&self) -> Vec<&CharacterizedEdge> {
        self.edges.iter().filter(|e| e.detected).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    pub fn summary(&self) -> GraphSummary {
        let detected: Vec<&CharacterizedEdge> = self.detected_edges();
        let avg_confidence = if detected.is_empty() {
            0.0
        } else {
            detected.iter().map(|e| e.confidence).sum::<f64>() / detected.len() as f64
        };

        let strongest = detected
            .iter()
            .max_by(|a, b| {
                a.rising_edge
                    .abs()
                    .partial_cmp(&b.rising_edge.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|e| EdgeSummary {
                sender_id: e.sender_id.clone(),
                receiver_id: e.receiver_id.clone(),
                channel: e.channel.clone(),
                rising_edge: e.rising_edge,
                confidence: e.confidence,
            });

        GraphSummary {
            node_count: self.nodes.len(),
            edge_count: self.edges.len(),
            detected_edge_count: detected.len(),
            avg_confidence,
            strongest_edge: strongest,
            built_at: self.built_at.clone(),
        }
    }

    /// Single-hop prediction (preserved for backward compat).
    pub fn predict(&self, sender_id: &str, impulse_scale: f64) -> Vec<Prediction> {
        self.detected_edges()
            .iter()
            .filter(|e| e.sender_id == sender_id)
            .map(|edge| {
                let shift = edge.response.predict_shift(impulse_scale);
                Prediction {
                    sender_id: sender_id.to_string(),
                    receiver_id: edge.receiver_id.clone(),
                    channel: edge.channel.clone(),
                    impulse_scale,
                    predicted_shift: shift,
                    confidence: edge.confidence,
                    noise_floor: edge.noise_floor,
                    snr_estimate: shift.abs() / edge.noise_floor.max(1e-12),
                    path: vec![sender_id.to_string(), edge.receiver_id.clone()],
                }
            })
            .collect()
    }

    /// Multi-hop prediction: compose sensitivities along all paths from sender.
    ///
    /// For each reachable node, finds the best path (highest confidence) and
    /// composes edge sensitivities multiplicatively. Returns predictions for
    /// both direct neighbors and distant nodes reachable through the graph.
    ///
    /// Naive linear composition: predicted_shift = product(sensitivities) × impulse_scale.
    /// Accurate for linear systems at weak coupling; diverges for strong/nonlinear
    /// coupling where intermediate node state shifts alter edge weights.
    pub fn predict_multihop(&self, sender_id: &str, impulse_scale: f64) -> Vec<Prediction> {
        use std::collections::VecDeque;

        let detected: Vec<&CharacterizedEdge> = self.detected_edges();

        // BFS from sender through detected edges. Track best path per (node, channel).
        // Key: (receiver_id, channel) → (composed_sensitivity, confidence, path)
        let mut best: HashMap<(String, ChannelId), (f64, f64, Vec<String>)> = HashMap::new();

        let mut queue: VecDeque<(String, ChannelId, f64, f64, Vec<String>)> = VecDeque::new();

        // Seed: all direct edges from sender
        for edge in &detected {
            if edge.sender_id == sender_id {
                let sens = match &edge.response {
                    ResponseFunction::StepResponse { sensitivity, .. } => *sensitivity,
                };
                queue.push_back((
                    edge.receiver_id.clone(),
                    edge.channel.clone(),
                    sens,
                    edge.confidence,
                    vec![sender_id.to_string(), edge.receiver_id.clone()],
                ));
            }
        }

        while let Some((node, channel, composed_sens, confidence, path)) = queue.pop_front() {
            let key = (node.clone(), channel.clone());

            // Keep the highest-confidence prediction per (node, channel)
            let is_better = match best.get(&key) {
                Some((_, existing_conf, _)) => confidence > *existing_conf,
                None => true,
            };

            if !is_better {
                continue;
            }

            best.insert(key.clone(), (composed_sens, confidence, path.clone()));

            // Avoid cycles
            if path.len() > self.nodes.len() {
                continue;
            }

            // Extend: find edges leaving this node on this channel
            for edge in &detected {
                if edge.sender_id == node && !path.contains(&edge.receiver_id) {
                    let edge_sens = match &edge.response {
                        ResponseFunction::StepResponse { sensitivity, .. } => *sensitivity,
                    };
                    let mut new_path = path.clone();
                    new_path.push(edge.receiver_id.clone());
                    queue.push_back((
                        edge.receiver_id.clone(),
                        edge.channel.clone(),
                        composed_sens * edge_sens,
                        confidence * edge.confidence,
                        new_path,
                    ));
                }
            }
        }

        best.into_iter()
            .map(|((receiver_id, channel), (composed_sens, confidence, path))| {
                let shift = composed_sens * impulse_scale;
                let noise_floor = detected
                    .iter()
                    .find(|e| e.receiver_id == receiver_id && e.channel == channel)
                    .map(|e| e.noise_floor)
                    .unwrap_or(1e-12);
                Prediction {
                    sender_id: sender_id.to_string(),
                    receiver_id,
                    channel,
                    impulse_scale,
                    predicted_shift: shift,
                    confidence,
                    noise_floor,
                    snr_estimate: shift.abs() / noise_floor.max(1e-12),
                    path,
                }
            })
            .collect()
    }
}

// ─── Summary Types ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct EdgeSummary {
    pub sender_id: String,
    pub receiver_id: String,
    pub channel: ChannelId,
    pub rising_edge: f64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphSummary {
    pub node_count: usize,
    pub edge_count: usize,
    pub detected_edge_count: usize,
    pub avg_confidence: f64,
    pub strongest_edge: Option<EdgeSummary>,
    pub built_at: String,
}

// ─── Prediction ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Prediction {
    pub sender_id: String,
    pub receiver_id: String,
    pub channel: ChannelId,
    pub impulse_scale: f64,
    pub predicted_shift: f64,
    pub confidence: f64,
    pub noise_floor: f64,
    pub snr_estimate: f64,
    pub path: Vec<String>,
}

// ─── Build Result ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct BuildResult {
    pub status: String,
    pub breeders_scanned: usize,
    pub pairs_evaluated: usize,
    pub edges_detected: usize,
    pub edges_total: usize,
    pub duration_secs: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_edge(
        sender: &str,
        receiver: &str,
        sensitivity: f64,
        detected: bool,
    ) -> CharacterizedEdge {
        CharacterizedEdge {
            sender_id: sender.to_string(),
            receiver_id: receiver.to_string(),
            channel: ChannelId::Objective(0),
            detected,
            confidence: 0.8,
            method: "cfar_block_step".to_string(),
            response: ResponseFunction::StepResponse {
                sensitivity,
                baseline: 0.5,
                recovery_fraction: 0.9,
            },
            noise_floor: 0.02,
            impulse_scale: 1.0,
            rising_edge: sensitivity * 0.5,
            falling_edge: sensitivity * 0.45,
            baseline_median: 0.5,
            push_median: 0.5 + sensitivity * 0.5,
            pause_median: 0.5,
            n_push_samples: 15,
            n_pause_samples: 15,
            n_baseline_samples: 15,
            characterized_at: String::new(),
            rounds_total: 3,
        }
    }

    fn make_node(id: &str) -> CausalNode {
        CausalNode {
            id: id.to_string(),
            label: id.to_string(),
            objectives: vec![],
            observations: vec![],
        }
    }

    #[test]
    fn test_predict_multihop_chain4() {
        // Chain: node-1 → node-2 → node-3 → node-4
        // Edge sensitivities: 0.7, 0.5, 0.3
        // Expected composed at node-4: 0.7 × 0.5 × 0.3 = 0.105
        let graph = CausalGraph {
            nodes: vec![
                make_node("node-1"),
                make_node("node-2"),
                make_node("node-3"),
                make_node("node-4"),
            ],
            edges: vec![
                make_edge("node-1", "node-2", 0.7, true),
                make_edge("node-2", "node-3", 0.5, true),
                make_edge("node-3", "node-4", 0.3, true),
            ],
            ..Default::default()
        };

        let preds = graph.predict_multihop("node-1", 1.0);
        assert_eq!(preds.len(), 3, "Should predict 3 reachable nodes");

        // Sort by path length for deterministic checking
        let mut sorted = preds.clone();
        sorted.sort_by_key(|p| p.path.len());

        // node-2: direct, sensitivity 0.7
        let p2 = &sorted[0];
        assert_eq!(p2.receiver_id, "node-2");
        assert_eq!(p2.path, vec!["node-1", "node-2"]);
        assert!(
            (p2.predicted_shift - 0.7).abs() < 0.001,
            "node-2 shift should be ~0.7, got {}",
            p2.predicted_shift
        );

        // node-3: two-hop, sensitivity 0.7 × 0.5 = 0.35
        let p3 = &sorted[1];
        assert_eq!(p3.receiver_id, "node-3");
        assert_eq!(p3.path, vec!["node-1", "node-2", "node-3"]);
        assert!(
            (p3.predicted_shift - 0.35).abs() < 0.001,
            "node-3 shift should be ~0.35, got {}",
            p3.predicted_shift
        );

        // node-4: three-hop, sensitivity 0.7 × 0.5 × 0.3 = 0.105
        let p4 = &sorted[2];
        assert_eq!(p4.receiver_id, "node-4");
        assert_eq!(
            p4.path,
            vec!["node-1", "node-2", "node-3", "node-4"]
        );
        assert!(
            (p4.predicted_shift - 0.105).abs() < 0.001,
            "node-4 shift should be ~0.105, got {}",
            p4.predicted_shift
        );
    }

    #[test]
    fn test_predict_single_hop_unchanged() {
        // Verify the original single-hop predict still works
        let graph = CausalGraph {
            nodes: vec![make_node("A"), make_node("B")],
            edges: vec![make_edge("A", "B", 0.6, true)],
            ..Default::default()
        };

        let preds = graph.predict("A", 1.0);
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].receiver_id, "B");
        assert_eq!(preds[0].path, vec!["A", "B"]);
        assert!((preds[0].predicted_shift - 0.6).abs() < 0.001);
    }

    #[test]
    fn test_predict_multihop_no_edges() {
        let graph = CausalGraph {
            nodes: vec![make_node("A"), make_node("B")],
            edges: vec![make_edge("A", "B", 0.6, false)], // NOT detected
            ..Default::default()
        };

        let preds = graph.predict_multihop("A", 1.0);
        assert!(preds.is_empty(), "Undetected edges should not propagate");
    }

    #[test]
    fn test_predict_multihop_cycle_protection() {
        // A → B → A (cycle). Should not loop forever.
        let graph = CausalGraph {
            nodes: vec![make_node("A"), make_node("B")],
            edges: vec![
                make_edge("A", "B", 0.5, true),
                make_edge("B", "A", 0.5, true),
            ],
            ..Default::default()
        };

        let preds = graph.predict_multihop("A", 1.0);
        // A→B is a valid prediction. B→A would cycle back to sender — skipped.
        assert!(
            preds.iter().any(|p| p.receiver_id == "B"),
            "Should predict B"
        );
        assert!(
            !preds.iter().any(|p| p.receiver_id == "A"),
            "Should not predict back to sender via cycle"
        );
    }

    // ─── CurveResponse ────────────────────────────────────────────

    #[test]
    fn test_curve_response_predict_shift_linearization() {
        let rf = ResponseFunction::CurveResponse {
            points: vec![(0.0, 0.0), (10.0, 2.0), (20.0, 4.0)],
            converged: false,
        };
        // Least-squares slope = 0.2 → shift at scale 5 is 1.0
        assert!(
            (rf.predict_shift(5.0) - 1.0).abs() < 1e-9,
            "linearized shift, got {}",
            rf.predict_shift(5.0)
        );
    }

    #[test]
    fn test_curve_response_eval_level_interpolates() {
        // Saturating curve: rises 0→1 over levels 0→20, flat afterwards.
        let rf = ResponseFunction::CurveResponse {
            points: vec![(0.0, 0.0), (20.0, 1.0), (40.0, 1.0)],
            converged: true,
        };
        assert!((rf.eval_level(10.0) - 0.5).abs() < 1e-9, "midpoint interpolation");
        assert!((rf.eval_level(30.0) - 1.0).abs() < 1e-9, "flat region");
        assert!((rf.eval_level(50.0) - 1.0).abs() < 1e-9, "clamped above range (saturation)");
        assert!((rf.eval_level(-5.0) - 0.0).abs() < 1e-9, "clamped below range");
    }

    #[test]
    fn test_artifact_roundtrip_with_curves() {
        use crate::probe_curves::{CurveEntry, CurveState};

        let edge = CharacterizedEdge {
            response: ResponseFunction::CurveResponse {
                points: vec![(20.0, 0.1), (40.0, 0.25), (60.0, 0.3)],
                converged: true,
            },
            ..make_edge("A", "B", 0.5, true)
        };
        let graph = CausalGraph {
            edges: vec![edge],
            curves: vec![CurveEntry {
                sender_id: "A".to_string(),
                param: "param_1".to_string(),
                state: CurveState {
                    num_points: 3,
                    last_delta: 0.01,
                    converged: true,
                    points: vec![(20.0, 0.1), (40.0, 0.25), (60.0, 0.3)],
                },
            }],
            ..Default::default()
        };

        let json = crate::artifact::export_artifact(&graph).unwrap();
        let back = crate::artifact::import_artifact(&json).unwrap();

        assert_eq!(back.curves.len(), 1);
        assert_eq!(back.curves[0].sender_id, "A");
        assert_eq!(back.curves[0].state.points, graph.curves[0].state.points);
        assert_eq!(back.curves[0].state.converged, true);
        match &back.edges[0].response {
            ResponseFunction::CurveResponse { points, converged } => {
                assert_eq!(points.len(), 3);
                assert!(*converged);
            }
            _ => panic!("expected CurveResponse after round-trip"),
        }
    }

    #[test]
    fn test_old_artifact_without_curves_still_imports() {
        // Pre-curve artifacts have no `curves` key — serde default keeps
        // them importable.
        let legacy = r#"{
            "nodes": [],
            "edges": [],
            "built_at": "2026-08-01T00:00:00Z",
            "detector": "cfar",
            "edges_detected": 0
        }"#;
        let back = crate::artifact::import_artifact(legacy).unwrap();
        assert!(back.curves.is_empty());
    }
}
