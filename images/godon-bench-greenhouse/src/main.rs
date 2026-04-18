// godon-bench-greenhouse -- Multi-Zone Greenhouse Simulation HTTP Server
//
// A self-contained benchmark target for the godon optimization engine.
// Simulates a greenhouse with multiple climate zones, each with temperature,
// humidity, CO2, and plant growth. The optimizer tunes heating, ventilation,
// shading, CO2 injection, lighting, and irrigation to maximize growth while
// minimizing energy and water usage.
//
// EXTENSIONS BEYOND BASIC GREENHOUSE:
//
//   Crop aging:       Optimal conditions shift through 4 developmental phases.
//                     The optimizer must find a time-varying policy, not a static one.
//
//   Irreversible damage: Sustained extreme temperatures permanently reduce growth.
//                     Reckless optimization has lasting consequences.
//
//   Critical windows: The flowering phase has 3× CO2 sensitivity. Getting CO2
//                     right during this window is dramatically more impactful.
//
//   Inter-greenhouse coupling: Neighbors silently modify each other's ambient
//                     conditions through 4 hidden channels (waste heat, CO2
//                     exhaust, power sag, humidity drift). Each greenhouse is
//                     a separate target with its own breeder. The coupling is
//                     invisible to the optimizer -- it only sees unexplained
//                     variance in its objectives.
//
// ENDPOINTS
//
//   POST /apply          Apply parameter set, run simulation, return metrics (JSON)
//   GET  /metrics         Current state in Prometheus exposition format
//   GET  /metrics/json    Current state as JSON
//   GET  /status          Full state including applied parameters and coupling info
//   GET  /health          Liveness check
//   POST /reset           Reset greenhouse to initial conditions (preserves coupling)
//
// ENVIRONMENT VARIABLES
//
//   PORT=8090                      HTTP listen port
//   GREENHOUSE_SCENARIO=simple     simple (2 zones), medium (4), complex (6)
//   GREENHOUSE_WEATHER=smooth      smooth, noisy, adversarial
//   GREENHOUSE_SEED=42             RNG seed for reproducibility
//   COUPLING_NEIGHBORS=            Comma-separated neighbor URLs (e.g. http://gh-2:8091)
//   COUPLING_FACTOR=0.0            Coupling strength (0=none, 0.05=weak, 0.2=strong)
//
// USAGE
//
//   # Standalone (no coupling)
//   docker run -p 8090:8090 ghcr.io/godon-dev/godon-bench-greenhouse
//
//   # Coupled pair
//   docker run -p 8090:8090 -e COUPLING_NEIGHBORS=http://gh-2:8091 -e COUPLING_FACTOR=0.1 ...
//   docker run -p 8091:8090 -e COUPLING_NEIGHBORS=http://gh-1:8090 -e COUPLING_FACTOR=0.1 ...
//
// INTEGRATION WITH GODON
//
//   Effectuator:   HTTP effectuator calls POST /apply
//   Reconnaissance: HTTP reconnaissance calls GET /metrics/json
//   Strain:        "bench_greenhouse" strain defines parameter ranges
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
use sim::{Greenhouse, NeighborStatus, Scenario, SharedGreenhouse, WeatherMode};
use std::net::SocketAddr;
use std::sync::Arc;
use types::*;

/// Prometheus metrics registry and gauge handles.
///
/// Each simulation metric is exposed as a Prometheus gauge so that godon's
/// reconnaissance scripts can scrape them. Zone-level metrics are indexed
/// (zone_0_temp, zone_1_temp, ...) to support per-zone monitoring.
///
/// New gauges track:
///   - Per-zone irreversible damage factors
///   - Outside CO2 and humidity (affected by coupling)
///   - Four coupling channel deltas (for post-hoc causality analysis)
struct AppMetrics {
    registry: Registry,
    zone_temp: Vec<Gauge>,
    zone_humidity: Vec<Gauge>,
    zone_co2: Vec<Gauge>,
    zone_growth_rate: Vec<Gauge>,
    zone_damage: Vec<Gauge>,
    growth_rate: Gauge,
    energy_kwh: Gauge,
    water_liters: Gauge,
    max_temp: Gauge,
    min_temp: Gauge,
    max_humidity: Gauge,
    max_co2: Gauge,
    outside_temp: Gauge,
    outside_co2: Gauge,
    outside_humidity: Gauge,
    solar_radiation: Gauge,
    coupling_delta_temp: Gauge,
    coupling_delta_co2: Gauge,
    coupling_delta_light: Gauge,
    coupling_delta_humidity: Gauge,
}

impl AppMetrics {
    fn new(zone_count: usize) -> Self {
        let registry = Registry::new();

        let zone_temp: Vec<Gauge> = (0..zone_count)
            .map(|i| Gauge::new(format!("greenhouse_zone_{i}_temp_celsius"), format!("Zone {i} temperature")).unwrap())
            .collect();

        let zone_humidity: Vec<Gauge> = (0..zone_count)
            .map(|i| Gauge::new(format!("greenhouse_zone_{i}_humidity_ratio"), format!("Zone {i} humidity")).unwrap())
            .collect();

        let zone_co2: Vec<Gauge> = (0..zone_count)
            .map(|i| Gauge::new(format!("greenhouse_zone_{i}_co2_ppm"), format!("Zone {i} CO2")).unwrap())
            .collect();

        let zone_growth_rate: Vec<Gauge> = (0..zone_count)
            .map(|i| Gauge::new(format!("greenhouse_zone_{i}_growth_rate"), format!("Zone {i} growth rate")).unwrap())
            .collect();

        let zone_damage: Vec<Gauge> = (0..zone_count)
            .map(|i| Gauge::new(format!("greenhouse_zone_{i}_damage_factor"), format!("Zone {i} damage/survival factor")).unwrap())
            .collect();

        let growth_rate = Gauge::new("greenhouse_growth_rate", "Average growth rate across all zones").unwrap();
        let energy_kwh = Gauge::new("greenhouse_energy_kwh", "Total energy consumed").unwrap();
        let water_liters = Gauge::new("greenhouse_water_liters", "Total water consumed").unwrap();
        let max_temp = Gauge::new("greenhouse_max_temp_celsius", "Maximum zone temperature").unwrap();
        let min_temp = Gauge::new("greenhouse_min_temp_celsius", "Minimum zone temperature").unwrap();
        let max_humidity = Gauge::new("greenhouse_max_humidity_ratio", "Maximum zone humidity").unwrap();
        let max_co2 = Gauge::new("greenhouse_max_co2_ppm", "Maximum zone CO2").unwrap();
        let outside_temp = Gauge::new("greenhouse_outside_temp_celsius", "Outside temperature").unwrap();
        let outside_co2 = Gauge::new("greenhouse_outside_co2_ppm", "Outside CO2").unwrap();
        let outside_humidity = Gauge::new("greenhouse_outside_humidity_ratio", "Outside humidity").unwrap();
        let solar_radiation = Gauge::new("greenhouse_solar_radiation_wm2", "Solar radiation").unwrap();
        let coupling_delta_temp = Gauge::new("greenhouse_coupling_delta_temp", "Coupling waste heat delta").unwrap();
        let coupling_delta_co2 = Gauge::new("greenhouse_coupling_delta_co2", "Coupling CO2 exhaust delta").unwrap();
        let coupling_delta_light = Gauge::new("greenhouse_coupling_delta_light", "Coupling power sag delta").unwrap();
        let coupling_delta_humidity = Gauge::new("greenhouse_coupling_delta_humidity", "Coupling humidity drift delta").unwrap();

        for g in &zone_temp { registry.register(Box::new(g.clone())).unwrap(); }
        for g in &zone_humidity { registry.register(Box::new(g.clone())).unwrap(); }
        for g in &zone_co2 { registry.register(Box::new(g.clone())).unwrap(); }
        for g in &zone_growth_rate { registry.register(Box::new(g.clone())).unwrap(); }
        for g in &zone_damage { registry.register(Box::new(g.clone())).unwrap(); }
        registry.register(Box::new(growth_rate.clone())).unwrap();
        registry.register(Box::new(energy_kwh.clone())).unwrap();
        registry.register(Box::new(water_liters.clone())).unwrap();
        registry.register(Box::new(max_temp.clone())).unwrap();
        registry.register(Box::new(min_temp.clone())).unwrap();
        registry.register(Box::new(max_humidity.clone())).unwrap();
        registry.register(Box::new(max_co2.clone())).unwrap();
        registry.register(Box::new(outside_temp.clone())).unwrap();
        registry.register(Box::new(outside_co2.clone())).unwrap();
        registry.register(Box::new(outside_humidity.clone())).unwrap();
        registry.register(Box::new(solar_radiation.clone())).unwrap();
        registry.register(Box::new(coupling_delta_temp.clone())).unwrap();
        registry.register(Box::new(coupling_delta_co2.clone())).unwrap();
        registry.register(Box::new(coupling_delta_light.clone())).unwrap();
        registry.register(Box::new(coupling_delta_humidity.clone())).unwrap();

        Self {
            registry,
            zone_temp,
            zone_humidity,
            zone_co2,
            zone_growth_rate,
            zone_damage,
            growth_rate,
            energy_kwh,
            water_liters,
            max_temp,
            min_temp,
            max_humidity,
            max_co2,
            outside_temp,
            outside_co2,
            outside_humidity,
            solar_radiation,
            coupling_delta_temp,
            coupling_delta_co2,
            coupling_delta_light,
            coupling_delta_humidity,
        }
    }

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
        for (i, d) in m.zone_damage.iter().enumerate() {
            self.zone_damage[i].set(*d);
        }
        self.growth_rate.set(m.growth_rate);
        self.energy_kwh.set(m.trial_energy_kwh);
        self.water_liters.set(m.trial_water_liters);
        self.max_temp.set(m.max_temp);
        self.min_temp.set(m.min_temp);
        self.max_humidity.set(m.max_humidity);
        self.max_co2.set(m.max_co2);
        self.outside_temp.set(m.outside_temp);
        self.outside_co2.set(m.outside_co2);
        self.outside_humidity.set(m.outside_humidity);
        self.solar_radiation.set(m.solar_radiation);
        self.coupling_delta_temp.set(m.coupling_delta_temp);
        self.coupling_delta_co2.set(m.coupling_delta_co2);
        self.coupling_delta_light.set(m.coupling_delta_light);
        self.coupling_delta_humidity.set(m.coupling_delta_humidity);
    }
}

/// Shared application state passed to all axum handlers.
struct AppState {
    greenhouse: SharedGreenhouse,
    metrics: Arc<AppMetrics>,
    /// HTTP client for fetching neighbor greenhouse statuses (coupling).
    http_client: reqwest::Client,
}

/// Fetch current status from all neighbor greenhouses for coupling computation.
///
/// Called before each /apply to refresh coupling deltas. Uses a 2-second timeout
/// per neighbor. Failures are logged but not fatal -- coupling gracefully degrades
/// to zero delta if neighbors are unreachable.
async fn fetch_neighbor_statuses(
    client: &reqwest::Client,
    neighbors: &[String],
) -> Vec<NeighborStatus> {
    let mut statuses = Vec::new();
    for url in neighbors {
        match client
            .get(format!("{}/status", url))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
        {
            Ok(resp) => {
                match resp.json::<StatusResponse>().await {
                    Ok(sr) => {
                        statuses.push(NeighborStatus {
                            zones: sr.zones,
                            trial_energy_kwh: sr.trial_energy_kwh,
                            trial_water_liters: sr.trial_water_liters,
                        });
                    }
                    Err(e) => info!("Coupling: parse error from {}: {}", url, e),
                }
            }
            Err(e) => info!("Coupling: fetch error from {}: {}", url, e),
        }
    }
    statuses
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

    // Inter-greenhouse coupling configuration.
    // COUPLING_NEIGHBORS: comma-separated URLs of neighbor greenhouses.
    // COUPLING_FACTOR: coupling strength (0.0 = none, 0.05 = weak, 0.2 = strong).
    let coupling_neighbors: Vec<String> = std::env::var("COUPLING_NEIGHBORS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let coupling_factor: f64 = std::env::var("COUPLING_FACTOR")
        .unwrap_or_else(|_| "0.0".to_string())
        .parse()
        .unwrap_or(0.0);

    info!(
        "Starting greenhouse bench - scenario: {:?}, zones: {}, weather: {:?}, seed: {}, coupling: {} neighbors @ factor {}",
        scenario, zone_count, weather_mode, seed, coupling_neighbors.len(), coupling_factor
    );

    let greenhouse = Arc::new(std::sync::Mutex::new(Greenhouse::with_coupling(
        scenario,
        weather_mode,
        seed,
        coupling_neighbors.clone(),
        coupling_factor,
    )));
    let metrics = Arc::new(AppMetrics::new(zone_count));
    let http_client = reqwest::Client::new();

    let state = AppState {
        greenhouse: greenhouse.clone(),
        metrics: metrics.clone(),
        http_client,
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

/// GET /status -- full greenhouse state including applied parameters and coupling info.
///
/// Returns per-zone state (including damage factors), outside conditions
/// (including coupling-modified CO2 and humidity), current crop phase,
/// and the coupling configuration (neighbors and factor).
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
            damage_factor: z.damage_factor,
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
    let crop_phase = gh.crop_phase_name().to_string();
    let coupling_factor = gh.coupling.factor;
    let coupling_neighbors = gh.coupling.neighbors.clone();

    axum::Json(StatusResponse {
        zones,
        outside_temp: gh.outside_temp,
        outside_co2: gh.outside_co2,
        outside_humidity: gh.outside_humidity,
        solar_radiation: gh.solar_radiation,
        tick: gh.tick,
        trial_energy_kwh: gh.trial_energy_kwh,
        trial_water_liters: gh.trial_water_liters,
        params,
        weather_mode,
        seed,
        crop_phase,
        coupling_factor,
        coupling_neighbors,
    })
}

/// POST /apply -- the core effectuation endpoint.
///
/// Receives the optimizer's suggested parameters, fetches neighbor statuses
/// for coupling, applies coupling deltas, runs the simulation forward, and
/// returns the resulting metrics.
///
/// The coupling fetch happens before simulation steps so that the latest
/// neighbor state is reflected in the ambient conditions.
async fn apply(
    State(state): State<Arc<AppState>>,
    axum::Json(req): axum::Json<ApplyRequest>,
) -> axum::Json<MetricsResponse> {
    let steps = req.sim_steps;

    // Fetch neighbor statuses outside the greenhouse lock to avoid deadlock
    // (neighbor might be fetching our status simultaneously).
    let neighbors = {
        let gh = state.greenhouse.lock().unwrap();
        gh.coupling.neighbors.clone()
    };
    let neighbor_statuses = fetch_neighbor_statuses(&state.http_client, &neighbors).await;

    let mut gh = state.greenhouse.lock().unwrap();
    if !neighbor_statuses.is_empty() {
        gh.apply_coupling(&neighbor_statuses);
    }
    gh.apply(req);
    gh.run_steps(steps);
    let m = gh.growth_metrics();
    state.metrics.update(&m);
    info!(
        "Applied params: growth={:.3}, energy={:.2}kWh, tick={}, phase={}",
        m.growth_rate, m.trial_energy_kwh, m.tick, m.crop_phase
    );
    axum::Json(m)
}

/// GET /metrics/json -- current metrics as JSON.
async fn metrics_json(State(state): State<Arc<AppState>>) -> axum::Json<MetricsResponse> {
    let mut gh = state.greenhouse.lock().unwrap();
    let m = gh.growth_metrics();
    state.metrics.update(&m);
    axum::Json(m)
}

/// GET /metrics -- Prometheus exposition format.
async fn metrics_endpoint(State(state): State<Arc<AppState>>) -> String {
    let mut gh = state.greenhouse.lock().unwrap();
    let m = gh.growth_metrics();
    state.metrics.update(&m);
    drop(gh);

    let encoder = prometheus::TextEncoder::new();
    let metric_families = state.metrics.registry.gather();
    encoder.encode_to_string(&metric_families).unwrap()
}

/// POST /reset -- reset the greenhouse to initial conditions.
///
/// Preserves coupling configuration (neighbors and factor) across reset
/// so that coupled greenhouses remain coupled after a trial reset.
async fn reset(State(state): State<Arc<AppState>>) -> axum::Json<HealthResponse> {
    let mut gh = state.greenhouse.lock().unwrap();
    let zone_count = gh.zones.len();
    let scenario = match zone_count {
        4 => Scenario::Medium,
        6 => Scenario::Complex,
        _ => Scenario::Simple,
    };

    let weather_mode = gh.weather_mode.clone();
    let seed = gh.seed;
    let neighbors = gh.coupling.neighbors.clone();
    let factor = gh.coupling.factor;

    *gh = Greenhouse::with_coupling(scenario, weather_mode, seed, neighbors, factor);
    info!("Reset greenhouse (coupling preserved)");

    axum::Json(HealthResponse {
        status: "reset".to_string(),
        zones: gh.zones.len(),
        tick: gh.tick,
        weather_mode: gh.weather_mode.as_str().to_string(),
        seed: gh.seed,
    })
}
