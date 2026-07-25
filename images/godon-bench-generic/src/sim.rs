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

    /// Recompute objectives for a node: base + coupling contributions + noise.
    fn recompute_objectives(&mut self, requesting_node: &str) -> Vec<f64> {
        // Compute base for all nodes first
        let mut base_values: HashMap<String, Vec<f64>> = HashMap::new();
        for (id, node) in &self.nodes {
            base_values.insert(id.clone(), node.compute_base());
        }

        let mut result = base_values[requesting_node].clone();

        // Add coupling contributions from edges pointing INTO this node.
        // Edge strengths drift over time if drift_rate > 0.
        for edge in &self.edges {
            if edge.to == requesting_node && edge.to_channel < result.len() {
                let effective_strength = if edge.drift_rate > 0.0 {
                    edge.strength * (1.0 + edge.drift_rate * self.total_ticks as f64)
                } else {
                    edge.strength
                };
                if let Some(source_base) = base_values.get(&edge.from) {
                    if edge.from_channel < source_base.len() {
                        result[edge.to_channel] += effective_strength * source_base[edge.from_channel];
                    }
                }
            }
        }

        // Add stacked noise
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

    pub fn get_status(&self, node_id: &str) -> Option<Vec<f64>> {
        self.nodes.get(node_id).map(|n| n.objectives.clone())
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
