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
                name: "wish_list",
                description: "List all declared wishes with their derived lifecycle state (declared, planned, assigned, unservable - a standing refusal -, acted, landed, missed, re_opened, corrected, returned, closed, plan_error).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "wish_declare",
                description: "Declare a wish. The body states its type. Steer: claims (the outcomes to hold, each with its band) and optional terms (the price the wish pays); one verdict, kept = every claim in band AND every term honored; omitted budget means upkeep indefinitely. Seek: a commissioned walk with its form (find_best or cover_now), region, criterion, budget, candidates - find_best returns the best found with its error bars and the trials spent; when the map already reads the answer, the refusal carries it. A malformed wish is rejected with the reason. Two declaration options, both optional: answer (now = the consult answers in the same breath; later = the default, answers surface as events and in status) and standard (all = the default for holds, one kept state; best = the default for walks, candidates count).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "description": "The wish body, verbatim. Two discriminators, both always stated: type picks the grammar (steer = the hold, seek = the commissioned walk); form is a field of the walk grammar only (find_best or cover_now). Steer speaks claims + terms; seek speaks tender (the walking tender's uuid), form, region, criterion, budget, candidates.",
                    "required": ["type"],
                    "allOf": [
                        {
                            "if": { "properties": { "type": { "const": "steer" } } },
                            "then": { "required": ["claims"] }
                        },
                        {
                            "if": { "properties": { "type": { "const": "seek" } } },
                            "then": { "required": ["form", "region"] }
                        }
                    ],
                    "properties": {
                        "type": {
                            "type": "string",
                            "description": "The wish's type, always stated: steer holds outcomes in their bands; seek commissions a walk",
                            "enum": ["steer", "seek"]
                        },
                        "form": {
                            "type": "string",
                            "description": "The walk's form (type seek, always stated): find_best searches under budget; cover_now walks a region first",
                            "enum": ["cover_now", "find_best"]
                        },
                        "tender": {
                            "type": "string",
                            "description": "The walking tender's uuid (always stated) - dial names repeat across tenders, so the region is read inside this one dial-set"
                        },
                        "region": {
                            "type": "object",
                            "description": "The dials to walk, each [lo, hi, is_int] - the bounds of the commission",
                            "additionalProperties": { "type": "array", "items": {} }
                        },
                        "criterion": {
                            "type": "object",
                            "description": "find_best declares what counts: the reading and the direction",
                            "properties": {
                                "outcome": { "type": "string", "description": "The reading to optimize" },
                                "direction": { "type": "string", "enum": ["maximize", "max", "minimize", "min"] }
                            }
                        },
                        "budget": { "type": "integer", "description": "find_best: the measured trials it may spend - one trial each" },
                        "candidates": { "type": "integer", "description": "Optional launch dice: N random joint settings tried first; they rank but never teach" },
                        "answer": {
                            "type": "string",
                            "enum": ["now", "later"],
                            "description": "When the reachability consult answers: now = in the same breath (the declare response carries it); later = the default, at serving time and in status"
                        },
                        "standard": {
                            "type": "string",
                            "enum": ["all", "best"],
                            "description": "The wish's standard of success: all = the hold default, one kept state (a half-kept wish is a lying wish); best = the walk default, the best of several candidates counts"
                        },
                        "claims": {
                            "type": "array",
                            "description": "The outcomes to hold: one reading, one band each. N >= 1.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "outcome": { "type": "string", "description": "Plain name of the measured value - must resolve on the map's outcome registry, e.g. 'chainend.shift'" },
                                    "band": {
                                        "type": "object",
                                        "description": "Acceptable range in the outcome's measurement units",
                                        "properties": {
                                            "lo": { "type": "number", "description": "Lower edge of the acceptable band" },
                                            "hi": { "type": "number", "description": "Upper edge of the acceptable band" },
                                            "target": { "type": "number", "description": "Aim point inside the band - receipt and reporting only, never judged" }
                                        }
                                    }
                                }
                            }
                        },
                        "terms": {
                            "type": "array",
                            "description": "The price: claims that carry no dial - the compile must honor them; a term broken is a miss with the same standing. M >= 0.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "outcome": { "type": "string", "description": "Plain name of the protected reading" },
                                    "band": { "type": "object", "description": "Range the wish must never push this reading out of" }
                                }
                            }
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
                    "required": ["claims"],
                    "additionalProperties": true
                }),
            },
            ToolDef {
                name: "wish_get",
                description: "Get one wish with its full event history (declared, planned, assigned, refused, acted, landed, missed, re_opened, corrected, returned, closed, plan_error).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "wish_id": { "type": "string", "description": "UUID of the wish" }
                    },
                    "required": ["wish_id"],
                    "additionalProperties": false
                }),
            },
            ToolDef {
                name: "wish_close",
                description: "Close a wish: append the 'closed' event and release the hold (idempotent on already-closed wishes).",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "wish_id": { "type": "string", "description": "UUID of the wish to close" }
                    },
                    "required": ["wish_id"],
                    "additionalProperties": false
                }),
            },

            ToolDef {
                name: "wish_update",
                description: "The holder corrects a wish: restated claims (and terms) on the same identity. Stamps a 'corrected' event with the previous band, then asks for a new plan. Works on living and unheld wishes.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "wish_id": { "type": "string", "description": "UUID of the wish to correct" },
                        "claims": {
                            "type": "array",
                            "description": "The restated aims: one reading, one band each. N >= 1.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "outcome": { "type": "string", "description": "Plain name of the measured value" },
                                    "band": { "type": "object", "description": "Acceptable range in the outcome's measurement units" }
                                }
                            }
                        },
                        "terms": {
                            "type": "array",
                            "description": "The restated price: protected readings the wish must never push out of band. M >= 0.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "outcome": { "type": "string", "description": "Plain name of the protected reading" },
                                    "band": { "type": "object", "description": "Range the wish must never push this reading out of" }
                                }
                            }
                        },
                        "reason": { "type": "string", "description": "Why the holder corrects (stamped into the trail)" }
                    },
                    "required": ["wish_id", "claims"],
                    "additionalProperties": true
                }),
            },
            ToolDef {
                name: "wish_delete",
                description: "Purge a wish: closes it first (unassign + dial revert), then removes registry rows (events cascade) and the book's card. Forgetting, not stopping.",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "wish_id": { "type": "string", "description": "UUID of the wish to purge" }
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

        Self {
            client,
            causal_client,
            tools,
        }
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
            "wish_list" => self.client.get("/wishes").await,
            "wish_get" => {
                require_id(id, "wish_id")?;
                self.client
                    .get(&format!("/wishes/{}", urlencoding::encode(id)))
                    .await
            }
            "wish_declare" => {
                // The wish body, verbatim - the door validates the
                // grammar (claims, terms), this layer never intervenes.
                self.client.post("/wishes", args.clone()).await
            }
            "wish_close" => {
                require_id(id, "wish_id")?;
                self.client
                    .post_empty(&format!("/wishes/{}/close", urlencoding::encode(id)))
                    .await
            }

            "wish_update" => {
                require_id(id, "wish_id")?;
                // The correction, verbatim (claims, terms, reason) -
                // the door validates; identity rides the URL.
                self.client
                    .post(
                        &format!("/wishes/{}/update", urlencoding::encode(id)),
                        args.clone(),
                    )
                    .await
            }
            "wish_delete" => {
                require_id(id, "wish_id")?;
                self.client
                    .delete(&format!("/wishes/{}", urlencoding::encode(id)))
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
                    .get(&format!("/impact/{}", urlencoding::encode(id)))
                    .await
            }
            "connectome_causes" => {
                require_id(id, "systemtender_id")?;
                self.causal_client
                    .get(&format!("/causes/{}", urlencoding::encode(id)))
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

#[cfg(test)]
mod wire_tests {
    use super::*;
    use crate::client::GodonClient;

    // ── the wire oracle: a local listener plays godon-api and causal ──
    // GodonClient already takes host:port — no seam needed, the real
    // HTTP path runs against a std-only listener that answers the
    // declared contract and records method, path, and body.

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    struct Recorded {
        method: String,
        path: String,
        body: String,
    }

    /// One (status, body) per expected request, answered in arrival
    /// order; dry script answers 404 so a mis-counted test fails
    /// loudly instead of hanging.
    struct FakeApi {
        port: u16,
        log: Arc<Mutex<Vec<Recorded>>>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = match status {
            200 => "OK",
            201 => "Created",
            400 => "Bad Request",
            404 => "Not Found",
            500 => "Internal Server Error",
            _ => "OK",
        };
        let resp = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            reason,
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).ok();
        stream.flush().ok();
    }

    fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    if buf.len() > 1_048_576 {
                        break;
                    }
                }
                Err(_) => return None,
            }
        }
        let head = String::from_utf8_lossy(&buf).to_string();
        let mut lines = head.split("\r\n");
        let request_line = lines.next()?.to_string();
        let mut parts = request_line.split_whitespace();
        let method = parts.next()?.to_string();
        let path = parts.next()?.to_string();
        let mut content_length = 0usize;
        for line in lines {
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        let header_end = head.find("\r\n\r\n").map(|i| i + 4).unwrap_or(buf.len());
        let mut body = buf[header_end.min(buf.len())..].to_vec();
        while body.len() < content_length {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        if body.len() > content_length {
            body.truncate(content_length);
        }
        Some(Recorded {
            method,
            path,
            body: String::from_utf8_lossy(&body).to_string(),
        })
    }

    impl FakeApi {
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
            let srv_log = Arc::clone(&log);
            let srv_stop = Arc::clone(&stop);
            let srv_queue = Arc::clone(&queue);
            let handle = std::thread::spawn(move || {
                while !srv_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                                .ok();
                            if let Some(recorded) = read_request(&mut stream) {
                                srv_log.lock().unwrap().push(recorded);
                                let (status, body) = srv_queue
                                    .lock()
                                    .unwrap()
                                    .pop_front()
                                    .unwrap_or((404, String::new()));
                                write_response(&mut stream, status, &body);
                            }
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

        fn client(&self) -> GodonClient {
            GodonClient::new("127.0.0.1".into(), self.port, false)
        }

        fn registry(&self, causal: &FakeApi) -> ToolRegistry {
            ToolRegistry::new(self.client(), causal.client())
        }

        fn recorded(&self) -> Vec<(String, String)> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .map(|r| (r.method.clone(), r.path.clone()))
                .collect()
        }

        fn bodies(&self, fragment: &str) -> Vec<serde_json::Value> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.path.contains(fragment))
                .map(|r| serde_json::from_str(&r.body).unwrap_or(serde_json::Value::Null))
                .collect()
        }
    }

    impl Drop for FakeApi {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
        }
    }

    fn ok_json(body: &str) -> (u16, String) {
        (200, body.to_string())
    }

    // ── the dispatch: every tool walks its exact wire path ──

    #[tokio::test]
    async fn list_and_get_walk_the_declared_paths() {
        let causal = FakeApi::start(vec![]);
        let f = FakeApi::start(vec![ok_json(r#"[{"id":"st-1"}]"#)]);
        let r = f.registry(&causal);
        let out = r
            .call_tool("systemtender_list", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(out, serde_json::json!([{"id":"st-1"}]));
        assert_eq!(f.recorded(), vec![("GET".into(), "/systemtenders".into())]);

        // the id rides the path, percent-encoded
        let f = FakeApi::start(vec![ok_json("{}")]);
        let causal = FakeApi::start(vec![]);
        let r = f.registry(&causal);
        r.call_tool(
            "systemtender_get",
            serde_json::json!({"systemtender_id": "st 9"}),
        )
        .await
        .unwrap();
        assert_eq!(
            f.recorded(),
            vec![("GET".into(), "/systemtenders/st%209".into())]
        );
    }

    #[tokio::test]
    async fn create_posts_the_declared_body() {
        let causal = FakeApi::start(vec![]);
        let f = FakeApi::start(vec![ok_json("{}")]);
        let r = f.registry(&causal);
        r.call_tool(
            "systemtender_create",
            serde_json::json!({"name": "bench", "config": {"seed": 42}}),
        )
        .await
        .unwrap();
        assert_eq!(f.recorded()[0].0, "POST");
        assert_eq!(f.recorded()[0].1, "/systemtenders");
        assert_eq!(
            f.bodies("/systemtenders")[0],
            serde_json::json!({"name": "bench", "config": {"seed": 42}})
        );

        // create without a name refuses before any wire traffic
        let f = FakeApi::start(vec![]);
        let causal = FakeApi::start(vec![]);
        let r = f.registry(&causal);
        assert!(r
            .call_tool("systemtender_create", serde_json::json!({"config": {}}))
            .await
            .is_err());
        assert!(f.recorded().is_empty());
    }

    #[tokio::test]
    async fn stop_and_start_post_empty_to_their_doors() {
        for (tool, door) in [
            ("systemtender_stop", "stop"),
            ("systemtender_start", "start"),
        ] {
            let causal = FakeApi::start(vec![]);
            let f = FakeApi::start(vec![ok_json("{}")]);
            let r = f.registry(&causal);
            r.call_tool(tool, serde_json::json!({"systemtender_id": "x"}))
                .await
                .unwrap();
            assert_eq!(
                f.recorded(),
                vec![("POST".into(), format!("/systemtenders/x/{}", door))],
                "{}",
                tool
            );
            assert_eq!(f.bodies("/systemtenders")[0], serde_json::Value::Null);
        }
    }

    #[tokio::test]
    async fn delete_honors_the_force_flag() {
        // absent force: the bare path
        let causal = FakeApi::start(vec![]);
        let f = FakeApi::start(vec![ok_json("{}")]);
        let r = f.registry(&causal);
        r.call_tool(
            "systemtender_delete",
            serde_json::json!({"systemtender_id": "x"}),
        )
        .await
        .unwrap();
        assert_eq!(
            f.recorded()[0],
            ("DELETE".into(), "/systemtenders/x".into())
        );

        // force: the query rides
        let causal = FakeApi::start(vec![]);
        let f = FakeApi::start(vec![ok_json("{}")]);
        let r = f.registry(&causal);
        r.call_tool(
            "systemtender_delete",
            serde_json::json!({"systemtender_id": "x", "force": true}),
        )
        .await
        .unwrap();
        assert_eq!(
            f.recorded()[0],
            ("DELETE".into(), "/systemtenders/x?force=true".into())
        );
    }

    #[tokio::test]
    async fn credentials_and_targets_pass_args_through() {
        // credential_create posts the args verbatim — the server shapes them
        let causal = FakeApi::start(vec![]);
        let f = FakeApi::start(vec![ok_json("{}")]);
        let r = f.registry(&causal);
        let args = serde_json::json!({"kind": "api_key", "secret": "s"});
        r.call_tool("credential_create", args.clone())
            .await
            .unwrap();
        assert_eq!(f.recorded()[0], ("POST".into(), "/credentials".into()));
        assert_eq!(f.bodies("/credentials")[0], args);

        // target_get names its own id field
        let causal = FakeApi::start(vec![]);
        let f = FakeApi::start(vec![ok_json("{}")]);
        let r = f.registry(&causal);
        r.call_tool("target_get", serde_json::json!({"target_id": "t1"}))
            .await
            .unwrap();
        assert_eq!(f.recorded(), vec![("GET".into(), "/targets/t1".into())]);
    }

    #[tokio::test]
    async fn ids_are_required_at_the_door() {
        let causal = FakeApi::start(vec![]);
        let f = FakeApi::start(vec![]);
        let r = f.registry(&causal);
        let err = r
            .call_tool("systemtender_get", serde_json::json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("systemtender_id"), "got: {err}");
        // nothing reached the wire
        assert!(f.recorded().is_empty());
    }

    #[tokio::test]
    async fn unknown_tool_refuses() {
        let causal = FakeApi::start(vec![]);
        let f = FakeApi::start(vec![]);
        let r = f.registry(&causal);
        let err = r
            .call_tool("not_a_tool", serde_json::json!({}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Unknown tool"), "got: {err}");
    }

    #[tokio::test]
    async fn causal_tools_ride_the_second_client_only() {
        let causal = FakeApi::start(vec![ok_json(r#"{"nodes":[]}"#)]);
        let f = FakeApi::start(vec![]);
        let r = f.registry(&causal);
        let out = r
            .call_tool("connectome_get", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(out, serde_json::json!({"nodes":[]}));
        assert_eq!(causal.recorded(), vec![("GET".into(), "/graph".into())]);
        // the main client stayed unspoken to
        assert!(f.recorded().is_empty());
    }

    #[tokio::test]
    async fn health_rides_the_main_client() {
        let causal = FakeApi::start(vec![]);
        let f = FakeApi::start(vec![ok_json(r#"{"status":"ok"}"#)]);
        let r = f.registry(&causal);
        r.call_tool("health", serde_json::json!({})).await.unwrap();
        assert_eq!(f.recorded(), vec![("GET".into(), "/health".into())]);
    }

    // ── the client's own contract: errors speak the server's words ──

    #[tokio::test]
    async fn client_errors_carry_the_servers_words() {
        let f = FakeApi::start(vec![(400, r#"{"error":"boom"}"#.into())]);
        let err = f.client().get("/x").await.unwrap_err().to_string();
        assert!(err.contains("godon-api: boom"), "got: {err}");

        let f = FakeApi::start(vec![(404, r#"{"message":"gone"}"#.into())]);
        let err = f.client().get("/x").await.unwrap_err().to_string();
        assert!(err.contains("godon-api: gone"), "got: {err}");

        // a body with no words falls back to the bare status
        let f = FakeApi::start(vec![(500, "teapot".into())]);
        let err = f.client().get("/x").await.unwrap_err().to_string();
        assert!(err.contains("500"), "got: {err}");
    }

    #[tokio::test]
    async fn empty_success_bodies_answer_empty_objects() {
        let f = FakeApi::start(vec![(200, String::new())]);
        let out = f.client().get("/x").await.unwrap();
        assert_eq!(out, serde_json::json!({}));
    }
}
