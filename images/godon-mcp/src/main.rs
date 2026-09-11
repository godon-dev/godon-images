mod client;
mod config;
mod protocol;
mod tools;

use config::Config;
use log::info;
use protocol::{JsonRpcRequest, JsonRpcResponse};
use std::net::SocketAddr;
use std::sync::Arc;
use tools::ToolRegistry;

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures::stream::Stream;
use std::convert::Infallible;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::{Any, CorsLayer};

#[derive(Clone)]
struct AppState {
    registry: Arc<ToolRegistry>,
    tx: broadcast::Sender<String>,
    port: u16,
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let config = Config::from_env();
    let addr: SocketAddr = format!("0.0.0.0:{}", config.port).parse().unwrap();

    let godon_client = client::GodonClient::new(
        config.api_hostname.clone(),
        config.api_port,
        config.api_insecure,
    );
    let causal_client = client::GodonClient::new(
        config.causal_hostname.clone(),
        config.causal_port,
        config.causal_insecure,
    );
    let registry = Arc::new(ToolRegistry::new(godon_client, causal_client));
    let (tx, _) = broadcast::channel::<String>(256);

    let state = AppState {
        registry,
        tx,
        port: config.port,
    };

    let app = Router::new()
        .route("/sse", get(sse_handler))
        .route("/message", post(message_handler))
        .route("/mcp", post(mcp_handler).get(mcp_get_blocked))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state);

    info!(
        "Starting godon-mcp on {} (streamable HTTP at /mcp, legacy SSE at /sse)",
        addr
    );
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn sse_handler(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.tx.subscribe();
    let port = state.port;
    let stream = BroadcastStream::new(rx).filter_map(|msg| async move {
        match msg {
            Ok(data) => Some(Ok(Event::default().data(data))),
            Err(_) => None,
        }
    });

    let init_event = Event::default()
        .event("endpoint")
        .data(format!("http://localhost:{}/message", port));

    use futures::StreamExt;
    let init_stream = futures::stream::once(async move { Ok(init_event) });
    let combined = init_stream.chain(stream);

    Sse::new(combined).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(30)),
    )
}

async fn message_handler(
    State(state): State<AppState>,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let response = handle_request(&state.registry, request).await;
    let body = serde_json::to_string(&response).unwrap_or_default();
    let _ = state.tx.send(body);

    (StatusCode::OK, Json(response))
}

async fn handle_request(
    registry: &ToolRegistry,
    request: JsonRpcRequest,
) -> JsonRpcResponse {
    match request.method.as_str() {
        "initialize" => {
            // echo the client's requested protocol version when sane;
            // streamable-HTTP clients (2025-03-26) and legacy SSE clients
            // (2024-11-05) both land here.
            let requested = request
                .params
                .as_ref()
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or("2025-03-26");
            let result = serde_json::json!({
                "protocolVersion": requested,
                "capabilities": {
                    "tools": { "listChanged": false }
                },
                "serverInfo": {
                    "name": "godon-mcp",
                    "version": option_env!("BUILD_VERSION").unwrap_or("0.0.0")
                }
            });
            JsonRpcResponse::success(request.id, result)
        }
        "notifications/initialized" => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: Some(serde_json::Value::Null),
            error: None,
        },
        "tools/list" => {
            let tools = registry.list_tools();
            JsonRpcResponse::success(request.id, serde_json::json!({ "tools": tools }))
        }
        "tools/call" => {
            let params = request.params.unwrap_or_default();
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default()));

            match registry.call_tool(tool_name, arguments).await {
                Ok(result) => {
                    let content = serde_json::json!({
                        "content": [{
                            "type": "text",
                            "text": serde_json::to_string_pretty(&result)
                                .unwrap_or_else(|_| result.to_string())
                        }]
                    });
                    JsonRpcResponse::success(request.id, content)
                }
                Err(e) => {
                    let content = serde_json::json!({
                        "content": [{
                            "type": "text",
                            "text": format!("Error: {}", e)
                        }],
                        "isError": true
                    });
                    JsonRpcResponse::success(request.id, content)
                }
            }
        }
        "ping" => JsonRpcResponse::success(request.id, serde_json::json!({})),
        _ => JsonRpcResponse::error(
            request.id,
            -32601,
            format!("Method not found: {}", request.method),
        ),
    }
}

/// Streamable HTTP transport (MCP 2025-03-26): POST /mcp with one
/// JSON-RPC message, response as application/json. Stateless - every
/// POST stands alone; server-initiated streams are not offered, so
/// GET /mcp answers 405 (allowed by the spec for tool-only servers).
async fn mcp_handler(
    State(state): State<AppState>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    // notifications carry no id: accept them, answer 202, no body
    let is_notification = request.get("id").is_none();

    let request: JsonRpcRequest = match serde_json::from_value(request) {
        Ok(req) => req,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": serde_json::Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {}", e) }
                })),
            )
                .into_response()
        }
    };

    let response = handle_request(&state.registry, request).await;
    if is_notification {
        StatusCode::ACCEPTED.into_response()
    } else {
        Json(response).into_response()
    }
}

async fn mcp_get_blocked() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}
