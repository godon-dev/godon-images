use log::info;
use rand::rngs::StdRng;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

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
    /// Offset drift start tick: the slide begins only at this tick -
    /// `offset(t) = rate × max(0, t − start)`. The world holds still
    /// while the map builds and the wish lands, then moves on
    /// schedule. Default 0 = the classic from-boot ramp.
    #[serde(default)]
    pub offset_drift_start_tick: Option<u64>,
    /// The declared drift list (v2): readout slides that cue when a
    /// bench-observable condition first holds (uptime seconds, global
    /// ticks, this node's dial positions), then ramp in WALL TIME.
    /// Multiple drifts compose additively — parallel by declaration.
    /// The door (`POST /{id}/drift/start`) cues the same machinery by
    /// hand; engine state is deliberately not a condition here (the
    /// engine is the subject — story cues come from outside).
    #[serde(default)]
    pub drifts: Vec<DriftConfig>,
    /// Exogenous pushes: declared outside signals entering this node's
    /// world — load spikes, weather-like forcing nobody controls and no
    /// agent owns. Bounded by declaration: a step ramps to its level and
    /// HOLDS (a destination); a sine oscillates within ±amplitude (a
    /// bounded orbit — its destination is a region, not a point).
    /// Additive per objective, like interaction terms. Pure in the
    /// clocks; truth reports the current value.
    #[serde(default)]
    pub exogenous: Vec<ExoConfig>,
}

/// One declared outside push.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExoConfig {
    pub name: Option<String>,
    /// "step" (ramp to amplitude, then hold) or "sine" (oscillate
    /// ±amplitude with the travel window as period — boot fails loud
    /// on unknown kinds, unknown clocks, or a windowless sine, which
    /// would read as zero).
    pub kind: String,
    pub amplitude: f64,
    /// Objective index the push feeds.
    #[serde(default)]
    pub objective: usize,
    #[serde(default)]
    pub clock: Option<String>,
    #[serde(default)]
    pub at_tick: u64,
    #[serde(default)]
    pub at_secs: Option<f64>,
    /// step: the ramp window; sine: the period. On the declared clock.
    #[serde(default)]
    pub travel_ticks: u64,
    #[serde(default)]
    pub travel_secs: Option<f64>,
}

/// The push's value at one instant — pure in the clocks.
fn exogenous_at(cfg: &ExoConfig, uptime_secs: f64, total_ticks: u64) -> f64 {
    let wall = cfg.clock.as_deref() == Some("wall");
    let (elapsed, window) = if wall {
        let at = cfg.at_secs.unwrap_or(0.0);
        ((uptime_secs - at).max(0.0), cfg.travel_secs.unwrap_or(0.0))
    } else {
        (
            total_ticks.saturating_sub(cfg.at_tick) as f64,
            cfg.travel_ticks as f64,
        )
    };
    if elapsed <= 0.0 {
        return 0.0;
    }
    match cfg.kind.as_str() {
        "sine" => {
            let period = if window > 0.0 { window } else { 1.0 };
            cfg.amplitude * (2.0 * std::f64::consts::PI * elapsed / period).sin()
        }
        _ => {
            if window <= 0.0 {
                cfg.amplitude
            } else {
                cfg.amplitude * (elapsed / window).min(1.0)
            }
        }
    }
}

/// Per-objective sum of a node's outside pushes at one instant.
fn exogenous_sum(nc: &NodeConfig, uptime_secs: f64, total_ticks: u64) -> Vec<f64> {
    let mut out = vec![0.0; nc.objectives];
    for exo in &nc.exogenous {
        if let Some(slot) = out.get_mut(exo.objective) {
            *slot += exogenous_at(exo, uptime_secs, total_ticks);
        }
    }
    out
}

/// One declared drift. When `when` first holds, the drift is cued:
/// from the cue on, the node's readout slides by
/// `rate_per_second × seconds since cue`, capped at ±cap.
/// WALL TIME, not ticks — the world's speed must not be an artifact
/// of trial cadence (receipt: godon#423 — the same rate input took
/// ~6 min of wall time in one room and ~10 in the next).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftConfig {
    #[serde(default)]
    pub name: Option<String>,
    pub rate_per_second: f64,
    #[serde(default)]
    pub cap: Option<f64>,
    #[serde(default)]
    pub when: DriftWhen,
}

/// The firing condition — everything the bench itself observes.
/// Fields AND together; an empty condition arms at first evaluation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DriftWhen {
    /// Bench uptime in seconds.
    #[serde(default)]
    pub seconds: Option<f64>,
    /// The global total tick counter.
    #[serde(default)]
    pub ticks: Option<u64>,
    /// This node's dial position (last applied value).
    #[serde(default)]
    pub param: Option<DriftParamWhen>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftParamWhen {
    pub param: usize,
    /// ">=" or "<="
    pub op: String,
    pub value: f64,
}

/// One scheduled shape target. `travel_ticks` 0 = instant step at
/// `at_tick`; otherwise the map travels linearly from the previous
/// settled shape to the target over that many ticks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MorphConfig {
    /// Cue tick (tick clock) — for clock = "wall" it doubles as the ORDER
    /// slot among the node's morphs; the cue itself is `at_secs`.
    #[serde(default)]
    pub at_tick: u64,
    /// Wall-clock cue in seconds of bench uptime. Used when clock = "wall".
    #[serde(default)]
    pub at_secs: Option<f64>,
    #[serde(default)]
    pub travel_ticks: u64,
    #[serde(default)]
    pub target_weights: Option<Vec<Vec<f64>>>,
    #[serde(default)]
    pub target_base: Option<String>,
    /// Trajectory clock for this event: "tick" (default — travel advances
    /// with global ticks, activity-paced) or "wall" (travel advances with
    /// bench uptime, agent-independent). Only the TRAVEL changes clock:
    /// the morph still enters the chain once ticks reach `at_tick` (keep
    /// it 0 for wall events), and the cue instant is `at_secs`. Receipts
    /// convert between clocks via the room's measured cadence (declared
    /// term).
    #[serde(default)]
    pub clock: Option<String>,
    /// Wall-clock travel window in seconds, when clock = "wall". Defaults
    /// to 0 — an instant step at the cue, the same shape travel_ticks = 0
    /// gives on the tick clock.
    #[serde(default)]
    pub travel_secs: Option<f64>,
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
    /// LEGACY unbounded linear edge ramp: strength × (1 + drift_rate ×
    /// total_ticks). Runs away by design; kept only for old configs.
    #[serde(default)]
    pub drift_rate: f64,
    /// Destination semantics: the coupling travels from `strength` to
    /// `target_strength` over the travel window, then HOLDS — the world
    /// settles. A negative target passes through zero and settles
    /// inverted. Mutually exclusive with `drift_rate` (boot fails loud
    /// if both are set).
    #[serde(default)]
    pub target_strength: Option<f64>,
    /// Cue tick for the destination travel — tick clock only. When
    /// clock = "wall" this field is ignored: the cue is `at_secs`
    /// (default 0.0, travel starts at boot).
    #[serde(default)]
    pub at_tick: Option<u64>,
    /// Wall-clock cue (seconds of uptime) when clock = "wall".
    #[serde(default)]
    pub at_secs: Option<f64>,
    #[serde(default)]
    pub travel_ticks: Option<u64>,
    #[serde(default)]
    pub clock: Option<String>,
    #[serde(default)]
    pub travel_secs: Option<f64>,
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
    /// Destination semantics for the Gaussian width: sigma travels from
    /// `gaussian_sigma` to `target_gaussian_sigma` over the window on
    /// the declared clock, then HOLDS — the world's readings get
    /// fuzzier (or tighter) and settle. The legacy `drift_rate` growth
    /// above is the unbounded form; this is the settling form.
    #[serde(default)]
    pub target_gaussian_sigma: Option<f64>,
    #[serde(default)]
    pub sigma_at_tick: Option<u64>,
    #[serde(default)]
    pub sigma_at_secs: Option<f64>,
    #[serde(default)]
    pub sigma_travel_ticks: Option<u64>,
    #[serde(default)]
    pub sigma_travel_secs: Option<f64>,
    #[serde(default)]
    pub sigma_clock: Option<String>,
}

/// The effective Gaussian width at one instant — pure in the clocks.
fn effective_gaussian_sigma(n: &NoiseConfig, uptime_secs: f64, total_ticks: u64) -> f64 {
    if let Some(target) = n.target_gaussian_sigma {
        let p = travel_progress(
            &n.sigma_clock,
            n.sigma_at_tick.unwrap_or(0),
            n.sigma_at_secs,
            n.sigma_travel_ticks.unwrap_or(0),
            n.sigma_travel_secs,
            uptime_secs,
            total_ticks,
        );
        n.gaussian_sigma + (target - n.gaussian_sigma) * p
    } else {
        n.gaussian_sigma
    }
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
            target_gaussian_sigma: None,
            sigma_at_tick: None,
            sigma_at_secs: None,
            sigma_travel_ticks: None,
            sigma_travel_secs: None,
            sigma_clock: None,
        }
    }
}

// ─── Node State ─────────────────────────────────────────────────────

/// A cued drift, running on wall time. `cued_uptime_secs` and
/// `cued_tick` are the replay stamps — both clocks at the cue.
#[derive(Debug, Clone, Serialize)]
pub struct LiveDrift {
    pub name: Option<String>,
    /// "declared" (from the config list) or "door" (POSTed by hand).
    pub origin: String,
    pub rate_per_second: f64,
    pub cap: Option<f64>,
    pub cued_uptime_secs: f64,
    pub cued_tick: u64,
}

#[derive(Debug, Clone)]
pub struct NodeState {
    #[allow(dead_code)]
    // kept: the node's name — constructed by the config loader, reads ride the config paths
    pub id: String,
    pub config: NodeConfig,
    pub params: Vec<f64>,
    pub objectives: Vec<f64>,
    pub tick: u64,
    /// Declared drifts that have already fired (cue-once semantics).
    pub drifts_cued: Vec<bool>,
    /// Drifts currently running (declared + door), composing additively.
    pub live_drifts: Vec<LiveDrift>,
}

impl NodeState {
    pub fn new(config: NodeConfig) -> Self {
        let params = vec![config.param_lower; config.params];
        let objectives = vec![0.0; config.objectives];
        let drifts_cued = vec![false; config.drifts.len()];
        Self {
            id: config.id.clone(),
            config,
            params,
            objectives,
            tick: 0,
            drifts_cued,
            live_drifts: Vec::new(),
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

        let normalized: Vec<f64> = self
            .params
            .iter()
            .map(|p| normalize(*p, self.config.param_lower, self.config.param_upper))
            .collect();

        for (obj_idx, slot) in result.iter_mut().enumerate() {
            if let Some(row) = weights.get(obj_idx) {
                for (param_idx, w) in row.iter().enumerate() {
                    if let Some(np) = normalized.get(param_idx) {
                        *slot += w * blend_base(base_a, base_b, p, *np);
                    }
                }
            }

            // Cross-parameter interaction contributions (always multiplicative)
            for inter in &self.config.interactions {
                if inter.objective == obj_idx {
                    let product: f64 = inter
                        .params
                        .iter()
                        .filter_map(|&pi| normalized.get(pi))
                        .product();
                    *slot += inter.weight * product;
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
        let normalized: Vec<f64> = self
            .params
            .iter()
            .map(|p| normalize(*p, self.config.param_lower, self.config.param_upper))
            .collect();
        for (obj_idx, slot) in result.iter_mut().enumerate() {
            if let Some(row) = weights.get(obj_idx) {
                for (param_idx, w) in row.iter().enumerate() {
                    if let Some(np) = normalized.get(param_idx) {
                        *slot += w * np;
                    }
                }
            }
        }
        result
    }

    /// Cross-parameter interaction terms, one per objective.
    fn interaction_terms(&self) -> Vec<f64> {
        let mut result = vec![0.0; self.config.objectives];
        let normalized: Vec<f64> = self
            .params
            .iter()
            .map(|p| normalize(*p, self.config.param_lower, self.config.param_upper))
            .collect();
        for inter in &self.config.interactions {
            if let Some(slot) = result.get_mut(inter.objective) {
                let product: f64 = inter
                    .params
                    .iter()
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
        "threshold" => {
            if np > 0.5 {
                1.0
            } else {
                0.0
            }
        }
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
    /// Bench boot time — the wall clock every live drift ramps on.
    pub boot: Instant,
}

pub type SharedSimulator = Arc<Mutex<Simulator>>;

/// Total offset contributed by running live drifts at a given uptime.
/// Pure function of the cue stamps — replayable from the record.
pub fn live_offset_at(live: &[LiveDrift], now_secs: f64) -> f64 {
    live.iter()
        .map(|d| {
            let raw = d.rate_per_second * (now_secs - d.cued_uptime_secs).max(0.0);
            match d.cap {
                Some(c) => raw.clamp(-c.abs(), c.abs()),
                None => raw,
            }
        })
        .sum()
}

/// Does a declared drift's condition hold right now? Everything the
/// bench itself observes; fields AND together; empty = always.
pub fn when_holds(w: &DriftWhen, uptime_secs: f64, total_ticks: u64, params: &[f64]) -> bool {
    if let Some(s) = w.seconds {
        if uptime_secs < s {
            return false;
        }
    }
    if let Some(t) = w.ticks {
        if total_ticks < t {
            return false;
        }
    }
    if let Some(p) = &w.param {
        let holds = match params.get(p.param) {
            Some(v) => match p.op.as_str() {
                ">=" => *v >= p.value,
                "<=" => *v <= p.value,
                _ => false,
            },
            None => false,
        };
        if !holds {
            return false;
        }
    }
    true
}

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
    /// Bench uptime at the report — the wall clock, for converting
    /// between tick-clocked and wall-clocked events (a declared term).
    pub uptime_secs: f64,
    pub objectives: Vec<f64>,
    pub effective_weights: Vec<Vec<f64>>,
    pub base_from: String,
    pub base_to: String,
    pub base_progress: f64,
    /// Total readout offset in force at the report (scheduled + live).
    pub offset_effective: f64,
    /// The Gaussian width in force at the report.
    pub noise_effective: f64,
    /// Per-objective sum of outside pushes in force at the report.
    pub exogenous_effective: Vec<f64>,
    pub edges: Vec<EdgeTruth>,
}

/// The event grammar's one clock rule. Cue and travel each run on the
/// declared clock — "tick" (global ticks, activity-paced: the world
/// advances with use) or "wall" (bench uptime, agent-independent: the
/// world advances on its own). Returns travel progress in [0, 1]:
/// 0 before the cue, 1 once the window has fully elapsed. Pure in
/// (uptime, ticks) — the world stays a function of its clocks.
fn travel_progress(
    clock: &Option<String>,
    at_tick: u64,
    at_secs: Option<f64>,
    travel_ticks: u64,
    travel_secs: Option<f64>,
    uptime_secs: f64,
    total_ticks: u64,
) -> f64 {
    if clock.as_deref() == Some("wall") {
        let at = at_secs.unwrap_or(0.0);
        let travel = travel_secs.unwrap_or(0.0);
        if travel <= 0.0 {
            return if uptime_secs >= at { 1.0 } else { 0.0 };
        }
        ((uptime_secs - at) / travel).clamp(0.0, 1.0)
    } else {
        if travel_ticks == 0 {
            return if total_ticks >= at_tick { 1.0 } else { 0.0 };
        }
        ((total_ticks.saturating_sub(at_tick)) as f64 / travel_ticks as f64).clamp(0.0, 1.0)
    }
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
                        w.len() == nc.objectives && w.iter().all(|row| row.len() == nc.params),
                        "node '{}': morph target_weights must be {}x{} (objectives x params)",
                        nc.id,
                        nc.objectives,
                        nc.params
                    );
                }
            }
        }

        // Fail loud at boot: an edge declaring BOTH a destination and the
        // legacy ramp would run two competing edge changes — declare one.
        for e in &config.edges {
            assert!(
                !(e.target_strength.is_some() && e.drift_rate != 0.0),
                "edge {}->{} declares target_strength AND drift_rate — one edge change form per edge",
                e.from, e.to
            );
        }

        // Fail loud at boot: a push declares its kind and clock, and a
        // sine carries a travel window (its period) — a windowless sine
        // would sample its own whole periods and read as exactly zero.
        for nc in &config.nodes {
            for exo in &nc.exogenous {
                assert!(
                    matches!(exo.kind.as_str(), "step" | "sine"),
                    "node {} exogenous kind '{}' unknown — step or sine",
                    nc.id,
                    exo.kind
                );
                assert!(
                    matches!(exo.clock.as_deref(), None | Some("tick") | Some("wall")),
                    "node {} exogenous clock '{:?}' unknown — tick or wall",
                    nc.id,
                    exo.clock
                );
                if exo.kind == "sine" {
                    let windowed = if exo.clock.as_deref() == Some("wall") {
                        exo.travel_secs.unwrap_or(0.0) > 0.0
                    } else {
                        exo.travel_ticks > 0
                    };
                    assert!(
                        windowed,
                        "node {} sine push needs a travel window (its period) — travel_secs on wall, travel_ticks on tick",
                        nc.id
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
            boot: Instant::now(),
        }
    }

    pub fn uptime_secs(&self) -> f64 {
        self.boot.elapsed().as_secs_f64()
    }

    /// Cue every declared drift whose condition first holds. Runs on
    /// the mutating paths (apply / live read) — `truth` stays
    /// read-only and never cues: an unfired drift has not moved the
    /// world, and the answer key must agree.
    fn cue_due_drifts(&mut self) {
        let now = self.uptime_secs();
        let tick = self.total_ticks;
        for node in self.nodes.values_mut() {
            let params = node.params.clone();
            let mut cued: Vec<LiveDrift> = Vec::new();
            for (i, d) in node.config.drifts.iter().enumerate() {
                if node.drifts_cued.get(i).copied().unwrap_or(true) {
                    continue;
                }
                if when_holds(&d.when, now, tick, &params) {
                    node.drifts_cued[i] = true;
                    cued.push(LiveDrift {
                        name: d.name.clone(),
                        origin: "declared".to_string(),
                        rate_per_second: d.rate_per_second,
                        cap: d.cap,
                        cued_uptime_secs: now,
                        cued_tick: tick,
                    });
                }
            }
            for d in &cued {
                info!(
                    "drift cued: node {} drift {} at tick {} uptime {:.3}s rate {:.6}/s cap {:?}",
                    node.id,
                    d.name.clone().unwrap_or_else(|| "-".to_string()),
                    d.cued_tick,
                    d.cued_uptime_secs,
                    d.rate_per_second,
                    d.cap
                );
            }
            node.live_drifts.extend(cued);
        }
    }

    /// The manual door (v1): cue a live drift on this node NOW.
    /// One door-origin drift per node — the second knock is the
    /// caller's 409, not a second drift.
    pub fn drift_start(
        &mut self,
        node_id: &str,
        rate_per_second: f64,
        cap: Option<f64>,
        name: Option<String>,
    ) -> Option<Result<LiveDrift, ()>> {
        let now = self.uptime_secs();
        let tick = self.total_ticks;
        let node = self.nodes.get_mut(node_id)?;
        if node.live_drifts.iter().any(|d| d.origin == "door") {
            return Some(Err(()));
        }
        let live = LiveDrift {
            name: name.or_else(|| Some("door".to_string())),
            origin: "door".to_string(),
            rate_per_second,
            cap,
            cued_uptime_secs: now,
            cued_tick: tick,
        };
        info!(
            "drift cued by door: node {} at tick {} uptime {:.3}s rate {:.6}/s cap {:?}",
            node.id, live.cued_tick, live.cued_uptime_secs, live.rate_per_second, live.cap
        );
        node.live_drifts.push(live.clone());
        Some(Ok(live))
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
        self.shape_at(nc, self.uptime_secs(), self.total_ticks)
    }

    /// The node's effective shape as a pure function of the two clocks —
    /// the replay property in miniature: same clocks in, same world out.
    fn shape_at(&self, nc: &NodeConfig, uptime_secs: f64, total_ticks: u64) -> EffShape {
        let mut sorted = nc.morphs.clone();
        sorted.sort_by_key(|m| m.at_tick);

        let mut weights = nc.weights.clone();
        let mut base = nc.base.clone();
        let mut inflight: Option<EffShape> = None;

        for m in &sorted {
            if m.at_tick > total_ticks {
                break;
            }
            let target_w = m.target_weights.clone().unwrap_or_else(|| weights.clone());
            let target_b = m.target_base.clone().unwrap_or_else(|| base.clone());
            let progress = travel_progress(
                &m.clock,
                m.at_tick,
                m.at_secs,
                m.travel_ticks,
                m.travel_secs,
                uptime_secs,
                total_ticks,
            );
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
    /// invert. Destination form: travels from the declared strength to
    /// `target_strength` over the window on the declared clock, then
    /// HOLDS — the world settles (a negative target settles inverted).
    /// Legacy form (drift_rate only): unbounded linear ramp, kept for
    /// old configs. Pure in the clocks.
    fn edge_strength(&self, e: &EdgeConfig) -> f64 {
        self.edge_strength_at(e, self.uptime_secs(), self.total_ticks)
    }

    fn edge_strength_at(&self, e: &EdgeConfig, uptime_secs: f64, total_ticks: u64) -> f64 {
        if let Some(target) = e.target_strength {
            let p = travel_progress(
                &e.clock,
                e.at_tick.unwrap_or(0),
                e.at_secs,
                e.travel_ticks.unwrap_or(0),
                e.travel_secs,
                uptime_secs,
                total_ticks,
            );
            e.strength + (target - e.strength) * p
        } else {
            e.strength * (1.0 + e.drift_rate * total_ticks as f64)
        }
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

                let mut updated = if node.config.intake == "through" {
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
                    let mut u =
                        node.compute_base_eff(&e.weights, &e.base_a, Some(&e.base_b), e.progress);
                    for (ch, inc) in incoming.iter().enumerate() {
                        if ch < u.len() {
                            u[ch] += inc;
                        }
                    }
                    u
                };

                // Outside pushes enter here — inside the update, so they
                // survive propagation and couple downstream like any real
                // forcing on the node would.
                let exo = exogenous_sum(&node.config, self.uptime_secs(), self.total_ticks);
                for (slot, push) in updated.iter_mut().zip(exo.iter()) {
                    *slot += push;
                }

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
        // Declared drifts cue on the mutating paths (cue-before-read:
        // the world moves, then we look at it).
        self.cue_due_drifts();

        let mut result = self.cascade()[requesting_node].clone();
        let now = self.uptime_secs();

        // Readout bias drift: part of the true signal, not of noise.
        // Legacy scheduled offset (tick function) + live drifts
        // (wall-time ramps) compose additively — both are world.
        let offset = self
            .nodes
            .get(requesting_node)
            .map(|n| {
                let elapsed = self
                    .total_ticks
                    .saturating_sub(n.config.offset_drift_start_tick.unwrap_or(0));
                let raw = n.config.offset_drift_rate * elapsed as f64;
                let scheduled = match n.config.offset_drift_cap {
                    Some(cap) => raw.clamp(-cap.abs(), cap.abs()),
                    None => raw,
                };
                scheduled + live_offset_at(&n.live_drifts, now)
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
        let elapsed = self
            .total_ticks
            .saturating_sub(node.config.offset_drift_start_tick.unwrap_or(0));
        let raw = node.config.offset_drift_rate * elapsed as f64;
        let scheduled = match node.config.offset_drift_cap {
            Some(cap) => raw.clamp(-cap.abs(), cap.abs()),
            None => raw,
        };
        // Live drifts count (they have moved the world); cueing does
        // NOT happen here — truth is the read-only answer key, and an
        // unfired declared drift has not moved anything yet.
        let offset = scheduled + live_offset_at(&node.live_drifts, self.uptime_secs());
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
            uptime_secs: self.uptime_secs(),
            objectives,
            effective_weights: eff.weights,
            base_from: eff.base_a,
            base_to: eff.base_b,
            base_progress: eff.progress,
            offset_effective: offset,
            noise_effective: effective_gaussian_sigma(
                &self.noise,
                self.uptime_secs(),
                self.total_ticks,
            ),
            exogenous_effective: exogenous_sum(&node.config, self.uptime_secs(), self.total_ticks),
            edges,
        })
    }

    fn generate_noise(&mut self) -> f64 {
        let mut noise = 0.0;

        // Gaussian component — under the effective (possibly settling) width
        let sigma = effective_gaussian_sigma(&self.noise, self.uptime_secs(), self.total_ticks);
        if sigma > 0.0 {
            noise += self.gaussian_raw() * sigma;
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
                // legacy nonstationary drift rides its own field — nothing to map
                "nonstationary" => {}
                _ => noise.gaussian_sigma = sigma,
            }
        }
    }
    noise
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
        assert!(
            (b[0] - 0.7).abs() < 1e-9,
            "b should read coupled 0.7, got {}",
            b[0]
        );
    }

    #[test]
    fn reads_advance_coupling_when_sender_changes() {
        let mut sim = two_node_sim();
        sim.apply("a", &[100.0]);
        let first = sim.get_status("b").unwrap()[0];
        sim.apply("a", &[0.0]); // a.obj back to 0
        let second = sim.get_status("b").unwrap()[0];
        assert!((first - 0.7).abs() < 1e-9);
        assert!(
            (second - 0.0).abs() < 1e-9,
            "b should track sender, got {}",
            second
        );
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
        assert!(
            (b[0] - 1.0 / 3.0).abs() < 1e-9,
            "door should bend incoming to 1/3, got {}",
            b[0]
        );
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
        assert!(
            (b[0] - 1.0).abs() < 1e-9,
            "door fires on combined level 0.8, expected 1.0, got {}",
            b[0]
        );
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
        assert!(
            (c[0] - expected).abs() < 1e-9,
            "nested chain should give {}, got {}",
            expected,
            c[0]
        );
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
        }))
        .unwrap();
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
        assert!(
            (b - expected).abs() < 1e-9 && expected < 0.0,
            "inverted coupling at T=750, got {} (want {})",
            b,
            expected
        );

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
        }))
        .unwrap();
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
        }))
        .unwrap();
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
    fn offset_drift_start_tick_holds_then_slides() {
        // the definable drift clock: the world holds still while the map
        // builds and the wish lands, then slides on schedule - closed
        // form preserved, offset(t) = rate x max(0, t - start)
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]],
                 "offset_drift_rate": 0.01, "offset_drift_start_tick": 3}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        }))
        .unwrap();
        let mut sim = Simulator::from_config(config);

        sim.apply("a", &[50.0]); // T=1: before start - still base
        assert!((sim.get_status("a").unwrap()[0] - 0.5).abs() < 1e-9);
        sim.apply("a", &[50.0]); // T=2: still before start
        sim.apply("a", &[50.0]); // T=3: start tick - offset 0
        sim.apply("a", &[50.0]); // T=4: 0.5 + 0.01 x (4-3)
        assert!((sim.get_status("a").unwrap()[0] - 0.51).abs() < 1e-9);
        sim.apply("a", &[50.0]); // T=5: 0.5 + 0.01 x 2
        let read = sim.get_status("a").unwrap()[0];
        assert!((read - 0.52).abs() < 1e-9, "T=5: got {}", read);

        // truth carries the same schedule - world and answer key agree
        let t = sim.truth("a").unwrap();
        assert!((t.objectives[0] - 0.52).abs() < 1e-9);
    }

    #[test]
    fn offset_drift_start_tick_defaults_to_zero() {
        // no start declared = the classic from-boot ramp, unchanged
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]],
                 "offset_drift_rate": 0.01}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        }))
        .unwrap();
        let mut sim = Simulator::from_config(config);

        sim.apply("a", &[50.0]); // T=1: 0.5 + 0.01x1
        assert!((sim.get_status("a").unwrap()[0] - 0.51).abs() < 1e-9);
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
        }))
        .unwrap();
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
        }))
        .unwrap();
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
        }))
        .unwrap();
        let mut sim = Simulator::from_config(config);
        let params = [20.0, 80.0];

        pump(&mut sim, &params, 50);
        assert!((sim.get_status("a").unwrap()[0] - 0.2).abs() < 1e-9, "T=50");
        pump(&mut sim, &params, 150);
        assert!(
            (sim.get_status("a").unwrap()[0] - 0.8).abs() < 1e-9,
            "T=150: first target settled"
        );
        pump(&mut sim, &params, 250);
        assert!(
            (sim.get_status("a").unwrap()[0] - 0.8).abs() < 1e-9,
            "T=250: holds"
        );
        pump(&mut sim, &params, 350);
        let read = sim.get_status("a").unwrap()[0];
        assert!(
            (read - 0.2).abs() < 1e-9,
            "T=350: second target settled, got {}",
            read
        );
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
        }))
        .unwrap();
        let mut sim = Simulator::from_config(config);
        for _ in 0..3 {
            sim.apply("a", &[100.0]);
        }

        let t1 = sim.truth("b").unwrap();
        let t2 = sim.truth("b").unwrap();
        assert_eq!(t1.total_ticks, 3, "truth reports the global tick count");
        assert!(
            (t1.objectives[0] - 0.7).abs() < 1e-9,
            "truth is noise-free cascade: got {}",
            t1.objectives[0]
        );
        assert_eq!(t1.objectives, t2.objectives, "truth never jitters");

        // the live read carries fresh noise around the same truth
        let live = sim.get_status("b").unwrap()[0];
        assert!(
            (live - 0.7).abs() < 0.5,
            "live read near truth, got {}",
            live
        );
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
        }))
        .unwrap();
        let _ = Simulator::from_config(config);
    }

    // ─── drift door + declared drift list (wall-time ramps) ──────────

    fn live_drift(rate: f64, cap: Option<f64>, cued_at: f64) -> LiveDrift {
        LiveDrift {
            name: Some("t".to_string()),
            origin: "door".to_string(),
            rate_per_second: rate,
            cap,
            cued_uptime_secs: cued_at,
            cued_tick: 0,
        }
    }

    #[test]
    fn live_offset_ramps_caps_sums_and_waits() {
        // uncapped ramp: 0.1/s, 30s past the cue -> 3.0
        assert!((live_offset_at(&[live_drift(0.1, None, 10.0)], 40.0) - 3.0).abs() < 1e-12);
        // capped: same ramp stops at 0.15
        assert!((live_offset_at(&[live_drift(0.1, Some(0.15), 10.0)], 40.0) - 0.15).abs() < 1e-12);
        // parallel: 0.1/s from t=10 plus 0.02/s from t=30, read at t=40
        let pair = [live_drift(0.1, None, 10.0), live_drift(0.02, None, 30.0)];
        assert!((live_offset_at(&pair, 40.0) - 3.2).abs() < 1e-12);
        // before its cue: zero
        assert!(live_offset_at(&[live_drift(0.1, None, 10.0)], 5.0).abs() < 1e-12);
    }

    #[test]
    fn when_holds_ands_conditions_and_rejects_unknown() {
        let w = DriftWhen::default();
        assert!(when_holds(&w, 0.0, 0, &[]), "empty condition always holds");

        let mut w = DriftWhen {
            seconds: Some(60.0),
            ..Default::default()
        };
        assert!(!when_holds(&w, 59.9, 0, &[]));
        assert!(when_holds(&w, 60.0, 0, &[]));

        w.ticks = Some(5);
        assert!(!when_holds(&w, 61.0, 4, &[]));
        assert!(when_holds(&w, 61.0, 5, &[]));

        w.param = Some(DriftParamWhen {
            param: 1,
            op: ">=".to_string(),
            value: 80.0,
        });
        assert!(!when_holds(&w, 61.0, 5, &[50.0, 79.9]));
        assert!(when_holds(&w, 61.0, 5, &[50.0, 80.0]));

        // unknown operator never holds — declared, not guessed
        w.param = Some(DriftParamWhen {
            param: 0,
            op: ">".to_string(),
            value: 0.0,
        });
        assert!(!when_holds(&w, 100.0, 100, &[50.0]));
        // out-of-range param index never holds
        w.param = Some(DriftParamWhen {
            param: 9,
            op: ">=".to_string(),
            value: 0.0,
        });
        assert!(!when_holds(&w, 100.0, 100, &[50.0]));
    }

    #[test]
    fn declared_drifts_cue_once_and_run_parallel() {
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]],
                 "drifts": [
                    {"name": "early", "rate_per_second": 0.5, "when": {"ticks": 2}},
                    {"name": "late", "rate_per_second": 0.25,
                     "when": {"param": {"param": 0, "op": ">=", "value": 80.0}}}
                 ]}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        }))
        .unwrap();
        let mut sim = Simulator::from_config(config);

        sim.apply("a", &[50.0]); // T=1: neither condition holds
        assert!(sim.nodes["a"].live_drifts.is_empty());

        sim.apply("a", &[50.0]); // T=2: "early" cues
        assert_eq!(sim.nodes["a"].live_drifts.len(), 1);
        assert_eq!(sim.nodes["a"].live_drifts[0].name.as_deref(), Some("early"));
        assert_eq!(sim.nodes["a"].live_drifts[0].cued_tick, 2);
        assert_eq!(sim.nodes["a"].live_drifts[0].origin, "declared");

        sim.apply("a", &[80.0]); // dial hits 80: "late" cues — parallel
        assert_eq!(sim.nodes["a"].live_drifts.len(), 2);

        pump(&mut sim, &[80.0], 50); // cue-once: nothing re-fires
        assert_eq!(sim.nodes["a"].live_drifts.len(), 2);
    }

    #[test]
    fn door_is_idempotent_per_node_and_moves_the_world() {
        let mut sim = two_node_sim();
        assert!(sim.drift_start("a", 10.0, Some(0.5), None).unwrap().is_ok());
        assert!(
            sim.drift_start("a", 0.1, None, None).unwrap().is_err(),
            "second knock refuses"
        );
        assert!(
            sim.drift_start("nope", 0.1, None, None).is_none(),
            "unknown node"
        );

        // 10/s capped 0.5: after >=120ms the readout has visibly slid
        // (a's base at default params is 0)
        std::thread::sleep(Duration::from_millis(120));
        let read = sim.get_status("a").unwrap()[0];
        assert!(read > 0.0, "door drift moves the world, got {}", read);
        let t = sim.truth("a").unwrap();
        assert!(
            t.objectives[0] > 0.0 && t.objectives[0] <= 0.5,
            "truth carries the live drift within cap, got {}",
            t.objectives[0]
        );
    }

    #[test]
    fn unfired_declared_drift_leaves_truth_alone() {
        // truth is the read-only answer key: no cueing, no movement
        // before the cue fires on a mutating path.
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]],
                 "drifts": [{"rate_per_second": 100.0, "when": {"ticks": 1000}}]}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        }))
        .unwrap();
        let mut sim = Simulator::from_config(config);
        sim.apply("a", &[100.0]); // cue check runs; ticks=1000 unmet
        assert!(sim.nodes["a"].live_drifts.is_empty());
        let t = sim.truth("a").unwrap();
        assert!(
            (t.objectives[0] - 1.0).abs() < 1e-9,
            "unfired drift leaves truth alone, got {}",
            t.objectives[0]
        );
    }

    #[test]
    fn door_composes_with_legacy_scheduled_offset() {
        let config: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0, "weights": [[1.0]],
                 "offset_drift_rate": 0.01}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        }))
        .unwrap();
        let mut sim = Simulator::from_config(config);
        sim.apply("a", &[50.0]); // T=1: 0.5 + 0.01 legacy
        assert!((sim.get_status("a").unwrap()[0] - 0.51).abs() < 1e-9);

        sim.drift_start("a", 1.0, None, None).unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let read = sim.truth("a").unwrap().objectives[0];
        assert!(
            read > 0.55 && read < 0.75,
            "legacy 0.51 + live ramp compose, got {}",
            read
        );
    }

    #[test]
    fn reset_clears_live_drifts_and_rearms_the_door() {
        let mut sim = two_node_sim();
        sim.drift_start("a", 10.0, None, None).unwrap().unwrap();
        assert_eq!(sim.nodes["a"].live_drifts.len(), 1);
        let config = sim.nodes["a"].config.clone();
        sim.nodes.insert("a".to_string(), NodeState::new(config));
        assert!(sim.nodes["a"].live_drifts.is_empty());
        assert!(sim.drift_start("a", 1.0, None, None).unwrap().is_ok());
    }

    // ── the event grammar (2026-11-19): destinations, clocks, pushes ──

    fn edge_destination_config(
        target: f64,
        clock: Option<String>,
        travel_ticks: u64,
        travel_secs: Option<f64>,
    ) -> BenchConfig {
        serde_json::from_value(serde_json::json!({
            "nodes": [
                {"id": "a", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]]},
                {"id": "b", "params": 1, "objectives": 1, "base": "linear",
                 "param_lower": 0.0, "param_upper": 100.0,
                 "weights": [[1.0]]}
            ],
            "edges": [
                {"from": "a", "from_channel": 0, "to": "b", "to_channel": 0,
                 "strength": 0.3, "target_strength": target,
                 "at_tick": 10, "travel_ticks": travel_ticks,
                 "travel_secs": travel_secs, "clock": clock}
            ],
            "noise": {"gaussian_sigma": 0.0}
        }))
        .unwrap()
    }

    #[test]
    fn edge_destination_travels_then_holds() {
        let sim = Simulator::from_config(edge_destination_config(0.9, None, 20, None));
        // before cue: declared strength
        assert!((sim.edge_strength_at(&sim.edges[0], 0.0, 5) - 0.3).abs() < 1e-12);
        // mid-travel: halfway
        let mid = sim.edge_strength_at(&sim.edges[0], 0.0, 20);
        assert!((mid - 0.6).abs() < 1e-12, "mid-travel {mid}");
        // after window: settled at destination, stays settled
        assert!((sim.edge_strength_at(&sim.edges[0], 0.0, 100) - 0.9).abs() < 1e-12);
        assert!((sim.edge_strength_at(&sim.edges[0], 0.0, 900) - 0.9).abs() < 1e-12);
    }

    #[test]
    fn edge_destination_inversion_settles_inverted() {
        let sim = Simulator::from_config(edge_destination_config(-0.3, None, 10, None));
        // passes through zero mid-travel, settles inverted
        let mid = sim.edge_strength_at(&sim.edges[0], 0.0, 15);
        assert!(
            (mid - 0.0).abs() < 1e-12,
            "through zero at mid-travel, got {mid}"
        );
        let settled = sim.edge_strength_at(&sim.edges[0], 0.0, 500);
        assert!(
            (settled - (-0.3)).abs() < 1e-12,
            "settles inverted, got {settled}"
        );
    }

    #[test]
    fn edge_destination_travels_on_wall_clock() {
        let sim = Simulator::from_config(edge_destination_config(
            0.9,
            Some("wall".to_string()),
            0,
            Some(10.0),
        ));
        // cue at at_secs default 0; travel 10s of uptime, ticks irrelevant
        assert!((sim.edge_strength_at(&sim.edges[0], 3.0, 9_999) - 0.3 - 0.6 * 0.3).abs() < 1e-12);
        assert!((sim.edge_strength_at(&sim.edges[0], 60.0, 0) - 0.9).abs() < 1e-12);
    }

    #[test]
    fn edge_declaring_both_forms_fails_loud_at_boot() {
        let cfg_json = serde_json::json!({
            "nodes": [{"id": "a", "params": 1, "objectives": 1, "base": "linear",
                        "weights": [[1.0]]}],
            "edges": [{"from": "a", "from_channel": 0, "to": "a", "to_channel": 0,
                        "strength": 0.3, "drift_rate": 0.001, "target_strength": 0.9}]
        });
        let result = std::panic::catch_unwind(|| {
            let _ =
                Simulator::from_config(serde_json::from_value::<BenchConfig>(cfg_json).unwrap());
        });
        assert!(
            result.is_err(),
            "both edge forms declared must fail at boot"
        );
    }

    #[test]
    fn exogenous_unknown_kind_fails_loud_at_boot() {
        let cfg_json = serde_json::json!({
            "nodes": [{"id": "a", "params": 1, "objectives": 1, "base": "linear",
                        "weights": [[1.0]],
                        "exogenous": [{"kind": "sep", "amplitude": 0.2}]}],
        });
        let result = std::panic::catch_unwind(|| {
            let _ =
                Simulator::from_config(serde_json::from_value::<BenchConfig>(cfg_json).unwrap());
        });
        assert!(result.is_err(), "unknown exogenous kind must fail at boot");
    }

    #[test]
    fn exogenous_windowless_sine_fails_loud_at_boot() {
        let cfg_json = serde_json::json!({
            "nodes": [{"id": "a", "params": 1, "objectives": 1, "base": "linear",
                        "weights": [[1.0]],
                        "exogenous": [{"kind": "sine", "amplitude": 0.3,
                                        "at_tick": 0, "travel_ticks": 0}]}],
        });
        let result = std::panic::catch_unwind(|| {
            let _ =
                Simulator::from_config(serde_json::from_value::<BenchConfig>(cfg_json).unwrap());
        });
        assert!(result.is_err(), "sine without a period must fail at boot");
    }

    #[test]
    fn morph_travels_on_wall_clock() {
        let cfg: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [{"id": "a", "params": 1, "objectives": 1, "base": "linear",
                        "weights": [[0.5]],
                        "morphs": [{"at_tick": 0, "at_secs": 2.0, "travel_secs": 4.0,
                                     "clock": "wall", "target_weights": [[1.0]]}]}],
            "noise": {"gaussian_sigma": 0.0}
        }))
        .unwrap();
        let sim = Simulator::from_config(cfg);
        let nc = &sim.nodes["a"].config;
        // before wall cue: declared weight
        let e0 = sim.shape_at(nc, 1.0, 1_000_000);
        assert!((e0.weights[0][0] - 0.5).abs() < 1e-12);
        // halfway on uptime, regardless of ticks
        let em = sim.shape_at(nc, 4.0, 0);
        assert!(
            (em.weights[0][0] - 0.75).abs() < 1e-12,
            "wall mid-travel {}",
            em.weights[0][0]
        );
        // settled
        let es = sim.shape_at(nc, 30.0, 7);
        assert!((es.weights[0][0] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn exogenous_step_ramps_then_holds_and_sine_stays_bounded() {
        let step = ExoConfig {
            name: None,
            kind: "step".to_string(),
            amplitude: 0.2,
            objective: 0,
            clock: None,
            at_tick: 10,
            at_secs: None,
            travel_ticks: 10,
            travel_secs: None,
        };
        assert_eq!(exogenous_at(&step, 0.0, 5), 0.0);
        assert!((exogenous_at(&step, 0.0, 15) - 0.1).abs() < 1e-12);
        assert!((exogenous_at(&step, 0.0, 10_000) - 0.2).abs() < 1e-12);

        let sine = ExoConfig {
            name: None,
            kind: "sine".to_string(),
            amplitude: 0.3,
            objective: 0,
            clock: Some("wall".to_string()),
            at_tick: 0,
            at_secs: Some(0.0),
            travel_ticks: 0,
            travel_secs: Some(4.0),
        };
        // period 4s: at t=1 a quarter turn → amplitude; bounded ±0.3 forever
        assert!((exogenous_at(&sine, 1.0, 0) - 0.3).abs() < 1e-9);
        for t in [0.3f64, 1.7, 5.9, 100.3, 2501.1] {
            let v = exogenous_at(&sine, t, 0);
            assert!(v.abs() <= 0.3 + 1e-9, "sine bounded, got {v} at {t}");
        }
    }

    #[test]
    fn noise_sigma_settles_at_declared_target() {
        let cfg: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [{"id": "a", "params": 1, "objectives": 1, "base": "linear",
                        "weights": [[1.0]]}],
            "noise": {"gaussian_sigma": 0.02, "target_gaussian_sigma": 0.06,
                       "sigma_at_tick": 5, "sigma_travel_ticks": 10}
        }))
        .unwrap();
        let sim = Simulator::from_config(cfg);
        assert!((effective_gaussian_sigma(&sim.noise, 0.0, 0) - 0.02).abs() < 1e-12);
        assert!((effective_gaussian_sigma(&sim.noise, 0.0, 10) - 0.04).abs() < 1e-12);
        assert!((effective_gaussian_sigma(&sim.noise, 0.0, 999) - 0.06).abs() < 1e-12);
    }

    #[test]
    fn truth_reports_the_effective_world() {
        let cfg: BenchConfig = serde_json::from_value(serde_json::json!({
            "nodes": [{"id": "a", "params": 1, "objectives": 1, "base": "linear",
                        "param_lower": 0.0, "param_upper": 100.0,
                        "weights": [[1.0]],
                        "exogenous": [{"kind": "step", "amplitude": 0.25,
                                        "at_tick": 0, "travel_ticks": 5}],
                        "drifts": [{"rate_per_second": 0.01, "cap": 0.5,
                                     "when": {"uptime_secs_gte": 0.0}}]}],
            "noise": {"gaussian_sigma": 0.02, "target_gaussian_sigma": 0.04,
                       "sigma_at_tick": 0, "sigma_travel_ticks": 10}
        }))
        .unwrap();
        let mut sim = Simulator::from_config(cfg);
        // pump past the push's travel window (5 ticks)
        for _ in 0..8 {
            sim.apply("a", &[50.0]);
        }
        // cue the declared drift by advancing wall time
        std::thread::sleep(std::time::Duration::from_millis(30));
        sim.apply("a", &[50.0]);
        let t = sim.truth("a").unwrap();
        assert!(t.uptime_secs >= 0.0);
        assert!(
            t.offset_effective > 0.0,
            "live drift counted: {}",
            t.offset_effective
        );
        assert!(
            t.noise_effective > 0.02 && t.noise_effective <= 0.04,
            "sigma settling, got {}",
            t.noise_effective
        );
        assert!(
            (t.exogenous_effective[0] - 0.25).abs() < 1e-12,
            "push settled, got {}",
            t.exogenous_effective[0]
        );
        // the push is IN the truth objectives: base linear 0.5 + push 0.25
        assert!(
            (t.objectives[0] - (0.5 + 0.25 + t.offset_effective)).abs() < 1e-9,
            "obj {} vs base+push+offset {}",
            t.objectives[0],
            0.5 + 0.25 + t.offset_effective
        );
    }

    // ── the golden trajectory: one committed run, replayed within tolerance ──
    //
    // The pointwise tests above pin single formulas; the golden pins
    // SEQUENCES — coupling under edge drift across ticks, a morph's
    // whole travel window, and the drift door's arithmetic at declared
    // stamps. The committed artifact (tests/generic_trajectory.json)
    // is written by an ignored regenerator and reviewed like a diff.
    // Regenerate after a DELIBERATE physics change:
    //   cargo test regenerate_the_golden_trajectory -- --ignored
    // Floats compare at 1e-9 RELATIVE (the 1-ULP codegen drift lesson
    // from the sister crates); structure and words compare exact.
    // The artifact is READ AT RUNTIME, never include_str!'d.

    fn golden_pair_config() -> BenchConfig {
        let config = serde_json::json!({
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
                 "strength": 0.7, "drift_rate": 0.01}
            ],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        });
        serde_json::from_value(config).unwrap()
    }

    fn golden_morph_config() -> BenchConfig {
        let config = serde_json::json!({
            "nodes": [
                {"id": "m", "params": 2, "objectives": 2, "base": "linear",
                 "param_lower": 0.0, "param_upper": 10.0,
                 "weights": [[1.0, 0.0], [0.0, 1.0]],
                 "morphs": [
                     {"at_tick": 2, "travel_ticks": 4,
                      "target_weights": [[2.0, 0.0], [0.0, 3.0]]}
                 ]}
            ],
            "edges": [],
            "noise": {"gaussian_sigma": 0.0, "colored_sigma": 0.0, "drift_rate": 0.0}
        });
        serde_json::from_value(config).unwrap()
    }

    fn truth_json(sim: &Simulator, node: &str) -> serde_json::Value {
        serde_json::to_value(sim.truth(node).expect("node exists")).unwrap()
    }

    fn replay_pair_run() -> Vec<serde_json::Value> {
        let mut sim = Simulator::from_config(golden_pair_config());
        // the seed env is a process-global race; pin the rng by hand
        sim.rng = rand::SeedableRng::seed_from_u64(42);
        let mut segments = Vec::new();
        for (tick, node, value) in [
            (1u64, "a", 100.0f64),
            (2, "b", 50.0),
            (3, "a", 0.0),
            (4, "b", 0.0),
            (5, "a", 60.0),
        ] {
            let applied = sim.apply(node, &[value]);
            assert_eq!(sim.total_ticks, tick, "one apply is one tick");
            segments.push(serde_json::json!({
                "apply": {"node": node, "value": value},
                "returned": applied,
                "truth_a": truth_json(&sim, "a"),
                "truth_b": truth_json(&sim, "b"),
            }));
        }
        segments
    }

    fn replay_morph_run() -> Vec<serde_json::Value> {
        let mut sim = Simulator::from_config(golden_morph_config());
        sim.rng = rand::SeedableRng::seed_from_u64(42);
        let mut segments = Vec::new();
        // six applies: before the morph window, through every quarter
        // of the travel, and settled after it
        for tick in 1..=6u64 {
            let applied = sim.apply("m", &[3.0, 7.0]);
            assert_eq!(sim.total_ticks, tick);
            segments.push(serde_json::json!({
                "tick": tick,
                "applied": applied,
                "truth_m": truth_json(&sim, "m"),
            }));
        }
        segments
    }

    /// The drift door's pure arithmetic at declared stamps — the wall
    /// clock never enters the answer key.
    fn replay_drift_arithmetic() -> Vec<serde_json::Value> {
        let mut cases = Vec::new();

        let ramp = vec![LiveDrift {
            name: Some("ramp".into()),
            origin: "declared".into(),
            rate_per_second: 0.5,
            cap: None,
            cued_uptime_secs: 10.0,
            cued_tick: 4,
        }];
        for now in [10.0f64, 16.0, 34.0] {
            cases.push(serde_json::json!({
                "case": "uncapped_ramp", "now": now,
                "offset": live_offset_at(&ramp, now),
            }));
        }

        let capped = vec![LiveDrift {
            name: Some("capped".into()),
            origin: "declared".into(),
            rate_per_second: 0.5,
            cap: Some(3.0),
            cued_uptime_secs: 10.0,
            cued_tick: 4,
        }];
        cases.push(serde_json::json!({
            "case": "cap_engages", "now": 20.0,
            "offset": live_offset_at(&capped, 20.0),
        }));

        let falling = vec![LiveDrift {
            name: Some("falling".into()),
            origin: "declared".into(),
            rate_per_second: -0.25,
            cap: Some(2.0),
            cued_uptime_secs: 0.0,
            cued_tick: 0,
        }];
        cases.push(serde_json::json!({
            "case": "negative_rate_signed_cap", "now": 20.0,
            "offset": live_offset_at(&falling, 20.0),
        }));
        cases.push(serde_json::json!({
            "case": "two_drifts_sum", "now": 20.0,
            "offset": live_offset_at(
                &[capped[0].clone(), falling[0].clone()],
                20.0,
            ),
        }));
        cases.push(serde_json::json!({
            "case": "no_drifts", "now": 100.0,
            "offset": live_offset_at(&[], 100.0),
        }));
        cases.push(serde_json::json!({
            "case": "before_the_cue_clamps_to_zero", "now": 5.0,
            "offset": live_offset_at(&ramp, 5.0),
        }));

        // the when-matrix: gates AND, param ops, unknowns refuse
        let when = |json: serde_json::Value| -> DriftWhen { serde_json::from_value(json).unwrap() };
        let params = vec![0.5, 2.0];
        for (label, w, now, ticks) in [
            ("empty_holds_always", when(serde_json::json!({})), 0.0, 0u64),
            (
                "seconds_gate",
                when(serde_json::json!({"seconds": 30})),
                29.9,
                0,
            ),
            (
                "seconds_gate_open",
                when(serde_json::json!({"seconds": 30})),
                30.0,
                0,
            ),
            (
                "ticks_gate",
                when(serde_json::json!({"ticks": 5})),
                100.0,
                4,
            ),
            (
                "both_gates_and",
                when(serde_json::json!({"seconds": 10, "ticks": 5})),
                20.0,
                3,
            ),
        ] {
            cases.push(serde_json::json!({
                "case": label, "holds": when_holds(&w, now, ticks, &params),
            }));
        }
        for (label, w) in [
            (
                "param_ge_holds",
                when(serde_json::json!({"param": {"param": 1, "op": ">=", "value": 2.0}})),
            ),
            (
                "param_le_refuses",
                when(serde_json::json!({"param": {"param": 1, "op": "<=", "value": 1.0}})),
            ),
            (
                "param_missing_index_refuses",
                when(serde_json::json!({"param": {"param": 9, "op": ">=", "value": 0.0}})),
            ),
            (
                "param_unknown_op_refuses",
                when(serde_json::json!({"param": {"param": 1, "op": ">", "value": 0.0}})),
            ),
        ] {
            cases.push(serde_json::json!({
                "case": label, "holds": when_holds(&w, 0.0, 0, &params),
            }));
        }

        // the door itself: first knock cues, second knock is refused
        let mut sim = Simulator::from_config(golden_pair_config());
        sim.rng = rand::SeedableRng::seed_from_u64(42);
        let first = sim.drift_start("a", 0.5, Some(3.0), Some("golden".into()));
        let second = sim.drift_start("a", 0.25, None, None);
        let normalized = match first {
            Some(Ok(mut d)) => {
                d.cued_uptime_secs = 10.0; // the wall stamp normalized away
                d
            }
            _ => panic!("first knock must cue"),
        };
        cases.push(serde_json::json!({
            "case": "door_first_knock_cues",
            "drift": serde_json::to_value(&normalized).unwrap(),
            "second_knock_refused": matches!(second, Some(Err(()))),
            "offset_at_16": live_offset_at(&[normalized], 16.0),
        }));

        cases
    }

    #[test]
    #[ignore = "writes the committed artifact — run deliberately, review the diff"]
    fn regenerate_the_golden_trajectory() {
        let artifact = serde_json::json!({
            "note": "the golden trajectory — regenerate via the ignored test, review like code",
            "runs": [
                {"label": "coupled_pair_edge_drift", "segments": replay_pair_run()},
                {"label": "morph_travel_window", "segments": replay_morph_run()},
                {"label": "drift_door_arithmetic", "cases": replay_drift_arithmetic()},
            ],
        });
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");
        std::fs::create_dir_all(dir).expect("tests dir");
        let path = format!("{dir}/generic_trajectory.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&artifact).expect("pretty"))
            .expect("write artifact");
        println!("wrote {path}");
    }

    /// float leaves agree within 1e-9 RELATIVE; structure and strings
    /// stay exact. (Same rule as the sister crates.)
    fn first_float_drift(
        path: &str,
        a: &serde_json::Value,
        b: &serde_json::Value,
    ) -> Option<String> {
        match (a, b) {
            (serde_json::Value::Number(x), serde_json::Value::Number(y)) => {
                let (fx, fy) = (x.as_f64()?, y.as_f64()?);
                let both_nan = fx.is_nan() && fy.is_nan();
                let scale = fx.abs().max(fy.abs().max(1.0));
                if both_nan || (fx - fy).abs() <= 1e-9 * scale {
                    None
                } else {
                    Some(format!("{path}: {fx} vs {fy}"))
                }
            }
            (serde_json::Value::Array(xs), serde_json::Value::Array(ys)) => {
                if xs.len() != ys.len() {
                    return Some(format!("{path}: {} entries vs {}", xs.len(), ys.len()));
                }
                xs.iter()
                    .zip(ys.iter())
                    .enumerate()
                    .find_map(|(i, (x, y))| first_float_drift(&format!("{path}[{i}]"), x, y))
            }
            (serde_json::Value::Object(xm), serde_json::Value::Object(ym)) => {
                if xm.len() != ym.len() {
                    return Some(format!("{path}: {} keys vs {}", xm.len(), ym.len()));
                }
                xm.iter().find_map(|(k, x)| {
                    // The wall clock is an observation stamp, not replayable
                    // physics: real uptime differs between runs by
                    // construction, exactly as a log timestamp would.
                    if k == "uptime_secs" {
                        return None;
                    }
                    let y = ym.get(k)?;
                    first_float_drift(&format!("{path}.{k}"), x, y)
                })
            }
            _ => {
                if a == b {
                    None
                } else {
                    Some(format!("{path}: {a} vs {b}"))
                }
            }
        }
    }

    fn assert_golden_segment_matches(
        label: &str,
        got: &serde_json::Value,
        want: &serde_json::Value,
    ) {
        if let Some(drift) = first_float_drift(label, got, want) {
            panic!("golden drift at {drift}");
        }
    }

    #[test]
    fn the_golden_trajectory_replays_within_tolerance() {
        let path = format!(
            "{}/tests/generic_trajectory.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw = std::fs::read_to_string(&path).expect("committed artifact present");
        let artifact: serde_json::Value =
            serde_json::from_str(&raw).expect("committed artifact parses");
        let runs = artifact["runs"].as_array().expect("artifact carries runs");
        assert_eq!(runs.len(), 3, "three golden runs");

        let replayed = [
            serde_json::json!({"label": "coupled_pair_edge_drift", "segments": replay_pair_run()}),
            serde_json::json!({"label": "morph_travel_window", "segments": replay_morph_run()}),
            serde_json::json!({"label": "drift_door_arithmetic", "cases": replay_drift_arithmetic()}),
        ];
        for (committed, now) in runs.iter().zip(replayed.iter()) {
            let label = committed["label"].as_str().unwrap_or("?");
            assert_eq!(
                now["label"].as_str().unwrap_or("?"),
                label,
                "run order is part of the contract"
            );
            assert_golden_segment_matches(label, now, committed);
        }
    }
}
