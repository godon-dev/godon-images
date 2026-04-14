// godon-bench-greenhouse -- Multi-Zone Greenhouse Simulation HTTP Server
//
// A self-contained benchmark target for the godon optimization engine.
// Simulates a greenhouse with multiple climate zones, each with temperature,
// humidity, CO2, and plant growth. The optimizer tunes heating, ventilation,
// shading, CO2 injection, lighting, and irrigation to maximize growth while
// minimizing energy and water usage.
//
// ENDPOINTS
//
//   POST /apply          Apply parameter set, run simulation, return metrics (JSON)
//   GET  /metrics         Current state in Prometheus exposition format
//   GET  /metrics/json    Current state as JSON
//   GET  /status          Full state including applied parameters
//   GET  /health          Liveness check
//   POST /reset           Reset greenhouse to initial conditions
//
// USAGE
//
//   docker run -p 8090:8090 ghcr.io/godon-dev/godon-bench-greenhouse
//   docker run -p 8090:8090 -e GREENHOUSE_SCENARIO=complex ghcr.io/godon-dev/godon-bench-greenhouse
//
// INTEGRATION WITH GODON
//
//   Effectuator:   HTTP effectuator (effectuation/http.py) calls POST /apply
//   Reconnaissance: Prometheus reconnaissance (reconnaissance/prometheus.py) calls GET /metrics
//   Strain:        New "greenhouse" strain defines parameter ranges and categories
//
// See sim.rs for the physics model documentation.

mod sim;
mod types;

use axum::{
    extract::State,
    routing::{get, post},
    Router,
};
use log::info;
use prometheus::{Gauge, Registry};
use sim::{Greenhouse, Scenario, SharedGreenhouse, WeatherMode};
use std::net::SocketAddr;
use std::sync::Arc;
use types::*;

/// Prometheus metrics registry and gauge handles.
///
/// Each simulation metric is exposed as a Prometheus gauge so that godon's
/// existing Prometheus reconnaissance script can scrape them without modification.
/// Zone-level metrics are indexed (zone_0_temp, zone_1_temp, ...) to support
/// per-zone monitoring and guardrails.
struct AppMetrics {
    registry: Registry,
    /// Per-zone temperature gauges
    zone_temp: Vec<Gauge>,
    /// Per-zone humidity gauges
    zone_humidity: Vec<Gauge>,
    /// Per-zone CO2 gauges
    zone_co2: Vec<Gauge>,
    /// Per-zone growth rate gauges
    zone_growth_rate: Vec<Gauge>,
    /// Average growth rate across all zones (primary objective)
    growth_rate: Gauge,
    /// Total energy consumed (objective to minimize)
    energy_kwh: Gauge,
    /// Total water consumed (objective to minimize)
    water_liters: Gauge,
    /// Maximum zone temperature (guardrail: must not exceed 40°C)
    max_temp: Gauge,
    /// Minimum zone temperature (guardrail: must not drop below 5°C)
    min_temp: Gauge,
    /// Maximum zone humidity (guardrail: must not exceed 0.9)
    max_humidity: Gauge,
    /// Maximum zone CO2 (guardrail: must not exceed 1500ppm)
    max_co2: Gauge,
    /// Current outside temperature (informational)
    outside_temp: Gauge,
    /// Current solar radiation (informational)
    solar_radiation: Gauge,
}

impl AppMetrics {
    fn new(zone_count: usize) -> Self {
        let registry = Registry::new();

        // Create per-zone metrics dynamically based on scenario zone count
        let zone_temp: Vec<Gauge> = (0..zone_count)
            .map(|i| {
                Gauge::new(
                    format!("greenhouse_zone_{i}_temp_celsius"),
                    format!("Zone {i} temperature"),
                ).unwrap()
            })
            .collect();

        let zone_humidity: Vec<Gauge> = (0..zone_count)
            .map(|i| {
                Gauge::new(
                    format!("greenhouse_zone_{i}_humidity_ratio"),
                    format!("Zone {i} humidity"),
                ).unwrap()
            })
            .collect();

        let zone_co2: Vec<Gauge> = (0..zone_count)
            .map(|i| {
                Gauge::new(
                    format!("greenhouse_zone_{i}_co2_ppm"),
                    format!("Zone {i} CO2"),
                ).unwrap()
            })
            .collect();

        let zone_growth_rate: Vec<Gauge> = (0..zone_count)
            .map(|i| {
                Gauge::new(
                    format!("greenhouse_zone_{i}_growth_rate"),
                    format!("Zone {i} growth rate"),
                ).unwrap()
            })
            .collect();

        // Global (non-per-zone) metrics
        let growth_rate = Gauge::new("greenhouse_growth_rate", "Average growth rate across all zones").unwrap();
        let energy_kwh = Gauge::new("greenhouse_energy_kwh", "Total energy consumed").unwrap();
        let water_liters = Gauge::new("greenhouse_water_liters", "Total water consumed").unwrap();
        let max_temp = Gauge::new("greenhouse_max_temp_celsius", "Maximum zone temperature").unwrap();
        let min_temp = Gauge::new("greenhouse_min_temp_celsius", "Minimum zone temperature").unwrap();
        let max_humidity = Gauge::new("greenhouse_max_humidity_ratio", "Maximum zone humidity").unwrap();
        let max_co2 = Gauge::new("greenhouse_max_co2_ppm", "Maximum zone CO2").unwrap();
        let outside_temp = Gauge::new("greenhouse_outside_temp_celsius", "Outside temperature").unwrap();
        let solar_radiation = Gauge::new("greenhouse_solar_radiation_wm2", "Solar radiation").unwrap();

        // Register all metrics with the Prometheus registry
        for g in &zone_temp { registry.register(Box::new(g.clone())).unwrap(); }
        for g in &zone_humidity { registry.register(Box::new(g.clone())).unwrap(); }
        for g in &zone_co2 { registry.register(Box::new(g.clone())).unwrap(); }
        for g in &zone_growth_rate { registry.register(Box::new(g.clone())).unwrap(); }
        registry.register(Box::new(growth_rate.clone())).unwrap();
        registry.register(Box::new(energy_kwh.clone())).unwrap();
        registry.register(Box::new(water_liters.clone())).unwrap();
        registry.register(Box::new(max_temp.clone())).unwrap();
        registry.register(Box::new(min_temp.clone())).unwrap();
        registry.register(Box::new(max_humidity.clone())).unwrap();
        registry.register(Box::new(max_co2.clone())).unwrap();
        registry.register(Box::new(outside_temp.clone())).unwrap();
        registry.register(Box::new(solar_radiation.clone())).unwrap();

        Self {
            registry,
            zone_temp,
            zone_humidity,
            zone_co2,
            zone_growth_rate,
            growth_rate,
            energy_kwh,
            water_liters,
            max_temp,
            min_temp,
            max_humidity,
            max_co2,
            outside_temp,
            solar_radiation,
        }
    }

    /// Push current simulation metrics into Prometheus gauges.
    /// Called after each /apply or /metrics request.
    fn update(&self, m: &MetricsResponse) {
        for (i, t) in m.zone_temps.iter().enumerate() {
            self.zone_temp[i].set(*t);
        }
        for (i, h) in m.zone_humidities.iter().enumerate() {
            self.zone_humidity[i].set(*h);
        }
        for (i, c) in m.zone_co2_levels.iter().enumerate() {
            self.zone_co2[i].set(*c);
        }
        for (i, g) in m.zone_growth_rates.iter().enumerate() {
            self.zone_growth_rate[i].set(*g);
        }
        self.growth_rate.set(m.growth_rate);
        self.energy_kwh.set(m.trial_energy_kwh);
        self.water_liters.set(m.trial_water_liters);
        self.max_temp.set(m.max_temp);
        self.min_temp.set(m.min_temp);
        self.max_humidity.set(m.max_humidity);
        self.max_co2.set(m.max_co2);
        self.outside_temp.set(m.outside_temp);
        self.solar_radiation.set(m.solar_radiation);
    }
}

/// Shared application state passed to all axum handlers.
struct AppState {
    greenhouse: SharedGreenhouse,
    metrics: Arc<AppMetrics>,
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Select scenario complexity from environment variable
    let scenario = match std::env::var("GREENHOUSE_SCENARIO")
        .unwrap_or_else(|_| "simple".to_string())
        .as_str()
    {
        "medium" => Scenario::Medium,
        "complex" => Scenario::Complex,
        _ => Scenario::Simple,
    };

    let zone_count = scenario.zone_count();

    let weather_mode = match std::env::var("GREENHOUSE_WEATHER")
        .unwrap_or_else(|_| "smooth".to_string())
        .as_str()
    {
        "noisy" => WeatherMode::Noisy,
        "adversarial" => WeatherMode::Adversarial,
        _ => WeatherMode::Smooth,
    };

    let seed: u64 = std::env::var("GREENHOUSE_SEED")
        .unwrap_or_else(|_| "42".to_string())
        .parse()
        .unwrap();

    info!("Starting greenhouse bench - scenario: {:?}, zones: {}, weather: {:?}, seed: {}",
          scenario, zone_count, weather_mode, seed);

    let greenhouse = Arc::new(std::sync::Mutex::new(Greenhouse::new(scenario, weather_mode, seed)));
    let metrics = Arc::new(AppMetrics::new(zone_count));

    let state = AppState {
        greenhouse: greenhouse.clone(),
        metrics: metrics.clone(),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/apply", post(apply))
        .route("/metrics", get(metrics_endpoint))
        .route("/metrics/json", get(metrics_json))
        .route("/reset", post(reset))
        .with_state(Arc::new(state));

    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "8090".to_string())
        .parse()
        .unwrap();

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("Listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// GET /health -- basic liveness check.
async fn health(State(state): State<Arc<AppState>>) -> axum::Json<HealthResponse> {
    let gh = state.greenhouse.lock().unwrap();
    axum::Json(HealthResponse {
        status: "ok".to_string(),
        zones: gh.zones.len(),
        tick: gh.tick,
        weather_mode: gh.weather_mode.as_str().to_string(),
        seed: gh.seed,
    })
}

/// GET /status -- full greenhouse state including the currently applied parameters.
async fn status(State(state): State<Arc<AppState>>) -> axum::Json<StatusResponse> {
    let gh = state.greenhouse.lock().unwrap();
    let zones: Vec<ZoneSnapshot> = gh
        .zones
        .iter()
        .map(|z| ZoneSnapshot {
            temp: z.temp,
            humidity: z.humidity,
            co2: z.co2,
            growth_rate: z.growth_accumulated,
        })
        .collect();

    let params = gh.last_params.as_ref().map(|p| ParamsSnapshot {
        heating_setpoints: p.heating_setpoints.clone(),
        vent_openings: p.vent_openings.clone(),
        shading: p.shading,
        co2_injection: p.co2_injection,
        light_intensity: p.light_intensity,
        irrigation: p.irrigation,
    });

    let weather_mode = gh.weather_mode.as_str().to_string();
    let seed = gh.seed;

    axum::Json(StatusResponse {
        zones,
        outside_temp: gh.outside_temp,
        solar_radiation: gh.solar_radiation,
        tick: gh.tick,
        trial_energy_kwh: gh.trial_energy_kwh,
        trial_water_liters: gh.trial_water_liters,
        params,
        weather_mode,
        seed,
    })
}

/// POST /apply -- the core effectuation endpoint.
///
/// Receives the optimizer's suggested parameters, applies them to the greenhouse,
/// runs the simulation forward, and returns the resulting metrics.
/// This is what godon's HTTP effectuator calls.
async fn apply(
    State(state): State<Arc<AppState>>,
    axum::Json(req): axum::Json<ApplyRequest>,
) -> axum::Json<MetricsResponse> {
    let steps = req.sim_steps;
    let mut gh = state.greenhouse.lock().unwrap();
    gh.apply(req);
    gh.run_steps(steps);
    let m = gh.growth_metrics();
    state.metrics.update(&m);
    info!(
        "Applied params: growth={:.3}, energy={:.2}kWh, tick={}",
        m.growth_rate, m.trial_energy_kwh, m.tick
    );
    axum::Json(m)
}

/// GET /metrics/json -- current metrics as JSON.
/// Alternative to Prometheus format for direct HTTP reconnaissance.
async fn metrics_json(State(state): State<Arc<AppState>>) -> axum::Json<MetricsResponse> {
    let mut gh = state.greenhouse.lock().unwrap();
    let m = gh.growth_metrics();
    state.metrics.update(&m);
    axum::Json(m)
}

/// GET /metrics -- Prometheus exposition format.
///
/// This is what godon's existing Prometheus reconnaissance script scrapes.
/// All metrics are gauges (current state, not counters).
async fn metrics_endpoint(State(state): State<Arc<AppState>>) -> String {
    let mut gh = state.greenhouse.lock().unwrap();
    let m = gh.growth_metrics();
    state.metrics.update(&m);
    drop(gh);

    // Encode all registered metrics into Prometheus text format
    let encoder = prometheus::TextEncoder::new();
    let metric_families = state.metrics.registry.gather();
    encoder.encode_to_string(&metric_families).unwrap()
}

/// POST /reset -- reset the greenhouse to initial conditions.
///
/// Used between optimization trials to start fresh. Tick counter resets to 0,
/// all zones return to default conditions, resource counters zeroed.
async fn reset(State(state): State<Arc<AppState>>) -> axum::Json<HealthResponse> {
    let mut gh = state.greenhouse.lock().unwrap();
    let zone_count = gh.zones.len();
    let scenario = match zone_count {
        4 => Scenario::Medium,
        6 => Scenario::Complex,
        _ => Scenario::Simple,
    };

    // Preserve weather mode and seed across reset for reproducibility
    let weather_mode = gh.weather_mode.clone();
    let seed = gh.seed;

    *gh = Greenhouse::new(scenario, weather_mode, seed);
    info!("Reset greenhouse");

    axum::Json(HealthResponse {
        status: "reset".to_string(),
        zones: gh.zones.len(),
        tick: gh.tick,
        weather_mode: gh.weather_mode.as_str().to_string(),
        seed: gh.seed,
    })
}
