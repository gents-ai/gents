use anyhow::Result;
use serde_json::Value;

use super::{
    COMPACT_CONVERSATION_METHOD, INTERJECT_METHOD, JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_PARAMS,
    JSONRPC_METHOD_NOT_FOUND,
};

#[derive(Debug)]
pub(crate) struct ShapedMethodNotFound {
    pub(crate) message: String,
}

impl std::fmt::Display for ShapedMethodNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ShapedMethodNotFound {}

pub(super) fn shaped_stub_error(method: &str, params: &Value) -> anyhow::Error {
    let session = optional_session_id(params).unwrap_or_default();
    let message = match method {
        INTERJECT_METHOD => format!(
            "x.ai/interject is not supported by the Gents Grok shim: the owned completion \
             loop has no formally specified injection transition, and writing a detached \
             AgentMessage would not affect provider input. No AgentMessage or \
             AgentRequest document was fabricated for session {session:?}, and no \
             x.ai/session/interjection notification was emitted."
        ),
        COMPACT_CONVERSATION_METHOD => format!(
            "x.ai/compact_conversation is not supported by the Gents Grok shim: \
             CompactionEntry is runtime-owned, its schema has no tokens_before or \
             tokens_after fields, and AgentSession has no usage counters. No \
             CompactionEntry or AgentSession field was fabricated for session \
             {session:?}."
        ),
        other => format!("method {other:?} is not supported by the Gents Grok shim"),
    };
    anyhow::Error::new(ShapedMethodNotFound { message })
}

pub(super) fn error_code_for(error: &anyhow::Error) -> i64 {
    if error.downcast_ref::<ShapedMethodNotFound>().is_some() {
        JSONRPC_METHOD_NOT_FOUND
    } else if error.downcast_ref::<InvalidParams>().is_some() {
        JSONRPC_INVALID_PARAMS
    } else {
        JSONRPC_INTERNAL_ERROR
    }
}

#[derive(Debug)]
struct InvalidParams {
    message: String,
}

impl std::fmt::Display for InvalidParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for InvalidParams {}

pub(super) fn invalid_params(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(InvalidParams {
        message: message.into(),
    })
}

pub(super) fn required_session_id(params: &Value) -> Result<String> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid_params("request requires a non-empty sessionId"))
}

fn optional_session_id(params: &Value) -> Option<String> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
