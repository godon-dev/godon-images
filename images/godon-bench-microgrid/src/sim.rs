use crate::types::*;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::SeedableRng;
use std::sync::{Arc, Mutex};

const NOMINAL_FREQUENCY: f64 = 50.0;
const NOMINAL_VOLTAGE: f64 = 11.0;
const BASE_GENERATION_KW: f64 = 1000.0;
const FREQUENCY_SENSITIVITY: f64 = 0.02;
const VOLTAGE_SENSITIVITY: f64 = 0.005;

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
}

impl CouplingState {
    pub fn new(neighbors: Vec<String>, factor: f64) -> Self {
        Self {
            neighbors,
            factor,
            last_delta_frequency: 0.0,
            last_delta_voltage: 0.0,
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
            return;
        }

        let mut total_neighbor_load = 0.0;
        for ns in neighbor_statuses {
            total_neighbor_load += Self::neighbor_net_load(ns);
        }

        let imbalance = self.net_load() + total_neighbor_load;
        let generation_ratio = BASE_GENERATION_KW / (BASE_GENERATION_KW + imbalance.abs().max(1.0));

        self.coupling.last_delta_frequency =
            -(imbalance / BASE_GENERATION_KW) * FREQUENCY_SENSITIVITY * factor * 50.0;
        self.coupling.last_delta_voltage =
            -(imbalance / BASE_GENERATION_KW) * VOLTAGE_SENSITIVITY * factor * 10.0
                * generation_ratio;
    }

    pub fn step(&mut self) {
        let params = match &self.last_params {
            Some(p) => p.clone(),
            None => return,
        };

        let net = self.net_load();
        let total_imbalance = net;
        let freq_deviation = -(total_imbalance / BASE_GENERATION_KW) * FREQUENCY_SENSITIVITY * 50.0;
        let voltage_deviation = -(total_imbalance / BASE_GENERATION_KW) * VOLTAGE_SENSITIVITY * 10.0;

        self.grid_frequency_hz =
            NOMINAL_FREQUENCY + freq_deviation + self.coupling.last_delta_frequency;
        self.grid_voltage_kv =
            NOMINAL_VOLTAGE + voltage_deviation + self.coupling.last_delta_voltage;

        let freq_dev = (self.grid_frequency_hz - NOMINAL_FREQUENCY).abs();
        let volt_dev = (self.grid_voltage_kv - NOMINAL_VOLTAGE).abs();

        let health_degradation = freq_dev * 0.001 + volt_dev * 0.005;
        if self.equipment_health > 0.3 {
            self.equipment_health -= health_degradation * 0.1;
            self.equipment_health = self.equipment_health.max(0.3);
        }
        if freq_dev < 0.5 && volt_dev < 0.2 {
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

        let freq_dev = (self.grid_frequency_hz - NOMINAL_FREQUENCY).abs();
        let volt_dev = (self.grid_voltage_kv - NOMINAL_VOLTAGE).abs();
        let freq_norm = 1.0 - (freq_dev / 2.5).min(1.0);
        let volt_norm = 1.0 - (volt_dev / 1.5).min(1.0);
        let efficiency = freq_norm * volt_norm;

        let effective_draw = params.as_ref().map_or(0.0, |p| p.power_draw.clamp(0.0, 1000.0));
        let throughput = effective_draw * 0.001 * efficiency * self.equipment_health;

        MetricsResponse {
            throughput,
            equipment_health: self.equipment_health,
            voltage_stability: volt_norm,
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
