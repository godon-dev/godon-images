use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;
use serde_json::json;

use crate::config::Config;
use crate::types::{Breeder, BreederCreate, BreederSummary, Credential, CredentialCreate, DeleteResponse, ErrorResponse};
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

pub async fn list_breeders(
    State(_config): State<Config>,
) -> Result<Json<Vec<BreederSummary>>, (StatusCode, Json<ErrorResponse>)> {
    let client = get_client()?;
    
    tokio::task::spawn_blocking(move || {
        client.list_breeders()
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to retrieve breeders: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn create_breeder(
    State(_config): State<Config>,
    Json(payload): Json<BreederCreate>,
) -> Result<(StatusCode, Json<BreederSummary>), (StatusCode, Json<ErrorResponse>)> {
    let client = get_client()?;
    
    let breeder_config = json!({
        "name": payload.name,
        "config": payload.config
    });
    
    tokio::task::spawn_blocking(move || {
        client.create_breeder(breeder_config)
            .map(|b| (StatusCode::CREATED, Json(b)))
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to create breeder: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn get_breeder(
    State(_config): State<Config>,
    Path(id): Path<String>,
) -> Result<Json<Breeder>, (StatusCode, Json<ErrorResponse>)> {
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
        client.get_breeder(&id)
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to retrieve breeder: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn update_breeder() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(ErrorResponse::new(
            "Update breeder functionality not implemented",
            "NOT_IMPLEMENTED"
        ))
    )
}

#[derive(Debug, Deserialize)]
pub struct DeleteParams {
    #[serde(default)]
    force: Option<String>,
}

pub async fn delete_breeder(
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
        client.delete_breeder(&id_clone, force)
            .map(|_| Json(DeleteResponse {
                id: id_clone.clone(),
                deleted: true,
                force: Some(force),
            }))
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to delete breeder: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn stop_breeder(
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
        client.stop_breeder(&id)
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to stop breeder: {}", e),
                    "INTERNAL_SERVER_ERROR"
                ))
            ))
    }).await.map_err(|e| (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse::new(format!("Task join error: {}", e), "INTERNAL_SERVER_ERROR"))
    ))?
}

pub async fn start_breeder(
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
        client.start_breeder(&id)
            .map(Json)
            .map_err(|e| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Failed to start breeder: {}", e),
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
