use rand::rngs::StdRng;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

// ─── Config ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchConfig {
    pub nodes: Vec<NodeConfig>,
    #[serde(default)]
    pub edges: Vec<EdgeConfig>,
    #[serde(default)]
    pub noise: NoiseConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub id: String,
    pub params: usize,
    pub objectives: usize,
    #[serde(default = "default_base")]
    pub base: String,
    /// Where incoming coupling enters:
    ///   "post"    (default) added to the output channel after this
    ///             node's own map — legacy behavior
    ///   "through" joins the channel state; the base map acts on the
    ///             combined level (own weighted input + incoming)
    #[serde(default = "default_intake")]
    pub intake: String,
    #[serde(default)]
    pub weights: Vec<Vec<f64>>,
    #[serde(default)]
    pub interactions: Vec<InteractionConfig>,
    #[serde(default)]
    pub param_lower: f64,
    #[serde(default)]
    pub param_upper: f64,
}

fn default_base() -> String {
    "linear".to_string()
}

fn default_intake() -> String {
    "post".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionConfig {
    pub params: Vec<usize>,
    pub weight: f64,
    pub objective: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeConfig {
    pub from: String,
    pub from_channel: usize,
    pub to: String,
    pub to_channel: usize,
    pub strength: f64,
    #[serde(default)]
    pub drift_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseConfig {
    /// White Gaussian jitter. Set to 0.0 to disable.
    #[serde(default = "default_gaussian")]
    pub gaussian_sigma: f64,
    /// Autocorrelated drift noise. Set to 0.0 to disable.
    #[serde(default)]
    pub colored_sigma: f64,
    /// Non-stationary noise growth rate. Set to 0.0 to disable.
    #[serde(default)]
    pub drift_rate: f64,
    // Legacy: single sigma + type (backwards compat with old configs)
    #[serde(default)]
    pub sigma: Option<f64>,
    #[serde(default)]
    pub noise_type: Option<String>,
}

fn default_gaussian() -> f64 {
    0.02
}

impl Default for NoiseConfig {
    fn default() -> Self {
        Self {
            gaussian_sigma: 0.02,
            colored_sigma: 0.0,
            drift_rate: 0.0,
            sigma: None,
            noise_type: None,
        }
    }
}

// ─── Node State ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NodeState {
    pub id: String,
    pub config: NodeConfig,
    pub params: Vec<f64>,
    pub objectives: Vec<f64>,
    pub tick: u64,
}

impl NodeState {
    pub fn new(config: NodeConfig) -> Self {
        let params = vec![config.param_lower; config.params];
        let objectives = vec![0.0; config.objectives];
        Self {
            id: config.id.clone(),
            config,
            params,
            objectives,
            tick: 0,
        }
    }

    pub fn apply_params(&mut self, params: &[f64]) {
        for (i, p) in params.iter().take(self.params.len()).enumerate() {
            self.params[i] = *p;
        }
    }

    /// Compute base objective from params, before coupling and noise.
    pub fn compute_base(&self) -> Vec<f64> {
        let n_obj = self.config.objectives;
        let mut result = vec![0.0; n_obj];

        let normalized: Vec<f64> = self.params.iter()
            .map(|p| normalize(*p, self.config.param_lower, self.config.param_upper))
            .collect();

        for obj_idx in 0..n_obj {
            // Per-parameter contribution (shape depends on base function)
            if let Some(row) = self.config.weights.get(obj_idx) {
                for (param_idx, w) in row.iter().enumerate() {
                    if let Some(np) = normalized.get(param_idx) {
                        result[obj_idx] += w * match self.config.base.as_str() {
                            "polynomial" => np * np,
                            "threshold" => if *np > 0.5 { 1.0 } else { 0.0 },
                            "saturation" => np / (1.0 + (np - 0.5).abs() * 4.0),
                            _ => *np, // linear + default
                        };
                    }
                }
            }

            // Cross-parameter interaction contributions (always multiplicative)
            for inter in &self.config.interactions {
                if inter.objective == obj_idx {
                    let product: f64 = inter.params.iter()
                        .filter_map(|&pi| normalized.get(pi))
                        .product();
                    result[obj_idx] += inter.weight * product;
                }
            }
        }

        result
    }

    /// Weighted own contributions per objective, unshaped — the node's
    /// input into each channel before any map runs.
    pub fn compute_channel_inputs(&self) -> Vec<f64> {
        let n_obj = self.config.objectives;
        let mut result = vec![0.0; n_obj];
        let normalized: Vec<f64> = self.params.iter()
            .map(|p| normalize(*p, self.config.param_lower, self.config.param_upper))
            .collect();
        for obj_idx in 0..n_obj {
            if let Some(row) = self.config.weights.get(obj_idx) {
                for (param_idx, w) in row.iter().enumerate() {
                    if let Some(np) = normalized.get(param_idx) {
                        result[obj_idx] += w * np;
                    }
                }
            }
        }
        result
    }

    /// The base map applied to a single channel-input value.
    fn shape_value(&self, v: f64) -> f64 {
        match self.config.base.as_str() {
            "polynomial" => v * v,
            "threshold" => if v > 0.5 { 1.0 } else { 0.0 },
            "saturation" => v / (1.0 + (v - 0.5).abs() * 4.0),
            _ => v, // linear + default
        }
    }

    /// Cross-parameter interaction terms, one per objective.
    fn interaction_terms(&self) -> Vec<f64> {
        let mut result = vec![0.0; self.config.objectives];
        let normalized: Vec<f64> = self.params.iter()
            .map(|p| normalize(*p, self.config.param_lower, self.config.param_upper))
            .collect();
        for inter in &self.config.interactions {
            if let Some(slot) = result.get_mut(inter.objective) {
                let product: f64 = inter.params.iter()
                    .filter_map(|&pi| normalized.get(pi))
                    .product();
                *slot += inter.weight * product;
            }
        }
        result
    }

    /// intake "through": the door — the channel state (own weighted
    /// input + incoming coupling) meets the base map once; interaction
    /// terms stay additive after it.
    pub fn map_channel_inputs(&self, incoming: &[f64]) -> Vec<f64> {
        let mut u = self.compute_channel_inputs();
        for (ch, inc) in incoming.iter().enumerate() {
            if ch < u.len() {
                u[ch] += inc;
            }
        }
        let terms = self.interaction_terms();
        u.iter()
            .map(|&v| self.shape_value(v))
            .zip(terms.iter())
            .map(|(shaped, term)| shaped + term)
            .collect()
    }
}

fn normalize(val: f64, lower: f64, upper: f64) -> f64 {
    if upper <= lower {
        return 0.0;
    }
    (val - lower) / (upper - lower)
}

// ─── Simulator ──────────────────────────────────────────────────────

pub struct Simulator {
    pub nodes: HashMap<String, NodeState>,
    pub edges: Vec<EdgeConfig>,
    pub noise: NoiseConfig,
    pub rng: StdRng,
    pub colored_state: f64,
    pub nonstationary_drift: f64,
    pub total_ticks: u64,
}

pub type SharedSimulator = Arc<Mutex<Simulator>>;

impl Simulator {
    pub fn from_config(config: BenchConfig) -> Self {
        let mut nodes = HashMap::new();
        for nc in &config.nodes {
            nodes.insert(nc.id.clone(), NodeState::new(nc.clone()));
        }

        let seed: u64 = std::env::var("GENERIC_SEED")
            .unwrap_or_else(|_| "42".to_string())
            .parse()
            .unwrap_or(42);

        // Handle legacy noise config (single sigma + type → new stacked format)
        let noise = resolve_noise(config.noise);

        Self {
            nodes,
            edges: config.edges,
            noise,
            rng: rand::SeedableRng::seed_from_u64(seed),
            colored_state: 0.0,
            nonstationary_drift: 0.0,
            total_ticks: 0,
        }
    }

    /// Apply params to a node, then recompute all node objectives.
    pub fn apply(&mut self, node_id: &str, params: &[f64]) -> Vec<f64> {
        if let Some(node) = self.nodes.get_mut(node_id) {
            node.apply_params(params);
            node.tick += 1;
        }
        self.total_ticks += 1;

        // Update non-stationary noise drift
        if self.noise.drift_rate > 0.0 {
            self.nonstationary_drift += self.noise.drift_rate;
        }

        self.recompute_objectives(node_id)
    }

    /// Recompute objectives for a node: base + cascaded coupling + noise.
    ///
    /// Coupling propagates through the entire graph via iterative relaxation
    /// (Jacobi iteration). Each pass extends signal one hop. For a graph with
    /// N nodes, N-1 passes suffice for exact cascade in any DAG. Cyclic graphs
    /// converge if loop gain < 1.
    fn recompute_objectives(&mut self, requesting_node: &str) -> Vec<f64> {
        // Initialize all nodes at their base (parameter-derived) values
        let mut coupled: HashMap<String, Vec<f64>> = HashMap::new();
        for (id, node) in &self.nodes {
            coupled.insert(id.clone(), node.compute_base());
        }

        // Iterative relaxation: propagate coupling through the graph.
        // Noise-free — coupling is physical, noise is measurement at readout.
        let max_iter = self.nodes.len().saturating_sub(1).max(1);
        for _ in 0..max_iter {
            let prev = coupled.clone();
            let mut converged = true;

            for (id, coupled_vals) in coupled.iter_mut() {
                let node = &self.nodes[id];

                // Incoming coupling into this node's channels, from the
                // previous relaxation pass.
                let mut incoming = vec![0.0; node.config.objectives];
                for edge in &self.edges {
                    if edge.to == *id && edge.to_channel < incoming.len() {
                        let effective_strength = if edge.drift_rate > 0.0 {
                            edge.strength * (1.0 + edge.drift_rate * self.total_ticks as f64)
                        } else {
                            edge.strength
                        };
                        if let Some(source) = prev.get(&edge.from) {
                            if edge.from_channel < source.len() {
                                incoming[edge.to_channel] +=
                                    effective_strength * source[edge.from_channel];
                            }
                        }
                    }
                }

                let mut updated = if node.config.intake == "through" {
                    // The door: incoming joins the channel state; the base
                    // map acts on the combined level.
                    node.map_channel_inputs(&incoming)
                } else {
                    // Legacy: coupling added to the output channel after
                    // this node's own map.
                    let mut u = node.compute_base();
                    for (ch, inc) in incoming.iter().enumerate() {
                        if ch < u.len() {
                            u[ch] += inc;
                        }
                    }
                    u
                };

                for (i, &new_v) in updated.iter().enumerate() {
                    if let Some(&old_v) = coupled_vals.get(i) {
                        if (new_v - old_v).abs() > 1e-10 {
                            converged = false;
                        }
                    }
                }
                *coupled_vals = updated;
            }

            if converged {
                break;
            }
        }

        // Add stacked noise to the requesting node only
        let mut result = coupled[requesting_node].clone();
        for val in result.iter_mut() {
            *val += self.generate_noise();
        }

        // Update stored objectives
        if let Some(n) = self.nodes.get_mut(requesting_node) {
            n.objectives = result.clone();
        }

        result
    }

    fn generate_noise(&mut self) -> f64 {
        let mut noise = 0.0;

        // Gaussian component
        if self.noise.gaussian_sigma > 0.0 {
            noise += self.gaussian_raw() * self.noise.gaussian_sigma;
        }

        // Colored component (autocorrelated)
        if self.noise.colored_sigma > 0.0 {
            self.colored_state = 0.7 * self.colored_state + 0.3 * self.gaussian_raw();
            noise += self.colored_state * self.noise.colored_sigma;
        }

        // Non-stationary component (growing amplitude)
        if self.noise.drift_rate > 0.0 {
            noise += self.gaussian_raw() * 0.02 * (1.0 + self.nonstationary_drift);
        }

        noise
    }

    fn gaussian_raw(&mut self) -> f64 {
        let u1: f64 = self.rng.gen_range(0.0001..1.0);
        let u2: f64 = self.rng.gen_range(0.0001..1.0);
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    pub fn get_status(&mut self, node_id: &str) -> Option<Vec<f64>> {
        // Live read: recompute coupled objectives (fresh measurement noise)
        // before returning. A stored snapshot would hide cross-node coupling
        // until the measured node itself is re-applied.
        if !self.nodes.contains_key(node_id) {
            return None;
        }
        Some(self.recompute_objectives(node_id))
    }

    pub fn list_nodes(&self) -> Vec<String> {
        self.nodes.keys().cloned().collect()
    }
}

/// Convert legacy noise config (sigma + noise_type) to the new stacked format.
fn resolve_noise(mut noise: NoiseConfig) -> NoiseConfig {
    // If legacy sigma/type are set and the new fields are at defaults, migrate
    if let (Some(sigma), Some(ntype)) = (noise.sigma, noise.noise_type.clone()) {
        if noise.gaussian_sigma == 0.02 && noise.colored_sigma == 0.0 && noise.drift_rate == 0.0 {
            // Only default gaussian set — override with legacy
            noise.gaussian_sigma = 0.0;
            match ntype.as_str() {
                "gaussian" => noise.gaussian_sigma = sigma,
                "colored" => noise.colored_sigma = sigma,
                "nonstationary" => noise.drift_rate = noise.drift_rate, // legacy drift_rate field
                _ => noise.gaussian_sigma = sigma,
            }
        }
    }
    noise
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_node_sim() -> Simulator {
        let config: serde_json::Value = serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]]},
                {"id": "b", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[0.5]]}
            ],
            "edges": [
                {"from": "a", "from_channel": 0, "to": "b", "to_channel": 0,
                 "strength": 0.7}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        });
        let bench: BenchConfig = serde_json::from_value(config).unwrap();
        Simulator::from_config(bench)
    }

    #[test]
    fn read_reflects_cross_node_coupling_without_receiver_apply() {
        // Regression: get_status must recompute, not return a stored
        // snapshot. Sender a applies param 100 (np=1) -> a.obj0 = 1.0;
        // coupled b.obj0 must read 0.7 WITHOUT b being re-applied
        // (b's own base is 0: its param defaults to lower).
        // The stale-read bug returned 0.0 here.
        let mut sim = two_node_sim();
        sim.apply("a", &[100.0]);
        let b = sim.get_status("b").expect("node b exists");
        assert!((b[0] - 0.7).abs() < 1e-9, "b should read coupled 0.7, got {}", b[0]);
    }

    #[test]
    fn reads_advance_coupling_when_sender_changes() {
        let mut sim = two_node_sim();
        sim.apply("a", &[100.0]);
        let first = sim.get_status("b").unwrap()[0];
        sim.apply("a", &[0.0]); // a.obj back to 0
        let second = sim.get_status("b").unwrap()[0];
        assert!((first - 0.7).abs() < 1e-9);
        assert!((second - 0.0).abs() < 1e-9, "b should track sender, got {}", second);
    }

    #[test]
    fn unknown_node_returns_none() {
        let mut sim = two_node_sim();
        assert!(sim.get_status("nope").is_none());
    }

    // ─── intake "through" (the door) ───────────────────────────────

    fn door_saturation_sim() -> Simulator {
        let config: serde_json::Value = serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]]},
                {"id": "b", "params": 1, "objectives": 1, "base": "saturation",
                 "intake": "through",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]]}
            ],
            "edges": [
                {"from": "a", "from_channel": 0, "to": "b", "to_channel": 0,
                 "strength": 1.0}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        });
        let bench: BenchConfig = serde_json::from_value(config).unwrap();
        Simulator::from_config(bench)
    }

    #[test]
    fn through_intake_door_bends_incoming_signal() {
        // a at full -> out 1.0. b's door is saturation over the combined
        // channel state (own parked at lower): saturation(1.0) = 1/3.
        // Legacy post intake would deliver the incoming 1.0 untouched.
        let mut sim = door_saturation_sim();
        sim.apply("a", &[100.0]);
        let b = sim.get_status("b").unwrap();
        assert!((b[0] - 1.0 / 3.0).abs() < 1e-9,
            "door should bend incoming to 1/3, got {}", b[0]);
    }

    #[test]
    fn through_intake_combines_own_and_incoming_before_map() {
        // b own param 50 (np 0.5 -> own input 0.5) + incoming 0.3 =
        // channel state 0.8; the threshold door fires -> 1.0.
        // Legacy would give threshold(0.5)=0 + 0.3 = 0.3.
        let config_json = serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]]},
                {"id": "b", "params": 1, "objectives": 1, "base": "threshold",
                 "intake": "through",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]]}
            ],
            "edges": [
                {"from": "a", "from_channel": 0, "to": "b", "to_channel": 0,
                 "strength": 0.3}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        });
        // a at full sends 1.0; edge gain 0.3 -> incoming 0.3
        let mut sim = Simulator::from_config(serde_json::from_value(config_json).unwrap());
        sim.apply("b", &[50.0]);
        sim.apply("a", &[100.0]);
        let b = sim.get_status("b").unwrap();
        assert!((b[0] - 1.0).abs() < 1e-9,
            "door fires on combined level 0.8, expected 1.0, got {}", b[0]);
    }

    #[test]
    fn through_intake_three_node_nested_arithmetic() {
        // a linear -> a. Edge 0.7 into b (saturation door, own parked).
        // Edge 0.5 into c (linear door, own parked).
        // a = 0.8: b state 0.56, sat(0.56) = 0.56/1.24; c = 0.5 * 0.56/1.24.
        let config: serde_json::Value = serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]]},
                {"id": "b", "params": 1, "objectives": 1, "base": "saturation",
                 "intake": "through",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]]},
                {"id": "c", "params": 1, "objectives": 1, "base": "linear",
                 "intake": "through",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]]}
            ],
            "edges": [
                {"from": "a", "from_channel": 0, "to": "b", "to_channel": 0,
                 "strength": 0.7},
                {"from": "b", "from_channel": 0, "to": "c", "to_channel": 0,
                 "strength": 0.5}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        });
        let mut sim = Simulator::from_config(serde_json::from_value(config).unwrap());
        sim.apply("a", &[80.0]);
        let c = sim.get_status("c").unwrap();
        let expected = 0.5 * 0.56 / 1.24;
        assert!((c[0] - expected).abs() < 1e-9,
            "nested chain should give {}, got {}", expected, c[0]);
    }
}
