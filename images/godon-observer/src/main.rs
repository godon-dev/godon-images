mod optuna_reader;

use clap::Parser;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
use log::{error, info};

use reqwest::blocking::Client;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use optuna_reader::OptunaReader;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, env = "HOST", default_value = "0.0.0.0")]
    host: String,

    #[clap(short, long, env = "PORT", default_value_t = 8089)]
    port: u16,

    #[clap(long, env = "PUSH_GATEWAY_URL", default_value = "http://pushgateway:9091")]
    push_gateway_url: String,

    #[clap(long, env = "GODON_API_URL", default_value = "http://godon-api:8080")]
    api_url: String,

    #[clap(long, default_value = "INFO")]
    log_level: String,
}

struct MetricsCache {
    metrics_text: String,
    pushgateway_reachable: f64,
    last_error: String,
}

struct ObserverState {
    push_gateway_url: String,
    cache: Arc<Mutex<MetricsCache>>,
    http_client: Client,
    optuna: OptunaReader,
    api_url: String,
}

impl ObserverState {
    fn new(push_gateway_url: String, api_url: String) -> Self {
        Self {
            push_gateway_url,
            cache: Arc::new(Mutex::new(MetricsCache {
                metrics_text: String::new(),
                pushgateway_reachable: 0.0,
                last_error: String::new(),
            })),
            http_client: Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            optuna: OptunaReader::from_env(),
            api_url,
        }
    }

    fn fetch_metrics(&self) {
        let url = format!("{}/metrics", self.push_gateway_url);
        match self.http_client.get(&url).send() {
            Ok(response) if response.status().is_success() => {
                if let Ok(text) = response.text() {
                    let mut cache = self.cache.lock().unwrap();
                    cache.metrics_text = text;
                    cache.pushgateway_reachable = 1.0;
                    cache.last_error = String::new();
                    info!("Successfully fetched metrics from Push Gateway");
                }
            }
            Ok(response) => {
                let mut cache = self.cache.lock().unwrap();
                cache.pushgateway_reachable = 0.0;
                cache.last_error = format!("HTTP {}", response.status());
                error!("Push Gateway returned: HTTP {}", response.status());
            }
            Err(e) => {
                let mut cache = self.cache.lock().unwrap();
                cache.pushgateway_reachable = 0.0;
                cache.last_error = format!("Connection failed: {}", e);
                error!("Push Gateway connection failed: {}", e);
            }
        }
    }

    fn get_metrics_text(&self) -> String {
        let cache = self.cache.lock().unwrap();
        let mut output = String::new();

        output.push_str("# HELP godon_observer_up Status of the Godon observer\n");
        output.push_str("# TYPE godon_observer_up gauge\n");
        output.push_str("godon_observer_up{status=\"success\"} 1\n\n");

        output.push_str("# HELP godon_observer_pushgateway_reachable Whether Push Gateway is reachable\n");
        output.push_str("# TYPE godon_observer_pushgateway_reachable gauge\n");
        output.push_str(&format!("godon_observer_pushgateway_reachable {}\n\n", cache.pushgateway_reachable));

        if cache.pushgateway_reachable == 1.0 && !cache.metrics_text.is_empty() {
            for line in cache.metrics_text.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    output.push_str(trimmed);
                    output.push('\n');
                }
            }
        }

        output
    }
}

fn json_response(status: StatusCode, body: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("Access-Control-Allow-Origin", "*")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn html_response(body: &str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/html; charset=utf-8")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn parse_query(uri: &hyper::Uri) -> std::collections::HashMap<String, String> {
    uri.query()
        .map(|q| {
            q.split('&')
                .filter_map(|pair| {
                    let mut kv = pair.split('=');
                    Some((kv.next()?.to_string(), kv.next()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn handle_request(req: Request<Body>, state: Arc<ObserverState>) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path().to_string();
    let path_parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if path == "/metrics" {
        state.fetch_metrics();
        let metrics_text = state.get_metrics_text();
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
            .body(Body::from(metrics_text))
            .unwrap());
    }

    if path == "/health" {
        let db_ok = state.optuna.health_check().await;
        let body = if db_ok { "OK" } else { "DEGRADED: db unreachable" };
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(body))
            .unwrap());
    }

    if path == "/dashboard" || path == "/dashboard/" {
        return Ok(html_response(DASHBOARD_HTML));
    }

    if path == "/d3.js" {
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/javascript; charset=utf-8")
            .body(Body::from(D3_JS))
            .unwrap());
    }

    // /api-proxy/breeders/<uuid> — proxy to godon-api for breeder config
    if path_parts.len() >= 3 && path_parts[0] == "api-proxy" && path_parts[1] == "breeders" {
        let api_path = format!("/breeders/{}", path_parts[2]);
        let url = format!("{}{}", state.api_url, api_path);
        return match state.http_client.get(&url).send() {
            Ok(response) if response.status().is_success() => {
                let body = response.text().unwrap_or_default();
                Ok(json_response(StatusCode::OK, &body))
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().unwrap_or_default();
                Ok(json_response(StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY), &body))
            }
            Err(e) => {
                error!("API proxy error: {}", e);
                Ok(json_response(StatusCode::BAD_GATEWAY, &format!("{{\"error\": \"api unreachable: {}\"}}", e)))
            }
        };
    }

    // /api/breeders/<uuid>/summary
    if path_parts.len() == 4 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "summary" {
        let breeder_id = path_parts[2].to_string();
        let study_name = format!("{}_study", breeder_id);

        let count = state.optuna.get_trial_count(&breeder_id, &study_name).await.unwrap_or(0);
        let attrs = state.optuna.get_study_user_attrs(&breeder_id, &study_name).await.unwrap_or_default();

        let json = serde_json::json!({
            "breeder_id": breeder_id,
            "study_name": study_name,
            "total_trials": count,
            "study_user_attributes": attrs,
        });
        return Ok(json_response(StatusCode::OK, &serde_json::to_string(&json).unwrap()));
    }

    // /api/breeders/<uuid>/studies
    if path_parts.len() == 4 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "studies" {
        let breeder_id = path_parts[2].to_string();
        return match state.optuna.list_studies(&breeder_id).await {
            Ok(studies) => {
                let json = serde_json::json!({"breeder_id": breeder_id, "studies": studies});
                Ok(json_response(StatusCode::OK, &serde_json::to_string(&json).unwrap()))
            }
            Err(e) => {
                error!("Failed to list studies: {}", e);
                Ok(json_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("{{\"error\": \"{}\"}}", e)))
            }
        };
    }

    // /api/breeders/<uuid>/trials/<study_name>
    if path_parts.len() >= 5 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "trials" {
        let breeder_id = path_parts[2].to_string();
        let study_name = path_parts[4].to_string();
        let query = parse_query(req.uri());
        let offset: i64 = query.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
        let limit: i64 = query.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);

        return match state.optuna.get_trials(&breeder_id, &study_name, offset, limit).await {
            Ok(trials) => {
                let json = serde_json::json!({
                    "breeder_id": breeder_id,
                    "study_name": study_name,
                    "offset": offset,
                    "limit": limit,
                    "trials": trials,
                });
                Ok(json_response(StatusCode::OK, &serde_json::to_string(&json).unwrap()))
            }
            Err(e) => {
                error!("Failed to load trials: {}", e);
                Ok(json_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("{{\"error\": \"{}\"}}", e)))
            }
        };
    }

    // /api/breeders/<uuid>/trials (auto-detect study name)
    if path_parts.len() >= 4 && path_parts[0] == "api" && path_parts[1] == "breeders" && path_parts[3] == "trials" {
        let breeder_id = path_parts[2].to_string();
        let study_name = format!("{}_study", breeder_id);
        let query = parse_query(req.uri());
        let offset: i64 = query.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
        let limit: i64 = query.get("limit").and_then(|v| v.parse().ok()).unwrap_or(100);

        return match state.optuna.get_trials(&breeder_id, &study_name, offset, limit).await {
            Ok(trials) => {
                let json = serde_json::json!({
                    "breeder_id": breeder_id,
                    "study_name": study_name,
                    "offset": offset,
                    "limit": limit,
                    "trials": trials,
                });
                Ok(json_response(StatusCode::OK, &serde_json::to_string(&json).unwrap()))
            }
            Err(e) => {
                error!("Failed to load trials: {}", e);
                Ok(json_response(StatusCode::INTERNAL_SERVER_ERROR, &format!("{{\"error\": \"{}\"}}", e)))
            }
        };
    }

    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("godon observer: try /metrics, /dashboard, /api/breeders/<uuid>/trials"))
        .unwrap())
}

const DASHBOARD_HTML: &str = include_str!("dashboard.html");
const D3_JS: &str = include_str!("d3.min.js");

#[tokio::main]
async fn main() {
    let args = Args::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(&args.log_level))
        .init();

    info!("Starting godon-observer v{}", env!("CARGO_PKG_VERSION"));
    info!("Push Gateway: {}", args.push_gateway_url);

    let state = Arc::new(ObserverState::new(args.push_gateway_url.clone(), args.api_url.clone()));
    let addr = format!("{}:{}", args.host, args.port);
    let addr = addr.parse().unwrap();

    let make_svc = make_service_fn(move |_| {
        let state = state.clone();
        async move {
            Ok::<_, hyper::Error>(service_fn(move |req| handle_request(req, state.clone())))
        }
    });

    let server = Server::bind(&addr).serve(make_svc);

    info!("Observer listening on http://{}", addr);
    info!("  /metrics   - Prometheus metrics");
    info!("  /dashboard - Visualization dashboard");
    info!("  /api/breeders/<uuid>/trials - Trial history");

    if let Err(e) = server.await {
        error!("Server error: {}", e);
        std::process::exit(1);
    }
}
