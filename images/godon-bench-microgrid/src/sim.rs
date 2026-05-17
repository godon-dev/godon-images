use crate::types::*;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::SeedableRng;
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
        self.cumulative_energy_kwh += grid_energy + local_energy;

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

        let coupling_freq_norm =
            self.coupling.last_delta_frequency / FREQ_NORMALIZATION_RANGE;
        let coupling_volt_norm =
            self.coupling.last_delta_voltage / VOLT_NORMALIZATION_RANGE;

        let freq_norm = self_freq_norm + coupling_freq_norm;
        let volt_norm = self_volt_norm + coupling_volt_norm;

        let neighbor_load = self.coupling.last_neighbor_load;
        let factor = self.coupling.factor;
        let direct_coupling = (neighbor_load / BASE_GENERATION_KW) * factor * COUPLING_DIRECT_SCALE;

        let efficiency = 0.5 * freq_norm + 0.5 * volt_norm;

        let neighbor_usage = (neighbor_load / BASE_GENERATION_KW).abs();
        let congestion = 1.0 - neighbor_usage * factor * COUPLING_CONGESTION_SCALE;
        let congestion = congestion.max(0.1);

        let effective_draw = params.as_ref().map_or(0.0, |p| p.power_draw.clamp(0.0, 1000.0));
        let base_throughput = effective_draw * 0.001 * efficiency * self.equipment_health * congestion;
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
            local_gen_kw: params.as_ref().map_or(0.0, |p| p.local_generation.clamp(0.0, 500.0)),
            storage_kw: params.as_ref().map_or(0.0, |p| p.storage_dispatch.clamp(-500.0, 500.0)),
            tick: self.tick,
            coupling_delta_frequency: self.coupling.last_delta_frequency,
            coupling_delta_voltage: self.coupling.last_delta_voltage,
        }
    }
}

pub type SharedMicrogrid = Arc<Mutex<Microgrid>>;
