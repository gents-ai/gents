use serde::Serialize;

use super::operations::DerivedCancelCauseView;

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
    pub request_id: Option<String>,
    pub message_sequence: Option<i64>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
    pub args: Option<String>,
    pub result: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cancel_cause: Option<DerivedCancelCauseView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolDetailFieldView {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolDetailValueView {
    pub raw_text: String,
    pub fields: Vec<ToolDetailFieldView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RenderedToolCallView {
    pub item_key: String,
    pub tool_name: String,
    pub status: Option<String>,
    pub status_kind: String,
    pub args: Option<ToolDetailValueView>,
    pub result: Option<ToolDetailValueView>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cancel_cause: Option<DerivedCancelCauseView>,
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
    pub interrupted_at: Option<String>,
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cancel_cause: Option<DerivedCancelCauseView>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub backend_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingTurnView {
    pub request_id: String,
    pub content: String,
    pub lifecycle_state: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum RenderedTimelineItem {
    UserMessage {
        item_key: String,
        sequence: Option<i64>,
        content: String,
    },
    AssistantMessage {
        item_key: String,
        sequence: Option<i64>,
        content: Option<String>,
        reasoning: Option<String>,
    },
    ToolGroup {
        item_key: String,
        message_sequence: Option<i64>,
        tools: Vec<RenderedToolCallView>,
    },
    PendingUserTurn {
        item_key: String,
        request_id: String,
        content: String,
        lifecycle_state: Option<String>,
        created_at: Option<String>,
    },
    LiveAssistant {
        item_key: String,
        content: Option<String>,
        reasoning: Option<String>,
    },
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
    pub pending_turn: Option<PendingTurnView>,
    pub timeline_items: Vec<RenderedTimelineItem>,
    #[allow(dead_code)]
    #[serde(skip_serializing)]
    pub messages: Vec<MessageView>,
    #[allow(dead_code)]
    #[serde(skip_serializing)]
    pub tool_calls: Vec<ToolCallView>,
    #[allow(dead_code)]
    #[serde(skip_serializing)]
    pub tool_results: Vec<ToolResultView>,
}
