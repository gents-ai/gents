use std::path::PathBuf;

use defra_agent_protocol::client_protocol::ClientTurnState;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopInitRequest {
    pub agent_home: Option<PathBuf>,
    pub desktop_home: Option<PathBuf>,
    pub label: Option<String>,
    pub dangerously_overwrite: bool,
    pub reset: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatSendRequest {
    pub agent_did: String,
    pub behavior_id: Option<String>,
    pub session_id: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationRenameRequest {
    pub session_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedPeerView {
    pub peer_id: String,
    pub label: String,
    pub agent_did: String,
    pub addr: String,
    pub source: Option<String>,
    pub graphql: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBootstrapSummary {
    pub default_agent_home: String,
    pub desktop_home: String,
    pub peer_directory_path: String,
    pub node_data_dir: String,
    pub agent_home_exists: bool,
    pub desktop_home_exists: bool,
    pub peer_directory_exists: bool,
    pub saved_peers: Vec<SavedPeerView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct P2PHealthView {
    pub status: String,
    pub connected_peer_count: usize,
    pub replicator_count: usize,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeView {
    pub process_state: Option<String>,
    pub reconcile_phase: Option<String>,
    pub last_reconcile_result: Option<String>,
    pub last_reconcile_error: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BehaviorView {
    pub behavior_id: String,
    pub display_name: String,
    pub model_name: Option<String>,
    pub enabled: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationSummary {
    pub session_id: String,
    pub title: Option<String>,
    pub preview_text: Option<String>,
    pub status: Option<String>,
    pub behavior_id: Option<String>,
    pub latest_request_id: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub turn_state: Option<String>,
    pub message_count: usize,
    pub tool_call_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeploymentView {
    pub peer_id: String,
    pub label: String,
    pub agent_did: String,
    pub addr: String,
    pub source: Option<String>,
    pub graphql: Option<String>,
    pub dial_succeeded: bool,
    pub last_error: Option<String>,
    pub default_behavior_id: Option<String>,
    pub runtime: Option<RuntimeView>,
    pub behaviors: Vec<BehaviorView>,
    pub conversations: Vec<ConversationSummary>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRuntimeSnapshot {
    pub local_peer_id: String,
    pub listen_addresses: Vec<String>,
    pub p2p_health: P2PHealthView,
    pub bootstrap_errors: Vec<String>,
    pub last_mutation_error: Option<String>,
    pub focused_request_id: Option<String>,
    pub configured_peer_count: usize,
    pub dialed_peer_count: usize,
    pub peer_issue_count: usize,
    pub row_count: usize,
    pub approx_serialized_bytes: usize,
    pub deployments: Vec<DeploymentView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopClientSnapshot {
    pub bootstrap: DesktopBootstrapSummary,
    pub client: Option<DesktopRuntimeSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageView {
    pub message_key: String,
    pub sequence: Option<i64>,
    pub role: Option<String>,
    pub content: Option<String>,
    pub display_role: Option<String>,
    pub display_content: Option<String>,
    pub reasoning: Option<String>,
    pub has_tool_calls: bool,
    pub has_tool_results: bool,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolCallView {
    pub tool_call_key: String,
    pub message_sequence: Option<i64>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
    pub args: Option<String>,
    pub result: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolResultView {
    pub tool_name: Option<String>,
    pub tool_input: Option<String>,
    pub output_text: Option<String>,
    pub truncated: Option<bool>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResponseView {
    pub status: Option<String>,
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub error_message: Option<String>,
    pub token_count: Option<i64>,
    pub materialized_message_sequence: Option<i64>,
    pub materialized_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopSessionSnapshot {
    pub session_id: String,
    pub agent_did: Option<String>,
    pub behavior_id: Option<String>,
    pub title: Option<String>,
    pub preview_text: Option<String>,
    pub status: Option<String>,
    pub turn_state: Option<String>,
    pub latest_request_id: Option<String>,
    pub latest_response: Option<ResponseView>,
    pub active_response_overlay: Option<ResponseView>,
    pub messages: Vec<MessageView>,
    pub tool_calls: Vec<ToolCallView>,
    pub tool_results: Vec<ToolResultView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatSendResult {
    pub session_id: String,
    pub request_id: String,
    pub agent_did: String,
    pub behavior_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientUpdateEvent {
    pub reason: &'static str,
}

pub(crate) fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn turn_state_label(state: ClientTurnState) -> &'static str {
    match state {
        ClientTurnState::WaitingForClaim => "waitingForClaim",
        ClientTurnState::Streaming => "streaming",
        ClientTurnState::Completed => "completed",
        ClientTurnState::Failed => "failed",
        ClientTurnState::Superseded => "superseded",
        ClientTurnState::Interrupted => "interrupted",
    }
}
