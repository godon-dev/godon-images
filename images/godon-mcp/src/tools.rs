use crate::client::GodonClient;
use anyhow::{bail, Result};
use log::info;
use serde_json::Value;

#[derive(Clone)]
pub struct ToolRegistry {
    client: GodonClient,
    causal_client: GodonClient,
    tools: Vec<ToolDef>,
}

#[derive(Clone)]
struct ToolDef {
    name: &'static str,
    description: &'static str,
    input_schema: Value,
}

impl ToolRegistry {
    pub fn new(client: GodonClient, causal_client: GodonClient) -> Self {
        let tools = vec![
            ToolDef {
                name: "systemtender_list",
                description: "List all optimization systemtenders with their current status (active, stopped, error).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "systemtender_get",
                description: "Get detailed information about a specific systemtender including its full configuration, status, and creation time.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "systemtender_id": { "type": "string", "description": "UUID of the systemtender" }
                    },
                    "required": ["systemtender_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "systemtender_create",
                description: "Create and start a new optimization systemtender. Requires a name and a godon config object (v0.3 schema with systemtender type, settings, objectives, effectuation, reconnaissance, and optional guardrails).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Human-readable name for this optimization run" },
                        "config": { "type": "object", "description": "Godon systemtender configuration (v0.3). Must include: meta.configVersion, systemtender.type, settings, objectives, effectuation. May include: guardrails, rollback_strategies, run, cooperation." }
                    },
                    "required": ["name", "config"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "systemtender_stop",
                description: "Gracefully stop a running systemtender. Workers complete their current trial before stopping.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "systemtender_id": { "type": "string", "description": "UUID of the systemtender to stop" }
                    },
                    "required": ["systemtender_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "systemtender_start",
                description: "Resume a previously stopped systemtender. Continues from existing trial history.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "systemtender_id": { "type": "string", "description": "UUID of the systemtender to resume" }
                    },
                    "required": ["systemtender_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "systemtender_delete",
                description: "Delete a systemtender and all its data (trial history, archive database). Use force=true to cancel running workers immediately.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "systemtender_id": { "type": "string", "description": "UUID of the systemtender to delete" },
                        "force": { "type": "boolean", "default": false, "description": "Force deletion even if workers are running" }
                    },
                    "required": ["systemtender_id"],
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
                description: "Register a new credential (SSH key, API token, database connection, HTTP basic auth) for systemtenders to authenticate against target systems.",
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
                name: "target_list",
                description: "List all registered target systems (servers, APIs) that systemtenders can optimize against.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "target_create",
                description: "Register a new target system (SSH server or HTTP API) for systemtenders to optimize against.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Unique name for this target" },
                        "target_type": { "type": "string", "enum": ["ssh", "http"], "description": "Type of target system" },
                        "spec": { "type": "object", "description": "Type-specific config. SSH: {address, username, ssh_key_variable_path}. HTTP: {url, auth_type}" },
                        "metadata": { "type": "object", "description": "Optional metadata for the target" }
                    },
                    "required": ["name", "target_type", "spec"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "target_get",
                description: "Get details of a specific target system by ID.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "target_id": { "type": "string", "description": "UUID of the target" }
                    },
                    "required": ["target_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "target_delete",
                description: "Delete a registered target system.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "target_id": { "type": "string", "description": "UUID of the target to delete" }
                    },
                    "required": ["target_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "steerwish_list",
                description: "List all declared steerwishes with their derived lifecycle state (declared, planned, refused, acted, landed, missed, re_opened, closed).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "steerwish_declare",
                description: "Declare a steerwish: a named measured outcome the system should bring into a band and hold. The map plans the input setting; refusals name their binding constraint. Omitted budget means upkeep indefinitely; omitted regime means standing. Judging is in/out of band only - the target is receipt and reporting.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "outcome": { "type": "string", "description": "Plain name of the measured value this wish is about - must resolve to exactly one entry in the map's outcome registry, e.g. 'chainend.shift'" },
                        "band": {
                            "type": "object",
                            "description": "Acceptable range in the outcome's measurement units",
                            "properties": {
                                "lo": { "type": "number", "description": "Lower edge of the acceptable band" },
                                "hi": { "type": "number", "description": "Upper edge of the acceptable band" },
                                "target": { "type": "number", "description": "Aim point inside the band - receipt and reporting only, never judged" }
                            },
                            "required": ["lo", "hi"]
                        },
                        "limits": {
                            "type": "object",
                            "description": "Guardrails checked at plan time; a refusal names the binding one",
                            "properties": {
                                "exclude": { "type": "array", "items": { "type": "string" }, "description": "Param names that may not be moved at all" },
                                "maxChange": { "type": "number", "description": "No input may end further from its neutral point than this fraction of its own range" }
                            }
                        },
                        "budget": { "type": "integer", "description": "Re-act allowance after drift events; omitted means upkeep indefinitely" },
                        "regime": { "type": "string", "enum": ["standing"], "description": "Closing rule of the wish; only standing exists today (held until closed)" }
                    },
                    "required": ["outcome", "band"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "steerwish_get",
                description: "Get one steerwish with its full event history (declared, planned, refused, acted, landed, missed, re_opened, closed).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "wish_id": { "type": "string", "description": "UUID of the steerwish" }
                    },
                    "required": ["wish_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "steerwish_close",
                description: "Close a steerwish: append the 'closed' event and release the hold (idempotent on already-closed wishes).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "wish_id": { "type": "string", "description": "UUID of the steerwish to close" }
                    },
                    "required": ["wish_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "connectome_get",
                description: "Get the live map: every node and characterized edge the system currently believes in, with fitted response, confidence, and noise floor. The map is partial by design - curves exist only where the system has probed - and always aging: check freshness before trusting.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "connectome_curves",
                description: "Get the measured response curves: per edge, the probe levels with measured shifts and honest error bars. Curves exist only where the system has actually probed.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "connectome_artifact",
                description: "Get the exported map artifact: the full connectome with curves and metadata, as persisted at last build.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "connectome_predict",
                description: "Predict the one-hop shift at a receiver for a push magnitude on a sender, from the measured map (linearized). Reads never touch the system.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sender_id": { "type": "string", "description": "The node to push" },
                        "impulse_scale": { "type": "number", "description": "Push magnitude" }
                    },
                    "required": ["sender_id", "impulse_scale"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "connectome_predict_multihop",
                description: "Predict the composed cascade shift along a measured path (P3 composition, linearized). Reads never touch the system.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sender_id": { "type": "string", "description": "The node to push" },
                        "impulse_scale": { "type": "number", "description": "Push magnitude" }
                    },
                    "required": ["sender_id", "impulse_scale"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "connectome_impact",
                description: "What has a given systemtender's probing moved: the measured impact of its pushes across the map.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "systemtender_id": { "type": "string", "description": "UUID of the systemtender" }
                    },
                    "required": ["systemtender_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "connectome_causes",
                description: "What feeds a given systemtender's nodes: the measured causes upstream of its patch of the map.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "systemtender_id": { "type": "string", "description": "UUID of the systemtender" }
                    },
                    "required": ["systemtender_id"],
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

        Self { client, causal_client, tools }
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
            .get("systemtender_id")
            .or_else(|| args.get("credential_id"))
            .or_else(|| args.get("target_id"))
            .or_else(|| args.get("wish_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match name {
            "systemtender_list" => self.client.get("/systemtenders").await,
            "systemtender_get" => {
                require_id(id, "systemtender_id")?;
                self.client
                    .get(&format!("/systemtenders/{}", urlencoding::encode(id)))
                    .await
            }
            "systemtender_create" => {
                let name_val = args["name"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("name required"))?;
                let config = args
                    .get("config")
                    .ok_or_else(|| anyhow::anyhow!("config object required"))?;
                let body = serde_json::json!({ "name": name_val, "config": config });
                self.client.post("/systemtenders", body).await
            }
            "systemtender_stop" => {
                require_id(id, "systemtender_id")?;
                self.client
                    .post_empty(&format!("/systemtenders/{}/stop", urlencoding::encode(id)))
                    .await
            }
            "systemtender_start" => {
                require_id(id, "systemtender_id")?;
                self.client
                    .post_empty(&format!("/systemtenders/{}/start", urlencoding::encode(id)))
                    .await
            }
            "systemtender_delete" => {
                require_id(id, "systemtender_id")?;
                let force = args["force"].as_bool().unwrap_or(false);
                let path = if force {
                    format!("/systemtenders/{}?force=true", urlencoding::encode(id))
                } else {
                    format!("/systemtenders/{}", urlencoding::encode(id))
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
            "target_list" => self.client.get("/targets").await,
            "target_get" => {
                require_id(id, "target_id")?;
                self.client
                    .get(&format!("/targets/{}", urlencoding::encode(id)))
                    .await
            }
            "target_create" => self.client.post("/targets", args).await,
            "target_delete" => {
                require_id(id, "target_id")?;
                self.client
                    .delete(&format!("/targets/{}", urlencoding::encode(id)))
                    .await
            }
            "steerwish_list" => self.client.get("/steerwishes").await,
            "steerwish_get" => {
                require_id(id, "wish_id")?;
                self.client
                    .get(&format!(
                        "/steerwishes/{}",
                        urlencoding::encode(id)
                    ))
                    .await
            }
            "steerwish_declare" => {
                if args.get("outcome").and_then(|v| v.as_str()).is_none() {
                    bail!("outcome required: the plain name of the measured value");
                }
                if args.get("band").is_none() {
                    bail!("band required: an object with lo and hi, in the outcome's measurement units");
                }
                let mut body = serde_json::json!({
                    "outcome": args["outcome"],
                    "band": args["band"],
                });
                if let Some(limits) = args.get("limits") {
                    body["limits"] = limits.clone();
                }
                if let Some(budget) = args.get("budget") {
                    body["budget"] = budget.clone();
                }
                if let Some(regime) = args.get("regime") {
                    body["regime"] = regime.clone();
                }
                self.client.post("/steerwishes", body).await
            }
            "steerwish_close" => {
                require_id(id, "wish_id")?;
                self.client
                    .post_empty(&format!(
                        "/steerwishes/{}/close",
                        urlencoding::encode(id)
                    ))
                    .await
            }
            "connectome_get" => self.causal_client.get("/graph").await,
            "connectome_curves" => self.causal_client.get("/curves").await,
            "connectome_artifact" => self.causal_client.get("/artifact").await,
            "connectome_predict" => {
                let sender = args["sender_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("sender_id required"))?;
                let scale = args["impulse_scale"].as_f64().unwrap_or(1.0);
                self.causal_client
                    .post(
                        "/predict",
                        serde_json::json!({ "sender_id": sender, "impulse_scale": scale }),
                    )
                    .await
            }
            "connectome_predict_multihop" => {
                let sender = args["sender_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("sender_id required"))?;
                let scale = args["impulse_scale"].as_f64().unwrap_or(1.0);
                self.causal_client
                    .post(
                        "/predict/multihop",
                        serde_json::json!({ "sender_id": sender, "impulse_scale": scale }),
                    )
                    .await
            }
            "connectome_impact" => {
                require_id(id, "systemtender_id")?;
                self.causal_client
                    .get(&format!(
                        "/impact/{}",
                        urlencoding::encode(id)
                    ))
                    .await
            }
            "connectome_causes" => {
                require_id(id, "systemtender_id")?;
                self.causal_client
                    .get(&format!(
                        "/causes/{}",
                        urlencoding::encode(id)
                    ))
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
