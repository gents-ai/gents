use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolServiceToolView {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolServiceTestResult {
    pub service_id: String,
    pub endpoint: String,
    pub status: String,
    pub tool_count: usize,
    pub tools: Vec<ToolServiceToolView>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskRunResult {
    pub request_doc_id: String,
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub agent_did: String,
    pub lifecycle_state: Option<gents_protocol::request_lifecycle::RequestLifecycleState>,
}

/// The request the runtime holds for a confirmed send (Lean
/// `ClientOptimisticSend`); a failed send is the command's error.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendResult {
    pub publication: ChatPublication,
    pub session_id: String,
    pub request_id: String,
    pub node_did: String,
    pub agent_did: Option<String>,
}

impl From<gents::client_operations::SubmittedRequest> for ChatSendResult {
    fn from(receipt: gents::client_operations::SubmittedRequest) -> Self {
        Self {
            publication: ChatPublication::Delivered,
            session_id: receipt.session_id,
            request_id: receipt.request_id,
            node_did: receipt.node_did,
            agent_did: receipt.agent_did,
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ConfiguredActionKind {
    Task,
    Schedule,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguredActionView {
    pub kind: ConfiguredActionKind,
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RequestOutputView {
    pub node_did: String,
    pub session_id: String,
    pub request_id: String,
    pub output: RequestOutputStateView,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RequestOutputStateView {
    Loading,
    TerminalMessage { content: String },
    TerminalNoMessage,
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ClientUpdateEvent {
    #[ts(type = "string")]
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_version: Option<u64>,
    /// A store update's fingerprint of what the session lists show (each
    /// row's status, title, activity and previews): equal across updates
    /// that change none of it, such as a followed turn's reply pieces.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub session_list: Option<String>,
}

impl ClientUpdateEvent {
    pub fn coarse(reason: &'static str) -> Self {
        Self {
            reason,
            store_version: None,
            session_list: None,
        }
    }

    /// `version` advances on every publication into any node's store;
    /// `session_list` is [`crate::client_runtime::ClientRuntime::session_list_fingerprint`].
    pub fn store(version: u64, session_list: u64) -> Self {
        Self {
            reason: "store",
            store_version: Some(version),
            session_list: Some(format!("{session_list:016x}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ChatPublication {
    Retained,
    Delivered,
}
