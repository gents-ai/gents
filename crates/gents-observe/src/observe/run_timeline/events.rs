use serde::{Deserialize, Serialize};

use super::rows::{
    RetrySummary, TimelineInferenceCallRow, TimelineRenderedRequestRef, TimelineRequestRow,
    TimelineSessionRow,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunTimeline {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    pub request: TimelineRequestRow,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<TimelineSessionRow>,
    pub child_request_ids: Vec<String>,
    #[serde(default)]
    pub inference_calls: Vec<TimelineInferenceCallRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rendered_request_refs: Vec<TimelineRenderedRequestRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background_completions:
        Vec<gents_toolcalls::background::diagnostics::BackgroundCompletionEpochDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_completion_diagnostics_error: Option<String>,
    pub events: Vec<RunTimelineEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunTimelineEvent {
    Request(Box<TimelineRequestEvent>),
    RenderedRequest(TimelineRenderedRequestEvent),
    InferenceCall(TimelineInferenceCallEvent),
    Compaction(TimelineCompactionEvent),
    ProviderContextReduction(TimelineProviderContextReductionEvent),
    Message(TimelineMessageEvent),
    ToolCall(TimelineToolCallEvent),
    GoalTransition(TimelineGoalTransitionEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompactionEvent {
    pub compaction_key: String,
    pub request_id: String,
    pub session_id: String,
    pub sequence: i64,
    pub summary: String,
    pub messages_compacted: i64,
    pub original_tokens: i64,
    pub compacted_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineProviderContextReductionEvent {
    pub reduction_key: String,
    pub reduction_doc_id: String,
    pub request_id: String,
    pub request_doc_id: String,
    pub session_id: String,
    pub reduction_index: i64,
    pub turn_index: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_reduction_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer_call_seq: Option<i64>,
    pub messages_compacted: i64,
    pub original_tokens: i64,
    pub compacted_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineRequestEvent {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by_trigger_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by_trigger_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by_correlation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by_trigger_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by_source_doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    pub input: gents_protocol::request_input::RequestInput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "RetrySummary::is_empty")]
    pub retry_summary: RetrySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineRenderedRequestEvent {
    pub capture_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_seq: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_index: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_hash: Option<String>,
    pub provenance_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reduction_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineInferenceCallEvent {
    pub call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_instance_id: Option<String>,
    pub request_id: String,
    pub call_seq: i64,
    pub attempt: i64,
    pub call_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    pub call_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_depth_at_enqueue: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_generation: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_config_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_total_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_completeness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_accounting: Option<gents_inference::rendered_request::ContextAccounting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineGoalState {
    pub status: String,
    pub consecutive_blocked_audits: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineGoalParentState {
    pub commit_cid: String,
    pub state: TimelineGoalState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineGoalTransitionEvent {
    pub goal_id: String,
    pub session_id: String,
    pub node_did: String,
    pub commit_cid: String,
    pub height: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parents: Vec<TimelineGoalParentState>,
    pub state: TimelineGoalState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_blocked_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_blocked_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_evidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineMessageEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub session_id: String,
    pub sequence: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    pub header: gents_protocol::output::TranscriptMessage,
    pub message: gents_protocol::message::Message,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineToolCallEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_sequence: Option<i64>,
    pub tool_name: String,
    pub tool_call_id: String,
    pub args: String,
    pub result: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_service_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_failure_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denial_reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub denied_argv: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_argument: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_subcommand: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub denied_prefix: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_network: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub await_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_cause: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}
