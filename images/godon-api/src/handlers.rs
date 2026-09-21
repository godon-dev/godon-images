use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use serde_json::Value;

use crate::config::Config;
use crate::types::{
    Steerwish, SteerwishSummary, Systemtender, SystemtenderCreate,
    SystemtenderSummary, SystemtenderUpdate, Credential, CredentialCreate, DeleteResponse,
    ErrorResponse, Target, TargetCreate,
};
use crate::windmill_adapter::WindmillClient;

static BUILD_VERSION: &str = match option_env!("BUILD_VERSION") {
    Some(v) => v,
    None => "dev",
};

static UUID_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").unwrap()
});

static NAME_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap()
});

fn get_client() -> Result<WindmillClient, (StatusCode, Json<ErrorResponse>)> {
    WindmillClient::new().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new(
                format!("Failed to initialize Windmill client: {}", e),
                "INTERNAL_SERVER_ERROR"
            ))
        )
    })
}

pub async fn root() -> Json<serde_json::Value> {
    Json(json!({"message": "Godon API is running"}))
}

pub async fn health() -> Json<serde_json::Value> {
    Json(json!({"status": "healthy", "service": "godon-api", "version": BUILD_VERSION}))
}

pub async fn list_systemtenders(
    State(_config): State<Config>,
) -> Result<Json<Vec<SystemtenderSummary>>, (StatusCode, Json<ErrorResponse>)> {
    let client = get_client()?;
    
    tokio::task::spawn_blocking(move || {
        client.list_systemtenders()
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to retrieve systemtenders: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn create_systemtender(
    State(_config): State<Config>,
    Json(payload): Json<SystemtenderCreate>,
) -> Result<(StatusCode, Json<SystemtenderSummary>), (StatusCode, Json<ErrorResponse>)> {
    let client = get_client()?;
    
    let systemtender_config = json!({
        "name": payload.name,
        "config": payload.config
    });
    
    tokio::task::spawn_blocking(move || {
        client.create_systemtender(systemtender_config)
            .map(|b| (StatusCode::CREATED, Json(b)))
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to create systemtender: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn get_systemtender(
    State(_config): State<Config>,
    Path(id): Path<String>,
) -> Result<Json<Systemtender>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"uuid": id})
            ))
        ));
    }
    
    let client = get_client()?;
    
    tokio::task::spawn_blocking(move || {
        client.get_systemtender(&id)
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to retrieve systemtender: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn update_systemtender(
    State(_config): State<Config>,
    Path(id): Path<String>,
    Json(payload): Json<SystemtenderUpdate>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"uuid": id})
            ))
        ));
    }

    if payload.config.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "Invalid config: config must be a non-empty object",
                "BAD_REQUEST"
            ))
        ));
    }

    let client = get_client()?;
    let force = payload.force.unwrap_or(false);

    tokio::task::spawn_blocking(move || {
        client.update_systemtender(&id, payload.config, force)
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to update systemtender: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

#[derive(Debug, Deserialize)]
pub struct DeleteParams {
    #[serde(default)]
    force: Option<String>,
}

pub async fn delete_systemtender(
    State(_config): State<Config>,
    Path(id): Path<String>,
    Query(params): Query<DeleteParams>,
) -> Result<Json<DeleteResponse>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"uuid": id})
            ))
        ));
    }

    let force = params.force
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    let client = get_client()?;
    let id_clone = id.clone();
    
    tokio::task::spawn_blocking(move || {
        client.delete_systemtender(&id_clone, force)
            .map(|_| Json(DeleteResponse {
                id: id_clone.clone(),
                deleted: true,
                force: Some(force),
            }))
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to delete systemtender: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn stop_systemtender(
    State(_config): State<Config>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"uuid": id})
            ))
        ));
    }

    let client = get_client()?;
    
    tokio::task::spawn_blocking(move || {
        client.stop_systemtender(&id)
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to stop systemtender: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn start_systemtender(
    State(_config): State<Config>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"uuid": id})
            ))
        ));
    }

    let client = get_client()?;
    
    tokio::task::spawn_blocking(move || {
        client.start_systemtender(&id)
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to start systemtender: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn list_credentials(
    State(_config): State<Config>,
) -> Result<Json<Vec<Credential>>, (StatusCode, Json<ErrorResponse>)> {
    let client = get_client()?;
    
    tokio::task::spawn_blocking(move || {
        client.list_credentials()
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to retrieve credentials: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn create_credential(
    State(_config): State<Config>,
    Json(payload): Json<CredentialCreate>,
) -> Result<(StatusCode, Json<Credential>), (StatusCode, Json<ErrorResponse>)> {
    let valid_types = ["ssh_private_key", "api_token", "database_connection", "http_basic_auth"];
    if !valid_types.contains(&payload.credential_type.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                format!("Invalid credentialType: '{}'. Must be one of: {}", payload.credential_type, valid_types.join(", ")),
                "BAD_REQUEST",
                json!({"credentialType": payload.credential_type})
            ))
        ));
    }

    if !NAME_REGEX.is_match(&payload.name) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                format!("Invalid name format: '{}'. Use only alphanumeric characters, hyphens, and underscores", payload.name),
                "BAD_REQUEST",
                json!({"name": payload.name})
            ))
        ));
    }

    if payload.content.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "Invalid content: content cannot be empty",
                "BAD_REQUEST"
            ))
        ));
    }

    let credential_data = json!({
        "name": payload.name,
        "credentialType": payload.credential_type,
        "description": payload.description.as_deref().unwrap_or(""),
        "content": payload.content,
    });

    let client = get_client()?;
    
    tokio::task::spawn_blocking(move || {
        client.create_credential(credential_data)
            .map(|c| (StatusCode::CREATED, Json(c)))
            .map_err(|e| {
                let error_msg = e.to_string().to_lowercase();
                if error_msg.contains("already exists") || error_msg.contains("400") {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(ErrorResponse::new(
                            "Credential with this name already exists",
                            "BAD_REQUEST"
                        ))
                    )
                } else {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorResponse::new(
                            format!("Failed to create credential: {}", e),
                            "INTERNAL_SERVER_ERROR"
                        ))
                    )
                }
            })
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn get_credential(
    State(_config): State<Config>,
    Path(id): Path<String>,
) -> Result<Json<Credential>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"credential_id": id})
            ))
        ));
    }

    let client = get_client()?;
    
    tokio::task::spawn_blocking(move || {
        client.get_credential(&id)
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to retrieve credential: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn delete_credential(
    State(_config): State<Config>,
    Path(id): Path<String>,
) -> Result<Json<DeleteResponse>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"credential_id": id})
            ))
        ));
    }

    let client = get_client()?;
    let id_clone = id.clone();
    
    tokio::task::spawn_blocking(move || {
        client.delete_credential(&id_clone)
            .map(|_| Json(DeleteResponse {
                id: id_clone.clone(),
                deleted: true,
                force: None,
            }))
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to delete credential: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn list_targets(
    State(_config): State<Config>,
) -> Result<Json<Vec<Target>>, (StatusCode, Json<ErrorResponse>)> {
    let client = get_client()?;

    tokio::task::spawn_blocking(move || {
        client.list_targets()
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to retrieve targets: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn create_target(
    State(_config): State<Config>,
    Json(payload): Json<TargetCreate>,
) -> Result<(StatusCode, Json<Target>), (StatusCode, Json<ErrorResponse>)> {
    let valid_types = ["ssh", "http"];
    if !valid_types.contains(&payload.target_type.as_str()) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                format!("Invalid targetType: '{}'. Must be one of: {}", payload.target_type, valid_types.join(", ")),
                "BAD_REQUEST",
                json!({"targetType": payload.target_type})
            ))
        ));
    }

    if !NAME_REGEX.is_match(&payload.name) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                format!("Invalid name format: '{}'. Use only alphanumeric characters, hyphens, and underscores", payload.name),
                "BAD_REQUEST",
                json!({"name": payload.name})
            ))
        ));
    }

    if payload.spec.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "Invalid spec: spec must be a non-empty object",
                "BAD_REQUEST"
            ))
        ));
    }

    let target_data = json!({
        "name": payload.name,
        "targetType": payload.target_type,
        "spec": payload.spec,
        "metadata": payload.metadata,
    });

    let client = get_client()?;

    tokio::task::spawn_blocking(move || {
        client.create_target(target_data)
            .map(|t| (StatusCode::CREATED, Json(t)))
            .map_err(|e| {
                let error_msg = e.to_string().to_lowercase();
                if error_msg.contains("already exists") || error_msg.contains("400") {
                    (
                        StatusCode::BAD_REQUEST,
                        Json(ErrorResponse::new(
                            "Target with this name already exists",
                            "BAD_REQUEST"
                        ))
                    )
                } else {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(ErrorResponse::new(
                            format!("Failed to create target: {}", e),
                            "INTERNAL_SERVER_ERROR"
                        ))
                    )
                }
            })
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn get_target(
    State(_config): State<Config>,
    Path(id): Path<String>,
) -> Result<Json<Target>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"target_id": id})
            ))
        ));
    }

    let client = get_client()?;

    tokio::task::spawn_blocking(move || {
        client.get_target(&id)
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to retrieve target: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn delete_target(
    State(_config): State<Config>,
    Path(id): Path<String>,
) -> Result<Json<DeleteResponse>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"target_id": id})
            ))
        ));
    }

    let client = get_client()?;
    let id_clone = id.clone();

    tokio::task::spawn_blocking(move || {
        client.delete_target(&id_clone)
            .map(|_| Json(DeleteResponse {
                id: id_clone.clone(),
                deleted: true,
                force: None,
            }))
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to delete target: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

// ─── Steerwishes ────────────────────────────────────────────────────
// Wish-shape freedom (ruled 2026-09-19, the wish-stack note): the body
// is the declarer's own; the controller owns validation (the door).
// The API forwards it verbatim - transport hygiene only.

pub async fn list_steerwishes(
    State(_config): State<Config>,
) -> Result<Json<Vec<SteerwishSummary>>, (StatusCode, Json<ErrorResponse>)> {
    let client = get_client()?;

    tokio::task::spawn_blocking(move || {
        client.list_steerwishes()
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to retrieve steerwishes: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn declare_steerwish(
    State(_config): State<Config>,
    Json(payload): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<Steerwish>), (StatusCode, Json<ErrorResponse>)> {
    // The wish body is not shaped here: the controller validates (the
    // door), the same arrangement as systemtender configs. Only transport
    // hygiene: a JSON object, so the declaration is addressable at all.
    if !payload.is_object() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "The wish body must be a JSON object; its grammar is validated at the door",
                "BAD_REQUEST"
            ))
        ));
    }

    let client = get_client()?;

    tokio::task::spawn_blocking(move || {
        client.create_steerwish(payload)
            .map(|w| (StatusCode::CREATED, Json(w)))
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to declare steerwish: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn get_steerwish(
    State(_config): State<Config>,
    Path(id): Path<String>,
) -> Result<Json<Steerwish>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"wish_id": id})
            ))
        ));
    }

    let client = get_client()?;

    tokio::task::spawn_blocking(move || {
        client.get_steerwish(&id)
            .map(Json)
            .map_err(|e| {
                let not_found = format!("{}", e).to_lowercase().contains("not found");
                (
                    if not_found { StatusCode::NOT_FOUND } else { StatusCode::INTERNAL_SERVER_ERROR },
                    Json(ErrorResponse::new(
                        format!("Failed to retrieve steerwish: {}", e),
                        if not_found { "NOT_FOUND" } else { "INTERNAL_SERVER_ERROR" }
                    ))
                )
            })
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn close_steerwish(
    State(_config): State<Config>,
    Path(id): Path<String>,
) -> Result<Json<Steerwish>, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&id) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::with_details(
                "Invalid UUID format",
                "BAD_REQUEST",
                json!({"wish_id": id})
            ))
        ));
    }

    let client = get_client()?;

    tokio::task::spawn_blocking(move || {
        client.close_steerwish(&id)
            .map(Json)
            .map_err(|e| {
                let not_found = format!("{}", e).to_lowercase().contains("not found");
                (
                    if not_found { StatusCode::NOT_FOUND } else { StatusCode::INTERNAL_SERVER_ERROR },
                    Json(ErrorResponse::new(
                        format!("Failed to close steerwish: {}", e),
                        if not_found { "NOT_FOUND" } else { "INTERNAL_SERVER_ERROR" }
                    ))
                )
            })
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

// ─── The map (relays to causal

fn get_causal_client() -> Result<crate::causal::CausalClient, (StatusCode, Json<ErrorResponse>)> {
    crate::causal::CausalClient::new().map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(
            format!("Failed to initialize causal client: {}", e),
            "INTERNAL_SERVER_ERROR"
        ))
    ))
}

use axum::response::{IntoResponse, Response};

async fn causal_relay<F>(relay: F) -> Result<Response, (StatusCode, Json<ErrorResponse>)>
where
    F: FnOnce() -> Result<(reqwest::StatusCode, Value), anyhow::Error> + Send + 'static,
{
    let (status, body) = relay().map_err(|e| (
        StatusCode::BAD_GATEWAY,
        Json(ErrorResponse::new(
            format!("causal relay error: {}", e),
            "BAD_GATEWAY"
        ))
    ))?;

    if status.is_success() {
        Ok(Json(body).into_response())
    } else if status == reqwest::StatusCode::NOT_FOUND {
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new(
                body.get("message").and_then(|m| m.as_str()).unwrap_or("not found in the map"),
                "NOT_FOUND"
            )),
        ))
    } else {
        Err((
            StatusCode::BAD_GATEWAY,
            Json(ErrorResponse::new(
                format!("causal relay error ({}): {}", status, body),
                "BAD_GATEWAY"
            )),
        ))
    }
}

pub async fn get_connectome(State(_config): State<Config>) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let client = get_causal_client()?;
    causal_relay(move || client.get("/graph")).await
}

pub async fn get_connectome_curves(State(_config): State<Config>) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let client = get_causal_client()?;
    causal_relay(move || client.get("/curves")).await
}

pub async fn get_connectome_artifact(State(_config): State<Config>) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let client = get_causal_client()?;
    causal_relay(move || client.get("/artifact")).await
}

pub async fn connectome_predict(
    State(_config): State<Config>,
    Json(body): Json<Value>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    if body.get("sender_id").and_then(|v| v.as_str()).is_none() {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse::new(
            "sender_id required: the node the push lands on",
            "BAD_REQUEST"
        ))));
    }
    let client = get_causal_client()?;
    causal_relay(move || client.post("/predict", &body)).await
}

pub async fn connectome_predict_multihop(
    State(_config): State<Config>,
    Json(body): Json<Value>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    if body.get("sender_id").and_then(|v| v.as_str()).is_none() {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse::new(
            "sender_id required: the node the push lands on",
            "BAD_REQUEST"
        ))));
    }
    let client = get_causal_client()?;
    causal_relay(move || client.post("/predict/multihop", &body)).await
}

pub async fn connectome_impact(
    State(_config): State<Config>,
    Path(systemtender_id): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&systemtender_id) {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse::new(
            "Invalid UUID format", "BAD_REQUEST"
        ))));
    }
    let client = get_causal_client()?;
    causal_relay(move || client.get(&format!("/impact/{}", systemtender_id))).await
}

pub async fn connectome_causes(
    State(_config): State<Config>,
    Path(systemtender_id): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    if !UUID_REGEX.is_match(&systemtender_id) {
        return Err((StatusCode::BAD_REQUEST, Json(ErrorResponse::new(
            "Invalid UUID format", "BAD_REQUEST"
        ))));
    }
    let client = get_causal_client()?;
    causal_relay(move || client.get(&format!("/causes/{}", systemtender_id))).await
}
