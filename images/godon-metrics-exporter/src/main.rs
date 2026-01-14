use clap::Parser;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
use log::{error, info, debug};
use prometheus::{Encoder, TextEncoder};
use reqwest::blocking::Client;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

/// Godon Metrics Exporter - Fetches metrics from Prometheus Push Gateway and exposes them
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Bind address
    #[clap(short, long, default_value = "127.0.0.1")]
    host: String,

    /// HTTP server port
    #[clap(short, long, default_value_t = 8089)]
    port: u16,

    /// Push Gateway URL
    #[clap(long, default_value = "http://pushgateway:9091")]
    push_gateway_url: String,

    /// Log level (DEBUG, INFO, WARN, ERROR)
    #[clap(long, default_value = "INFO")]
    log_level: String,
}

struct MetricsCache {
    metrics_text: String,
    pushgateway_reachable: f64,
    last_error: String,
}

struct GodonExporter {
    push_gateway_url: String,
    cache: Arc<Mutex<MetricsCache>>,
    http_client: Client,
}

impl GodonExporter {
    fn new(push_gateway_url: String) -> Self {
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
        }
    }

    fn fetch_metrics(&self) {
        let url = format!("{}/metrics", self.push_gateway_url);

        debug!("Fetching metrics from: {}", url);

        match self.http_client.get(&url).send() {
            Ok(response) => {
                if response.status().is_success() {
                    match response.text() {
                        Ok(text) => {
                            let mut cache = self.cache.lock().unwrap();
                            cache.metrics_text = text;
                            cache.pushgateway_reachable = 1.0;
                            cache.last_error = String::new();
                            info!("Successfully fetched metrics from Push Gateway");
                        }
                        Err(e) => {
                            let mut cache = self.cache.lock().unwrap();
                            cache.pushgateway_reachable = 0.0;
                            cache.last_error = format!("Failed to read response: {}", e);
                            error!("Failed to read response: {}", e);
                        }
                    }
                } else {
                    let mut cache = self.cache.lock().unwrap();
                    cache.pushgateway_reachable = 0.0;
                    cache.last_error = format!("HTTP {}", response.status());
                    error!("Failed to fetch metrics: HTTP {}", response.status());
                }
            }
            Err(e) => {
                let mut cache = self.cache.lock().unwrap();
                cache.pushgateway_reachable = 0.0;
                cache.last_error = format!("Connection failed: {}", e);
                error!("Failed to connect to Push Gateway: {}", e);
            }
        }
    }

    fn get_metrics_text(&self) -> String {
        let cache = self.cache.lock().unwrap();

        let mut output = String::new();

        // Add exporter status
        output.push_str("# HELP godon_metrics_exporter_up Status of the Godon metrics exporter\n");
        output.push_str("# TYPE godon_metrics_exporter_up gauge\n");
        output.push_str("godon_metrics_exporter_up{status=\"success\"} 1\n\n");

        // Add Push Gateway reachability metric
        output.push_str("# HELP godon_metrics_pushgateway_reachable Whether Push Gateway is reachable\n");
        output.push_str("# TYPE godon_metrics_pushgateway_reachable gauge\n");
        output.push_str(&format!("godon_metrics_pushgateway_reachable {}\n\n", cache.pushgateway_reachable));

        // If Push Gateway is reachable, add its metrics
        if cache.pushgateway_reachable == 1.0 && !cache.metrics_text.is_empty() {
            // Parse and forward metrics from Push Gateway
            for line in cache.metrics_text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }

                // Forward the metric line as-is
                output.push_str(trimmed);
                output.push('\n');
            }
        } else if !cache.last_error.is_empty() {
            // Add error info as a metric
            output.push_str(&format!("# Last error: {}\n", cache.last_error));
        }

        output
    }
}

async fn serve_metrics(req: Request<Body>, exporter: Arc<GodonExporter>) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path();

    match path {
        "/metrics" => {
            // Fetch fresh metrics from Push Gateway
            exporter.fetch_metrics();

            // Get the metrics text
            let metrics_text = exporter.get_metrics_text();

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
                .body(Body::from(metrics_text))
                .unwrap())
        }
        "/health" => Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("OK"))
            .unwrap()),
        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("Try /metrics"))
            .unwrap()),
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(&args.log_level))
        .init();

    info!("Starting Godon Metrics Exporter v{}", env!("CARGO_PKG_VERSION"));

    // Create exporter
    let exporter = Arc::new(GodonExporter::new(args.push_gateway_url.clone()));

    // Build the address
    let addr = format!("{}:{}", args.host, args.port);
    let addr = addr.parse().unwrap();

    info!("Push Gateway URL: {}", args.push_gateway_url);
    info!("Listening on http://{}", addr);

    // Create a service that responds to all requests
    let make_svc = make_service_fn(move |_| {
        let exporter = exporter.clone();
        async move {
            Ok::<_, hyper::Error>(service_fn(move |req| {
                serve_metrics(req, exporter.clone())
            }))
        }
    });

    // Create the server
    let server = Server::bind(&addr).serve(make_svc);

    info!("Metrics endpoint: http://{}/metrics", addr);

    // Run the server
    if let Err(e) = server.await {
        error!("Server error: {}", e);
        std::process::exit(1);
    }
}
