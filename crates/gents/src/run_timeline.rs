use std::collections::BTreeSet;

use chrono::DateTime;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunTimelineRows {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_manifest: Option<crate::run_timeline_manifest::RunTimelineSourceManifest>,
    pub request: TimelineRequestRow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<TimelineSessionRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<TimelineConversationRow>,
    #[serde(default)]
    pub requests: Vec<TimelineRequestRow>,
    #[serde(default)]
    pub messages: Vec<TimelineMessageRow>,
    #[serde(default)]
    pub tool_calls: Vec<TimelineToolCallRow>,
    #[serde(default)]
    pub inference_calls: Vec<TimelineInferenceCallRow>,
    #[serde(default)]
    pub rendered_requests: Vec<TimelineRenderedRequestRow>,
    #[serde(default)]
    pub response_outcomes: Vec<TimelineResponseOutcomeRow>,
    #[serde(default)]
    pub compaction_entries: Vec<TimelineCompactionEntryRow>,
    #[serde(default)]
    pub responses: Vec<TimelineResponseRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunTimeline {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<String>,
    pub request: TimelineRequestRow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_manifest: Option<crate::run_timeline_manifest::RunTimelineSourceManifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<TimelineSessionRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<TimelineConversationRow>,
    pub child_request_ids: Vec<String>,
    #[serde(default)]
    pub inference_calls: Vec<TimelineInferenceCallRow>,
    pub events: Vec<RunTimelineEvent>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineRequestRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt_requested_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by_parent_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by_parent_tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "RetrySummary::is_empty")]
    pub retry_summary: RetrySummary,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrySummary {
    pub retry_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_transient_error: Option<String>,
    pub recovered: bool,
}

impl RetrySummary {
    fn is_empty(&self) -> bool {
        self.retry_count == 0 && self.last_transient_error.is_none() && !self.recovered
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineMessageRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub sequence: i64,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineResponseRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized_message_sequence: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineInferenceCallRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub call_id: String,
    #[serde(default)]
    pub request_id: String,
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
    #[serde(default)]
    pub call_kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineToolCallRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
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
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_composite_commit_cid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_signer_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omission_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omission_composite_commit_cid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omission_signer_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_doc_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_composite_commit_cid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_signer_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_fact: Option<TimelineToolResultFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omission_fact: Option<TimelineToolOutputOmissionFact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_fact: Option<TimelineToolApprovalFact>,
    #[serde(default)]
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
    pub cancel_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_cause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineToolResultFact {
    pub doc_id: String,
    pub composite_commit_cid: String,
    pub signer_did: String,
    pub tool_call_doc_id: String,
    pub tool_call_composite_commit_cid: String,
    pub tool_call_signer_did: String,
    pub output_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineToolOutputOmissionFact {
    pub doc_id: String,
    pub composite_commit_cid: String,
    pub signer_did: String,
    pub tool_call_doc_id: String,
    pub tool_call_composite_commit_cid: String,
    pub tool_call_signer_did: String,
    pub source_phase: String,
    pub terminal_phase: String,
    pub reason: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineToolApprovalFact {
    pub doc_id: String,
    pub composite_commit_cid: String,
    pub signer_did: String,
    pub tool_call_doc_id: String,
    pub tool_call_composite_commit_cid: String,
    pub tool_call_signer_did: String,
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TimelineRenderedRequestRow {
    #[serde(flatten)]
    pub row: gents_protocol::row::RenderedRequestRow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<crate::SignedDocumentVersionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_json_field_cid: Option<String>,
}

impl Serialize for TimelineRenderedRequestRow {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut value = serde_json::to_value(&self.row).map_err(serde::ser::Error::custom)?;
        let object = value.as_object_mut().ok_or_else(|| {
            serde::ser::Error::custom("RenderedRequest metadata did not serialize as an object")
        })?;
        // Provider request bodies are deliberately outside ordinary timeline
        // and projection DTOs even if a future loader accidentally populates
        // the protocol row's privileged field.
        object.remove("request_json");
        if let Some(exact) = &self.exact {
            object.insert(
                "exact".to_string(),
                serde_json::to_value(exact).map_err(serde::ser::Error::custom)?,
            );
        }
        if let Some(field_cid) = &self.request_json_field_cid {
            object.insert(
                "request_json_field_cid".to_string(),
                serde_json::Value::String(field_cid.clone()),
            );
        }
        value.serialize(serializer)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineResponseOutcomeRow {
    #[serde(flatten)]
    pub row: gents_protocol::row::AgentResponseOutcomeRow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<crate::SignedDocumentVersionRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineCompactionEntryRow {
    #[serde(flatten)]
    pub row: gents_protocol::row::CompactionEntryRow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<crate::SignedDocumentVersionRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSessionRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineConversationRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunTimelineEvent {
    Request(TimelineRequestEvent),
    InferenceCall(TimelineInferenceCallEvent),
    RenderedRequest(TimelineRenderedRequestEvent),
    Compaction(TimelineCompactionEvent),
    Message(TimelineMessageEvent),
    ToolCall(TimelineToolCallEvent),
    Response(TimelineResponseEvent),
    ResponseOutcome(TimelineResponseOutcomeEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineRequestEvent {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lifecycle_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "RetrySummary::is_empty")]
    pub retry_summary: RetrySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineInferenceCallEvent {
    pub call_id: String,
    pub request_id: String,
    pub call_seq: i64,
    pub attempt: i64,
    pub call_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_id: Option<String>,
    pub call_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

/// Metadata-safe provider input fact. The canonical `request_json` body is
/// deliberately absent; callers need a separate explicit sensitive-body read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineRenderedRequestEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact: Option<crate::SignedDocumentVersionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_json_field_cid: Option<String>,
    pub capture_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_call_doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_call_composite_commit_cid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_scope_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_scope_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_index: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_manifest_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCompactionEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact: Option<crate::SignedDocumentVersionRef>,
    pub compaction_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_manifest_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_manifest_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineMessageEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub session_id: String,
    pub sequence: i64,
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
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
    pub result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_fact: Option<TimelineToolResultFact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omission_fact: Option<TimelineToolOutputOmissionFact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_fact: Option<TimelineToolApprovalFact>,
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
    pub cancel_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_cause: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineResponseEvent {
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub materialized_message_sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineResponseOutcomeEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exact: Option<crate::SignedDocumentVersionRef>,
    pub request_doc_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_message_doc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_message_composite_commit_cid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_message_sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

pub fn build_run_timeline(mut rows: RunTimelineRows) -> RunTimeline {
    let source_manifest = rows.source_manifest.take();
    let root_request_id = rows.request.request_id.clone();
    let session_id = rows.request.session_id.clone();
    let mut included_request_ids = BTreeSet::from([root_request_id.clone()]);
    for request in &rows.requests {
        if request
            .caused_by_parent_request_id
            .as_deref()
            .is_some_and(|parent| parent == root_request_id)
        {
            included_request_ids.insert(request.request_id.clone());
        }
    }
    for tool_call in &rows.tool_calls {
        if let Some(child_request_id) = nonempty(tool_call.child_request_id.as_deref()) {
            included_request_ids.insert(child_request_id.to_string());
        }
    }
    let inference_calls = sorted_inference_calls(rows.inference_calls, &included_request_ids);
    apply_retry_summaries(&mut rows.request, &mut rows.requests, &inference_calls);

    let mut events = Vec::new();
    push_request_event(&mut events, &rows.request);
    for request in &rows.requests {
        if request.request_id != root_request_id
            && included_request_ids.contains(&request.request_id)
        {
            push_request_event(&mut events, request);
        }
    }

    for call in &inference_calls {
        events.push(RunTimelineEvent::InferenceCall(
            TimelineInferenceCallEvent {
                call_id: call.call_id.clone(),
                request_id: call.request_id.clone(),
                call_seq: call.call_seq,
                attempt: call.attempt,
                call_state: call.call_state.clone(),
                failure_reason: call.failure_reason.clone(),
                backend_id: call.backend_id.clone(),
                call_kind: call.call_kind.clone(),
                queued_at: call.queued_at.clone(),
                started_at: call.started_at.clone(),
                ended_at: call.ended_at.clone(),
            },
        ));
    }

    for rendered in &rows.rendered_requests {
        let row = &rendered.row;
        if should_include_event(
            nonempty(row.request_id.as_deref()),
            nonempty(row.session_id.as_deref()),
            &included_request_ids,
            session_id.as_deref(),
        ) {
            let scope = row.parsed_capture_scope();
            let (manifest_version, provenance_status) = rendered_provenance_metadata(row);
            events.push(RunTimelineEvent::RenderedRequest(
                TimelineRenderedRequestEvent {
                    doc_id: row.doc_id.clone(),
                    exact: rendered.exact.clone(),
                    request_json_field_cid: rendered.request_json_field_cid.clone(),
                    capture_key: row.capture_key.clone(),
                    request_id: row.request_id.clone(),
                    request_doc_id: row.request_doc_id.clone(),
                    inference_call_doc_id: row.inference_call_doc_id.clone(),
                    inference_call_composite_commit_cid: row
                        .inference_call_composite_commit_cid
                        .clone(),
                    capture_scope: row.capture_scope.clone(),
                    capture_scope_kind: scope
                        .as_ref()
                        .map(|scope| scope.kind().as_str().to_string()),
                    capture_scope_sequence: scope.as_ref().and_then(|scope| scope.sequence()),
                    turn_index: row.turn_index,
                    attempt: row.attempt,
                    capture_version: row.capture_version,
                    provenance_manifest_version: manifest_version,
                    provenance_status,
                    model_name: row.model_name.clone(),
                    source: row.source.clone(),
                    prompt_hash: row.prompt_hash.clone(),
                    tools_hash: row.tools_hash.clone(),
                    timestamp: row.created_at.clone(),
                },
            ));
        }
    }

    for compaction in &rows.compaction_entries {
        let row = &compaction.row;
        if should_include_event(
            None,
            nonempty(row.session_id.as_deref()),
            &included_request_ids,
            session_id.as_deref(),
        ) {
            events.push(RunTimelineEvent::Compaction(TimelineCompactionEvent {
                doc_id: row.doc_id.clone(),
                exact: compaction.exact.clone(),
                compaction_key: row.compaction_key.clone(),
                session_id: row.session_id.clone(),
                sequence: row.sequence,
                source_manifest_version: row.source_manifest_version,
                source_manifest_json: row.source_manifest_json.clone(),
                summary: row.summary.clone(),
                timestamp: row.created_at.clone(),
            }));
        }
    }

    for message in &rows.messages {
        let request_id = nonempty(message.request_id.as_deref())
            .map(ToOwned::to_owned)
            .or_else(|| {
                infer_request_id_for_message(message, &rows.responses, &rows.requests)
                    .map(ToOwned::to_owned)
            });
        if should_include_event(
            request_id.as_deref(),
            Some(message.session_id.as_str()),
            &included_request_ids,
            session_id.as_deref(),
        ) {
            events.push(RunTimelineEvent::Message(TimelineMessageEvent {
                request_id,
                session_id: message.session_id.clone(),
                sequence: message.sequence,
                role: message.role.clone(),
                content: message.content.clone(),
                timestamp: message.timestamp.clone(),
            }));
        }
    }

    for tool_call in &rows.tool_calls {
        let request_id = infer_request_id_for_tool_call(tool_call, &rows.requests, &rows.responses);
        if should_include_event(
            request_id.as_deref(),
            Some(tool_call.session_id.as_str()),
            &included_request_ids,
            session_id.as_deref(),
        ) {
            events.push(RunTimelineEvent::ToolCall(TimelineToolCallEvent {
                request_id,
                session_id: tool_call.session_id.clone(),
                message_sequence: tool_call.message_sequence,
                tool_name: tool_call.tool_name.clone(),
                tool_call_id: tool_call.tool_call_id.clone(),
                args: tool_call.args.clone(),
                result: match (&tool_call.result_fact, &tool_call.omission_fact) {
                    (Some(result), None) => result.output_text.clone(),
                    (None, Some(_)) => String::new(),
                    // Non-terminal rows may only have the mutable streaming
                    // preview. Terminal exactness is enforced while facts are
                    // attached by the timeline fetcher.
                    (None, None) => tool_call.result.clone(),
                    (Some(_), Some(_)) => String::new(),
                },
                result_fact: tool_call.result_fact.clone(),
                omission_fact: tool_call.omission_fact.clone(),
                approval_fact: tool_call.approval_fact.clone(),
                status: tool_call.status.clone(),
                lifecycle_state: tool_call.lifecycle_state.clone(),
                selected_service_id: tool_call.selected_service_id.clone(),
                selected_tool_name: tool_call.selected_tool_name.clone(),
                tool_failure_class: tool_call.tool_failure_class.clone(),
                denial_reason: tool_call.denial_reason.clone(),
                denied_argv: tool_call.denied_argv.clone(),
                denied_command: tool_call.denied_command.clone(),
                denied_argument: tool_call.denied_argument.clone(),
                denied_subcommand: tool_call.denied_subcommand.clone(),
                denied_prefix: tool_call.denied_prefix.clone(),
                policy_mode: tool_call.policy_mode.clone(),
                policy_network: tool_call.policy_network.clone(),
                latency_ms: tool_call.latency_ms,
                await_mode: tool_call.await_mode.clone(),
                cancel_policy: tool_call.cancel_policy.clone(),
                cancel_cause: tool_call.cancel_cause.clone(),
                child_request_id: tool_call.child_request_id.clone(),
                started_at: tool_call.started_at.clone(),
                completed_at: tool_call.completed_at.clone(),
            }));
        }
    }

    for response in &rows.responses {
        if included_request_ids.contains(&response.request_id) {
            events.push(RunTimelineEvent::Response(TimelineResponseEvent {
                request_id: response.request_id.clone(),
                session_id: response.session_id.clone(),
                status: response.status.clone(),
                content: response.content.clone(),
                reasoning: response.reasoning.clone(),
                error_message: response.error_message.clone(),
                materialized_message_sequence: response.materialized_message_sequence,
                timestamp: first_nonempty([
                    response.completed_at.as_deref(),
                    response.materialized_at.as_deref(),
                    response.created_at.as_deref(),
                ])
                .map(ToOwned::to_owned),
            }));
        }
    }

    for outcome in &rows.response_outcomes {
        let row = &outcome.row;
        if row
            .request_id
            .as_deref()
            .is_some_and(|request_id| included_request_ids.contains(request_id))
            || rows.request.doc_id.as_deref() == Some(row.request_doc_id.as_str())
        {
            events.push(RunTimelineEvent::ResponseOutcome(
                TimelineResponseOutcomeEvent {
                    doc_id: row.doc_id.clone(),
                    exact: outcome.exact.clone(),
                    request_doc_id: row.request_doc_id.clone(),
                    request_id: row.request_id.clone(),
                    session_id: row.session_id.clone(),
                    outcome_kind: row.outcome_kind.clone(),
                    reason_code: row.reason_code.clone(),
                    final_message_doc_id: row.final_message_doc_id.clone(),
                    final_message_composite_commit_cid: row
                        .final_message_composite_commit_cid
                        .clone(),
                    final_message_sequence: row.final_message_sequence,
                    timestamp: row.terminalized_at.clone(),
                },
            ));
        }
    }

    events.sort_by_key(event_sort_key);
    let child_request_ids = child_request_ids(&included_request_ids, &root_request_id);

    RunTimeline {
        request_id: root_request_id,
        session_id,
        agent_did: first_owned([
            rows.request.agent_did.as_deref(),
            rows.conversation
                .as_ref()
                .and_then(|conversation| conversation.agent_did.as_deref()),
        ]),
        behavior_id: first_owned([
            rows.request.behavior_id.as_deref(),
            rows.conversation
                .as_ref()
                .and_then(|conversation| conversation.behavior_id.as_deref()),
            rows.session
                .as_ref()
                .and_then(|session| session.behavior_id.as_deref()),
        ]),
        request: rows.request,
        source_manifest,
        session: rows.session,
        conversation: rows.conversation,
        child_request_ids,
        inference_calls,
        events,
    }
}

fn sorted_inference_calls(
    inference_calls: Vec<TimelineInferenceCallRow>,
    included_request_ids: &BTreeSet<String>,
) -> Vec<TimelineInferenceCallRow> {
    let mut inference_calls = inference_calls
        .into_iter()
        .filter(|call| included_request_ids.contains(&call.request_id))
        .collect::<Vec<_>>();
    inference_calls.sort_by_key(|call| {
        (
            call.request_id.clone(),
            call.call_seq,
            call.queued_at
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            call.call_id.clone(),
        )
    });
    inference_calls
}

fn apply_retry_summaries(
    root_request: &mut TimelineRequestRow,
    requests: &mut [TimelineRequestRow],
    inference_calls: &[TimelineInferenceCallRow],
) {
    root_request.retry_summary = retry_summary_for_request(root_request, inference_calls);
    for request in requests {
        request.retry_summary = retry_summary_for_request(request, inference_calls);
    }
}

fn retry_summary_for_request(
    request: &TimelineRequestRow,
    inference_calls: &[TimelineInferenceCallRow],
) -> RetrySummary {
    let request_calls = inference_calls
        .iter()
        .filter(|call| call.request_id == request.request_id)
        .filter(|call| call.call_kind == "inference")
        .collect::<Vec<_>>();

    let mut retry_count = 0;
    let mut last_transient_error = None;
    for (index, call) in request_calls.iter().enumerate() {
        if call.call_state == "failed" && index + 1 < request_calls.len() {
            retry_count += 1;
            last_transient_error = call.failure_reason.clone();
        }
    }

    RetrySummary {
        retry_count,
        last_transient_error,
        recovered: request_completed(request) && retry_count > 0,
    }
}

fn request_completed(request: &TimelineRequestRow) -> bool {
    [
        request.lifecycle_state.as_deref(),
        request.status.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|state| state == "completed")
}

fn push_request_event(events: &mut Vec<RunTimelineEvent>, request: &TimelineRequestRow) {
    events.push(RunTimelineEvent::Request(TimelineRequestEvent {
        request_id: request.request_id.clone(),
        parent_request_id: request.caused_by_parent_request_id.clone(),
        parent_tool_call_id: request.caused_by_parent_tool_call_id.clone(),
        agent_did: request.agent_did.clone(),
        behavior_id: request.behavior_id.clone(),
        session_id: request.session_id.clone(),
        status: request.status.clone(),
        lifecycle_state: request.lifecycle_state.clone(),
        failure_reason: request.failure_reason.clone(),
        metadata: request.metadata.clone(),
        timestamp: request.created_at.clone(),
        retry_summary: request.retry_summary.clone(),
    }));
}

fn infer_request_id_for_message<'a>(
    message: &TimelineMessageRow,
    responses: &'a [TimelineResponseRow],
    requests: &'a [TimelineRequestRow],
) -> Option<&'a str> {
    responses
        .iter()
        .filter(|response| response.session_id.as_deref() == Some(message.session_id.as_str()))
        .filter_map(|response| {
            let materialized = response.materialized_message_sequence?;
            (materialized >= message.sequence)
                .then_some((response.request_id.as_str(), materialized))
        })
        .min_by_key(|(_, materialized)| *materialized)
        .map(|(request_id, _)| request_id)
        .or_else(|| {
            let mut session_requests = requests.iter().filter(|request| {
                request.session_id.as_deref() == Some(message.session_id.as_str())
            });
            let request = session_requests.next()?;
            session_requests
                .next()
                .is_none()
                .then_some(request.request_id.as_str())
        })
}

fn infer_request_id_for_tool_call(
    tool_call: &TimelineToolCallRow,
    requests: &[TimelineRequestRow],
    responses: &[TimelineResponseRow],
) -> Option<String> {
    if let Some(request_id) = nonempty(tool_call.request_id.as_deref()) {
        return Some(request_id.to_string());
    }

    if let Some(sequence) = tool_call.message_sequence {
        if let Some(response) = responses
            .iter()
            .filter(|response| {
                response.session_id.as_deref() == Some(tool_call.session_id.as_str())
            })
            .filter_map(|response| {
                let materialized = response.materialized_message_sequence?;
                (materialized >= sequence).then_some((response.request_id.as_str(), materialized))
            })
            .min_by_key(|(_, materialized)| *materialized)
            .map(|(request_id, _)| request_id)
        {
            return Some(response.to_string());
        }
    }

    if let Some(started_at) = tool_call.started_at.as_deref().and_then(timestamp_millis) {
        if let Some(request_id) = requests
            .iter()
            .filter(|request| request.session_id.as_deref() == Some(tool_call.session_id.as_str()))
            .filter_map(|request| {
                let created_at = request.created_at.as_deref().and_then(timestamp_millis)?;
                (created_at <= started_at).then_some((request.request_id.as_str(), created_at))
            })
            .max_by_key(|(_, created_at)| *created_at)
            .map(|(request_id, _)| request_id)
        {
            return Some(request_id.to_string());
        }
    }

    let mut session_requests = requests
        .iter()
        .filter(|request| request.session_id.as_deref() == Some(tool_call.session_id.as_str()));
    let request = session_requests.next()?;
    session_requests
        .next()
        .is_none()
        .then(|| request.request_id.clone())
}

fn should_include_event(
    request_id: Option<&str>,
    event_session_id: Option<&str>,
    included_request_ids: &BTreeSet<String>,
    root_session_id: Option<&str>,
) -> bool {
    request_id
        .map(|request_id| included_request_ids.contains(request_id))
        .unwrap_or_else(|| event_session_id.is_some() && event_session_id == root_session_id)
}

fn event_sort_key(event: &RunTimelineEvent) -> (i64, i64, i64, String) {
    match event {
        RunTimelineEvent::Request(event) => (
            event
                .timestamp
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            0,
            -1,
            event.request_id.clone(),
        ),
        RunTimelineEvent::InferenceCall(event) => (
            first_nonempty([
                event.queued_at.as_deref(),
                event.started_at.as_deref(),
                event.ended_at.as_deref(),
            ])
            .and_then(timestamp_millis)
            .unwrap_or(i64::MIN),
            1,
            event.call_seq,
            event.call_id.clone(),
        ),
        RunTimelineEvent::RenderedRequest(event) => (
            event
                .timestamp
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            2,
            event.turn_index.unwrap_or(i64::MAX),
            event.capture_key.clone(),
        ),
        RunTimelineEvent::Compaction(event) => (
            event
                .timestamp
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            3,
            event.sequence.unwrap_or(i64::MAX),
            event.compaction_key.clone(),
        ),
        RunTimelineEvent::Message(event) => (
            event
                .timestamp
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            4,
            event.sequence,
            format!("{}:{}", event.session_id, event.sequence),
        ),
        RunTimelineEvent::ToolCall(event) => (
            event
                .started_at
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            5,
            event.message_sequence.unwrap_or(i64::MAX),
            event.tool_call_id.clone(),
        ),
        RunTimelineEvent::Response(event) => (
            event
                .timestamp
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            6,
            event.materialized_message_sequence.unwrap_or(i64::MAX),
            event.request_id.clone(),
        ),
        RunTimelineEvent::ResponseOutcome(event) => (
            event
                .timestamp
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            7,
            event.final_message_sequence.unwrap_or(i64::MAX),
            event.request_doc_id.clone(),
        ),
    }
}

fn rendered_provenance_metadata(
    row: &gents_protocol::row::RenderedRequestRow,
) -> (Option<u64>, Option<String>) {
    let Some(raw) = nonempty(row.provenance_json.as_deref()) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return (None, Some("unsupported_manifest".to_string()));
    };
    (
        value
            .get("manifest_version")
            .and_then(serde_json::Value::as_u64),
        value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
    )
}

fn child_request_ids(
    included_request_ids: &BTreeSet<String>,
    root_request_id: &str,
) -> Vec<String> {
    included_request_ids
        .iter()
        .filter(|request_id| request_id.as_str() != root_request_id)
        .cloned()
        .collect()
}

fn first_owned<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    first_nonempty(values).map(ToOwned::to_owned)
}

fn first_nonempty<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<&'a str> {
    values
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn timestamp_millis(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn empty_vec_if_null<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(deserializer)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ordered_timeline_with_inferred_tool_call_and_child_request() {
        let rows = RunTimelineRows {
            request: TimelineRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:test:amy".to_string()),
                behavior_id: Some("amy".to_string()),
                session_id: Some("session-1".to_string()),
                lifecycle_state: Some("complete".to_string()),
                created_at: Some("2026-05-04T12:00:01Z".to_string()),
                ..Default::default()
            },
            requests: vec![
                TimelineRequestRow {
                    request_id: "req-1".to_string(),
                    session_id: Some("session-1".to_string()),
                    created_at: Some("2026-05-04T12:00:01Z".to_string()),
                    ..Default::default()
                },
                TimelineRequestRow {
                    request_id: "child-1".to_string(),
                    session_id: Some("session-2".to_string()),
                    caused_by_parent_request_id: Some("req-1".to_string()),
                    caused_by_parent_tool_call_id: Some("call-delegate".to_string()),
                    created_at: Some("2026-05-04T12:00:04Z".to_string()),
                    ..Default::default()
                },
            ],
            messages: vec![TimelineMessageRow {
                doc_id: None,
                session_id: "session-1".to_string(),
                request_id: Some("req-1".to_string()),
                request_doc_id: Some("request-doc-1".to_string()),
                agent_did: Some("did:test:amy".to_string()),
                sequence: 2,
                role: "assistant".to_string(),
                content: "calling tool".to_string(),
                timestamp: Some("2026-05-04T12:00:02Z".to_string()),
            }],
            tool_calls: vec![TimelineToolCallRow {
                session_id: "session-1".to_string(),
                message_sequence: Some(2),
                tool_name: "delegate".to_string(),
                tool_call_id: "call-delegate".to_string(),
                status: "completed".to_string(),
                child_request_id: Some("child-1".to_string()),
                denial_reason: Some("not allowed".to_string()),
                started_at: Some("2026-05-04T12:00:03Z".to_string()),
                completed_at: Some("2026-05-04T12:00:03.500Z".to_string()),
                ..Default::default()
            }],
            responses: vec![TimelineResponseRow {
                request_id: "req-1".to_string(),
                session_id: Some("session-1".to_string()),
                status: Some("completed".to_string()),
                materialized_message_sequence: Some(4),
                completed_at: Some("2026-05-04T12:00:05Z".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };

        let timeline = build_run_timeline(rows);

        assert_eq!(timeline.request_id, "req-1");
        assert_eq!(timeline.child_request_ids, vec!["child-1"]);
        assert_eq!(timeline.events.len(), 5);
        assert!(matches!(timeline.events[0], RunTimelineEvent::Request(_)));
        assert!(matches!(timeline.events[1], RunTimelineEvent::Message(_)));
        assert!(matches!(timeline.events[2], RunTimelineEvent::ToolCall(_)));
        assert!(matches!(timeline.events[3], RunTimelineEvent::Request(_)));
        assert!(matches!(timeline.events[4], RunTimelineEvent::Response(_)));
        let RunTimelineEvent::ToolCall(tool) = &timeline.events[2] else {
            panic!("expected tool call");
        };
        assert_eq!(tool.request_id.as_deref(), Some("req-1"));
        assert_eq!(tool.child_request_id.as_deref(), Some("child-1"));
        assert_eq!(tool.denial_reason.as_deref(), Some("not allowed"));
    }

    #[test]
    fn retry_summary_counts_failed_attempt_rows_with_successors() {
        let rows = RunTimelineRows {
            request: TimelineRequestRow {
                request_id: "req-recovered".to_string(),
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                created_at: Some("2026-05-04T12:00:00Z".to_string()),
                ..Default::default()
            },
            inference_calls: vec![
                inference_call("req-recovered", 3, "completed", None),
                inference_call("req-recovered", 1, "failed", Some("first transient")),
                inference_call("req-recovered", 2, "failed", Some("second transient")),
            ],
            ..Default::default()
        };

        let timeline = build_run_timeline(rows);

        assert_eq!(
            timeline.request.retry_summary,
            RetrySummary {
                retry_count: 2,
                last_transient_error: Some("second transient".to_string()),
                recovered: true,
            }
        );
        assert_eq!(
            timeline
                .inference_calls
                .iter()
                .map(|call| call.call_seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        let retry_summary = timeline.events.iter().find_map(|event| match event {
            RunTimelineEvent::Request(request) => Some(&request.retry_summary),
            _ => None,
        });
        assert_eq!(retry_summary, Some(&timeline.request.retry_summary));
    }

    #[test]
    fn retry_summary_excludes_terminal_failed_attempt_without_successor() {
        let rows = RunTimelineRows {
            request: TimelineRequestRow {
                request_id: "req-failed".to_string(),
                status: Some("failed".to_string()),
                lifecycle_state: Some("error".to_string()),
                created_at: Some("2026-05-04T12:00:00Z".to_string()),
                ..Default::default()
            },
            inference_calls: vec![
                inference_call("req-failed", 1, "failed", Some("retried transient")),
                inference_call("req-failed", 2, "failed", Some("terminal provider error")),
            ],
            ..Default::default()
        };

        let timeline = build_run_timeline(rows);

        assert_eq!(
            timeline.request.retry_summary,
            RetrySummary {
                retry_count: 1,
                last_transient_error: Some("retried transient".to_string()),
                recovered: false,
            }
        );
    }

    #[test]
    fn rendered_request_metadata_serialization_never_exposes_provider_body() {
        let row = TimelineRenderedRequestRow {
            row: gents_protocol::row::RenderedRequestRow {
                doc_id: Some("render-doc".to_string()),
                capture_key: "request:turn:0".to_string(),
                request_doc_id: Some("request-doc".to_string()),
                request_source_commit_cid: None,
                request_source_signer_did: None,
                request_claim_commit_cid: None,
                request_claim_signer_did: None,
                inference_call_doc_id: None,
                inference_call_composite_commit_cid: None,
                inference_call_signer_did: None,
                request_id: Some("request-id".to_string()),
                session_id: Some("session-id".to_string()),
                agent_did: Some("did:key:agent".to_string()),
                requester_did: None,
                behavior_id: Some("general".to_string()),
                capture_scope: Some("request".to_string()),
                turn_index: Some(0),
                attempt: Some(1),
                capture_version: Some(1),
                model_name: Some("model".to_string()),
                source: Some("completion_loop".to_string()),
                request_json: Some("{\"secret\":\"provider-body\"}".to_string()),
                prompt_hash: Some("prompt-hash".to_string()),
                tools_hash: Some("tools-hash".to_string()),
                provenance_json: None,
                created_at: Some("2026-08-08T00:00:00Z".to_string()),
            },
            exact: None,
            request_json_field_cid: Some("field-cid".to_string()),
        };
        let encoded = serde_json::to_string(&row).unwrap();
        assert!(!encoded.contains("\"request_json\":"));
        assert!(!encoded.contains("provider-body"));
        assert!(encoded.contains("request_json_field_cid"));
    }

    #[test]
    fn omission_fact_is_exported_without_fabricating_tool_output() {
        let omission = TimelineToolOutputOmissionFact {
            doc_id: "omission-doc".to_string(),
            composite_commit_cid: "omission-cid".to_string(),
            signer_did: "did:key:agent".to_string(),
            tool_call_doc_id: "tool-call-doc".to_string(),
            tool_call_composite_commit_cid: "tool-call-source-cid".to_string(),
            tool_call_signer_did: "did:key:agent".to_string(),
            source_phase: "running".to_string(),
            terminal_phase: "failed".to_string(),
            reason: "executionLost".to_string(),
            detail: "worker disappeared".to_string(),
            created_at: Some("2026-08-08T00:00:00Z".to_string()),
        };
        let timeline = build_run_timeline(RunTimelineRows {
            request: TimelineRequestRow {
                request_id: "request".to_string(),
                session_id: Some("session".to_string()),
                ..Default::default()
            },
            tool_calls: vec![TimelineToolCallRow {
                doc_id: Some("tool-call-doc".to_string()),
                request_id: Some("request".to_string()),
                session_id: "session".to_string(),
                tool_call_id: "call".to_string(),
                tool_name: "worker".to_string(),
                result: "synthetic failure text must not become output".to_string(),
                lifecycle_state: Some("failed".to_string()),
                omission_fact: Some(omission.clone()),
                ..Default::default()
            }],
            ..Default::default()
        });

        let event = timeline.events.iter().find_map(|event| match event {
            RunTimelineEvent::ToolCall(event) => Some(event),
            _ => None,
        });
        let event = event.expect("tool event");
        assert_eq!(event.result, "");
        assert_eq!(event.result_fact, None);
        assert_eq!(event.omission_fact.as_ref(), Some(&omission));
        let json = serde_json::to_value(event).expect("serialize tool event");
        assert!(json.get("omission_fact").is_some());
        assert!(json.get("result_fact").is_none());
    }

    fn inference_call(
        request_id: &str,
        call_seq: i64,
        call_state: &str,
        failure_reason: Option<&str>,
    ) -> TimelineInferenceCallRow {
        TimelineInferenceCallRow {
            call_id: format!("call-{call_seq}"),
            request_id: request_id.to_string(),
            call_seq,
            attempt: 1,
            call_state: call_state.to_string(),
            failure_reason: failure_reason.map(ToOwned::to_owned),
            queued_at: Some(format!("2026-05-04T12:00:0{call_seq}Z")),
            backend_id: Some("backend-a".to_string()),
            call_kind: "inference".to_string(),
            ..Default::default()
        }
    }
}
