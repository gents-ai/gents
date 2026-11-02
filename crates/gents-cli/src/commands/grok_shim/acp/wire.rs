use anyhow::{Context, Result};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub(crate) struct AcpRequest {
    pub(crate) id: Option<Value>,
    pub(crate) method: String,
    pub(crate) params: Value,
}

impl AcpRequest {
    pub(crate) fn from_payload(payload: &str) -> Result<Self> {
        let value: Value = serde_json::from_str(payload)
            .with_context(|| "decoding Grok shim ACP payload".to_string())?;
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("ACP payload is not a JSON object"))?;
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("ACP payload has no method"))?
            .to_string();
        let method = internal_method(&method).to_owned();
        let id = match object.get("id") {
            Some(Value::Null) | None => None,
            Some(id) => Some(id.clone()),
        };
        let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
        Ok(Self { id, method, params })
    }

    pub(super) fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AcpDispatch {
    pub(crate) notifications: Vec<String>,
    pub(crate) response: Option<String>,
}

pub(super) struct RequestOutcome {
    pub(super) notifications: Vec<Value>,
    pub(super) result: Value,
}

pub(super) fn response_line(id: &Value, result: &Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    }))
    .expect("JSON-RPC response envelope is serializable")
}

pub(super) fn error_line(id: &Value, code: i64, message: &str) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }))
    .expect("JSON-RPC error envelope is serializable")
}

pub(in crate::commands::grok_shim) fn wire_method(method: &str) -> std::borrow::Cow<'_, str> {
    if method.starts_with("x.ai/") {
        std::borrow::Cow::Owned(format!("_{method}"))
    } else {
        std::borrow::Cow::Borrowed(method)
    }
}

pub(in crate::commands::grok_shim) fn internal_method(method: &str) -> &str {
    if method.starts_with("_x.ai/") {
        &method[1..]
    } else {
        method
    }
}

pub(super) fn notification_line(method: &str, params: &Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": wire_method(method),
        "params": params,
    }))
    .expect("JSON-RPC notification envelope is serializable")
}
