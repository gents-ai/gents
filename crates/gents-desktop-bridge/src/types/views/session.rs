use serde::Serialize;
use ts_rs::TS;

use super::operations::DerivedCancelCauseView;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MessageView {
    pub message_key: String,
    pub request_id: Option<String>,
    pub sequence: Option<i64>,
    pub role: Option<String>,
    pub display_role: Option<String>,
    pub display_content: Option<String>,
    pub reasoning: Option<String>,
    pub has_tool_calls: bool,
    pub has_tool_results: bool,
    /// Explicit canonical reconstruction outcome.  A header is durable even
    /// while one of its immutable dependencies is still replicating, so the
    /// UI must not mistake absent presentation text for an empty message.
    pub reconstruction_state: ReconstructionState,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub reconstruction_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub denied_dependency_doc_id: Option<String>,
    pub runtime_control: bool,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ReconstructionState {
    Ready,
    Loading,
    Denied,
    Invalid,
}

/// Immutable canonical-output reconstruction state.  This is carried through
/// the rendered timeline so consumers render a received loading/denied/invalid
/// fact rather than inferring one from blank text.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MessageReconstructionView {
    pub state: ReconstructionState,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub denied_dependency_doc_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallView {
    pub tool_call_key: String,
    pub request_id: Option<String>,
    pub message_sequence: Option<i64>,
    pub tool_name: Option<String>,
    pub tool_call_id: Option<String>,
    pub args: Option<String>,
    pub partial_output_tail: Option<String>,
    pub result: Option<String>,
    /// Availability of the immutable canonical arguments and result payloads.
    /// Tool execution status remains in `lifecycle_state`.
    pub reconstruction: MessageReconstructionView,
    pub status: Option<String>,
    pub lifecycle_state: Option<String>,
    pub child_request_id: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub started_at: Option<String>,
    pub deadline_at: Option<String>,
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub denial: Option<CommandDenialView>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub cancel_cause: Option<DerivedCancelCauseView>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommandDenialView {
    pub category: String,
    pub category_label: String,
    pub rule_id: String,
    pub reason_line: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub denied_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub denied_argument: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub denied_subcommand: Option<String>,
    pub diagnostic: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolDiffLineView {
    pub kind: ToolDiffLineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ToolDiffLineKind {
    Added,
    Removed,
    Context,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ToolPresentationView {
    #[serde(rename_all = "camelCase")]
    Command {
        command: String,
        exit_code: Option<i64>,
        timed_out: bool,
        failed: bool,
        duration_ms: Option<i64>,
        cwd: Option<String>,
        execution_mode: Option<String>,
        network_mode: Option<String>,
        stdout: String,
        stderr: String,
        fallback_output: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    FileRead {
        operation: String,
        target: Option<String>,
        returned_count: Option<i64>,
        total_count: Option<i64>,
        truncated: bool,
        body: String,
        fallback_output: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    FileEdit {
        operation: String,
        path: Option<String>,
        created: Option<bool>,
        replacements_applied: Option<i64>,
        diff: Vec<ToolDiffLineView>,
        fallback_output: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Subagent {
        action: String,
        name: Option<String>,
        child_request_id: Option<String>,
        description: Option<String>,
        output: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Process {
        action: String,
        target: Option<String>,
        description: Option<String>,
        output: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Mcp {
        service_id: Option<String>,
        selected_tool_name: Option<String>,
        arguments: Option<String>,
        output: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Generic {
        summary: Option<String>,
        input: Option<String>,
        output: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RenderedToolCallView {
    pub item_key: String,
    pub tool_name: String,
    pub status_kind: String,
    /// Request that issued this call, in the same logical identity as
    /// `child_request_id` and a subagent lineage root. A session transcript
    /// spans several requests; this is how a row names the one it belongs to.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub child_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub await_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub cancel_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub deadline_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub completed_at: Option<String>,
    pub presentation: ToolPresentationView,
    pub reconstruction: MessageReconstructionView,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub partial_output_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub denial: Option<CommandDenialView>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub cancel_cause: Option<DerivedCancelCauseView>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PendingTurnView {
    pub request_id: String,
    /// Internal reconciliation identity. The wire view keeps its existing
    /// shape; canonical messages link to the physical request document, not
    /// the caller-authored logical request id.
    #[serde(skip)]
    #[ts(skip)]
    pub(crate) request_doc_id: Option<String>,
    pub content: String,
    pub selected_skill_ids: Vec<String>,
    pub lifecycle_state: Option<String>,
    pub created_at: Option<String>,
}

/// Read-only presentation of the latest request's terminal facts. The request
/// remains the owner; this is not response storage or a second lifecycle.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RequestOutcomeView {
    pub failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub cancel_cause: Option<DerivedCancelCauseView>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum RenderedTimelineItem {
    #[serde(rename_all = "camelCase")]
    UserMessage {
        item_key: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        #[ts(optional = nullable)]
        request_id: Option<String>,
        sequence: Option<i64>,
        content: Option<String>,
        timestamp: Option<String>,
        reconstruction: MessageReconstructionView,
    },
    #[serde(rename_all = "camelCase")]
    AssistantMessage {
        item_key: String,
        sequence: Option<i64>,
        content: Option<String>,
        reasoning: Option<String>,
        timestamp: Option<String>,
        reconstruction: MessageReconstructionView,
    },
    #[serde(rename_all = "camelCase")]
    ToolGroup {
        item_key: String,
        message_sequence: Option<i64>,
        tools: Vec<RenderedToolCallView>,
    },
    #[serde(rename_all = "camelCase")]
    PendingUserTurn {
        item_key: String,
        request_id: String,
        content: String,
        selected_skill_ids: Vec<String>,
        lifecycle_state: Option<String>,
        created_at: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    LiveAssistant {
        item_key: String,
        content: Option<String>,
        reasoning: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GoalView {
    pub goal_id: String,
    pub objective: Option<String>,
    pub status: Option<String>,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub active_time_seconds: i64,
    pub consecutive_blocked_audits: i64,
    pub continuation_sequence: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
    pub last_blocked_reason: Option<String>,
    pub last_failure: Option<String>,
    pub completion_evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RetryEligibilityView {
    pub eligible: bool,
    pub denial_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactionView {
    pub compaction_key: String,
    pub sequence: Option<i64>,
    pub messages_compacted: i64,
    pub original_tokens: Option<i64>,
    pub compacted_tokens: Option<i64>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionContextComponentsView {
    pub messages: i64,
    pub documents: i64,
    pub tool_schemas: i64,
    pub additional_parameters: i64,
    pub output_schema: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionRequestContextView {
    pub request_id: String,
    pub call_id: String,
    pub call_sequence: i64,
    pub turn_index: i64,
    pub attempt: i64,
    pub estimator: String,
    pub estimated_input_tokens: i64,
    pub context_window: i64,
    pub compaction_threshold_tokens: i64,
    pub configured_max_output_tokens: Option<i64>,
    pub effective_max_output_tokens: Option<i64>,
    pub compaction_reason: String,
    pub pre_compaction_input_tokens: Option<i64>,
    pub components: SessionContextComponentsView,
}

/// Observable context pressure for the session. `last_request` is the exact,
/// prompt-free accounting captured at the most recent provider boundary; the
/// remaining fields are estimates computed directly from the durable rows.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionContextView {
    /// True when the durable transcript/context rows were read to exhaustion.
    /// False means the remaining fields describe only the bounded visible page.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub transcript_totals_exact: Option<bool>,
    pub estimated_durable_tokens: i64,
    pub estimated_conversation_tokens: i64,
    pub context_window: i64,
    pub compaction_threshold: f64,
    pub compaction_threshold_tokens: i64,
    pub compaction_strategy: String,
    pub durable_message_count: i64,
    pub provider_message_count: i64,
    pub total_compacted_messages: i64,
    pub compactions: Vec<SessionCompactionView>,
    pub last_request: Option<SessionRequestContextView>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionTimelinePageView {
    pub total_items: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub total_items_exact: Option<bool>,
    pub page_items: i64,
    pub has_older: bool,
    pub has_newer: bool,
    pub oldest_item_key: Option<String>,
    pub newest_item_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub query_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub queried_rows: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub message_query_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub tool_call_query_limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionProjectionRevisionView {
    pub store_version: u64,
    pub reconcile_version: u64,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionLiveTextPatchView {
    /// unchanged | append | replace
    pub mode: String,
    pub value: String,
    pub byte_len: usize,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionLiveDeltaView {
    /// delta | unchanged | snapshotRequired
    pub outcome: String,
    pub revision: SessionProjectionRevisionView,
    pub request_id: String,
    pub turn_state: Option<String>,
    pub status: Option<String>,
    pub content: Option<SessionLiveTextPatchView>,
    pub reasoning: Option<SessionLiveTextPatchView>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionHydrationView {
    pub session_id: String,
    pub agent_did: String,
    pub phase: String,
    pub merged_count: usize,
    pub covered_count: usize,
    pub served_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionSnapshot {
    pub session_id: String,
    pub agent_did: Option<String>,
    pub behavior_id: Option<String>,
    pub title: Option<String>,
    pub preview_text: Option<String>,
    pub status: Option<String>,
    pub goal: Option<GoalView>,
    pub turn_state: Option<String>,
    pub latest_request_id: Option<String>,
    pub retry_eligibility: RetryEligibilityView,
    pub latest_request_outcome: Option<RequestOutcomeView>,
    pub pending_turn: Option<PendingTurnView>,
    pub context: SessionContextView,
    pub timeline_items: Vec<RenderedTimelineItem>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub hydration: Option<SessionHydrationView>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub timeline_page: Option<SessionTimelinePageView>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub projection_revision: Option<SessionProjectionRevisionView>,
    #[allow(dead_code)]
    #[serde(skip_serializing)]
    #[ts(skip)]
    pub messages: Vec<MessageView>,
    #[allow(dead_code)]
    #[serde(skip_serializing)]
    #[ts(skip)]
    pub tool_calls: Vec<ToolCallView>,
}
