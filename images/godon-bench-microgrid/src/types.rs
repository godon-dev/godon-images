use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyRequest {
    pub power_draw: f64,
    pub storage_dispatch: f64,
    pub local_generation: f64,
    #[serde(default = "default_sim_steps")]
    pub sim_steps: u64,
}

fn default_sim_steps() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize)]
pub struct MetricsResponse {
    pub throughput: f64,
    pub equipment_health: f64,
    pub voltage_stability: f64,
    pub energy_consumption_kwh: f64,
    pub grid_frequency_hz: f64,
    pub grid_voltage_kv: f64,
    pub local_load_kw: f64,
    pub local_gen_kw: f64,
    pub storage_kw: f64,
    pub tick: u64,
    pub coupling_delta_frequency: f64,
    pub coupling_delta_voltage: f64,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StatusResponse {
    pub grid_frequency_hz: f64,
    pub grid_voltage_kv: f64,
    pub power_draw: f64,
    pub storage_dispatch: f64,
    pub local_generation: f64,
    pub throughput: f64,
    pub equipment_health: f64,
    pub energy_consumption_kwh: f64,
    pub tick: u64,
    pub coupling_factor: f64,
    pub coupling_neighbors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub tick: u64,
    pub seed: u64,
}
