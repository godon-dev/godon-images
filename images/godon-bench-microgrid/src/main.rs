mod sim;
mod types;

use axum::{
    extract::State,
    routing::{get, post},
    Router,
};
use log::info;
use prometheus::{Gauge, Registry};
use sim::{Microgrid, NeighborStatus, SharedMicrogrid};
use std::net::SocketAddr;
use std::sync::Arc;
use types::*;

struct AppMetrics {
    registry: Registry,
    throughput: Gauge,
    equipment_health: Gauge,
    voltage_stability: Gauge,
    energy_consumption_kwh: Gauge,
    grid_frequency_hz: Gauge,
    grid_voltage_kv: Gauge,
    local_load_kw: Gauge,
    local_gen_kw: Gauge,
    storage_kw: Gauge,
    coupling_delta_frequency: Gauge,
    coupling_delta_voltage: Gauge,
}

impl AppMetrics {
    fn new() -> Self {
        let registry = Registry::new();
        let throughput = Gauge::new("microgrid_throughput", "Effective throughput").unwrap();
        let equipment_health = Gauge::new("microgrid_equipment_health", "Equipment health factor").unwrap();
        let voltage_stability = Gauge::new("microgrid_voltage_stability", "Voltage stability").unwrap();
        let energy_consumption_kwh = Gauge::new("microgrid_energy_consumption_kwh", "Cumulative energy consumption").unwrap();
        let grid_frequency_hz = Gauge::new("microgrid_grid_frequency_hz", "Grid frequency").unwrap();
        let grid_voltage_kv = Gauge::new("microgrid_grid_voltage_kv", "Grid voltage").unwrap();
        let local_load_kw = Gauge::new("microgrid_local_load_kw", "Local power draw").unwrap();
        let local_gen_kw = Gauge::new("microgrid_local_gen_kw", "Local generation").unwrap();
        let storage_kw = Gauge::new("microgrid_storage_kw", "Storage dispatch").unwrap();
        let coupling_delta_frequency = Gauge::new("microgrid_coupling_delta_frequency", "Coupling frequency delta").unwrap();
        let coupling_delta_voltage = Gauge::new("microgrid_coupling_delta_voltage", "Coupling voltage delta").unwrap();

        registry.register(Box::new(throughput.clone())).unwrap();
        registry.register(Box::new(equipment_health.clone())).unwrap();
        registry.register(Box::new(voltage_stability.clone())).unwrap();
        registry.register(Box::new(energy_consumption_kwh.clone())).unwrap();
        registry.register(Box::new(grid_frequency_hz.clone())).unwrap();
        registry.register(Box::new(grid_voltage_kv.clone())).unwrap();
        registry.register(Box::new(local_load_kw.clone())).unwrap();
        registry.register(Box::new(local_gen_kw.clone())).unwrap();
        registry.register(Box::new(storage_kw.clone())).unwrap();
        registry.register(Box::new(coupling_delta_frequency.clone())).unwrap();
        registry.register(Box::new(coupling_delta_voltage.clone())).unwrap();

        Self {
            registry,
            throughput,
            equipment_health,
            voltage_stability,
            energy_consumption_kwh,
            grid_frequency_hz,
            grid_voltage_kv,
            local_load_kw,
            local_gen_kw,
            storage_kw,
            coupling_delta_frequency,
            coupling_delta_voltage,
        }
    }

    fn update(&self, m: &MetricsResponse) {
        self.throughput.set(m.throughput);
        self.equipment_health.set(m.equipment_health);
        self.voltage_stability.set(m.voltage_stability);
        self.energy_consumption_kwh.set(m.energy_consumption_kwh);
        self.grid_frequency_hz.set(m.grid_frequency_hz);
        self.grid_voltage_kv.set(m.grid_voltage_kv);
        self.local_load_kw.set(m.local_load_kw);
        self.local_gen_kw.set(m.local_gen_kw);
        self.storage_kw.set(m.storage_kw);
        self.coupling_delta_frequency.set(m.coupling_delta_frequency);
        self.coupling_delta_voltage.set(m.coupling_delta_voltage);
    }
}

struct AppState {
    microgrid: SharedMicrogrid,
    metrics: Arc<AppMetrics>,
    http_client: reqwest::Client,
}

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
            Ok(resp) => match resp.json::<StatusResponse>().await {
                Ok(sr) => statuses.push(NeighborStatus {
                    power_draw: sr.power_draw,
                    local_generation: sr.local_generation,
                    storage_dispatch: sr.storage_dispatch,
                }),
                Err(e) => info!("Coupling: parse error from {}: {}", url, e),
            },
            Err(e) => info!("Coupling: fetch error from {}: {}", url, e),
        }
    }
    statuses
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let seed: u64 = std::env::var("MICROGRID_SEED")
        .unwrap_or_else(|_| "42".to_string())
        .parse()
        .unwrap();

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
        "Starting microgrid bench - seed: {}, coupling: {} neighbors @ factor {}",
        seed,
        coupling_neighbors.len(),
        coupling_factor
    );

    let microgrid = Arc::new(std::sync::Mutex::new(Microgrid::with_coupling(
        seed,
        coupling_neighbors.clone(),
        coupling_factor,
    )));
    let metrics = Arc::new(AppMetrics::new());
    let http_client = reqwest::Client::new();

    let state = AppState {
        microgrid: microgrid.clone(),
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

async fn health(State(state): State<Arc<AppState>>) -> axum::Json<HealthResponse> {
    let mg = state.microgrid.lock().unwrap();
    axum::Json(HealthResponse {
        status: "ok".to_string(),
        tick: mg.tick,
        seed: mg.seed,
    })
}

async fn status(State(state): State<Arc<AppState>>) -> axum::Json<StatusResponse> {
    let mut mg = state.microgrid.lock().unwrap();
    let m = mg.metrics();
    let params = mg.last_params.as_ref();

    axum::Json(StatusResponse {
        grid_frequency_hz: mg.grid_frequency_hz,
        grid_voltage_kv: mg.grid_voltage_kv,
        power_draw: params.map_or(0.0, |p| p.power_draw),
        storage_dispatch: params.map_or(0.0, |p| p.storage_dispatch),
        local_generation: params.map_or(0.0, |p| p.local_generation),
        throughput: m.throughput,
        equipment_health: m.equipment_health,
        energy_consumption_kwh: m.energy_consumption_kwh,
        tick: mg.tick,
        coupling_factor: mg.coupling.factor,
        coupling_neighbors: mg.coupling.neighbors.clone(),
    })
}

async fn apply(
    State(state): State<Arc<AppState>>,
    axum::Json(req): axum::Json<ApplyRequest>,
) -> axum::Json<MetricsResponse> {
    let steps = req.sim_steps;

    let neighbors = {
        let mg = state.microgrid.lock().unwrap();
        mg.coupling.neighbors.clone()
    };
    let neighbor_statuses = fetch_neighbor_statuses(&state.http_client, &neighbors).await;

    let mut mg = state.microgrid.lock().unwrap();
    if !neighbor_statuses.is_empty() {
        mg.apply_coupling(&neighbor_statuses);
    }
    mg.apply(req);
    mg.run_steps(steps);
    let m = mg.metrics();
    state.metrics.update(&m);
    info!(
        "Applied params: throughput={:.3}, freq={:.2}Hz, voltage={:.2}kV, tick={}",
        m.throughput, m.grid_frequency_hz, m.grid_voltage_kv, m.tick
    );
    axum::Json(m)
}

async fn metrics_json(State(state): State<Arc<AppState>>) -> axum::Json<MetricsResponse> {
    let mut mg = state.microgrid.lock().unwrap();
    let m = mg.metrics();
    state.metrics.update(&m);
    axum::Json(m)
}

async fn metrics_endpoint(State(state): State<Arc<AppState>>) -> String {
    let mut mg = state.microgrid.lock().unwrap();
    let m = mg.metrics();
    state.metrics.update(&m);
    drop(mg);

    let encoder = prometheus::TextEncoder::new();
    let metric_families = state.metrics.registry.gather();
    encoder.encode_to_string(&metric_families).unwrap()
}

async fn reset(State(state): State<Arc<AppState>>) -> axum::Json<HealthResponse> {
    let mut mg = state.microgrid.lock().unwrap();
    let seed = mg.seed;
    let neighbors = mg.coupling.neighbors.clone();
    let factor = mg.coupling.factor;
    *mg = Microgrid::with_coupling(seed, neighbors, factor);
    info!("Reset microgrid (coupling preserved)");
    axum::Json(HealthResponse {
        status: "reset".to_string(),
        tick: 0,
        seed,
    })
}
