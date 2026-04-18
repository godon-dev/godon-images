// godon-bench-greenhouse -- API Type Definitions
//
// Request and response types for the greenhouse bench HTTP API.
//
// The API is designed to match the godon optimization loop:
//   1. POST /apply  -- effectuation: apply a parameter set, run simulation, return metrics
//   2. GET /metrics  -- reconnaissance: read current state in Prometheus format
//   3. GET /metrics/json -- reconnaissance: read current state as JSON
//   4. POST /reset  -- start a fresh trial
//
// Parameters map to what a greenhouse strain would suggest via Optuna trials.

use serde::{Deserialize, Serialize};

/// Request body for POST /apply -- the optimizer's suggested parameter set.
///
/// This is what the godon HTTP effectuator sends. Each field corresponds to a
/// tunable subsystem in the greenhouse. The optimizer explores combinations of
/// these values to maximize growth while minimizing resource usage.
#[derive(Debug, Clone, Deserialize)]
pub struct ApplyRequest {
    /// Target temperature per zone in °C. The heating system drives toward this.
    /// Broad range: setpoints below outside temp trigger cooling (via ventilation).
    pub heating_setpoints: Vec<f64>,

    /// Ventilation opening per zone, 0.0 (closed) to 1.0 (fully open).
    /// Higher venting cools the zone but loses CO2 and reduces humidity.
    /// This tradeoff is one of the key multi-objective tensions.
    pub vent_openings: Vec<f64>,

    /// Shade/blind position, 0.0 (no shading) to 1.0 (fully shaded).
    /// Global across all zones. Shading reduces solar heat gain AND light for
    /// photosynthesis, creating another tradeoff (cooling vs growth).
    pub shading: f64,

    /// CO2 injection rate. Higher = more CO2 available for photosynthesis,
    /// but also higher cost. Lost through ventilation.
    pub co2_injection: f64,

    /// Supplemental grow light intensity. Adds to solar radiation for growth
    /// calculation but consumes energy. Useful when solar radiation is low.
    pub light_intensity: f64,

    /// Irrigation rate. Drives transpiration (humidity) and plant growth.
    /// Too little = drought stress, too much = overwatering penalty.
    pub irrigation: f64,

    /// Number of simulation timesteps to run after applying parameters.
    /// Each step = 0.1 simulated hours. Default 60 = 6 simulated hours.
    /// Allows the optimizer to evaluate parameters over different time horizons.
    #[serde(default = "default_sim_steps")]
    pub sim_steps: u64,
}

fn default_sim_steps() -> u64 {
    60
}

/// Response from GET /metrics/json and the inner return of POST /apply.
///
/// Contains the simulation state after applying parameters. The optimizer
/// uses specific fields as objectives (growth_rate) and guardrails (max_temp,
/// max_humidity, max_co2, min_damage).
///
/// Additional observable metrics:
///   - zone_damage: per-zone survival rates (0.3-1.0). Observable like wilting
///     plants. Usable as guardrail: min_damage < 0.7 triggers rollback.
///   - coupling_delta_*: hidden coupling channel magnitudes. Not directly
///     actionable by the optimizer but valuable for post-hoc causality analysis
///     (cross-correlation, Granger causality between breeder time series).
#[derive(Debug, Clone, Serialize)]
pub struct MetricsResponse {
    /// Current temperature per zone (°C).
    pub zone_temps: Vec<f64>,
    /// Current humidity per zone (ratio 0-1).
    pub zone_humidities: Vec<f64>,
    /// Current CO2 concentration per zone (ppm).
    pub zone_co2_levels: Vec<f64>,
    /// Instantaneous growth rate per zone (0-1 range).
    pub zone_growth_rates: Vec<f64>,
    /// Per-zone damage/survival factor (0.3-1.0). Slowly accumulates under
    /// extreme temps, slowly recovers under normal conditions. Observable
    /// metric -- usable as guardrail (min_damage < 0.7 = rollback trigger).
    pub zone_damage: Vec<f64>,

    /// Average growth rate across all zones. **Primary objective to maximize.**
    pub growth_rate: f64,
    /// Cumulative energy consumed this trial (kWh). **Objective to minimize.**
    pub trial_energy_kwh: f64,
    /// Cumulative water consumed this trial (liters). **Objective to minimize.**
    pub trial_water_liters: f64,

    /// Highest temperature across all zones. Guardrail (>40°C = plant death).
    pub max_temp: f64,
    /// Lowest temperature across all zones. Guardrail (<5°C = plant death).
    pub min_temp: f64,
    /// Highest humidity across all zones. Guardrail (>0.9 = disease risk).
    pub max_humidity: f64,
    /// Highest CO2 across all zones. Guardrail (>1500ppm = waste/unsafe).
    pub max_co2: f64,

    /// Current outside temperature (°C). Includes coupling waste heat.
    pub outside_temp: f64,
    /// Current outside CO2 (ppm). Includes coupling exhaust delta.
    pub outside_co2: f64,
    /// Current outside humidity (ratio). Includes coupling drift delta.
    pub outside_humidity: f64,
    /// Current solar radiation (W/m²). Drifts with weather model.
    pub solar_radiation: f64,
    /// Simulation tick counter.
    pub tick: u64,
    /// Current crop developmental phase: "seedling", "vegetative", "flowering", "fruiting".
    pub crop_phase: String,

    /// Coupling waste heat delta (°C). Neighbor excess temp × factor.
    pub coupling_delta_temp: f64,
    /// Coupling CO2 exhaust delta (ppm). Neighbor energy × factor.
    pub coupling_delta_co2: f64,
    /// Coupling power sag delta (fraction). Reduces effective light intensity.
    pub coupling_delta_light: f64,
    /// Coupling humidity drift delta (ratio). Neighbor moisture migration.
    pub coupling_delta_humidity: f64,
}

/// Response from GET /status -- full greenhouse state including applied params
/// and coupling configuration. Used by neighbor greenhouses for coupling fetch.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct StatusResponse {
    pub zones: Vec<ZoneSnapshot>,
    pub outside_temp: f64,
    /// Outside CO2 (ppm). Includes coupling exhaust delta from neighbors.
    pub outside_co2: f64,
    /// Outside humidity (ratio). Includes coupling drift delta from neighbors.
    pub outside_humidity: f64,
    pub solar_radiation: f64,
    pub tick: u64,
    pub trial_energy_kwh: f64,
    pub trial_water_liters: f64,
    pub params: Option<ParamsSnapshot>,
    pub weather_mode: String,
    pub seed: u64,
    /// Current crop developmental phase name.
    pub crop_phase: String,
    /// Coupling strength factor (0.0 = uncoupled).
    pub coupling_factor: f64,
    /// URLs of coupled neighbor greenhouses.
    pub coupling_neighbors: Vec<String>,
}

/// Snapshot of a single zone's state. Used in status response and coupling fetch.
/// damage_factor has a serde default of 1.0 for backward compatibility with
/// neighbors running older greenhouse versions.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ZoneSnapshot {
    pub temp: f64,
    pub humidity: f64,
    pub co2: f64,
    pub growth_rate: f64,
    /// Irreversible damage factor (0.3-1.0). Default 1.0 for backward compat.
    #[serde(default = "default_damage")]
    pub damage_factor: f64,
}

fn default_damage() -> f64 {
    1.0
}

/// Snapshot of the currently applied parameters.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ParamsSnapshot {
    pub heating_setpoints: Vec<f64>,
    pub vent_openings: Vec<f64>,
    pub shading: f64,
    pub co2_injection: f64,
    pub light_intensity: f64,
    pub irrigation: f64,
}

/// Response from GET /health -- basic liveness check.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub zones: usize,
    pub tick: u64,
    pub weather_mode: String,
    pub seed: u64,
}
