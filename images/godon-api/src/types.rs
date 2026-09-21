use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemtenderSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Systemtender {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemtenderCreate {
    pub name: String,
    pub config: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemtenderUpdate {
    pub config: serde_json::Value,
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: String,
    pub name: String,
    #[serde(rename = "credentialType")]
    pub credential_type: String,
    pub description: Option<String>,
    #[serde(rename = "windmillVariable")]
    pub windmill_variable: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "lastUsedAt", skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialCreate {
    pub name: String,
    #[serde(rename = "credentialType")]
    pub credential_type: String,
    pub description: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Target {
    pub id: String,
    pub name: String,
    #[serde(rename = "targetType")]
    pub target_type: String,
    pub spec: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "lastUsedAt", skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetCreate {
    pub name: String,
    #[serde(rename = "targetType")]
    pub target_type: String,
    pub spec: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub message: String,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteResponse {
    pub id: String,
    pub deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

impl ErrorResponse {
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            details: None,
        }
    }

    pub fn with_details(message: impl Into<String>, code: impl Into<String>, details: serde_json::Value) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            details: Some(details),
        }
    }
}

// ─── Steerwish ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteerwishEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(rename = "at")]
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The wish as the API carries it: a lifecycle envelope plus the body
/// verbatim. The envelope (id, state, timestamps, events) is the front
/// door's own concern - addressing and the book. The body is the
/// controller's object (wish-shape freedom, 2026-09-21): whatever
/// grammar the door validates - today outcome+band, later claims and
/// terms - passes through unshaped, in both directions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Steerwish {
    pub id: String,
    pub state: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<SteerwishEvent>,
    #[serde(flatten)]
    pub body: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SteerwishSummary {
    pub id: String,
    pub state: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(flatten)]
    pub body: serde_json::Map<String, serde_json::Value>,
}
