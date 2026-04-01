use crate::client::GodonClient;
use anyhow::{bail, Result};
use log::info;
use serde_json::Value;

#[derive(Clone)]
pub struct ToolRegistry {
    client: GodonClient,
    tools: Vec<ToolDef>,
}

#[derive(Clone)]
struct ToolDef {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
}

impl ToolRegistry {
    pub fn new(client: GodonClient) -> Self {
        let tools = vec![
            ToolDef {
                name: "breeder_list",
                description: "List all optimization breeders with their current status (active, stopped, error).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "breeder_get",
                description: "Get detailed information about a specific breeder including its full configuration, status, and creation time.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "breeder_id": { "type": "string", "description": "UUID of the breeder" }
                    },
                    "required": ["breeder_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "breeder_create",
                description: "Create and start a new optimization breeder. Requires a name and a godon config object (v0.3 schema with breeder type, settings, objectives, effectuation, reconnaissance, and optional guardrails).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Human-readable name for this optimization run" },
                        "config": { "type": "object", "description": "Godon breeder configuration (v0.3). Must include: meta.configVersion, breeder.type, settings, objectives, effectuation. May include: guardrails, rollback_strategies, run, cooperation." }
                    },
                    "required": ["name", "config"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "breeder_stop",
                description: "Gracefully stop a running breeder. Workers complete their current trial before stopping.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "breeder_id": { "type": "string", "description": "UUID of the breeder to stop" }
                    },
                    "required": ["breeder_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "breeder_start",
                description: "Resume a previously stopped breeder. Continues from existing trial history.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "breeder_id": { "type": "string", "description": "UUID of the breeder to resume" }
                    },
                    "required": ["breeder_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "breeder_delete",
                description: "Delete a breeder and all its data (trial history, archive database). Use force=true to cancel running workers immediately.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "breeder_id": { "type": "string", "description": "UUID of the breeder to delete" },
                        "force": { "type": "boolean", "default": false, "description": "Force deletion even if workers are running" }
                    },
                    "required": ["breeder_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "credential_list",
                description: "List all stored credentials (SSH keys, API tokens, etc.) registered with godon.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "credential_create",
                description: "Register a new credential (SSH key, API token, database connection, HTTP basic auth) for breeders to authenticate against target systems.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique name for this credential" },
                        "credential_type": { "type": "string", "enum": ["ssh_private_key", "api_token", "database_connection", "http_basic_auth"] },
                        "content": { "type": "string", "description": "The credential content (SSH key, token value, etc.)" },
                        "description": { "type": "string", "description": "Human-readable description" }
                    },
                    "required": ["name", "credential_type"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "credential_get",
                description: "Get details of a specific credential by ID.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "credential_id": { "type": "string", "description": "UUID of the credential" }
                    },
                    "required": ["credential_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "credential_delete",
                description: "Delete a stored credential.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "credential_id": { "type": "string", "description": "UUID of the credential to delete" }
                    },
                    "required": ["credential_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "health",
                description: "Check the health of the godon platform.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
        ];

        Self { client, tools }
    }

    pub fn list_tools(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.input_schema
                })
            })
            .collect()
    }

    pub async fn call_tool(&self, name: &str, args: Value) -> Result<Value> {
        info!("Tool call: {} args={}", name, args);
        let id = args
            .get("breeder_id")
            .or_else(|| args.get("credential_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match name {
            "breeder_list" => self.client.get("/breeders").await,
            "breeder_get" => {
                require_id(id, "breeder_id")?;
                self.client
                    .get(&format!("/breeders/{}", urlencoding::encode(id)))
                    .await
            }
            "breeder_create" => {
                let name_val = args["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("name required"))?;
                let config = args
                    .get("config")
                    .ok_or_else(|| anyhow::anyhow!("config object required"))?;
                let body = serde_json::json!({ "name": name_val, "config": config });
                self.client.post("/breeders", body).await
            }
            "breeder_stop" => {
                require_id(id, "breeder_id")?;
                self.client
                    .post_empty(&format!("/breeders/{}/stop", urlencoding::encode(id)))
                    .await
            }
            "breeder_start" => {
                require_id(id, "breeder_id")?;
                self.client
                    .post_empty(&format!("/breeders/{}/start", urlencoding::encode(id)))
                    .await
            }
            "breeder_delete" => {
                require_id(id, "breeder_id")?;
                let force = args["force"].as_bool().unwrap_or(false);
                let path = if force {
                    format!("/breeders/{}?force=true", urlencoding::encode(id))
                } else {
                    format!("/breeders/{}", urlencoding::encode(id))
                };
                self.client.delete(&path).await
            }
            "credential_list" => self.client.get("/credentials").await,
            "credential_get" => {
                require_id(id, "credential_id")?;
                self.client
                    .get(&format!("/credentials/{}", urlencoding::encode(id)))
                    .await
            }
            "credential_create" => self.client.post("/credentials", args).await,
            "credential_delete" => {
                require_id(id, "credential_id")?;
                self.client
                    .delete(&format!("/credentials/{}", urlencoding::encode(id)))
                    .await
            }
            "health" => self.client.get("/health").await,
            _ => bail!("Unknown tool: {}", name),
        }
    }
}

fn require_id(id: &str, field: &str) -> Result<()> {
    if id.is_empty() {
        bail!("{} is required", field);
    }
    Ok(())
}
