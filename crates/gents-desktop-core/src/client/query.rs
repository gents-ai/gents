use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use gents::document_config::{
    AgentBehavior, AgentContext, AgentPrincipal, ChainKeyBindingDocument, CompactionConfig,
    DatastoreToolSurfaceDocument, EventSource, InferenceBackend, InferenceBackendObservation,
    InferenceExecution, InferenceProfile, InferenceSampling, Schedule, ScheduleObservation,
    SkillDocument, SubagentTargetDocument, Task, ToolServiceRegistry, Tools, Trigger,
    TriggerObservation,
};
use gents_protocol::graphql::escape_graphql_string;
use gents_protocol::row::{
    AgentBehaviorReadinessRow, AgentMessageRow, AgentRequestRow, AgentResponseRow, AgentRuntimeRow,
    AgentToolCallRow, AgentToolResultRow, CompactionEntryRow, GoalRow, MailboxItemRow,
};
use gents_protocol::schemas::{
    AGENT_BEHAVIOR_NAME, AGENT_BEHAVIOR_READINESS_NAME, AGENT_MESSAGE_NAME, AGENT_PRINCIPAL_NAME,
    AGENT_REQUEST_NAME, AGENT_RESPONSE_NAME, AGENT_RUNTIME_NAME, AGENT_SESSION_NAME,
    AGENT_TOOL_CALL_NAME, AGENT_TOOL_RESULT_NAME, COMPACTION_ENTRY_NAME, GOAL_NAME,
    INFERENCE_BACKEND_NAME, INFERENCE_PROFILE_NAME, MAILBOX_ITEM_NAME, SCHEDULE_NAME, SKILL_NAME,
    TASK_NAME, TOOLS_NAME, TOOL_SERVICE_REGISTRY_NAME, TRIGGER_NAME,
};
use gents_protocol::session::AgentSession;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

use super::peer_directory::PeerRecord;
use super::store::{ClientStore, ClientStoreRows};

mod agent_scope;
mod document_patches;
mod session_transcript;
mod snapshot_loaders;

pub use agent_scope::load_agent_scoped_snapshot;
pub use document_patches::fetch_doc_patch;
pub(crate) use document_patches::{
    is_transcript_content_collection, supports_doc_patch_collection,
};
#[cfg(test)]
use session_transcript::tool_group_cursor_sequence;
pub use session_transcript::{
    load_session_context_store, load_session_diagnostics_store, load_session_transcript_page,
};
pub(crate) use snapshot_loaders::*;

pub const DEFAULT_SESSION_TRANSCRIPT_PAGE_SIZE: usize = 40;
pub const MAX_SESSION_TRANSCRIPT_PAGE_SIZE: usize = 80;
pub(super) const SESSION_TRANSCRIPT_TOOL_CALL_ROW_BUDGET: usize = 320;

#[derive(Debug)]
pub struct SessionTranscriptQueryPage {
    pub store: ClientStore,
    pub query_count: u64,
    pub queried_rows: usize,
    pub message_query_limit: usize,
    pub tool_call_query_limit: usize,
    pub source_exhausted: bool,
    pub has_newer: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct TranscriptCursorRow {
    pub(super) sequence: Option<i64>,
}

pub(super) const AGENT_PRINCIPAL_FIELDS: &str =
    "agent_did display_name default_behavior_id enabled created_at created_by tags";
pub(super) const AGENT_BEHAVIOR_FIELDS: &str = "behavior_id agent_did display_name description context_id inference_profile_id enabled tags created_at";
pub(super) const AGENT_RUNTIME_FIELDS: &str = "agent_did reconcile_phase behavior_executor_capacity behavior_executor_queue_depth behavior_executor_status_json last_reconcile_result last_reconcile_error last_reconcile_completed_at updated_at";
pub(super) const AGENT_BEHAVIOR_READINESS_FIELDS: &str = "agent_did snapshot_json updated_at";
pub(super) const AGENT_REQUEST_FIELDS: &str = "_docID request_id agent_did requester_did behavior_id session_id retry_parent_request retry_root_request superseded_by_request content max_total_tokens input lifecycle_state backend_id execution_origin caused_by_trigger_id caused_by_trigger_kind caused_by_correlation caused_by_trigger_context caused_by_source_doc_id caused_by_parent_request_id failure_reason terminalized_at terminal_redrive_attempts created_at claimed_at deadline retry_count max_retries interrupt_requested_at valid_until workspace_id workspace_authority workspace_owner_agent_did workspace_seal_hash";
pub(super) const AGENT_RESPONSE_FIELDS: &str = "response_key request_id request_doc_id agent_did requester_did behavior_id session_id content reasoning status error_message token_count progress_seq reasoning_progress_seq materialized_message_sequence materialized_at created_at completed_at interrupted_at";
pub(super) const AGENT_MESSAGE_FIELDS: &str =
    "message_key session_id request_id requester_did sequence role content reasoning timestamp";
pub(super) const AGENT_SESSION_FIELDS: &str = "session_id agent_did requester_did behavior_id created_at closed_at title tags provenance observation";
pub(super) const GOAL_FIELDS: &str = "goal_id session_id agent_did creation_key objective status token_budget tokens_used active_time_seconds active_started_at consecutive_blocked_audits last_blocked_request_id last_blocked_reason last_continued_from_request_id continuation_sequence wrapup_requested wrapup_completed infrastructure_retry_count last_failure completion_evidence created_at updated_at";
pub(super) const AGENT_TOOL_CALL_FIELDS: &str = "tool_call_key session_id request_id requester_did message_sequence tool_name tool_call_id args result status lifecycle_state child_request_id await_mode cancel_policy deadline_at cancel_cause started_at completed_at selected_service_id selected_tool_name tool_failure_class denial_reason denied_argv denied_command denied_argument denied_subcommand denied_prefix policy_mode policy_network latency_ms partial_output_tail partial_output_seq";
pub(super) const AGENT_TOOL_RESULT_FIELDS: &str = "_docID agent_did requester_did session_id tool_name tool_input output_text truncated truncation_metadata tool_call_doc_id created_at discarded_because_interrupted";
pub(super) const COMPACTION_ENTRY_FIELDS: &str = "compaction_key session_id requester_did sequence summary files_read files_modified messages_compacted compacted_through_sequence original_tokens compacted_tokens created_at";
pub(super) const TASK_FIELDS: &str = "task_id agent_did display_name description behavior_id prompt_template goal_objective_template goal_token_budget hooks enabled output_schema_ref created_at updated_at tags";
pub(super) const SKILL_FIELDS: &str = "skill_id agent_did name description instructions tool_refs display_name interface_json enabled created_at tags";
pub(super) const SCHEDULE_FIELDS: &str =
    "schedule_id agent_did display_name cadence created_at updated_at tags";
pub(super) const SCHEDULE_OBSERVATION_FIELDS: &str = "trigger_id next_run_at";
pub(super) const TRIGGER_FIELDS: &str = "agent_did trigger_id display_name description task_id source enabled concurrency created_at updated_at tags";
pub(super) const TRIGGER_OBSERVATION_FIELDS: &str =
    "trigger_id last_attempt_at last_fired_source_doc_id last_status last_error fire_count";
pub(super) const TOOLS_FIELDS: &str = "tools_id agent_did display_name host remote subagents built_ins datastore integrations self_config tags";
pub(super) const AGENT_CONTEXT_FIELDS: &str = "context_id agent_did display_name description system_prompt tools_id compaction_id skill_ids tags";
pub(super) const COMPACTION_CONFIG_FIELDS: &str = "compaction_id agent_did display_name strategy threshold keep_recent_tokens tool_result_max_chars summary_max_output_tokens summary_file_list_max inference_profile_id tags";
pub(super) const INFERENCE_BACKEND_FIELDS: &str = "backend_id agent_did name provider_kind openai_wire_api endpoint auth connect_timeout_secs discovery_timeout_secs max_concurrent max_queue_depth enabled tags";
pub(super) const INFERENCE_BACKEND_OBSERVATION_FIELDS: &str =
    "backend_id catalogs last_probe probe_status";
pub(super) const INFERENCE_PROFILE_FIELDS: &str = "profile_id agent_did display_name description backend_id model_name reasoning_effort context_window max_output_tokens sampling_id execution_id tags";
pub(super) const INFERENCE_SAMPLING_FIELDS: &str = "sampling_id agent_did display_name temperature top_p top_k seed min_p frequency_penalty presence_penalty repetition_penalty tags";
pub(super) const INFERENCE_EXECUTION_FIELDS: &str = "execution_id agent_did display_name max_turns max_total_tokens stream_batch_ms stream_liveness_timeout_secs deadline_duration_secs retry_policy_id tags";
pub(super) const TOOL_SERVICE_REGISTRY_FIELDS: &str = "service_id agent_did display_name description hostname tailscale_ip lan_ip mcp_port mcp_path send_agent_did enabled tags";
pub(super) const EVENT_SOURCE_FIELDS: &str = "event_source_id agent_did display_name source_collection event_kind filter correlation_field group workspace_authority created_at updated_at tags";
pub(super) const SUBAGENT_TARGET_FIELDS: &str =
    "target_id agent_did target_agent_did behavior_id name description tags";
pub(super) const DATASTORE_TOOL_SURFACE_FIELDS: &str =
    "surface_id agent_did display_name enabled entries created_at tags";
pub(super) const CHAIN_KEY_BINDING_FIELDS: &str =
    "binding_id agent_did address key_backend attestation created_at revoked_at tags";
pub(super) const MAILBOX_ITEM_FIELDS: &str = "_docID item_key requester_did agent_did status kind action title summary payload source_kind source_id session_id request_id graph_run_id cause_doc_id target_agent_did target_behavior_id expected_collection parent_item_id deadline_at created_at updated_at resolved_at resolved_doc_id";

/// Load only the selected request's session transcript slice from the embedded
/// replica. This is the bounded polling fallback for a dropped/coalesced
/// observer event; it does not reload every session for the agent.
pub async fn load_chat_patch(node: &EmbeddedNode, request_id: &str) -> Result<ClientStore> {
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return Ok(ClientStore::default());
    }

    let lookup_query = local_request_lookup_query(request_id);
    let lookup_data =
        execute_local_graphql_query(node, &lookup_query, "local request lookup").await?;
    let request_rows: Vec<AgentRequestRow> = parse_query_rows(&lookup_data, "AgentRequest")?;
    let Some(session_id) = request_rows
        .first()
        .and_then(|row| row.session_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    else {
        return Ok(ClientStore::from_rows(ClientStoreRows {
            requests: request_rows,
            responses: parse_query_rows(&lookup_data, "AgentResponse")?,
            ..ClientStoreRows::default()
        }));
    };

    let patch_query = remote_chat_patch_query(&session_id);
    let data = execute_local_graphql_query(node, &patch_query, "local chat patch").await?;
    chat_patch_from_data(&data)
}

fn chat_patch_from_data(data: &Value) -> Result<ClientStore> {
    Ok(ClientStore::from_rows(ClientStoreRows {
        requests: parse_query_rows(&data, "AgentRequest")?,
        responses: parse_query_rows(&data, "AgentResponse")?,
        sessions: parse_query_rows(&data, "AgentSession")?,
        goals: parse_query_rows(&data, "Goal")?,
        ..ClientStoreRows::default()
    }))
}

pub(super) async fn execute_local_graphql_query(
    node: &EmbeddedNode,
    query: &str,
    operation: &str,
) -> Result<Value> {
    let response = gents::graphql::graphql_with_transaction_retry(node, query, operation).await?;
    response
        .data
        .with_context(|| format!("{operation} returned no data"))
}

pub(super) async fn load_rows<T>(node: &EmbeddedNode, root: &str, query: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let operation = format!("query for {root}");
    let response = gents::graphql::graphql_with_transaction_retry(node, query, &operation).await?;

    let data = response
        .data
        .with_context(|| format!("query for {root} returned no data"))?;
    let rows = data
        .get(root)
        .ok_or_else(|| anyhow!("query for {root} missing root field"))?;

    match rows {
        Value::Null => Ok(Vec::new()),
        Value::Array(rows) => {
            let mut parsed = Vec::with_capacity(rows.len());
            for row in rows {
                match serde_json::from_value(row.clone()) {
                    Ok(row) => parsed.push(row),
                    Err(error) => tracing::warn!(
                        target: "gents_desktop_core::query",
                        root,
                        error = %error,
                        "skipping malformed observed row"
                    ),
                }
            }
            Ok(parsed)
        }
        other => Err(anyhow!(
            "query for {root} returned non-array payload: {other}"
        )),
    }
}

pub(super) fn parse_query_rows<T>(data: &Value, root: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let rows = data
        .get(root)
        .ok_or_else(|| anyhow!("query result missing root field {root}"))?;
    parse_row_array(rows, root)
}

pub(super) fn parse_row_array<T>(rows: &Value, root: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    match rows {
        Value::Null => Ok(Vec::new()),
        Value::Array(rows) => {
            let mut parsed = Vec::with_capacity(rows.len());
            for row in rows {
                match serde_json::from_value(row.clone()) {
                    Ok(row) => parsed.push(row),
                    Err(error) => tracing::warn!(
                        target: "gents_desktop_core::query",
                        root,
                        error = %error,
                        "skipping malformed query row"
                    ),
                }
            }
            Ok(parsed)
        }
        other => Err(anyhow!(
            "query result for {root} returned non-array payload: {other}"
        )),
    }
}

fn local_request_lookup_query(request_id: &str) -> String {
    let request_id = escape_graphql_string(request_id);
    format!(
        r#"
query DesktopLocalRequestLookup {{
  AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{ {AGENT_REQUEST_FIELDS} }}
  AgentResponse(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {AGENT_RESPONSE_FIELDS} }}
}}
"#
    )
}

fn remote_chat_patch_query(session_id: &str) -> String {
    let session_id = escape_graphql_string(session_id);
    format!(
        r#"
query DesktopRemoteChatPatch {{
  AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_REQUEST_FIELDS} }}
  AgentResponse(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_RESPONSE_FIELDS} }}
  AgentSession(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_SESSION_FIELDS} }}
  Goal(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {GOAL_FIELDS} }}
}}
"#
    )
}

#[cfg(test)]
mod tests {
    mod projection_and_patches;
    mod query_edges;
    mod transcript_pagination;
}
