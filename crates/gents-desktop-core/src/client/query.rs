use std::collections::HashSet;

use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::graphql::escape_graphql_string;
use gents_protocol::row::{
    AgentBehaviorRow, AgentConversationRow, AgentMessageRow, AgentPrincipalRow, AgentRequestRow,
    AgentResponseRow, AgentRuntimeRow, AgentSessionRow, AgentToolCallRow, AgentToolResultRow,
    CompactionEntryRow, EventTriggerRow, GoalRow, InferenceBackendRow, InferenceProfileRow,
    MailboxItemRow, ScheduleRow, SkillRow, TaskRow, ToolSelectionRow, ToolServiceRegistryRow,
};
use gents_protocol::schemas::{
    AGENT_BEHAVIOR_NAME, AGENT_CONVERSATION_NAME, AGENT_MESSAGE_NAME, AGENT_PRINCIPAL_NAME,
    AGENT_REQUEST_NAME, AGENT_RESPONSE_NAME, AGENT_RUNTIME_NAME, AGENT_SESSION_NAME,
    AGENT_TOOL_CALL_NAME, AGENT_TOOL_RESULT_NAME, COMPACTION_ENTRY_NAME, EVENT_TRIGGER_NAME,
    GOAL_NAME, INFERENCE_BACKEND_NAME, INFERENCE_PROFILE_NAME, MAILBOX_ITEM_NAME, SCHEDULE_NAME,
    SKILL_NAME, TASK_NAME, TOOL_SELECTION_NAME, TOOL_SERVICE_REGISTRY_NAME,
};
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
    "agent_did display_name default_behavior_id enabled created_at created_by";
pub(super) const AGENT_BEHAVIOR_FIELDS: &str = "behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled skill_refs skill_excludes created_at";
pub(super) const AGENT_RUNTIME_FIELDS: &str = "agent_did process_state reconcile_phase active_generation router_generation default_behavior_id runnable_behavior_count unavailable_behavior_count behavior_executor_capacity behavior_executor_queue_depth last_reconcile_result last_reconcile_error last_reconcile_completed_at updated_at";
pub(super) const AGENT_CONVERSATION_FIELDS: &str = "session_id agent_name agent_did requester_did behavior_id title title_source preview_text status created_at updated_at latest_request_id";
pub(super) const AGENT_REQUEST_FIELDS: &str = "request_id agent_did requester_did behavior_id session_id retry_parent_request retry_root_request superseded_by_request content temperature top_p top_k seed max_tokens max_total_tokens metadata status lifecycle_state backend_id execution_origin caused_by_trigger_id caused_by_trigger_kind caused_by_correlation caused_by_trigger_context caused_by_source_doc_id caused_by_parent_request_id failure_reason terminalized_at terminal_redrive_attempts created_at claimed_at deadline retry_count max_retries interrupt_requested_at valid_until workspace_id workspace_authority workspace_owner_deployment_id workspace_seal_hash";
pub(super) const AGENT_RESPONSE_FIELDS: &str = "response_key request_id agent_did requester_did behavior_id session_id content reasoning status error_message token_count progress_seq materialized_message_sequence materialized_at created_at completed_at interrupted_at";
pub(super) const AGENT_MESSAGE_FIELDS: &str =
    "message_key session_id request_id requester_did sequence role content reasoning timestamp";
pub(super) const AGENT_SESSION_FIELDS: &str =
    "session_id agent_name requester_did behavior_id started ended status";
pub(super) const GOAL_FIELDS: &str = "goal_id session_id agent_did objective status token_budget tokens_used active_time_seconds active_started_at consecutive_blocked_audits last_blocked_request_id last_blocked_reason last_continued_from_request_id continuation_sequence wrapup_requested wrapup_completed infrastructure_retry_count last_failure completion_evidence created_at updated_at";
pub(super) const AGENT_TOOL_CALL_FIELDS: &str = "tool_call_key session_id request_id requester_did message_sequence tool_name tool_call_id args result status lifecycle_state child_request_id await_mode cancel_policy deadline_at cancel_cause started_at completed_at selected_service_id selected_tool_name tool_failure_class denial_reason denied_argv denied_command denied_argument denied_subcommand denied_prefix policy_mode policy_network latency_ms partial_output_tail partial_output_seq";
pub(super) const AGENT_TOOL_RESULT_FIELDS: &str = "agent_did requester_did session_id tool_name tool_input output_text truncated truncation_metadata conversation_doc_id created_at discarded_because_interrupted";
pub(super) const COMPACTION_ENTRY_FIELDS: &str = "compaction_key session_id requester_did sequence summary files_read files_modified messages_compacted compacted_through_sequence original_tokens compacted_tokens created_at";
pub(super) const TASK_FIELDS: &str = "task_id name description behavior_id prompt_template enabled output_schema_ref created_at updated_at";
pub(super) const SKILL_FIELDS: &str = "skill_id agent_did scope name description instructions tool_refs display_name interface_json enabled created_at";
pub(super) const SCHEDULE_FIELDS: &str = "schedule_id task_id interval_secs cron timezone missed_run_policy enabled concurrency next_run_at last_attempt_at last_status last_error fire_count created_at updated_at";
pub(super) const EVENT_TRIGGER_FIELDS: &str = "trigger_id task_id source_collection event_kind filter correlation_field fire_mode expected_count expected_count_field group_timeout_secs group_min_count workspace_authority enabled concurrency created_at updated_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count";
pub(super) const TOOL_SELECTION_FIELDS: &str = "selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode command_execution_policy command_allowed_argv_prefixes command_forbidden_argv_prefixes command_network_mode cli_tool_names enable_meta_tools allowed_mcp_service_ids required_mcp_service_ids delegate_to backgroundable_tool_names enable_memory enable_session_history_tool enable_context_budget enable_defra_query defra_query_collections subagent_targets subagent_spawn_enabled subagent_steering_enabled subagent_background_enabled subagent_allow_cross_deployment cross_deployment_spawn_timeout_seconds tool_policy_version write_tools datastore_tool_surface_ids eth_tool_ids subagent_default_await_mode enable_self_config self_config_categories self_config_no_lockout self_config_dry_run enable_lsp lsp_config";
pub(super) const INFERENCE_BACKEND_FIELDS: &str = "backend_id name provider_kind openai_wire_api endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status";
pub(super) const INFERENCE_PROFILE_FIELDS: &str = "profile_id display_name context_window max_output_tokens max_turns temperature top_p top_k seed min_p frequency_penalty presence_penalty repetition_penalty reasoning_effort stream_batch_ms stream_liveness_timeout_secs deadline_duration_secs retry_max_transport retry_backoff_ms retry_max_resample retry_allow_repair retry_interactive_max";
pub(super) const TOOL_SERVICE_REGISTRY_FIELDS: &str = "service_id display_name description hostname tailscale_ip lan_ip mcp_port mcp_path status version updated_at";
pub(super) const MAILBOX_ITEM_FIELDS: &str = "_docID item_key requester_did agent_did status kind action title summary payload source_kind source_id session_id request_id graph_run_id cause_doc_id target_agent_did target_behavior_id expected_collection parent_item_id deadline_at created_at updated_at resolved_at resolved_doc_id";

/// Load only the selected request's conversation slice from the embedded
/// replica. This is the bounded polling fallback for a dropped/coalesced
/// observer event; it does not reload every conversation for the agent.
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
        conversations: parse_query_rows(&data, "AgentConversation")?,
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
    let response = node.execute(query).await;
    if response.has_errors() {
        bail!(
            "{operation} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    response
        .data
        .with_context(|| format!("{operation} returned no data"))
}

pub(super) async fn load_rows<T>(node: &EmbeddedNode, root: &str, query: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let response = node.execute(query).await;
    if response.has_errors() {
        bail!(
            "query for {root} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

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
  AgentConversation(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_CONVERSATION_FIELDS} }}
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
