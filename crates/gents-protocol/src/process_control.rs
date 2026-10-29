use serde::{Deserialize, Serialize};

pub const PROCESS_CONTROL_PATH: &str = "/processes/control";

/// A displayed native invocation is addressed by both its physical row and
/// logical identity; neither a session identifier nor a process identifier
/// alone grants control.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct OperatorProcessControlRequest {
    pub node_did: String,
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_call_doc_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum ProcessControlOutcome {
    Cancelled { live_execution_cancelled: bool },
    Lost,
    Unverified,
    AlreadyTerminal { state: String },
    NotBackground,
    NotFound,
}
