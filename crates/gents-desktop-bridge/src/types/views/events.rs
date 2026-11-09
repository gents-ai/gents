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
    pub session_id: String,
    pub request_id: String,
    pub node_did: String,
    pub agent_did: Option<String>,
}

impl From<gents_desktop_core::client::SubmittedRequest> for ChatSendResult {
    fn from(receipt: gents_desktop_core::client::SubmittedRequest) -> Self {
        Self {
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
}

impl ClientUpdateEvent {
    pub fn coarse(reason: &'static str) -> Self {
        Self {
            reason,
            store_version: None,
        }
    }

    pub fn store(notice: gents_desktop_core::client::StoreUpdateNotice) -> Self {
        Self {
            reason: "store",
            store_version: Some(notice.revision.store_version),
        }
    }
}
