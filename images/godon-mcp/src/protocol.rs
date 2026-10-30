use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: serde_json::Value, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_responses_speak_jsonrpc_20_and_echo_the_id() {
        let r = JsonRpcResponse::success(serde_json::json!(7), serde_json::json!({"a": 1}));
        assert_eq!(r.jsonrpc, "2.0");
        assert_eq!(r.id, serde_json::json!(7));
        assert_eq!(r.result, Some(serde_json::json!({"a": 1})));
        assert!(r.error.is_none());
    }

    #[test]
    fn error_responses_carry_the_code_and_never_a_result() {
        let r = JsonRpcResponse::error(serde_json::json!("abc"), -32601, "Method not found".into());
        assert_eq!(r.jsonrpc, "2.0");
        assert_eq!(r.id, serde_json::json!("abc"));
        assert!(r.result.is_none());
        let e = r.error.unwrap();
        assert_eq!(e.code, -32601);
        assert_eq!(e.message, "Method not found");
        assert!(e.data.is_none());
    }

    #[test]
    fn serialization_omits_the_absent_side() {
        let ok = serde_json::to_string(&JsonRpcResponse::success(
            serde_json::json!(1),
            serde_json::json!({}),
        ))
        .unwrap();
        assert!(!ok.contains("error"));

        let err = serde_json::to_string(&JsonRpcResponse::error(
            serde_json::json!(1),
            -1,
            "x".into(),
        ))
        .unwrap();
        assert!(!err.contains("result"));
        assert!(err.contains("error"));
    }
}
