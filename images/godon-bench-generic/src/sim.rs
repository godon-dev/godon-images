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
    pub param_lower: f64,
    #[serde(default)]
    pub param_upper: f64,
}

fn default_base() -> String {
    "linear".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeConfig {
    pub from: String,
    pub from_channel: usize,
    pub to: String,
    pub to_channel: usize,
    pub strength: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseConfig {
    #[serde(default = "default_sigma")]
    pub sigma: f64,
    #[serde(default = "default_noise_type")]
    pub noise_type: String,
    #[serde(default)]
    pub drift_rate: f64,
}

fn default_sigma() -> f64 {
    0.02
}

fn default_noise_type() -> String {
    "gaussian".to_string()
}

impl Default for NoiseConfig {
    fn default() -> Self {
        Self {
            sigma: 0.02,
            noise_type: "gaussian".to_string(),
            drift_rate: 0.0,
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
    pub current_drift: f64,
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
            current_drift: 0.0,
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

        let normalized_params: Vec<f64> = self.params.iter()
            .map(|p| normalize(*p, self.config.param_lower, self.config.param_upper))
            .collect();

        for obj_idx in 0..n_obj {
            if let Some(row) = self.config.weights.get(obj_idx) {
                for (param_idx, w) in row.iter().enumerate() {
                    if let Some(np) = normalized_params.get(param_idx) {
                        match self.config.base.as_str() {
                            "polynomial" => result[obj_idx] += w * np * np,
                            "threshold" => result[obj_idx] += w * (if *np > 0.5 { 1.0 } else { 0.0 }),
                            _ => result[obj_idx] += w * np, // linear + default
                        }
                    }
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
    pub http_client: reqwest::Client,
    pub colored_state: f64,
    pub current_drift_global: f64,
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

        Self {
            nodes,
            edges: config.edges,
            noise: config.noise,
            rng: rand::SeedableRng::seed_from_u64(seed),
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            colored_state: 0.0,
            current_drift_global: 0.0,
        }
    }

    /// Apply params to a node, then recompute all node objectives.
    pub fn apply(&mut self, node_id: &str, params: &[f64]) -> Vec<f64> {
        if let Some(node) = self.nodes.get_mut(node_id) {
            node.apply_params(params);
            node.tick += 1;
        }

        // Update drift if non-stationary noise
        if self.noise.noise_type == "nonstationary" {
            self.current_drift_global += self.noise.drift_rate;
        }

        self.recompute_objectives(node_id)
    }

    /// Recompute objectives for a node: base + coupling contributions + noise.
    fn recompute_objectives(&mut self, requesting_node: &str) -> Vec<f64> {
        // Compute base for all nodes first (immutable borrow)
        let mut base_values: HashMap<String, Vec<f64>> = HashMap::new();
        for (id, node) in &self.nodes {
            base_values.insert(id.clone(), node.compute_base());
        }

        // Get the requesting node
        let node = match self.nodes.get(requesting_node) {
            Some(n) => n,
            None => return vec![],
        };

        let mut result = base_values[requesting_node].clone();

        // Add coupling contributions from edges pointing INTO this node
        for edge in &self.edges {
            if edge.to == requesting_node && edge.to_channel < result.len() {
                if let Some(source_base) = base_values.get(&edge.from) {
                    if edge.from_channel < source_base.len() {
                        result[edge.to_channel] += edge.strength * source_base[edge.from_channel];
                    }
                }
            }
        }

        // Add noise
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
        match self.noise.noise_type.as_str() {
            "gaussian" => {
                let u1: f64 = self.rng.gen_range(0.0001..1.0);
                let u2: f64 = self.rng.gen_range(0.0001..1.0);
                let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
                z * self.noise.sigma
            }
            "colored" => {
                // Autocorrelated noise — slower changing than white
                self.colored_state = 0.7 * self.colored_state + 0.3 * self.gaussian_raw();
                self.colored_state * self.noise.sigma
            }
            "nonstationary" => {
                let z = self.gaussian_raw();
                z * self.noise.sigma * (1.0 + self.current_drift_global)
            }
            _ => 0.0,
        }
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
