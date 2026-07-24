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
}

impl ResponseFunction {
    pub fn predict_shift(&self, impulse_scale: f64) -> f64 {
        match self {
            ResponseFunction::StepResponse { sensitivity, .. } => sensitivity * impulse_scale,
        }
    }
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
