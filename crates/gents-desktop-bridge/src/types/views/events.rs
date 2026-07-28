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
    pub agent_did: String,
    pub behavior_id: String,
    pub status: Option<String>,
    pub lifecycle_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendResult {
    pub session_id: String,
    pub request_id: String,
    pub agent_did: String,
    pub behavior_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ClientUpdateEvent {
    /// Coarse ping reason: store | health | lifecycle | config.
    #[ts(type = "string")]
    pub reason: &'static str,
}
