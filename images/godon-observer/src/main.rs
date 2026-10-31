mod optuna_reader;

use clap::Parser;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
use log::{error, info};

use optuna_reader::OptunaReader;
use reqwest::blocking::Client;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, env = "HOST", default_value = "0.0.0.0")]
    host: String,

    #[clap(short, long, env = "PORT", default_value_t = 8089)]
    port: u16,

    #[clap(
        long,
        env = "PUSH_GATEWAY_URL",
        default_value = "http://pushgateway:9091"
    )]
    push_gateway_url: String,

    #[clap(long, env = "GODON_API_URL", default_value = "http://godon-api:8080")]
    api_url: String,

    #[clap(
        long,
        env = "GODON_CAUSAL_URL",
        default_value = "http://godon-causal:8091"
    )]
    causal_url: String,

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
    causal_url: String,
}

impl ObserverState {
    fn new(push_gateway_url: String, api_url: String, causal_url: String) -> Self {
        Self {
            push_gateway_url,
            cache: Arc::new(Mutex::new(MetricsCache {
                metrics_text: String::new(),
                pushgateway_reachable: 0.0,
                last_error: String::new(),
            })),
            http_client: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            optuna: OptunaReader::from_env(),
            api_url,
            causal_url,
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

        output.push_str(
            "# HELP godon_observer_pushgateway_reachable Whether Push Gateway is reachable\n",
        );
        output.push_str("# TYPE godon_observer_pushgateway_reachable gauge\n");
        output.push_str(&format!(
            "godon_observer_pushgateway_reachable {}\n\n",
            cache.pushgateway_reachable
        ));

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

async fn handle_request(
    req: Request<Body>,
    state: Arc<ObserverState>,
) -> Result<Response<Body>, hyper::Error> {
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
        let body = if db_ok {
            "OK"
        } else {
            "DEGRADED: db unreachable"
        };
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

    // /api/active-systemtenders — interference active systemtenders
    if path_parts.len() == 2 && path_parts[0] == "api" && path_parts[1] == "active-systemtenders" {
        return match state.optuna.get_active_systemtenders().await {
            Ok(status) => Ok(json_response(
                StatusCode::OK,
                &serde_json::to_string(&status).unwrap_or_default(),
            )),
            Err(e) => {
                error!("Active systemtenders error: {}", e);
                Ok(json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("{{\"error\": \"{}\"}}", e),
                ))
            }
        };
    }

    // /api/connectome — proxy to godon-causal for the full graph
    if path_parts.len() == 2 && path_parts[0] == "api" && path_parts[1] == "connectome" {
        let causal_url = format!("{}/graph", state.causal_url);
        return match state.http_client.get(&causal_url).send() {
            Ok(response) if response.status().is_success() => {
                let body = response.text().unwrap_or_default();
                Ok(json_response(StatusCode::OK, &body))
            }
            Ok(response) => Ok(json_response(
                response.status(),
                &format!("{{\"error\": \"causal returned {}\"}}", response.status()),
            )),
            Err(e) => Ok(json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                &format!("{{\"error\": \"causal unreachable: {}\"}}", e),
            )),
        };
    }

    // /api/coupling-detection/<sender_id>/<receiver_id>
    // Proxied to godon-causal's /detect endpoint. Falls back to local
    // detection if causal is unreachable (gradual migration).
    if (path_parts.len() == 4 && path_parts[0] == "api" && path_parts[1] == "coupling-detection")
        || (path_parts.len() == 4
            && path_parts[0] == "api"
            && path_parts[1] == "watermark-detection")
    {
        let sender_id = path_parts[2].to_string();
        let receiver_id = path_parts[3].to_string();

        // Try causal first
        let causal_url = format!("{}/detect/{}/{}", state.causal_url, sender_id, receiver_id);
        match state.http_client.get(&causal_url).send() {
            Ok(response) if response.status().is_success() => {
                let body = response.text().unwrap_or_default();
                return Ok(json_response(StatusCode::OK, &body));
            }
            Ok(response) => {
                info!(
                    "causal returned HTTP {} for detection, falling back to local",
                    response.status()
                );
            }
            Err(e) => {
                info!(
                    "causal unreachable ({}), falling back to local detection",
                    e
                );
            }
        }

        // Fallback: local detection
        return match state
            .optuna
            .detect_watermark_coupling(&sender_id, &receiver_id)
            .await
        {
            Ok(result) => Ok(json_response(
                StatusCode::OK,
                &serde_json::to_string(&result).unwrap_or_default(),
            )),
            Err(e) => {
                error!("Watermark detection error: {:?}", e);
                Ok(json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("{{\"error\": \"{}\"}}", e),
                ))
            }
        };
    }

    // /api/systemtenders — list all systemtenders with trial summaries
    if path_parts.len() == 2 && path_parts[0] == "api" && path_parts[1] == "systemtenders" {
        let url = format!("{}/systemtenders", state.api_url);
        return match state.http_client.get(&url).send() {
            Ok(response) if response.status().is_success() => {
                let body = response.text().unwrap_or_default();
                match serde_json::from_str::<serde_json::Value>(&body) {
                    Ok(serde_json::Value::Array(systemtenders)) => {
                        let mut enriched = Vec::new();
                        for b in &systemtenders {
                            let id = b
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let study_name = format!("{}_study", id);
                            let trial_count = state
                                .optuna
                                .get_trial_count(&id, &study_name)
                                .await
                                .unwrap_or(0);
                            let mut obj = b.clone();
                            obj.as_object_mut().map(|m| {
                                m.insert("total_trials".into(), serde_json::json!(trial_count))
                            });
                            enriched.push(obj);
                        }
                        Ok(json_response(
                            StatusCode::OK,
                            &serde_json::to_string(&enriched).unwrap(),
                        ))
                    }
                    _ => Ok(json_response(StatusCode::OK, &body)),
                }
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().unwrap_or_default();
                Ok(json_response(
                    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                    &body,
                ))
            }
            Err(e) => {
                error!("API list systemtenders error: {}", e);
                Ok(json_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("{{\"error\": \"api unreachable: {}\"}}", e),
                ))
            }
        };
    }

    // /api-proxy/systemtenders/<uuid> — proxy to godon-api for systemtender config
    if path_parts.len() >= 3 && path_parts[0] == "api-proxy" && path_parts[1] == "systemtenders" {
        let api_path = format!("/systemtenders/{}", path_parts[2]);
        let url = format!("{}{}", state.api_url, api_path);
        return match state.http_client.get(&url).send() {
            Ok(response) if response.status().is_success() => {
                let body = response.text().unwrap_or_default();
                Ok(json_response(StatusCode::OK, &body))
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().unwrap_or_default();
                Ok(json_response(
                    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                    &body,
                ))
            }
            Err(e) => {
                error!("API proxy error: {}", e);
                Ok(json_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("{{\"error\": \"api unreachable: {}\"}}", e),
                ))
            }
        };
    }

    // /api/systemtenders/<uuid>/summary
    if path_parts.len() == 4
        && path_parts[0] == "api"
        && path_parts[1] == "systemtenders"
        && path_parts[3] == "summary"
    {
        let systemtender_id = path_parts[2].to_string();
        let study_name = format!("{}_study", systemtender_id);

        let count = state
            .optuna
            .get_trial_count(&systemtender_id, &study_name)
            .await
            .unwrap_or(0);
        let attrs = state
            .optuna
            .get_study_user_attrs(&systemtender_id, &study_name)
            .await
            .unwrap_or_default();

        let json = serde_json::json!({
            "systemtender_id": systemtender_id,
            "study_name": study_name,
            "total_trials": count,
            "study_user_attributes": attrs,
        });
        return Ok(json_response(
            StatusCode::OK,
            &serde_json::to_string(&json).unwrap(),
        ));
    }

    // /api/systemtenders/<uuid>/studies
    if path_parts.len() == 4
        && path_parts[0] == "api"
        && path_parts[1] == "systemtenders"
        && path_parts[3] == "studies"
    {
        let systemtender_id = path_parts[2].to_string();
        return match state.optuna.list_studies(&systemtender_id).await {
            Ok(studies) => {
                let json =
                    serde_json::json!({"systemtender_id": systemtender_id, "studies": studies});
                Ok(json_response(
                    StatusCode::OK,
                    &serde_json::to_string(&json).unwrap(),
                ))
            }
            Err(e) => {
                error!("Failed to list studies: {}", e);
                Ok(json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("{{\"error\": \"{}\"}}", e),
                ))
            }
        };
    }

    // /api/systemtenders/<uuid>/trials/<study_name>
    if path_parts.len() >= 5
        && path_parts[0] == "api"
        && path_parts[1] == "systemtenders"
        && path_parts[3] == "trials"
    {
        let systemtender_id = path_parts[2].to_string();
        let study_name = path_parts[4].to_string();
        let query = parse_query(req.uri());
        let offset: i64 = query
            .get("offset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let limit: i64 = query
            .get("limit")
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        return match state
            .optuna
            .get_trials(&systemtender_id, &study_name, offset, limit)
            .await
        {
            Ok(trials) => {
                let json = serde_json::json!({
                    "systemtender_id": systemtender_id,
                    "study_name": study_name,
                    "offset": offset,
                    "limit": limit,
                    "trials": trials,
                });
                Ok(json_response(
                    StatusCode::OK,
                    &serde_json::to_string(&json).unwrap(),
                ))
            }
            Err(e) => {
                error!("Failed to load trials: {}", e);
                Ok(json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("{{\"error\": \"{}\"}}", e),
                ))
            }
        };
    }

    // /api/systemtenders/<uuid>/trials (auto-detect study name)
    if path_parts.len() >= 4
        && path_parts[0] == "api"
        && path_parts[1] == "systemtenders"
        && path_parts[3] == "trials"
    {
        let systemtender_id = path_parts[2].to_string();
        let study_name = format!("{}_study", systemtender_id);
        let query = parse_query(req.uri());
        let offset: i64 = query
            .get("offset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let limit: i64 = query
            .get("limit")
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);

        return match state
            .optuna
            .get_trials(&systemtender_id, &study_name, offset, limit)
            .await
        {
            Ok(trials) => {
                let json = serde_json::json!({
                    "systemtender_id": systemtender_id,
                    "study_name": study_name,
                    "offset": offset,
                    "limit": limit,
                    "trials": trials,
                });
                Ok(json_response(
                    StatusCode::OK,
                    &serde_json::to_string(&json).unwrap(),
                ))
            }
            Err(e) => {
                error!("Failed to load trials: {}", e);
                Ok(json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &format!("{{\"error\": \"{}\"}}", e),
                ))
            }
        };
    }

    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from(
            "godon observer: try /metrics, /dashboard, /api/systemtenders/<uuid>/trials",
        ))
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

    let state = Arc::new(ObserverState::new(
        args.push_gateway_url.clone(),
        args.api_url.clone(),
        args.causal_url.clone(),
    ));
    let addr = format!("{}:{}", args.host, args.port);
    let addr = addr.parse().unwrap();

    let make_svc = make_service_fn(move |_| {
        let state = state.clone();
        async move { Ok::<_, hyper::Error>(service_fn(move |req| handle_request(req, state.clone()))) }
    });

    let server = Server::bind(&addr).serve(make_svc);

    info!("Observer listening on http://{}", addr);
    info!("  /metrics   - Prometheus metrics");
    info!("  /dashboard - Visualization dashboard");
    info!("  /api/systemtenders/<uuid>/trials - Trial history");

    if let Err(e) = server.await {
        error!("Server error: {}", e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    // ── the std-only listener, fifth of its name: scripted answers,
    // recorded requests, 404 when dry ──

    struct FakeUpstream {
        port: u16,
        log: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeUpstream {
        fn start(script: Vec<(u16, String)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let log = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let queue = Arc::new(Mutex::new(
                script
                    .into_iter()
                    .collect::<std::collections::VecDeque<_>>(),
            ));
            let (q2, l2, s2) = (Arc::clone(&queue), Arc::clone(&log), Arc::clone(&stop));
            let handle = std::thread::spawn(move || {
                while !s2.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                                .ok();
                            let mut buf = Vec::new();
                            let mut chunk = [0u8; 4096];
                            loop {
                                match stream.read(&mut chunk) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        buf.extend_from_slice(&chunk[..n]);
                                        let head = String::from_utf8_lossy(&buf).to_string();
                                        if head.find("\r\n\r\n").is_some() {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            let head = String::from_utf8_lossy(&buf).to_string();
                            let req_line =
                                head.split("\r\n").next().unwrap_or_default().to_string();
                            l2.lock().unwrap().push(req_line);
                            let (status, body) = q2
                                .lock()
                                .unwrap()
                                .pop_front()
                                .unwrap_or((404, String::new()));
                            let resp = format!(
                                "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                status,
                                body.len(),
                                body
                            );
                            stream.write_all(resp.as_bytes()).ok();
                            stream.flush().ok();
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                port,
                log,
                stop,
                thread: Some(handle),
            }
        }

        fn url(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }

        fn requests(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    impl Drop for FakeUpstream {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(h) = self.thread.take() {
                let _ = h.join();
            }
        }
    }

    fn ok(body: &str) -> (u16, String) {
        (200, body.to_string())
    }

    async fn body_text(resp: Response<Body>) -> (StatusCode, String, Option<String>) {
        let status = resp.status();
        let ctype = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string(), ctype)
    }

    fn get(path: &str) -> Request<Body> {
        Request::builder().uri(path).body(Body::empty()).unwrap()
    }

    fn state(push: &str, api: &str, causal: &str) -> Arc<ObserverState> {
        // reqwest's blocking client handshakes with its own internal
        // runtime at build; constructing it on a plain dedicated
        // thread keeps every async context out of the handshake
        let (push, api, causal) = (push.to_string(), api.to_string(), causal.to_string());
        std::thread::spawn(move || Arc::new(ObserverState::new(push, api, causal)))
            .join()
            .unwrap()
    }

    // ── the static doors, no upstreams needed ──

    #[tokio::test]
    async fn the_static_doors_answer_their_content_types() {
        let s = state(
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
        );
        let (status, body, ctype) =
            body_text(handle_request(get("/dashboard"), s.clone()).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(ctype.unwrap().starts_with("text/html"));
        assert!(!body.is_empty());

        let (status, body, ctype) =
            body_text(handle_request(get("/d3.js"), s.clone()).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(ctype.unwrap().starts_with("application/javascript"));
        assert!(!body.is_empty());
    }

    // ── the proxied and degraded doors: one env window ──

    #[test]
    fn the_wire_doors_speak_their_contracts() {
        // a dead DB: every DB touch degrades, never hangs
        std::env::set_var("GODON_ARCHIVE_DB_SERVICE_HOST", "127.0.0.1");
        std::env::set_var("GODON_ARCHIVE_DB_SERVICE_PORT", "1");

        // /metrics: the push gateway's text rides through verbatim
        let push = FakeUpstream::start(vec![ok("observer_up 1\n")]);
        let causal = FakeUpstream::start(vec![]);
        let s = state(&push.url(), "http://127.0.0.1:1", &causal.url());
        let (status, body, ctype) = futures::executor::block_on(async {
            body_text(handle_request(get("/metrics"), s.clone()).await.unwrap()).await
        });
        assert_eq!(status, StatusCode::OK);
        assert!(ctype.unwrap().starts_with("text/plain"));
        // the pushed text rides through inside the observer's own
        // prometheus envelope
        assert!(body.contains("observer_up 1\n"), "got: {body}");
        assert_eq!(push.requests(), vec!["GET /metrics HTTP/1.1".to_string()]);

        // (the /health and dark-fallback doors mix blocking-reqwest with
        // tokio-postgres inside one async handler — no unit executor
        // drives both; integration territory, see #416)

        // /api/connectome: causal's answer proxied as JSON
        let causal = FakeUpstream::start(vec![ok(r#"{"nodes": 7}"#)]);
        let s = state("http://127.0.0.1:1", "http://127.0.0.1:1", &causal.url());
        let (status, body, ctype) = futures::executor::block_on(async {
            body_text(
                handle_request(get("/api/connectome"), s.clone())
                    .await
                    .unwrap(),
            )
            .await
        });
        assert_eq!(status, StatusCode::OK);
        assert!(ctype.unwrap().starts_with("application/json"));
        assert_eq!(body, r#"{"nodes": 7}"#);
        assert_eq!(causal.requests(), vec!["GET /graph HTTP/1.1".to_string()]);

        // causal refuses: the refusal is named, status carried
        let causal = FakeUpstream::start(vec![(500, String::new())]);
        let s = state("http://127.0.0.1:1", "http://127.0.0.1:1", &causal.url());
        let (status, body, _) = futures::executor::block_on(async {
            body_text(
                handle_request(get("/api/connectome"), s.clone())
                    .await
                    .unwrap(),
            )
            .await
        });
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("causal returned 500"), "got: {body}");

        std::env::remove_var("GODON_ARCHIVE_DB_SERVICE_HOST");
        std::env::remove_var("GODON_ARCHIVE_DB_SERVICE_PORT");
    }
}
