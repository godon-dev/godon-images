use crate::types::*;
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::sync::{Arc, Mutex};

const NOMINAL_FREQUENCY: f64 = 50.0;
const NOMINAL_VOLTAGE: f64 = 11.0;
const BASE_GENERATION_KW: f64 = 1000.0;
const FREQUENCY_SENSITIVITY: f64 = 0.02;
const VOLTAGE_SENSITIVITY: f64 = 0.005;
const COUPLING_FREQ_SCALE: f64 = 30000.0;
const COUPLING_VOLT_SCALE: f64 = 15000.0;
const FREQ_NORMALIZATION_RANGE: f64 = 25.0;
const VOLT_NORMALIZATION_RANGE: f64 = 15.0;
const COUPLING_CONGESTION_SCALE: f64 = 0.5;
const COUPLING_DIRECT_SCALE: f64 = 10.0;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct NeighborStatus {
    pub power_draw: f64,
    pub local_generation: f64,
    pub storage_dispatch: f64,
}

pub struct CouplingState {
    pub neighbors: Vec<String>,
    pub factor: f64,
    pub last_delta_frequency: f64,
    pub last_delta_voltage: f64,
    pub last_neighbor_load: f64,
}

impl CouplingState {
    pub fn new(neighbors: Vec<String>, factor: f64) -> Self {
        Self {
            neighbors,
            factor,
            last_delta_frequency: 0.0,
            last_delta_voltage: 0.0,
            last_neighbor_load: 0.0,
        }
    }
}

pub struct Microgrid {
    pub tick: u64,
    pub grid_frequency_hz: f64,
    pub grid_voltage_kv: f64,
    pub equipment_health: f64,
    pub cumulative_energy_kwh: f64,
    pub last_params: Option<ApplyRequest>,
    pub seed: u64,
    #[allow(dead_code)] // kept: the seeded generator rides the state shape
    rng: ChaCha8Rng,
    pub coupling: CouplingState,
}

impl Microgrid {
    pub fn with_coupling(seed: u64, neighbors: Vec<String>, coupling_factor: f64) -> Self {
        Self {
            tick: 0,
            grid_frequency_hz: NOMINAL_FREQUENCY,
            grid_voltage_kv: NOMINAL_VOLTAGE,
            equipment_health: 1.0,
            cumulative_energy_kwh: 0.0,
            last_params: None,
            seed,
            rng: ChaCha8Rng::seed_from_u64(seed),
            coupling: CouplingState::new(neighbors, coupling_factor),
        }
    }

    pub fn apply(&mut self, req: ApplyRequest) {
        self.last_params = Some(req);
    }

    fn net_load(&self) -> f64 {
        match &self.last_params {
            Some(p) => {
                let effective_draw = p.power_draw.clamp(0.0, 1000.0);
                let gen = p.local_generation.clamp(0.0, 500.0);
                let storage = p.storage_dispatch.clamp(-500.0, 500.0);
                effective_draw - gen - storage
            }
            None => 0.0,
        }
    }

    fn neighbor_net_load(status: &NeighborStatus) -> f64 {
        let draw = status.power_draw.clamp(0.0, 1000.0);
        let gen = status.local_generation.clamp(0.0, 500.0);
        let storage = status.storage_dispatch.clamp(-500.0, 500.0);
        draw - gen - storage
    }

    pub fn apply_coupling(&mut self, neighbor_statuses: &[NeighborStatus]) {
        let factor = self.coupling.factor;
        if factor <= 0.0 || neighbor_statuses.is_empty() {
            self.coupling.last_delta_frequency = 0.0;
            self.coupling.last_delta_voltage = 0.0;
            self.coupling.last_neighbor_load = 0.0;
            return;
        }

        let mut total_neighbor_load = 0.0;
        for ns in neighbor_statuses {
            total_neighbor_load += Self::neighbor_net_load(ns);
        }
        self.coupling.last_neighbor_load = total_neighbor_load;

        let load_ratio = total_neighbor_load / BASE_GENERATION_KW;

        self.coupling.last_delta_frequency =
            -load_ratio * FREQUENCY_SENSITIVITY * factor * COUPLING_FREQ_SCALE;
        self.coupling.last_delta_voltage =
            -load_ratio * VOLTAGE_SENSITIVITY * factor * COUPLING_VOLT_SCALE;
    }

    pub fn step(&mut self) {
        let params = match &self.last_params {
            Some(p) => p.clone(),
            None => return,
        };

        let net = self.net_load();
        let self_freq_deviation = -(net / BASE_GENERATION_KW) * FREQUENCY_SENSITIVITY * 50.0;
        let self_volt_deviation = -(net / BASE_GENERATION_KW) * VOLTAGE_SENSITIVITY * 10.0;

        self.grid_frequency_hz =
            NOMINAL_FREQUENCY + self_freq_deviation + self.coupling.last_delta_frequency;
        self.grid_voltage_kv =
            NOMINAL_VOLTAGE + self_volt_deviation + self.coupling.last_delta_voltage;

        let self_freq_dev = self_freq_deviation.abs();
        let self_volt_dev = self_volt_deviation.abs();

        let health_degradation = self_freq_dev * 0.001 + self_volt_dev * 0.005;
        if self.equipment_health > 0.3 {
            self.equipment_health -= health_degradation * 0.1;
            self.equipment_health = self.equipment_health.max(0.3);
        }
        if self_freq_dev < 0.5 && self_volt_dev < 0.2 {
            self.equipment_health = (self.equipment_health + 0.0001).min(1.0);
        }

        let dt = 0.1;
        let grid_energy = (net.abs() * 0.001) * dt;
        let local_energy = (params.local_generation.clamp(0.0, 500.0) * 0.0005) * dt;
        // Instantaneous rate, not cumulative. Same pattern as greenhouse growth_rate.
        self.cumulative_energy_kwh = grid_energy + local_energy;

        self.tick += 1;
    }

    pub fn run_steps(&mut self, steps: u64) {
        for _ in 0..steps {
            self.step();
        }
    }

    pub fn metrics(&mut self) -> MetricsResponse {
        let params = self.last_params.clone();

        let net = self.net_load();
        let self_freq_dev = (-(net / BASE_GENERATION_KW) * FREQUENCY_SENSITIVITY * 50.0).abs();
        let self_volt_dev = (-(net / BASE_GENERATION_KW) * VOLTAGE_SENSITIVITY * 10.0).abs();

        let self_freq_norm = 1.0 - (self_freq_dev / FREQ_NORMALIZATION_RANGE).min(1.0);
        let self_volt_norm = 1.0 - (self_volt_dev / VOLT_NORMALIZATION_RANGE).min(1.0);

        let coupling_freq_norm = self.coupling.last_delta_frequency / FREQ_NORMALIZATION_RANGE;
        let coupling_volt_norm = self.coupling.last_delta_voltage / VOLT_NORMALIZATION_RANGE;

        let freq_norm = self_freq_norm + coupling_freq_norm;
        let volt_norm = self_volt_norm + coupling_volt_norm;

        let neighbor_load = self.coupling.last_neighbor_load;
        let factor = self.coupling.factor;
        let direct_coupling = (neighbor_load / BASE_GENERATION_KW) * factor * COUPLING_DIRECT_SCALE;

        let efficiency = 0.5 * freq_norm + 0.5 * volt_norm;

        let neighbor_usage = (neighbor_load / BASE_GENERATION_KW).abs();
        let congestion = 1.0 - neighbor_usage * factor * COUPLING_CONGESTION_SCALE;
        let congestion = congestion.max(0.1);

        let effective_draw = params
            .as_ref()
            .map_or(0.0, |p| p.power_draw.clamp(0.0, 1000.0));
        let base_throughput =
            effective_draw * 0.001 * efficiency * self.equipment_health * congestion;
        let throughput = base_throughput + direct_coupling;

        let voltage_stability = volt_norm + direct_coupling;
        let equipment_health = self.equipment_health + direct_coupling * 0.1;

        MetricsResponse {
            throughput,
            equipment_health,
            voltage_stability,
            energy_consumption_kwh: self.cumulative_energy_kwh,
            grid_frequency_hz: self.grid_frequency_hz,
            grid_voltage_kv: self.grid_voltage_kv,
            local_load_kw: effective_draw,
            local_gen_kw: params
                .as_ref()
                .map_or(0.0, |p| p.local_generation.clamp(0.0, 500.0)),
            storage_kw: params
                .as_ref()
                .map_or(0.0, |p| p.storage_dispatch.clamp(-500.0, 500.0)),
            tick: self.tick,
            coupling_delta_frequency: self.coupling.last_delta_frequency,
            coupling_delta_voltage: self.coupling.last_delta_voltage,
        }
    }
}

pub type SharedMicrogrid = Arc<Mutex<Microgrid>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn params(draw: f64, gen: f64, storage: f64) -> ApplyRequest {
        ApplyRequest {
            power_draw: draw,
            local_generation: gen,
            storage_dispatch: storage,
            sim_steps: 1,
        }
    }

    fn neighbor(draw: f64, gen: f64, storage: f64) -> NeighborStatus {
        NeighborStatus {
            power_draw: draw,
            local_generation: gen,
            storage_dispatch: storage,
        }
    }

    // ── the load model: clamps and the net balance ──

    #[test]
    fn full_draw_no_gen_sags_frequency_by_exactly_one_hz() {
        let mut mg = Microgrid::with_coupling(42, vec![], 0.0);
        mg.apply(params(1000.0, 0.0, 0.0));
        mg.step();
        assert!((mg.grid_frequency_hz - 49.0).abs() < 1e-9);
        assert!((mg.grid_voltage_kv - 10.95).abs() < 1e-9);
    }

    #[test]
    fn draws_above_the_clamp_behave_as_the_clamp() {
        let mut over = Microgrid::with_coupling(42, vec![], 0.0);
        over.apply(params(5000.0, 0.0, 0.0));
        over.step();
        let mut at = Microgrid::with_coupling(42, vec![], 0.0);
        at.apply(params(1000.0, 0.0, 0.0));
        at.step();
        assert_eq!(over.grid_frequency_hz, at.grid_frequency_hz);
        assert_eq!(over.grid_voltage_kv, at.grid_voltage_kv);
    }

    #[test]
    fn generation_and_storage_offset_the_draw() {
        let mut mg = Microgrid::with_coupling(42, vec![], 0.0);
        mg.apply(params(1000.0, 500.0, 500.0));
        mg.step();
        // net = 0 → both grid quantities sit at nominal
        assert!((mg.grid_frequency_hz - 50.0).abs() < 1e-9);
        assert!((mg.grid_voltage_kv - 11.0).abs() < 1e-9);
    }

    #[test]
    fn generation_above_the_clamp_behaves_as_the_clamp() {
        let mut over = Microgrid::with_coupling(42, vec![], 0.0);
        over.apply(params(2000.0, 5000.0, 0.0));
        over.step();
        let mut at = Microgrid::with_coupling(42, vec![], 0.0);
        at.apply(params(1000.0, 500.0, 0.0));
        at.step();
        assert_eq!(over.grid_frequency_hz, at.grid_frequency_hz);
    }

    #[test]
    fn export_storage_deepens_the_sag() {
        let mut import = Microgrid::with_coupling(42, vec![], 0.0);
        import.apply(params(1000.0, 0.0, 0.0));
        import.step();
        let mut export = Microgrid::with_coupling(42, vec![], 0.0);
        export.apply(params(1000.0, 0.0, -500.0));
        export.step();
        // exporting storage raises net load 1000 → 1500: deviation grows 1.0 → 1.5 Hz
        assert!((export.grid_frequency_hz - 48.5).abs() < 1e-9);
        assert!(export.grid_frequency_hz < import.grid_frequency_hz);
    }

    #[test]
    fn step_without_params_is_a_no_op() {
        let mut mg = Microgrid::with_coupling(42, vec![], 0.0);
        mg.step();
        mg.run_steps(10);
        assert_eq!(mg.tick, 0);
        assert_eq!(mg.grid_frequency_hz, 50.0);
    }

    // ── coupling: neighbors pull the shared rails down ──

    #[test]
    fn a_loaded_neighbor_pulls_frequency_and_voltage_by_the_scaled_amount() {
        let mut mg = Microgrid::with_coupling(42, vec!["n".into()], 1.0);
        mg.apply_coupling(&[neighbor(1000.0, 0.0, 0.0)]);
        // load_ratio 1 → delta_freq = -1 * 0.02 * 1 * 30000, delta_volt = -1 * 0.005 * 1 * 15000
        assert!((mg.coupling.last_delta_frequency - (-600.0)).abs() < 1e-9);
        assert!((mg.coupling.last_delta_voltage - (-75.0)).abs() < 1e-9);
        assert!((mg.coupling.last_neighbor_load - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn neighbor_loads_sum_before_scaling() {
        let mut one = Microgrid::with_coupling(42, vec![], 1.0);
        one.apply_coupling(&[neighbor(1000.0, 0.0, 0.0)]);
        let mut two = Microgrid::with_coupling(42, vec![], 1.0);
        two.apply_coupling(&[neighbor(600.0, 0.0, 0.0), neighbor(400.0, 0.0, 0.0)]);
        assert_eq!(
            one.coupling.last_delta_frequency,
            two.coupling.last_delta_frequency
        );
        assert_eq!(
            one.coupling.last_delta_voltage,
            two.coupling.last_delta_voltage
        );
    }

    #[test]
    fn zero_factor_or_no_neighbors_zeroes_the_coupling_state() {
        let mut mg = Microgrid::with_coupling(42, vec!["n".into()], 0.0);
        mg.apply_coupling(&[neighbor(1000.0, 0.0, 0.0)]);
        assert_eq!(mg.coupling.last_delta_frequency, 0.0);
        assert_eq!(mg.coupling.last_delta_voltage, 0.0);
        assert_eq!(mg.coupling.last_neighbor_load, 0.0);

        let mut mg = Microgrid::with_coupling(42, vec![], 1.0);
        mg.apply_coupling(&[]);
        assert_eq!(mg.coupling.last_delta_frequency, 0.0);
    }

    #[test]
    fn coupling_deltas_add_onto_the_self_deviation_in_step() {
        let mut mg = Microgrid::with_coupling(42, vec!["n".into()], 1.0);
        mg.apply_coupling(&[neighbor(1000.0, 0.0, 0.0)]);
        mg.apply(params(1000.0, 0.0, 0.0));
        mg.step();
        // self sag -1.0 Hz plus coupling -600 Hz
        assert!((mg.grid_frequency_hz - (-551.0)).abs() < 1e-9);
    }

    // ── equipment health: wear under load, recovery at rest, the floor ──

    #[test]
    fn heavy_load_wears_health_by_the_exact_rate() {
        let mut mg = Microgrid::with_coupling(42, vec![], 0.0);
        mg.apply(params(1000.0, 0.0, 0.0));
        mg.step();
        // degradation = 1.0*0.001 + 0.05*0.005 = 0.00125, applied at 0.1
        assert!((mg.equipment_health - (1.0 - 0.000125)).abs() < 1e-12);
    }

    #[test]
    fn balanced_operation_recovers_health_up_to_the_cap() {
        let mut mg = Microgrid::with_coupling(42, vec![], 0.0);
        mg.equipment_health = 0.9;
        mg.apply(params(1000.0, 500.0, 500.0));
        mg.step();
        assert!((mg.equipment_health - (0.9 + 0.0001)).abs() < 1e-12);

        mg.equipment_health = 1.0;
        mg.step();
        assert_eq!(mg.equipment_health, 1.0);
    }

    #[test]
    fn sustained_overload_bottoms_out_at_the_health_floor() {
        let mut mg = Microgrid::with_coupling(42, vec![], 0.0);
        mg.apply(params(1000.0, 0.0, 0.0));
        mg.run_steps(50_000);
        assert!((mg.equipment_health - 0.3).abs() < 1e-9);
    }

    // ── energy: the instantaneous rate, not a cumulative sum ──

    #[test]
    fn energy_is_the_instantaneous_rate_of_the_current_params() {
        let mut mg = Microgrid::with_coupling(42, vec![], 0.0);
        mg.apply(params(1000.0, 500.0, 0.0));
        mg.step();
        // net 500 → grid (500*0.001)*0.1 + local (500*0.0005)*0.1
        assert!((mg.cumulative_energy_kwh - 0.075).abs() < 1e-12);
        mg.run_steps(5);
        assert!((mg.cumulative_energy_kwh - 0.075).abs() < 1e-12);
        assert_eq!(mg.tick, 6);
    }

    // ── metrics: throughput, stability, congestion ──

    #[test]
    fn metrics_at_rest_are_nominal() {
        let mut mg = Microgrid::with_coupling(42, vec![], 0.0);
        let m = mg.metrics();
        assert_eq!(m.tick, 0);
        assert_eq!(m.local_load_kw, 0.0);
        assert!((m.voltage_stability - 1.0).abs() < 1e-12);
        assert!((m.throughput - 0.0).abs() < 1e-12);
    }

    #[test]
    fn throughput_combines_efficiency_health_and_congestion() {
        let mut mg = Microgrid::with_coupling(42, vec![], 0.0);
        mg.apply(params(1000.0, 0.0, 0.0));
        mg.step();
        let m = mg.metrics();
        let freq_norm = 1.0 - 1.0 / 25.0;
        let volt_norm = 1.0 - 0.05 / 15.0;
        let efficiency = 0.5 * freq_norm + 0.5 * volt_norm;
        let expected = 1000.0 * 0.001 * efficiency * (1.0 - 0.000125);
        assert!((m.throughput - expected).abs() < 1e-9);
        assert!((m.local_load_kw - 1000.0).abs() < 1e-12);
    }

    #[test]
    fn a_coupled_neighbor_adds_direct_throughput_and_congestion() {
        let mut mg = Microgrid::with_coupling(42, vec!["n".into()], 1.0);
        mg.apply_coupling(&[neighbor(1000.0, 0.0, 0.0)]);
        mg.apply(params(1000.0, 0.0, 0.0));
        mg.step();
        let m = mg.metrics();
        // the coupling deltas are large relative to their normalization ranges,
        // so both norms go NEGATIVE — the direct term must overcome that
        let freq_norm = (1.0 - 1.0 / 25.0) + (-600.0 / 25.0);
        let volt_norm = (1.0 - 0.05 / 15.0) + (-75.0 / 15.0);
        let efficiency = 0.5 * freq_norm + 0.5 * volt_norm;
        let congestion = 1.0 - 1.0 * 1.0 * 0.5;
        let expected = 1000.0 * 0.001 * efficiency * (1.0 - 0.000125) * congestion + 10.0;
        assert!((m.throughput - expected).abs() < 1e-9);
        assert!((m.voltage_stability - (volt_norm + 10.0)).abs() < 1e-9);
        assert!((m.coupling_delta_frequency - (-600.0)).abs() < 1e-9);
    }

    #[test]
    fn extreme_neighbor_load_floors_congestion_at_a_tenth() {
        let mut mg = Microgrid::with_coupling(42, vec!["n".into()], 1.0);
        // three loaded neighbors: usage 3 → 1 - 1.5 is negative, clamped to 0.1
        mg.apply_coupling(&[
            neighbor(1000.0, 0.0, 0.0),
            neighbor(1000.0, 0.0, 0.0),
            neighbor(1000.0, 0.0, 0.0),
        ]);
        mg.apply(params(1000.0, 0.0, 0.0));
        mg.step();
        let m = mg.metrics();
        // three loaded neighbors: deltas triple → -1800 Hz / -225 kV before normalization
        let freq_norm = (1.0 - 1.0 / 25.0) + (-1800.0 / 25.0);
        let volt_norm = (1.0 - 0.05 / 15.0) + (-225.0 / 15.0);
        let efficiency = 0.5 * freq_norm + 0.5 * volt_norm;
        let direct = 3.0 * 1.0 * 10.0;
        let expected = 1000.0 * 0.001 * efficiency * (1.0 - 0.000125) * 0.1 + direct;
        assert!((m.throughput - expected).abs() < 1e-9);
    }

    #[test]
    fn reset_rebuilds_from_the_same_seed_with_coupling_preserved() {
        let mut mg = Microgrid::with_coupling(7, vec!["n".into()], 0.5);
        mg.apply(params(1000.0, 0.0, 0.0));
        mg.run_steps(20);
        let fresh = Microgrid::with_coupling(7, vec!["n".into()], 0.5);
        assert_eq!(fresh.seed, mg.seed);
        assert_eq!(fresh.tick, 0);
        assert_eq!(fresh.coupling.neighbors, mg.coupling.neighbors);
        assert_eq!(fresh.coupling.factor, mg.coupling.factor);
    }
}
