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
    /// Shape drift: the node's map morphs from its declared weights/base
    /// toward each entry's target, in tick order. Travel is linear in
    /// ticks and saturates — the world settles into the target shape.
    /// Overlapping travel windows: a later entry supersedes the previous
    /// one and restarts from its target.
    #[serde(default)]
    pub morphs: Vec<MorphConfig>,
    /// Readout bias drift: this node's objectives slide by
    /// `offset_drift_rate × total_ticks`. A sensor/ambient bias at THIS
    /// node's readout — it does not cascade into other nodes. This is
    /// the canonical drift of the re-walk design: the curve slides,
    /// direction and slope survive.
    #[serde(default)]
    pub offset_drift_rate: f64,
    /// Offset drift cap: once the readout has slid this far, it stops -
    /// the world moved to a defined point and settled (drift with a
    /// destination). None = unbounded (the classic ramp).
    #[serde(default)]
    pub offset_drift_cap: Option<f64>,
}

/// One scheduled shape target. `travel_ticks` 0 = instant step at
/// `at_tick`; otherwise the map travels linearly from the previous
/// settled shape to the target over that many ticks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MorphConfig {
    pub at_tick: u64,
    #[serde(default)]
    pub travel_ticks: u64,
    #[serde(default)]
    pub target_weights: Option<Vec<Vec<f64>>>,
    #[serde(default)]
    pub target_base: Option<String>,
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

    /// Compute base objective from params, before coupling and noise,
    /// under the EFFECTIVE shape (morph-aware weights and base blend).
    pub fn compute_base_eff(
        &self,
        weights: &[Vec<f64>],
        base_a: &str,
        base_b: Option<&str>,
        p: f64,
    ) -> Vec<f64> {
        let n_obj = self.config.objectives;
        let mut result = vec![0.0; n_obj];

        let normalized: Vec<f64> = self.params.iter()
            .map(|p| normalize(*p, self.config.param_lower, self.config.param_upper))
            .collect();

        for obj_idx in 0..n_obj {
            if let Some(row) = weights.get(obj_idx) {
                for (param_idx, w) in row.iter().enumerate() {
                    if let Some(np) = normalized.get(param_idx) {
                        result[obj_idx] += w * blend_base(base_a, base_b, p, *np);
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
    pub fn compute_channel_inputs_eff(&self, weights: &[Vec<f64>]) -> Vec<f64> {
        let n_obj = self.config.objectives;
        let mut result = vec![0.0; n_obj];
        let normalized: Vec<f64> = self.params.iter()
            .map(|p| normalize(*p, self.config.param_lower, self.config.param_upper))
            .collect();
        for obj_idx in 0..n_obj {
            if let Some(row) = weights.get(obj_idx) {
                for (param_idx, w) in row.iter().enumerate() {
                    if let Some(np) = normalized.get(param_idx) {
                        result[obj_idx] += w * np;
                    }
                }
            }
        }
        result
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
    pub fn map_channel_inputs_eff(
        &self,
        incoming: &[f64],
        weights: &[Vec<f64>],
        base_a: &str,
        base_b: Option<&str>,
        p: f64,
    ) -> Vec<f64> {
        let mut u = self.compute_channel_inputs_eff(weights);
        for (ch, inc) in incoming.iter().enumerate() {
            if ch < u.len() {
                u[ch] += inc;
            }
        }
        let terms = self.interaction_terms();
        u.iter()
            .map(|&v| blend_base(base_a, base_b, p, v))
            .zip(terms.iter())
            .map(|(shaped, term)| shaped + term)
            .collect()
    }
}

/// One base map evaluated at a normalized input.
fn apply_base(name: &str, np: f64) -> f64 {
    match name {
        "polynomial" => np * np,
        "threshold" => if np > 0.5 { 1.0 } else { 0.0 },
        "saturation" => np / (1.0 + (np - 0.5).abs() * 4.0),
        _ => np, // linear + default
    }
}

/// The base blend: (1−p) of shape A plus p of shape B. No B → pure A.
fn blend_base(base_a: &str, base_b: Option<&str>, p: f64, np: f64) -> f64 {
    match base_b {
        None => apply_base(base_a, np),
        Some(b) => (1.0 - p) * apply_base(base_a, np) + p * apply_base(b, np),
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

/// The node's shape at one instant: morphed weights and, while a
/// travel is in flight, the base blend from A toward B.
#[derive(Debug, Clone)]
pub struct EffShape {
    pub weights: Vec<Vec<f64>>,
    pub base_a: String,
    pub base_b: String,
    pub progress: f64,
}

/// One edge's coupling at the current tick.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeTruth {
    pub from: String,
    pub to: String,
    pub to_channel: usize,
    pub strength_effective: f64,
}

/// The ground-truth read: noise-free objectives at the current tick
/// (readout bias included — it is world, not measurement), the node's
/// effective shape, and the effective coupling strengths.
#[derive(Debug, Clone, Serialize)]
pub struct TruthReport {
    pub node_id: String,
    pub total_ticks: u64,
    pub objectives: Vec<f64>,
    pub effective_weights: Vec<Vec<f64>>,
    pub base_from: String,
    pub base_to: String,
    pub base_progress: f64,
    pub edges: Vec<EdgeTruth>,
}

fn lerp_weights(a: &[Vec<f64>], b: &[Vec<f64>], p: f64) -> Vec<Vec<f64>> {
    a.iter()
        .zip(b.iter())
        .map(|(ra, rb)| {
            ra.iter()
                .zip(rb.iter())
                .map(|(x, y)| x + (y - x) * p)
                .collect()
        })
        .collect()
}

impl Simulator {
    pub fn from_config(config: BenchConfig) -> Self {
        // Fail loud at boot: a mis-declared morph would silently corrupt
        // the ground truth the whole validation run leans on.
        for nc in &config.nodes {
            let mut last_tick: Option<u64> = None;
            for m in &nc.morphs {
                if let Some(last) = last_tick {
                    assert!(
                        m.at_tick >= last,
                        "node '{}': morph at_tick {} precedes an earlier morph's window (at_tick {})",
                        nc.id, m.at_tick, last
                    );
                }
                last_tick = Some(m.at_tick + m.travel_ticks);
                if m.target_weights.is_none() && m.target_base.is_none() {
                    panic!(
                        "node '{}': morph at tick {} carries no target (set target_weights and/or target_base)",
                        nc.id, m.at_tick
                    );
                }
                if let Some(w) = &m.target_weights {
                    assert!(
                        w.len() == nc.objectives
                            && w.iter().all(|row| row.len() == nc.params),
                        "node '{}': morph target_weights must be {}x{} (objectives x params)",
                        nc.id, nc.objectives, nc.params
                    );
                }
            }
        }

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

    /// The node's shape at the current tick: declared start, advanced
    /// through every scheduled morph. A morph reached but still
    /// traveling leaves a blend in flight (progress < 1); a completed
    /// morph settles into its target.
    fn effective_shape(&self, nc: &NodeConfig) -> EffShape {
        let mut sorted = nc.morphs.clone();
        sorted.sort_by_key(|m| m.at_tick);

        let mut weights = nc.weights.clone();
        let mut base = nc.base.clone();
        let mut inflight: Option<EffShape> = None;

        for m in &sorted {
            if m.at_tick > self.total_ticks {
                break;
            }
            let target_w = m.target_weights.clone().unwrap_or_else(|| weights.clone());
            let target_b = m.target_base.clone().unwrap_or_else(|| base.clone());
            let progress = if m.travel_ticks == 0 {
                1.0
            } else {
                ((self.total_ticks - m.at_tick) as f64 / m.travel_ticks as f64).clamp(0.0, 1.0)
            };
            if progress >= 1.0 {
                weights = target_w;
                base = target_b;
                inflight = None;
            } else {
                inflight = Some(EffShape {
                    weights: lerp_weights(&weights, &target_w, progress),
                    base_a: base.clone(),
                    base_b: target_b.clone(),
                    progress,
                });
                weights = target_w;
                base = target_b;
            }
        }

        inflight.unwrap_or(EffShape {
            weights,
            base_a: base.clone(),
            base_b: base,
            progress: 1.0,
        })
    }

    /// Signed edge drift: the coupling travels through zero and can
    /// invert. Deterministic in the global tick count.
    fn edge_strength(&self, e: &EdgeConfig) -> f64 {
        e.strength * (1.0 + e.drift_rate * self.total_ticks as f64)
    }

    /// Noise-free cascade of the whole graph at the current tick, under
    /// each node's effective shape. The coupling is physical — noise is
    /// measurement at readout and lives only in the live paths.
    fn cascade(&self) -> HashMap<String, Vec<f64>> {
        let eff: HashMap<String, EffShape> = self
            .nodes
            .iter()
            .map(|(id, n)| (id.clone(), self.effective_shape(&n.config)))
            .collect();

        let mut coupled: HashMap<String, Vec<f64>> = HashMap::new();
        for (id, n) in &self.nodes {
            let e = &eff[id];
            coupled.insert(
                id.clone(),
                n.compute_base_eff(&e.weights, &e.base_a, Some(&e.base_b), e.progress),
            );
        }

        let max_iter = self.nodes.len().saturating_sub(1).max(1);
        for _ in 0..max_iter {
            let prev = coupled.clone();
            let mut converged = true;

            for (id, coupled_vals) in coupled.iter_mut() {
                let node = &self.nodes[id];
                let e = &eff[id];

                let mut incoming = vec![0.0; node.config.objectives];
                for edge in &self.edges {
                    if edge.to == *id && edge.to_channel < incoming.len() {
                        let effective_strength = self.edge_strength(edge);
                        if let Some(source) = prev.get(&edge.from) {
                            if edge.from_channel < source.len() {
                                incoming[edge.to_channel] +=
                                    effective_strength * source[edge.from_channel];
                            }
                        }
                    }
                }

                let updated = if node.config.intake == "through" {
                    node.map_channel_inputs_eff(
                        &incoming,
                        &e.weights,
                        &e.base_a,
                        Some(&e.base_b),
                        e.progress,
                    )
                } else {
                    // Legacy: coupling added to the output channel after
                    // this node's own map.
                    let mut u = node.compute_base_eff(
                        &e.weights,
                        &e.base_a,
                        Some(&e.base_b),
                        e.progress,
                    );
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

        coupled
    }

    fn recompute_objectives(&mut self, requesting_node: &str) -> Vec<f64> {
        let mut result = self.cascade()[requesting_node].clone();

        // Readout bias drift: part of the true signal, not of noise.
        let offset = self
            .nodes
            .get(requesting_node)
            .map(|n| {
                let raw = n.config.offset_drift_rate * self.total_ticks as f64;
                match n.config.offset_drift_cap {
                    Some(cap) => raw.clamp(-cap.abs(), cap.abs()),
                    None => raw,
                }
            })
            .unwrap_or(0.0);
        for val in result.iter_mut() {
            *val += offset + self.generate_noise();
        }

        // Update stored objectives
        if let Some(n) = self.nodes.get_mut(requesting_node) {
            n.objectives = result.clone();
        }

        result
    }

    /// The answer key: noise-free current objectives (readout bias
    /// included — it is world, not measurement), the node's effective
    /// shape, and every edge's effective strength at the current tick.
    /// Provably read-only: no RNG is touched, so truth never disturbs
    /// the live noise stream.
    pub fn truth(&self, node_id: &str) -> Option<TruthReport> {
        let coupled = self.cascade();
        let node = self.nodes.get(node_id)?;
        let eff = self.effective_shape(&node.config);
        let mut objectives = coupled.get(node_id)?.clone();
        let raw = node.config.offset_drift_rate * self.total_ticks as f64;
        let offset = match node.config.offset_drift_cap {
            Some(cap) => raw.clamp(-cap.abs(), cap.abs()),
            None => raw,
        };
        for v in objectives.iter_mut() {
            *v += offset;
        }
        let edges = self
            .edges
            .iter()
            .map(|e| EdgeTruth {
                from: e.from.clone(),
                to: e.to.clone(),
                to_channel: e.to_channel,
                strength_effective: self.edge_strength(e),
            })
            .collect();
        Some(TruthReport {
            node_id: node_id.to_string(),
            total_ticks: self.total_ticks,
            objectives,
            effective_weights: eff.weights,
            base_from: eff.base_a,
            base_to: eff.base_b,
            base_progress: eff.progress,
            edges,
        })
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

    // ─── drift: signed edge drift, offset, morph schedule, truth ──────

    fn pump(sim: &mut Simulator, params: &[f64], until_tick: u64) {
        while sim.total_ticks < until_tick {
            sim.apply("a", params);
        }
    }

    fn two_node_drift_sim(drift: f64) -> Simulator {
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]]},
                {"id": "b", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[0.0]]}
            ],
            "edges": [
                {"from": "a", "from_channel": 0, "to": "b", "to_channel": 0,
                 "strength": 0.7, "drift_rate": drift}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        })).unwrap();
        Simulator::from_config(config)
    }

    #[test]
    fn signed_edge_drift_decays_through_zero_and_inverts() {
        // strength 0.7, drift −0.002/tick: 0.7×(1−0.002T) — the coupling
        // shrinks, hits zero at T=500, and points backwards after.
        let mut sim = two_node_drift_sim(-0.002);
        sim.apply("a", &[100.0]); // a.obj = 1.0, T = 1
        let b = sim.get_status("b").unwrap()[0];
        assert!((b - 0.7 * (1.0 - 0.002)).abs() < 1e-9, "T=1: got {}", b);

        pump(&mut sim, &[100.0], 500);
        let b = sim.get_status("b").unwrap()[0];
        assert!(b.abs() < 1e-9, "zero coupling at T=500, got {}", b);

        pump(&mut sim, &[100.0], 750);
        let b = sim.get_status("b").unwrap()[0];
        let expected = 0.7 * (1.0 - 0.002 * 750.0);
        assert!((b - expected).abs() < 1e-9 && expected < 0.0,
            "inverted coupling at T=750, got {} (want {})", b, expected);

        // the answer key reports the inverted effective strength
        let t = sim.truth("b").unwrap();
        assert!((t.edges[0].strength_effective - expected).abs() < 1e-9);
    }

    #[test]
    fn offset_drift_slides_the_readout_linearly() {
        // the canonical re-walk drift: the curve slides, slope intact
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]],
                 "offset_drift_rate": 0.01}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        })).unwrap();
        let mut sim = Simulator::from_config(config);

        sim.apply("a", &[50.0]); // T=1: 0.5 + 0.01×1
        assert!((sim.get_status("a").unwrap()[0] - 0.51).abs() < 1e-9);
        sim.apply("a", &[50.0]); // T=2: 0.5 + 0.01×2
        let read = sim.get_status("a").unwrap()[0];
        assert!((read - 0.52).abs() < 1e-9, "T=2: got {}", read);

        // truth carries the same shifted signal — the world moved,
        // the measurement is honest about it
        let t = sim.truth("a").unwrap();
        assert!((t.objectives[0] - 0.52).abs() < 1e-9);
    }

    #[test]
    fn offset_drift_cap_stops_the_slide() {
        // drift with a destination: the world moves to a defined point
        // and settles - the capped slide equals the uncapped one until
        // the cap, then holds flat
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]],
                 "offset_drift_rate": 0.01, "offset_drift_cap": 0.015}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        })).unwrap();
        let mut sim = Simulator::from_config(config);

        sim.apply("a", &[50.0]); // T=1: 0.5 + 0.01x1
        sim.apply("a", &[50.0]); // T=2: 0.5 + 0.01x2 (at the cap)
        let at_cap = sim.get_status("a").unwrap()[0];
        assert!((at_cap - 0.515).abs() < 1e-9, "T=2: got {}", at_cap);

        sim.apply("a", &[50.0]); // T=3: past the cap - the world has settled
        let settled = sim.get_status("a").unwrap()[0];
        assert!((settled - 0.515).abs() < 1e-9, "T=3: got {}", settled);

        let t = sim.truth("a").unwrap();
        assert!((t.objectives[0] - 0.515).abs() < 1e-9);
    }

    #[test]
    fn weights_morph_travels_then_holds() {
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 2, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0, 0.0]],
                 "morphs": [{"at_tick": 100, "travel_ticks": 100,
                             "target_weights": [[0.0, 1.0]]}]}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        })).unwrap();
        let mut sim = Simulator::from_config(config);
        let params = [20.0, 80.0]; // normalized 0.2 / 0.8

        pump(&mut sim, &params, 50); // before the break: pure start shape
        assert!((sim.get_status("a").unwrap()[0] - 0.2).abs() < 1e-9);

        pump(&mut sim, &params, 150); // mid-travel: half way
        assert!((sim.get_status("a").unwrap()[0] - 0.5).abs() < 1e-9);

        pump(&mut sim, &params, 250); // settled: influence moved to param_1
        let read = sim.get_status("a").unwrap()[0];
        assert!((read - 0.8).abs() < 1e-9, "settled: got {}", read);

        // the dial that mattered stopped mattering; the quiet one took over
        let t = sim.truth("a").unwrap();
        assert!((t.effective_weights[0][0]).abs() < 1e-9);
        assert!((t.effective_weights[0][1] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn base_morph_blends_linear_into_saturation() {
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]],
                 "morphs": [{"at_tick": 100, "travel_ticks": 100,
                             "target_base": "saturation"}]}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        })).unwrap();
        let mut sim = Simulator::from_config(config);
        let params = [60.0]; // normalized 0.6: linear 0.6, saturation 0.6/1.4

        pump(&mut sim, &params, 50);
        assert!((sim.get_status("a").unwrap()[0] - 0.6).abs() < 1e-9);

        pump(&mut sim, &params, 150);
        let mid = 0.5 * 0.6 + 0.5 * (0.6 / 1.4);
        assert!((sim.get_status("a").unwrap()[0] - mid).abs() < 1e-9);

        pump(&mut sim, &params, 250);
        let read = sim.get_status("a").unwrap()[0];
        assert!((read - 0.6 / 1.4).abs() < 1e-9, "settled sat: got {}", read);
    }

    #[test]
    fn morph_schedule_settles_through_both_targets_in_order() {
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 2, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0, 0.0]],
                 "morphs": [
                    {"at_tick": 100, "target_weights": [[0.0, 1.0]]},
                    {"at_tick": 300, "target_weights": [[1.0, 0.0]]}
                 ]}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        })).unwrap();
        let mut sim = Simulator::from_config(config);
        let params = [20.0, 80.0];

        pump(&mut sim, &params, 50);
        assert!((sim.get_status("a").unwrap()[0] - 0.2).abs() < 1e-9, "T=50");
        pump(&mut sim, &params, 150);
        assert!((sim.get_status("a").unwrap()[0] - 0.8).abs() < 1e-9, "T=150: first target settled");
        pump(&mut sim, &params, 250);
        assert!((sim.get_status("a").unwrap()[0] - 0.8).abs() < 1e-9, "T=250: holds");
        pump(&mut sim, &params, 350);
        let read = sim.get_status("a").unwrap()[0];
        assert!((read - 0.2).abs() < 1e-9, "T=350: second target settled, got {}", read);
    }

    #[test]
    fn truth_is_noise_free_stable_and_counts_ticks() {
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]]},
                {"id": "b", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[0.0]]}
            ],
            "edges": [
                {"from": "a", "from_channel": 0, "to": "b", "to_channel": 0, "strength": 0.7}
            ],
            "noise": {"gaussian_sigma": 0.1, "colored_sigma": 0.0, "drift_rate": 0.0}
        })).unwrap();
        let mut sim = Simulator::from_config(config);
        for _ in 0..3 {
            sim.apply("a", &[100.0]);
        }

        let t1 = sim.truth("b").unwrap();
        let t2 = sim.truth("b").unwrap();
        assert_eq!(t1.total_ticks, 3, "truth reports the global tick count");
        assert!((t1.objectives[0] - 0.7).abs() < 1e-9,
            "truth is noise-free cascade: got {}", t1.objectives[0]);
        assert_eq!(t1.objectives, t2.objectives, "truth never jitters");

        // the live read carries fresh noise around the same truth
        let live = sim.get_status("b").unwrap()[0];
        assert!((live - 0.7).abs() < 0.5, "live read near truth, got {}", live);
    }

    #[test]
    #[should_panic(expected = "target_weights must be")]
    fn morph_with_wrong_dimensions_fails_loud_at_boot() {
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]],
                 "morphs": [{"at_tick": 10, "target_weights": [[1.0, 0.0]]}]}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        })).unwrap();
        let _ = Simulator::from_config(config);
    }
}
