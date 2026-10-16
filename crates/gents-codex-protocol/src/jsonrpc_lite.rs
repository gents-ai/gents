use serde::Deserialize;
use serde::Serialize;
use std::fmt;

pub const JSONRPC_VERSION: &str = "2.0";

#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Deserialize, Serialize, Hash, Eq)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Integer(i64),
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(value) => f.write_str(value),
            Self::Integer(value) => write!(f, "{value}"),
        }
    }
}

pub type Result = serde_json::Value;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum JSONRPCMessage {
    Request(JSONRPCRequest),
    Notification(JSONRPCNotification),
    Response(JSONRPCResponse),
    Error(JSONRPCError),
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct JSONRPCRequest {
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<W3cTraceContext>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct JSONRPCNotification {
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct JSONRPCResponse {
    pub id: RequestId,
    pub result: Result,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct JSONRPCError {
    pub error: JSONRPCErrorError,
    pub id: RequestId,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct JSONRPCErrorError {
    pub code: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    pub message: String,
}
use crate::core_types::W3cTraceContext;
