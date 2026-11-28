use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};

use super::events::TimelineGoalState;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunTimelineRows {
    pub request: TimelineRequestRow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<TimelineSessionRow>,
    #[serde(default)]
    pub requests: Vec<TimelineRequestRow>,
    #[serde(default)]
    pub messages: Vec<TimelineMessageRow>,
    #[serde(default)]
    pub tool_calls: Vec<TimelineToolCallRow>,
    #[serde(default)]
    pub goal_versions: Vec<TimelineGoalVersionRow>,
    #[serde(default)]
    pub inference_calls: Vec<TimelineInferenceCallRow>,
    #[serde(default)]
    pub compactions: Vec<TimelineCompactionRow>,
    #[serde(default)]
    pub provider_context_reductions: Vec<TimelineProviderContextReductionRow>,
    #[serde(default)]
    pub rendered_requests: Vec<TimelineRenderedRequestRow>,
    #[serde(default)]
    pub rendered_request_refs: Vec<TimelineRenderedRequestRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineRenderedRequestRef {
    #[serde(default, rename = "_docID")]
    pub doc_id: String,
    #[serde(default)]
    pub request_doc_id: String,
    #[serde(default)]
    pub request_commit_cid: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineRequestRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<i64>,
    #[serde(
        default,
        deserialize_with = "gents_documents::deserialize_default_on_null"
    )]
    pub input: gents_protocol::request_input::RequestInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<RequestLifecycleState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by_trigger_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by_trigger_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by_correlation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by_trigger_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by_source_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by_parent_request_id: Option<String>,
    #[serde(default, skip_serializing)]
    pub caused_by_parent_request_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by_parent_tool_call_id: Option<String>,
    #[serde(default, skip_serializing)]
    pub caused_by_parent_tool_call_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_owner_node_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_authority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_seal_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_origin: Option<String>,
    #[serde(default, skip_serializing_if = "RetrySummary::is_empty")]
    pub retry_summary: RetrySummary,
}

impl From<gents_protocol::row::AgentRequestRow> for TimelineRequestRow {
    fn from(row: gents_protocol::row::AgentRequestRow) -> Self {
        Self {
            doc_id: row.doc_id,
            request_id: row.request_id,
            node_did: row.node_did,
            requester_did: row.requester_did,
            agent_did: row.agent_did,
            session_id: row.session_id,
            content: row.content,
            max_total_tokens: row.max_total_tokens,
            input: row.input.unwrap_or_default(),
            lifecycle_state: row.lifecycle_state,
            backend_id: row.backend_id,
            failure_reason: row.failure_reason,
            created_at: row.created_at,
            retry_count: row.retry_count,
            interrupt_generation: row.interrupt_generation,
            caused_by_trigger_id: row.caused_by_trigger_id,
            caused_by_trigger_kind: row.caused_by_trigger_kind,
            caused_by_correlation: row.caused_by_correlation,
            caused_by_trigger_context: row.caused_by_trigger_context,
            caused_by_source_doc_id: row.caused_by_source_doc_id,
            caused_by_parent_request_id: row.caused_by_parent_request_id,
            caused_by_parent_request_doc_id: row.caused_by_parent_request_doc_id,
            caused_by_parent_tool_call_id: row.caused_by_parent_tool_call_id,
            caused_by_parent_tool_call_doc_id: row.caused_by_parent_tool_call_doc_id,
            workspace_id: row.workspace_id,
            workspace_owner_node_did: row.workspace_owner_node_did,
            workspace_authority: row.workspace_authority,
            workspace_seal_hash: row.workspace_seal_hash,
            execution_origin: row.execution_origin,
            retry_summary: RetrySummary::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrySummary {
    pub retry_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transient_error: Option<String>,
    pub recovered: bool,
}

impl RetrySummary {
    pub(super) fn is_empty(&self) -> bool {
        self.retry_count == 0 && self.last_transient_error.is_none() && !self.recovered
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineMessageRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default, skip_serializing)]
    pub request_doc_id: Option<String>,
    #[serde(default)]
    pub sequence: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    pub header: gents_protocol::output::TranscriptMessage,
    pub message: gents_protocol::message::Message,
}

impl TimelineMessageRow {
    pub(crate) fn from_canonical(
        doc_id: String,
        header: gents_protocol::output::TranscriptMessage,
        message: gents_protocol::message::Message,
    ) -> Self {
        Self {
            doc_id: Some(doc_id),
            session_id: header.session_id.clone(),
            request_doc_id: header.request_doc_id.clone(),
            sequence: header.sequence as i64,
            timestamp: Some(header.created_at.clone()),
            node_did: Some(header.node_did.clone()),
            header,
            message,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineInferenceCallRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_instance_id: Option<String>,
    #[serde(default)]
    pub request_id: String,
    #[serde(default, skip_serializing)]
    pub request_doc_id: Option<String>,
    #[serde(default)]
    pub call_seq: i64,
    #[serde(default)]
    pub attempt: i64,
    #[serde(default)]
    pub call_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_did: Option<String>,
    #[serde(default)]
    pub call_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_depth_at_enqueue: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_config_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_total_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_completeness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_accounting_json: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineGoalVersionRow {
    #[serde(default, skip_serializing)]
    pub goal_doc_id: String,
    #[serde(default)]
    pub goal_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub node_did: String,
    #[serde(default)]
    pub commit_cid: String,
    #[serde(default)]
    pub height: i64,
    #[serde(default)]
    pub parent_commit_cids: Vec<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub wrapup_requested: bool,
    #[serde(default)]
    pub wrapup_completed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
    #[serde(default)]
    pub tokens_used: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

impl TimelineGoalVersionRow {
    pub(super) fn semantic_state(&self) -> TimelineGoalState {
        TimelineGoalState {
            status: self.status.clone(),
            wrapup_requested: self.wrapup_requested,
            wrapup_completed: self.wrapup_completed,
        }
    }
}

/// Never carries `request_json`: the fetch layer never selects the captured body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineRenderedRequestRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub capture_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_index: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineToolCallRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing)]
    pub request_doc_id: Option<String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_sequence: Option<i64>,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_call_id: String,
    #[serde(default)]
    pub args: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default, deserialize_with = "empty_string_if_null")]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_service_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_failure_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denial_reason: Option<String>,
    #[serde(default, deserialize_with = "empty_vec_if_null")]
    pub denied_argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_argument: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_subcommand: Option<String>,
    #[serde(default, deserialize_with = "empty_vec_if_null")]
    pub denied_prefix: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub await_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_cause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_request_id: Option<String>,
    /// Why a derived call (a script's child, a spawned process) has no
    /// arguments or result here: its canonical payload could not be read.
    /// The row keeps its lifecycle facts so one such call never hides a run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_unavailable: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompactionRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub compaction_key: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing)]
    pub request_doc_id: Option<String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub sequence: i64,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub messages_compacted: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacted_through_sequence: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_message_versions: Option<Vec<gents_protocol::transcript::TranscriptFactRef>>,
    #[serde(default)]
    pub original_tokens: i64,
    #[serde(default)]
    pub compacted_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineProviderContextReductionRow {
    #[serde(default, rename = "_docID")]
    pub doc_id: String,
    #[serde(default)]
    pub reduction_key: String,
    #[serde(default)]
    pub request_id: String,
    #[serde(default)]
    pub request_doc_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub reduction_index: i64,
    #[serde(default)]
    pub turn_index: i64,
    #[serde(default)]
    pub parent_reduction_key: Option<String>,
    #[serde(default)]
    pub producer_call_id: Option<String>,
    #[serde(default)]
    pub producer_call_seq: Option<i64>,
    #[serde(default)]
    pub messages_compacted: i64,
    #[serde(default)]
    pub original_tokens: i64,
    #[serde(default)]
    pub compacted_tokens: i64,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSessionRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(flatten)]
    pub session: gents_protocol::session::AgentSession,
}

fn empty_vec_if_null<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default())
}

fn empty_string_if_null<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}
