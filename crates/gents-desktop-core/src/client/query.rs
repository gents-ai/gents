use std::collections::HashSet;

use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::graphql::escape_graphql_string;
use gents_protocol::row::{
    AgentBehaviorRow, AgentConversationRow, AgentMessageRow, AgentPrincipalRow, AgentRequestRow,
    AgentResponseRow, AgentRuntimeRow, AgentSessionRow, AgentToolCallRow, AgentToolResultRow,
    CompactionEntryRow, EventTriggerRow, GoalRow, InferenceBackendRow, InferenceProfileRow,
    ScheduleRow, SkillRow, TaskRow, ToolSelectionRow, ToolServiceRegistryRow,
};
use gents_protocol::schemas::{
    AGENT_BEHAVIOR_NAME, AGENT_CONVERSATION_NAME, AGENT_MESSAGE_NAME, AGENT_PRINCIPAL_NAME,
    AGENT_REQUEST_NAME, AGENT_RESPONSE_NAME, AGENT_RUNTIME_NAME, AGENT_SESSION_NAME,
    AGENT_TOOL_CALL_NAME, AGENT_TOOL_RESULT_NAME, COMPACTION_ENTRY_NAME, EVENT_TRIGGER_NAME,
    GOAL_NAME, INFERENCE_BACKEND_NAME, INFERENCE_PROFILE_NAME, SCHEDULE_NAME, SKILL_NAME,
    TASK_NAME, TOOL_SELECTION_NAME, TOOL_SERVICE_REGISTRY_NAME,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

use super::peer_directory::PeerRecord;
use super::store::{ClientStore, ClientStoreRows};

pub const DEFAULT_SESSION_TRANSCRIPT_PAGE_SIZE: usize = 40;
pub const MAX_SESSION_TRANSCRIPT_PAGE_SIZE: usize = 80;
const SESSION_TRANSCRIPT_TOOL_CALL_ROW_BUDGET: usize = 320;

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
struct TranscriptCursorRow {
    sequence: Option<i64>,
}

const AGENT_PRINCIPAL_FIELDS: &str =
    "agent_did display_name default_behavior_id enabled created_at created_by";
const AGENT_BEHAVIOR_FIELDS: &str = "behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled skill_refs skill_excludes created_at";
const AGENT_RUNTIME_FIELDS: &str = "agent_did process_state reconcile_phase active_generation router_generation default_behavior_id runnable_behavior_count unavailable_behavior_count behavior_executor_capacity behavior_executor_queue_depth last_reconcile_result last_reconcile_error last_reconcile_completed_at updated_at";
const AGENT_CONVERSATION_FIELDS: &str = "session_id agent_name agent_did requester_did behavior_id title title_source preview_text status created_at updated_at latest_request_id";
const AGENT_REQUEST_FIELDS: &str = "request_id agent_did requester_did behavior_id session_id retry_parent_request retry_root_request superseded_by_request content temperature top_p top_k seed max_tokens max_total_tokens metadata status lifecycle_state backend_id execution_origin caused_by_trigger_id caused_by_trigger_kind caused_by_correlation caused_by_trigger_context caused_by_parent_request_id failure_reason terminalized_at terminal_redrive_attempts created_at claimed_at deadline retry_count max_retries interrupt_requested_at valid_until workspace_id workspace_authority workspace_owner_deployment_id workspace_seal_hash";
const AGENT_RESPONSE_FIELDS: &str = "response_key request_id agent_did requester_did behavior_id session_id content reasoning status error_message token_count progress_seq materialized_message_sequence materialized_at created_at completed_at interrupted_at";
const AGENT_MESSAGE_FIELDS: &str =
    "message_key session_id request_id requester_did sequence role content reasoning timestamp";
const AGENT_SESSION_FIELDS: &str =
    "session_id agent_name requester_did behavior_id started ended status";
const GOAL_FIELDS: &str = "goal_id session_id agent_did objective status token_budget tokens_used active_time_seconds active_started_at consecutive_blocked_audits last_blocked_request_id last_blocked_reason last_continued_from_request_id continuation_sequence wrapup_requested wrapup_completed infrastructure_retry_count last_failure completion_evidence created_at updated_at";
const AGENT_TOOL_CALL_FIELDS: &str = "tool_call_key session_id request_id requester_did message_sequence tool_name tool_call_id args result status lifecycle_state child_request_id await_mode cancel_policy deadline_at cancel_cause started_at completed_at selected_service_id selected_tool_name tool_failure_class denial_reason denied_argv denied_command denied_argument denied_subcommand denied_prefix policy_mode policy_network latency_ms partial_output_tail partial_output_seq";
const AGENT_TOOL_RESULT_FIELDS: &str = "agent_did requester_did session_id tool_name tool_input output_text truncated truncation_metadata conversation_doc_id created_at discarded_because_interrupted";
const COMPACTION_ENTRY_FIELDS: &str = "compaction_key session_id requester_did sequence summary files_read files_modified messages_compacted compacted_through_sequence original_tokens compacted_tokens created_at";
const TASK_FIELDS: &str = "task_id name description behavior_id prompt_template enabled output_schema_ref created_at updated_at";
const SKILL_FIELDS: &str = "skill_id agent_did scope name description instructions tool_refs display_name interface_json enabled created_at";
const SCHEDULE_FIELDS: &str = "schedule_id task_id interval_secs cron timezone missed_run_policy enabled concurrency next_run_at last_attempt_at last_status last_error fire_count created_at updated_at";
const EVENT_TRIGGER_FIELDS: &str = "trigger_id task_id source_collection event_kind filter correlation_field fire_mode expected_count expected_count_field group_timeout_secs group_min_count workspace_authority enabled concurrency created_at updated_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count";
const TOOL_SELECTION_FIELDS: &str = "selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode command_execution_policy command_allowed_argv_prefixes command_forbidden_argv_prefixes command_network_mode cli_tool_names enable_meta_tools allowed_mcp_service_ids delegate_to backgroundable_tool_names enable_memory enable_session_history_tool enable_context_budget enable_defra_query defra_query_collections subagent_targets subagent_spawn_enabled subagent_steering_enabled subagent_background_enabled subagent_allow_cross_deployment cross_deployment_spawn_timeout_seconds tool_policy_version write_tools subagent_default_await_mode enable_self_config self_config_categories self_config_no_lockout self_config_dry_run enable_lsp lsp_config";
const INFERENCE_BACKEND_FIELDS: &str = "backend_id name provider_kind openai_wire_api endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status";
const INFERENCE_PROFILE_FIELDS: &str = "profile_id display_name context_window max_output_tokens max_turns temperature top_p top_k seed min_p frequency_penalty presence_penalty repetition_penalty reasoning_effort stream_batch_ms stream_liveness_timeout_secs deadline_duration_secs retry_max_transport retry_backoff_ms retry_max_resample retry_allow_repair retry_interactive_max";
const TOOL_SERVICE_REGISTRY_FIELDS: &str = "service_id display_name description hostname tailscale_ip lan_ip mcp_port mcp_path status version updated_at";

pub async fn load_full_snapshot(node: &EmbeddedNode) -> Result<ClientStore> {
    Ok(ClientStore::from_rows(ClientStoreRows {
        agent_principals: load_agent_principals(node).await?,
        behaviors: load_agent_behaviors(node).await?,
        runtimes: load_agent_runtimes(node).await?,
        conversations: load_agent_conversations(node).await?,
        requests: load_agent_requests(node).await?,
        responses: load_agent_responses(node).await?,
        messages: load_agent_messages(node).await?,
        sessions: load_agent_sessions(node).await?,
        goals: load_goals(node).await?,
        tool_calls: load_agent_tool_calls(node).await?,
        tool_results: load_agent_tool_results(node).await?,
        compaction_entries: load_compaction_entries(node).await?,
        tasks: load_tasks(node).await?,
        schedules: load_schedules(node).await?,
        event_triggers: load_event_triggers(node).await?,
        skills: load_skills(node).await?,
        tool_selections: load_tool_selections(node).await?,
        inference_backends: load_inference_backends(node).await?,
        inference_profiles: load_inference_profiles(node).await?,
        tool_service_registries: load_tool_service_registries(node).await?,
        ..ClientStoreRows::default()
    }))
}

pub async fn load_full_snapshot_with_peer_records(
    node: &EmbeddedNode,
    peers: &[PeerRecord],
    requester_did: &str,
) -> Result<ClientStore> {
    let mut rows = load_full_snapshot(node).await?.to_rows();
    isolate_legacy_bearer_rows(&mut rows, peers, requester_did);
    Ok(ClientStore::from_rows(rows))
}

/// Bearer replication is requester-scoped, but an upgraded database can still
/// contain rows received by the old unfiltered replicator. Keep those rows
/// durable for diagnostics while excluding them from every client projection.
pub(crate) fn isolate_legacy_bearer_rows(
    rows: &mut ClientStoreRows,
    peers: &[PeerRecord],
    requester_did: &str,
) {
    let bearer_dids = peers
        .iter()
        .filter(|peer| peer.is_bearer_pairing())
        .map(|peer| peer.agent_did.as_str())
        .collect::<HashSet<_>>();
    if bearer_dids.is_empty() {
        return;
    }

    let is_bearer_did = |did: Option<&str>| did.is_some_and(|did| bearer_dids.contains(did));
    let requester_matches = |did: Option<&str>| did.is_some_and(|did| did == requester_did);
    let mut bearer_sessions = rows
        .conversations
        .iter()
        .filter(|row| is_bearer_did(row.agent_did.as_deref()))
        .map(|row| row.session_id.clone())
        .collect::<HashSet<_>>();
    bearer_sessions.extend(
        rows.requests
            .iter()
            .filter(|row| is_bearer_did(row.agent_did.as_deref()))
            .filter_map(|row| row.session_id.clone()),
    );
    bearer_sessions.extend(
        rows.responses
            .iter()
            .filter(|row| is_bearer_did(row.agent_did.as_deref()))
            .filter_map(|row| row.session_id.clone()),
    );
    bearer_sessions.extend(
        rows.tool_results
            .iter()
            .filter(|row| is_bearer_did(row.agent_did.as_deref()))
            .filter_map(|row| row.session_id.clone()),
    );

    rows.conversations.retain(|row| {
        !is_bearer_did(row.agent_did.as_deref()) || requester_matches(row.requester_did.as_deref())
    });
    rows.requests.retain(|row| {
        !is_bearer_did(row.agent_did.as_deref()) || requester_matches(row.requester_did.as_deref())
    });
    rows.responses.retain(|row| {
        !is_bearer_did(row.agent_did.as_deref()) || requester_matches(row.requester_did.as_deref())
    });
    retain_rows_with_sources(
        &mut rows.tool_results,
        &mut rows.tool_result_source_agent_dids,
        |row| {
            !is_bearer_did(row.agent_did.as_deref())
                || requester_matches(row.requester_did.as_deref())
        },
    );
    retain_rows_with_sources(
        &mut rows.messages,
        &mut rows.message_source_agent_dids,
        |row| {
            !row.session_id
                .as_deref()
                .is_some_and(|session_id| bearer_sessions.contains(session_id))
                || requester_matches(row.requester_did.as_deref())
        },
    );
    retain_rows_with_sources(
        &mut rows.sessions,
        &mut rows.session_source_agent_dids,
        |row| {
            !bearer_sessions.contains(&row.session_id)
                || requester_matches(row.requester_did.as_deref())
        },
    );
    retain_rows_with_sources(
        &mut rows.tool_calls,
        &mut rows.tool_call_source_agent_dids,
        |row| {
            !row.session_id
                .as_deref()
                .is_some_and(|session_id| bearer_sessions.contains(session_id))
                || requester_matches(row.requester_did.as_deref())
        },
    );
    retain_rows_with_sources(
        &mut rows.compaction_entries,
        &mut rows.compaction_entry_source_agent_dids,
        |row| {
            !row.session_id
                .as_deref()
                .is_some_and(|session_id| bearer_sessions.contains(session_id))
                || requester_matches(row.requester_did.as_deref())
        },
    );
    // Goal was never part of the requester-scoped conversation template, so
    // any bearer-owned goal in the local store necessarily came from the old
    // broad replicator.
    rows.goals
        .retain(|row| !bearer_dids.contains(row.agent_did.as_str()));

    // Principal/runtime projections are not part of the signed client grant.
    // Agent configuration is included by the conversation/machine template
    // and must remain visible after it arrives through the local replica.
    rows.agent_principals
        .retain(|row| !bearer_dids.contains(row.agent_did.as_str()));
    rows.runtimes
        .retain(|row| !bearer_dids.contains(row.agent_did.as_str()));
}

pub async fn load_agent_scoped_snapshot_with_peer_records(
    node: &EmbeddedNode,
    agent_did: &str,
    peers: &[PeerRecord],
    requester_did: &str,
) -> Result<ClientStore> {
    let mut rows = load_agent_scoped_snapshot(node, agent_did).await?.to_rows();
    isolate_legacy_bearer_rows(&mut rows, peers, requester_did);
    Ok(ClientStore::from_rows(rows))
}

fn retain_rows_with_sources<T>(
    rows: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    mut keep: impl FnMut(&T) -> bool,
) {
    sources.resize(rows.len(), None);
    let mut kept_rows = Vec::with_capacity(rows.len());
    let mut kept_sources = Vec::with_capacity(sources.len());
    for (row, source) in std::mem::take(rows)
        .into_iter()
        .zip(std::mem::take(sources))
    {
        if keep(&row) {
            kept_rows.push(row);
            kept_sources.push(source);
        }
    }
    *rows = kept_rows;
    *sources = kept_sources;
}

pub async fn load_agent_principals(node: &EmbeddedNode) -> Result<Vec<AgentPrincipalRow>> {
    load_rows(
        node,
        "AgentPrincipal",
        "query { AgentPrincipal { agent_did display_name default_behavior_id enabled created_at created_by } }",
    )
    .await
}

pub async fn load_agent_behaviors(node: &EmbeddedNode) -> Result<Vec<AgentBehaviorRow>> {
    load_rows(
        node,
        "AgentBehavior",
        "query { AgentBehavior { behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled skill_refs skill_excludes created_at } }",
    )
    .await
}

pub async fn load_agent_runtimes(node: &EmbeddedNode) -> Result<Vec<AgentRuntimeRow>> {
    load_rows(
        node,
        AGENT_RUNTIME_NAME,
        &format!("query {{ {AGENT_RUNTIME_NAME} {{ {AGENT_RUNTIME_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_conversations(node: &EmbeddedNode) -> Result<Vec<AgentConversationRow>> {
    load_rows(
        node,
        AGENT_CONVERSATION_NAME,
        &format!("query {{ {AGENT_CONVERSATION_NAME} {{ {AGENT_CONVERSATION_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_requests(node: &EmbeddedNode) -> Result<Vec<AgentRequestRow>> {
    load_rows(
        node,
        AGENT_REQUEST_NAME,
        &format!("query {{ {AGENT_REQUEST_NAME} {{ {AGENT_REQUEST_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_responses(node: &EmbeddedNode) -> Result<Vec<AgentResponseRow>> {
    load_rows(
        node,
        AGENT_RESPONSE_NAME,
        &format!("query {{ {AGENT_RESPONSE_NAME} {{ {AGENT_RESPONSE_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_messages(node: &EmbeddedNode) -> Result<Vec<AgentMessageRow>> {
    load_rows(
        node,
        AGENT_MESSAGE_NAME,
        &format!("query {{ {AGENT_MESSAGE_NAME} {{ {AGENT_MESSAGE_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_sessions(node: &EmbeddedNode) -> Result<Vec<AgentSessionRow>> {
    load_rows(
        node,
        AGENT_SESSION_NAME,
        &format!("query {{ {AGENT_SESSION_NAME} {{ {AGENT_SESSION_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_goals(node: &EmbeddedNode) -> Result<Vec<GoalRow>> {
    load_rows(
        node,
        "Goal",
        &format!("query {{ Goal {{ {GOAL_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_tool_calls(node: &EmbeddedNode) -> Result<Vec<AgentToolCallRow>> {
    load_rows(
        node,
        "AgentToolCall",
        &format!("query {{ AgentToolCall {{ {AGENT_TOOL_CALL_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_agent_tool_results(node: &EmbeddedNode) -> Result<Vec<AgentToolResultRow>> {
    load_rows(
        node,
        AGENT_TOOL_RESULT_NAME,
        &format!("query {{ {AGENT_TOOL_RESULT_NAME} {{ {AGENT_TOOL_RESULT_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_compaction_entries(node: &EmbeddedNode) -> Result<Vec<CompactionEntryRow>> {
    load_rows(
        node,
        COMPACTION_ENTRY_NAME,
        &format!("query {{ {COMPACTION_ENTRY_NAME} {{ {COMPACTION_ENTRY_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_tasks(node: &EmbeddedNode) -> Result<Vec<TaskRow>> {
    load_rows(
        node,
        "Task",
        "query { Task { task_id name description behavior_id prompt_template enabled output_schema_ref created_at updated_at } }",
    )
    .await
}

pub async fn load_skills(node: &EmbeddedNode) -> Result<Vec<SkillRow>> {
    load_rows(
        node,
        SKILL_NAME,
        &format!("query {{ {SKILL_NAME} {{ {SKILL_FIELDS} }} }}"),
    )
    .await
}

pub async fn load_schedules(node: &EmbeddedNode) -> Result<Vec<ScheduleRow>> {
    load_rows(
        node,
        "Schedule",
        "query { Schedule { schedule_id task_id interval_secs cron timezone missed_run_policy enabled concurrency next_run_at last_attempt_at last_status last_error fire_count created_at updated_at } }",
    )
    .await
}

pub async fn load_event_triggers(node: &EmbeddedNode) -> Result<Vec<EventTriggerRow>> {
    load_rows(
        node,
        "EventTrigger",
        "query { EventTrigger { trigger_id task_id source_collection event_kind filter correlation_field fire_mode expected_count expected_count_field group_timeout_secs group_min_count workspace_authority enabled concurrency created_at updated_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count } }",
    )
    .await
}

pub async fn load_tool_selections(node: &EmbeddedNode) -> Result<Vec<ToolSelectionRow>> {
    load_rows(
        node,
        "ToolSelection",
        "query { ToolSelection { selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode command_execution_policy command_allowed_argv_prefixes command_forbidden_argv_prefixes command_network_mode cli_tool_names enable_meta_tools allowed_mcp_service_ids delegate_to backgroundable_tool_names enable_memory enable_session_history_tool enable_context_budget enable_defra_query defra_query_collections subagent_targets subagent_spawn_enabled subagent_steering_enabled subagent_background_enabled subagent_allow_cross_deployment cross_deployment_spawn_timeout_seconds tool_policy_version write_tools subagent_default_await_mode enable_self_config self_config_categories self_config_no_lockout self_config_dry_run enable_lsp lsp_config } }",
    )
    .await
}

pub async fn load_inference_backends(node: &EmbeddedNode) -> Result<Vec<InferenceBackendRow>> {
    load_rows(
        node,
        "InferenceBackend",
        "query { InferenceBackend { backend_id name provider_kind openai_wire_api endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status } }",
    )
    .await
}

pub async fn load_inference_profiles(node: &EmbeddedNode) -> Result<Vec<InferenceProfileRow>> {
    load_rows(
        node,
        "InferenceProfile",
        "query { InferenceProfile { profile_id display_name context_window max_output_tokens max_turns temperature top_p top_k seed min_p frequency_penalty presence_penalty repetition_penalty reasoning_effort stream_batch_ms stream_liveness_timeout_secs deadline_duration_secs retry_max_transport retry_backoff_ms retry_max_resample retry_allow_repair retry_interactive_max } }",
    )
    .await
}

pub async fn load_tool_service_registries(
    node: &EmbeddedNode,
) -> Result<Vec<ToolServiceRegistryRow>> {
    load_rows(
        node,
        "ToolServiceRegistry",
        "query { ToolServiceRegistry { service_id display_name description hostname tailscale_ip lan_ip mcp_port mcp_path status version updated_at } }",
    )
    .await
}

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

    let patch_query = remote_chat_patch_query(&session_id, request_id);
    let data = execute_local_graphql_query(node, &patch_query, "local chat patch").await?;
    chat_patch_from_data(&data)
}

fn tool_group_cursor_sequence(cursor: &str) -> Option<i64> {
    cursor
        .strip_prefix("tools-")
        .and_then(|value| value.parse::<i64>().ok())
}

async fn resolve_transcript_cursor_sequence(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: Option<&str>,
    requester_did: Option<&str>,
    cursor: &str,
) -> Result<i64> {
    if let Some(sequence) = tool_group_cursor_sequence(cursor) {
        return Ok(sequence);
    }
    let session_id = escape_graphql_string(session_id);
    let message_key = escape_graphql_string(cursor);
    let agent_filter = agent_did
        .map(escape_graphql_string)
        .map(|agent_did| format!(", agent_did: {{ _eq: \"{agent_did}\" }}"))
        .unwrap_or_default();
    let requester_filter = requester_did
        .map(escape_graphql_string)
        .map(|requester_did| format!(", requester_did: {{ _eq: \"{requester_did}\" }}"))
        .unwrap_or_default();
    let rows: Vec<TranscriptCursorRow> = load_rows(
        node,
        AGENT_MESSAGE_NAME,
        &format!(
            r#"query {{
  AgentMessage(
    filter: {{
      session_id: {{ _eq: "{session_id}" }},
      message_key: {{ _eq: "{message_key}" }}{agent_filter}{requester_filter}
    }},
    limit: 1
  ) {{ sequence }}
}}"#
        ),
    )
    .await?;
    rows.first()
        .and_then(|row| row.sequence)
        .ok_or_else(|| anyhow!("session transcript cursor is no longer present: {cursor}"))
}

/// Query a bounded transcript window directly from DefraDB. The cursor is a
/// bridge item key, but is resolved to the durable sequence space before the
/// page query so inserts at the tip cannot shift an older page. Messages and
/// tool groups are independently overscanned because one sequence may produce
/// both timeline items; the bridge performs the final visible-item limit.
pub async fn load_session_transcript_page(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: Option<&str>,
    requester_did: Option<&str>,
    before_item_key: Option<&str>,
    requested_limit: Option<usize>,
) -> Result<SessionTranscriptQueryPage> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        bail!("session transcript query requires a session id");
    }
    let limit = requested_limit
        .unwrap_or(DEFAULT_SESSION_TRANSCRIPT_PAGE_SIZE)
        .clamp(1, MAX_SESSION_TRANSCRIPT_PAGE_SIZE);
    let message_query_limit = limit.saturating_add(1);
    let tool_call_query_limit = SESSION_TRANSCRIPT_TOOL_CALL_ROW_BUDGET.saturating_add(1);
    let before_sequence = match before_item_key {
        Some(cursor) => Some(
            resolve_transcript_cursor_sequence(node, session_id, agent_did, requester_did, cursor)
                .await?,
        ),
        None => None,
    };
    let escaped_session_id = escape_graphql_string(session_id);
    let agent_filter = agent_did
        .map(escape_graphql_string)
        .map(|agent_did| format!(", agent_did: {{ _eq: \"{agent_did}\" }}"))
        .unwrap_or_default();
    let requester_filter = requester_did
        .map(escape_graphql_string)
        .map(|requester_did| format!(", requester_did: {{ _eq: \"{requester_did}\" }}"))
        .unwrap_or_default();
    let message_sequence_filter = before_sequence
        .map(|sequence| format!(", sequence: {{ _lt: {sequence} }}"))
        .unwrap_or_default();
    let message_query = format!(
        r#"query DesktopSessionTranscriptMessages {{
  AgentMessage(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter}{message_sequence_filter} }},
    order: [{{ sequence: DESC }}, {{ message_key: DESC }}],
    limit: {message_query_limit}
  ) {{ {AGENT_MESSAGE_FIELDS} }}
}}"#
    );
    let started = std::time::Instant::now();
    let message_data =
        execute_local_graphql_query(node, &message_query, "session transcript messages").await?;
    let queried_messages: Vec<AgentMessageRow> =
        parse_query_rows(&message_data, AGENT_MESSAGE_NAME)?;
    if queried_messages.iter().any(|row| row.sequence.is_none()) {
        bail!(
            "session transcript contains legacy messages without a sequence; bounded pagination cannot represent that schema state losslessly"
        );
    }
    let messages_exhausted = queried_messages.len() < message_query_limit;
    let mut messages = queried_messages.clone();
    let deferred_message_sequence = (!messages_exhausted)
        .then(|| queried_messages.last().and_then(|row| row.sequence))
        .flatten();
    if let Some(sequence) = deferred_message_sequence {
        messages.retain(|row| row.sequence != Some(sequence));
        if messages.is_empty() {
            bail!(
                "session transcript has more than {limit} messages at sequence {sequence}; the sequence-atomic page budget cannot represent it losslessly"
            );
        }
    }

    // Tool calls are bounded to the same sequence window as the message
    // overscan. This keeps a long tool-bearing session pageable while still
    // bounding a pathological single tool group.
    let mut tool_sequence_conditions = Vec::new();
    if let Some(sequence) = before_sequence {
        tool_sequence_conditions.push(format!("_lt: {sequence}"));
    }
    if let Some(sequence) = deferred_message_sequence {
        tool_sequence_conditions.push(format!("_gt: {sequence}"));
    } else if !messages_exhausted {
        if let Some(sequence) = messages.iter().filter_map(|row| row.sequence).min() {
            tool_sequence_conditions.push(format!("_gte: {sequence}"));
        }
    }
    let tool_sequence_filter = if tool_sequence_conditions.is_empty() {
        String::new()
    } else {
        format!(
            ", message_sequence: {{ {} }}",
            tool_sequence_conditions.join(", ")
        )
    };
    let tool_query = format!(
        r#"query DesktopSessionTranscriptTools {{
  AgentToolCall(
    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{agent_filter}{requester_filter}{tool_sequence_filter} }},
    order: [{{ message_sequence: DESC }}, {{ tool_call_key: DESC }}],
    limit: {tool_call_query_limit}
  ) {{ {AGENT_TOOL_CALL_FIELDS} }}
}}"#
    );
    let tool_data =
        execute_local_graphql_query(node, &tool_query, "session transcript tools").await?;
    let queried_tool_calls: Vec<AgentToolCallRow> =
        parse_query_rows(&tool_data, AGENT_TOOL_CALL_NAME)?;
    if queried_tool_calls
        .iter()
        .any(|row| row.message_sequence.is_none())
    {
        bail!(
            "session transcript contains legacy tool calls without a message sequence; bounded pagination cannot represent that schema state losslessly"
        );
    }
    let tools_exhausted = queried_tool_calls.len() < tool_call_query_limit;
    let mut tool_calls = queried_tool_calls.clone();
    if !tools_exhausted {
        let deferred_sequence = queried_tool_calls
            .last()
            .and_then(|row| row.message_sequence)
            .expect("null tool sequences rejected above");
        tool_calls.retain(|row| row.message_sequence != Some(deferred_sequence));
        // The overscan row proves every lower sequence is outside the complete
        // tool window too. Defer their messages with the boundary group so the
        // bridge never renders a message whose tools were truncated.
        messages.retain(|row| {
            row.sequence
                .is_some_and(|sequence| sequence > deferred_sequence)
        });
        if tool_calls.is_empty() {
            bail!(
                "session transcript has more than {} tool calls at sequence {deferred_sequence}; the sequence-atomic tool budget cannot represent it losslessly",
                SESSION_TRANSCRIPT_TOOL_CALL_ROW_BUDGET
            );
        }
    }
    let source_exhausted = messages_exhausted && tools_exhausted;
    let queried_rows = queried_messages
        .len()
        .saturating_add(queried_tool_calls.len());
    let query_count = 2 + u64::from(before_item_key.is_some());
    tracing::debug!(
        target: "gents_desktop_core::query",
        session_id,
        before_sequence,
        requested_limit = limit,
        message_query_limit,
        tool_call_query_limit,
        query_count,
        queried_rows,
        source_exhausted,
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "loaded bounded DefraDB transcript page"
    );
    Ok(SessionTranscriptQueryPage {
        store: ClientStore::from_rows(ClientStoreRows {
            messages,
            tool_calls,
            ..ClientStoreRows::default()
        }),
        query_count,
        queried_rows,
        message_query_limit,
        tool_call_query_limit,
        source_exhausted,
        has_newer: before_sequence.is_some(),
    })
}

fn chat_patch_from_data(data: &Value) -> Result<ClientStore> {
    Ok(ClientStore::from_rows(ClientStoreRows {
        conversations: parse_query_rows(&data, "AgentConversation")?,
        requests: parse_query_rows(&data, "AgentRequest")?,
        responses: parse_query_rows(&data, "AgentResponse")?,
        messages: parse_query_rows(&data, "AgentMessage")?,
        sessions: parse_query_rows(&data, "AgentSession")?,
        goals: parse_query_rows(&data, "Goal")?,
        tool_calls: parse_query_rows(&data, "AgentToolCall")?,
        ..ClientStoreRows::default()
    }))
}

async fn execute_local_graphql_query(
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

async fn load_rows<T>(node: &EmbeddedNode, root: &str, query: &str) -> Result<Vec<T>>
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

fn parse_query_rows<T>(data: &Value, root: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let rows = data
        .get(root)
        .ok_or_else(|| anyhow!("query result missing root field {root}"))?;
    parse_row_array(rows, root)
}

fn parse_row_array<T>(rows: &Value, root: &str) -> Result<Vec<T>>
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

fn remote_chat_patch_query(session_id: &str, request_id: &str) -> String {
    let session_id = escape_graphql_string(session_id);
    let request_id = escape_graphql_string(request_id);
    format!(
        r#"
query DesktopRemoteChatPatch {{
  AgentConversation(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_CONVERSATION_FIELDS} }}
  AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_REQUEST_FIELDS} }}
  AgentResponse(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_RESPONSE_FIELDS} }}
  AgentMessage(filter: {{ _and: [{{ session_id: {{ _eq: "{session_id}" }} }}, {{ request_id: {{ _eq: "{request_id}" }} }}] }}) {{ {AGENT_MESSAGE_FIELDS} }}
  AgentSession(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_SESSION_FIELDS} }}
  Goal(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {GOAL_FIELDS} }}
  AgentToolCall(filter: {{ _and: [{{ session_id: {{ _eq: "{session_id}" }} }}, {{ request_id: {{ _eq: "{request_id}" }} }}] }}) {{ {AGENT_TOOL_CALL_FIELDS} }}
}}
"#
    )
}

/// Fetch the rows for a specific set of `(collection, doc_id)` pairs and
/// return them as a single-collection `ClientStore` patch suitable for
/// `ObservedStore::merge_snapshot`. Empty `doc_ids` returns an empty store.
/// Unknown `collection_name` errors so callers can fall back to a scoped
/// reload.
pub async fn fetch_doc_patch(
    node: &EmbeddedNode,
    collection_name: &str,
    doc_ids: &[&str],
) -> Result<ClientStore> {
    if doc_ids.is_empty() {
        return Ok(ClientStore::default());
    }

    let in_clause = doc_ids
        .iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");

    let mut rows = ClientStoreRows::default();
    match collection_name {
        AGENT_PRINCIPAL_NAME => {
            rows.agent_principals = load_rows(
                node,
                AGENT_PRINCIPAL_NAME,
                &format!("query {{ {AGENT_PRINCIPAL_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_PRINCIPAL_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_BEHAVIOR_NAME => {
            rows.behaviors = load_rows(
                node,
                AGENT_BEHAVIOR_NAME,
                &format!("query {{ {AGENT_BEHAVIOR_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_BEHAVIOR_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_RUNTIME_NAME => {
            rows.runtimes = load_rows(
                node,
                AGENT_RUNTIME_NAME,
                &format!("query {{ {AGENT_RUNTIME_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_RUNTIME_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_CONVERSATION_NAME => {
            rows.conversations = load_rows(
                node,
                AGENT_CONVERSATION_NAME,
                &format!("query {{ {AGENT_CONVERSATION_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_CONVERSATION_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_REQUEST_NAME => {
            rows.requests = load_rows(
                node,
                AGENT_REQUEST_NAME,
                &format!("query {{ {AGENT_REQUEST_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_REQUEST_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_RESPONSE_NAME => {
            rows.responses = load_rows(
                node,
                AGENT_RESPONSE_NAME,
                &format!("query {{ {AGENT_RESPONSE_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_RESPONSE_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_MESSAGE_NAME => {
            rows.messages = load_rows(
                node,
                AGENT_MESSAGE_NAME,
                &format!("query {{ {AGENT_MESSAGE_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_SESSION_NAME => {
            rows.sessions = load_rows(
                node,
                AGENT_SESSION_NAME,
                &format!("query {{ {AGENT_SESSION_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_SESSION_FIELDS} }} }}"),
            )
            .await?;
        }
        GOAL_NAME => {
            rows.goals = load_rows(
                node,
                GOAL_NAME,
                &format!("query {{ {GOAL_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {GOAL_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_TOOL_CALL_NAME => {
            rows.tool_calls = load_rows(
                node,
                AGENT_TOOL_CALL_NAME,
                &format!("query {{ {AGENT_TOOL_CALL_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_TOOL_CALL_FIELDS} }} }}"),
            )
            .await?;
        }
        AGENT_TOOL_RESULT_NAME => {
            rows.tool_results = load_rows(
                node,
                AGENT_TOOL_RESULT_NAME,
                &format!("query {{ {AGENT_TOOL_RESULT_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {AGENT_TOOL_RESULT_FIELDS} }} }}"),
            )
            .await?;
        }
        COMPACTION_ENTRY_NAME => {
            rows.compaction_entries = load_rows(
                node,
                COMPACTION_ENTRY_NAME,
                &format!("query {{ {COMPACTION_ENTRY_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {COMPACTION_ENTRY_FIELDS} }} }}"),
            )
            .await?;
        }
        TASK_NAME => {
            rows.tasks = load_rows(
                node,
                TASK_NAME,
                &format!("query {{ {TASK_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {TASK_FIELDS} }} }}"),
            )
            .await?;
        }
        SCHEDULE_NAME => {
            rows.schedules = load_rows(
                node,
                SCHEDULE_NAME,
                &format!("query {{ {SCHEDULE_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {SCHEDULE_FIELDS} }} }}"),
            )
            .await?;
        }
        EVENT_TRIGGER_NAME => {
            rows.event_triggers = load_rows(
                node,
                EVENT_TRIGGER_NAME,
                &format!("query {{ {EVENT_TRIGGER_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {EVENT_TRIGGER_FIELDS} }} }}"),
            )
            .await?;
        }
        SKILL_NAME => {
            rows.skills = load_rows(
                node,
                SKILL_NAME,
                &format!("query {{ {SKILL_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {SKILL_FIELDS} }} }}"),
            )
            .await?;
        }
        TOOL_SELECTION_NAME => {
            rows.tool_selections = load_rows(
                node,
                TOOL_SELECTION_NAME,
                &format!("query {{ {TOOL_SELECTION_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {TOOL_SELECTION_FIELDS} }} }}"),
            )
            .await?;
        }
        INFERENCE_BACKEND_NAME => {
            rows.inference_backends = load_rows(
                node,
                INFERENCE_BACKEND_NAME,
                &format!("query {{ {INFERENCE_BACKEND_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {INFERENCE_BACKEND_FIELDS} }} }}"),
            )
            .await?;
        }
        INFERENCE_PROFILE_NAME => {
            rows.inference_profiles = load_rows(
                node,
                INFERENCE_PROFILE_NAME,
                &format!("query {{ {INFERENCE_PROFILE_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {INFERENCE_PROFILE_FIELDS} }} }}"),
            )
            .await?;
        }
        TOOL_SERVICE_REGISTRY_NAME => {
            rows.tool_service_registries = load_rows(
                node,
                TOOL_SERVICE_REGISTRY_NAME,
                &format!("query {{ {TOOL_SERVICE_REGISTRY_NAME}(filter: {{ _docID: {{ _in: [{in_clause}] }} }}) {{ {TOOL_SERVICE_REGISTRY_FIELDS} }} }}"),
            )
            .await?;
        }
        other => bail!("fetch_doc_patch: unknown collection {other}"),
    }
    Ok(ClientStore::from_rows(rows))
}

pub(crate) fn supports_doc_patch_collection(collection_name: &str) -> bool {
    matches!(
        collection_name,
        AGENT_PRINCIPAL_NAME
            | AGENT_BEHAVIOR_NAME
            | AGENT_RUNTIME_NAME
            | AGENT_CONVERSATION_NAME
            | AGENT_REQUEST_NAME
            | AGENT_RESPONSE_NAME
            | AGENT_MESSAGE_NAME
            | AGENT_SESSION_NAME
            | GOAL_NAME
            | AGENT_TOOL_CALL_NAME
            | AGENT_TOOL_RESULT_NAME
            | COMPACTION_ENTRY_NAME
            | TASK_NAME
            | SCHEDULE_NAME
            | EVENT_TRIGGER_NAME
            | SKILL_NAME
            | TOOL_SELECTION_NAME
            | INFERENCE_BACKEND_NAME
            | INFERENCE_PROFILE_NAME
            | TOOL_SERVICE_REGISTRY_NAME
    )
}

/// Load a snapshot of all rows for a specific `agent_did`. Agent-keyed
/// collections (including Goal) are filtered by `agent_did`; transcript collections
/// (Message, Session, ToolCall, CompactionEntry) are filtered by the
/// session_id list derived from the agent's conversations. Control-plane
/// collections (InferenceBackend, InferenceProfile, ToolServiceRegistry,
/// Task, Schedule, EventTrigger) load in full — they're operator-authored
/// and small.
pub async fn load_agent_scoped_snapshot(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<ClientStore> {
    let did = escape_graphql_string(agent_did);
    let did_filter = format!("filter: {{ agent_did: {{ _eq: \"{did}\" }} }}");

    // Agent-keyed collections.
    let agent_principals: Vec<AgentPrincipalRow> = load_rows(
        node,
        AGENT_PRINCIPAL_NAME,
        &format!("query {{ {AGENT_PRINCIPAL_NAME}({did_filter}) {{ {AGENT_PRINCIPAL_FIELDS} }} }}"),
    )
    .await?;
    let behaviors: Vec<AgentBehaviorRow> = load_rows(
        node,
        AGENT_BEHAVIOR_NAME,
        &format!("query {{ {AGENT_BEHAVIOR_NAME}({did_filter}) {{ {AGENT_BEHAVIOR_FIELDS} }} }}"),
    )
    .await?;
    let runtimes: Vec<AgentRuntimeRow> = load_rows(
        node,
        AGENT_RUNTIME_NAME,
        &format!("query {{ {AGENT_RUNTIME_NAME}({did_filter}) {{ {AGENT_RUNTIME_FIELDS} }} }}"),
    )
    .await?;
    let conversations: Vec<AgentConversationRow> = load_rows(
        node,
        AGENT_CONVERSATION_NAME,
        &format!(
            "query {{ {AGENT_CONVERSATION_NAME}({did_filter}) {{ {AGENT_CONVERSATION_FIELDS} }} }}"
        ),
    )
    .await?;
    let requests: Vec<AgentRequestRow> = load_rows(
        node,
        AGENT_REQUEST_NAME,
        &format!("query {{ {AGENT_REQUEST_NAME}({did_filter}) {{ {AGENT_REQUEST_FIELDS} }} }}"),
    )
    .await?;
    let responses: Vec<AgentResponseRow> = load_rows(
        node,
        AGENT_RESPONSE_NAME,
        &format!("query {{ {AGENT_RESPONSE_NAME}({did_filter}) {{ {AGENT_RESPONSE_FIELDS} }} }}"),
    )
    .await?;
    let tool_results: Vec<AgentToolResultRow> = load_rows(
        node,
        AGENT_TOOL_RESULT_NAME,
        &format!(
            "query {{ {AGENT_TOOL_RESULT_NAME}({did_filter}) {{ {AGENT_TOOL_RESULT_FIELDS} }} }}"
        ),
    )
    .await?;
    let goals: Vec<GoalRow> = load_rows(
        node,
        GOAL_NAME,
        &format!("query {{ {GOAL_NAME}({did_filter}) {{ {GOAL_FIELDS} }} }}"),
    )
    .await?;
    let tool_selections: Vec<ToolSelectionRow> = load_rows(
        node,
        TOOL_SELECTION_NAME,
        &format!("query {{ {TOOL_SELECTION_NAME}({did_filter}) {{ {TOOL_SELECTION_FIELDS} }} }}"),
    )
    .await?;

    // Derive session_id list from the agent's conversations and sessions.
    let mut session_ids: HashSet<String> = HashSet::new();
    for c in &conversations {
        session_ids.insert(c.session_id.clone());
    }
    for r in &requests {
        if let Some(sid) = r.session_id.as_deref() {
            session_ids.insert(sid.to_string());
        }
    }
    for goal in &goals {
        session_ids.insert(goal.session_id.clone());
    }

    // Session-keyed collections.
    let (messages, sessions, tool_calls, compaction_entries) = if session_ids.is_empty() {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new())
    } else {
        let session_in = session_ids
            .iter()
            .map(|s| format!("\"{}\"", escape_graphql_string(s)))
            .collect::<Vec<_>>()
            .join(", ");
        let session_filter = format!("filter: {{ session_id: {{ _in: [{session_in}] }} }}");
        let messages: Vec<AgentMessageRow> = load_rows(
            node,
            AGENT_MESSAGE_NAME,
            &format!(
                "query {{ {AGENT_MESSAGE_NAME}({session_filter}) {{ {AGENT_MESSAGE_FIELDS} }} }}"
            ),
        )
        .await?;
        let sessions: Vec<AgentSessionRow> = load_rows(
            node,
            AGENT_SESSION_NAME,
            &format!(
                "query {{ {AGENT_SESSION_NAME}({session_filter}) {{ {AGENT_SESSION_FIELDS} }} }}"
            ),
        )
        .await?;
        let tool_calls: Vec<AgentToolCallRow> = load_rows(
            node,
            AGENT_TOOL_CALL_NAME,
            &format!("query {{ {AGENT_TOOL_CALL_NAME}({session_filter}) {{ {AGENT_TOOL_CALL_FIELDS} }} }}"),
        )
        .await?;
        let compaction_entries: Vec<CompactionEntryRow> = load_rows(
            node,
            COMPACTION_ENTRY_NAME,
            &format!("query {{ {COMPACTION_ENTRY_NAME}({session_filter}) {{ {COMPACTION_ENTRY_FIELDS} }} }}"),
        )
        .await?;
        (messages, sessions, tool_calls, compaction_entries)
    };

    // Control-plane (load in full; small).
    let tasks = load_tasks(node).await?;
    let schedules = load_schedules(node).await?;
    let event_triggers = load_event_triggers(node).await?;
    let skills = load_skills(node).await?;
    let inference_backends = load_inference_backends(node).await?;
    let inference_profiles = load_inference_profiles(node).await?;
    let tool_service_registries = load_tool_service_registries(node).await?;

    Ok(ClientStore::from_rows(ClientStoreRows {
        agent_principals,
        behaviors,
        runtimes,
        conversations,
        requests,
        responses,
        messages,
        sessions,
        goals,
        tool_calls,
        tool_results,
        compaction_entries,
        tasks,
        schedules,
        event_triggers,
        skills,
        tool_selections,
        inference_backends,
        inference_profiles,
        tool_service_registries,
        ..ClientStoreRows::default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::schema::ensure_runtime_schemas;
    use defra_node::NodeBuilder;
    use gents_protocol::schemas::AGENT_MESSAGE_NAME;
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn upgraded_bearer_snapshot_isolates_unscoped_legacy_rows() {
        let mut peer = PeerRecord::new("Remote", "ticket", "did:key:remote");
        peer.source = Some(super::super::core::bearer_pairing::BEARER_PAIRING_SOURCE.to_string());
        let mut rows = ClientStoreRows {
            agent_principals: vec![serde_json::from_value(json!({
                "agent_did": "did:key:remote",
                "display_name": "legacy remote config"
            }))
            .unwrap()],
            behaviors: vec![serde_json::from_value(json!({
                "behavior_id": "default",
                "agent_did": "did:key:remote",
                "display_name": "Replicated config"
            }))
            .unwrap()],
            conversations: vec![
                serde_json::from_value(json!({
                    "session_id": "allowed",
                    "agent_did": "did:key:remote",
                    "requester_did": "did:key:local"
                }))
                .unwrap(),
                serde_json::from_value(json!({
                    "session_id": "foreign",
                    "agent_did": "did:key:remote",
                    "requester_did": "did:key:other"
                }))
                .unwrap(),
            ],
            messages: vec![
                serde_json::from_value(json!({
                    "message_key": "allowed:1",
                    "session_id": "allowed",
                    "requester_did": "did:key:local"
                }))
                .unwrap(),
                serde_json::from_value(json!({
                    "message_key": "foreign:1",
                    "session_id": "foreign",
                    "requester_did": "did:key:other"
                }))
                .unwrap(),
            ],
            goals: vec![serde_json::from_value(json!({
                "goal_id": "legacy-goal",
                "session_id": "foreign",
                "agent_did": "did:key:remote"
            }))
            .unwrap()],
            ..ClientStoreRows::default()
        };

        isolate_legacy_bearer_rows(&mut rows, &[peer], "did:key:local");

        assert_eq!(
            rows.conversations
                .iter()
                .map(|row| row.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["allowed"]
        );
        assert_eq!(
            rows.messages
                .iter()
                .filter_map(|row| row.session_id.as_deref())
                .collect::<Vec<_>>(),
            vec!["allowed"]
        );
        assert_eq!(rows.message_source_agent_dids, vec![None]);
        assert!(rows.goals.is_empty());
        assert!(rows.agent_principals.is_empty());
        assert_eq!(rows.behaviors.len(), 1);
    }

    #[tokio::test]
    async fn fetch_doc_patch_returns_only_matching_rows() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");

        let mutation = r#"mutation {
            create_AgentMessage(input: {
                message_key: "sess-1:1",
                session_id: "sess-1",
                sequence: 1,
                role: "user",
                content: "hello",
                timestamp: "2026-05-07T00:00:00Z"
            }) { _docID }
            second: create_AgentMessage(input: {
                message_key: "sess-1:2",
                session_id: "sess-1",
                sequence: 2,
                role: "assistant",
                content: "hi",
                timestamp: "2026-05-07T00:00:01Z"
            }) { _docID }
        }"#;
        let response = node.execute(mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        // DefraDB's create_* mutations return an array, so each value is
        // [{_docID: "..."}] rather than {_docID: "..."}.
        let doc_ids: Vec<String> = response
            .data
            .as_ref()
            .and_then(|d| d.as_object())
            .map(|o| {
                o.values()
                    .filter_map(|v| {
                        v.as_array()
                            .and_then(|a| a.first())
                            .and_then(|x| x.get("_docID"))
                            .and_then(|x| x.as_str())
                            .map(String::from)
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(doc_ids.len(), 2);

        let target_id = doc_ids[0].clone();
        let patch = fetch_doc_patch(node.as_ref(), AGENT_MESSAGE_NAME, &[&target_id])
            .await
            .expect("fetch_doc_patch");
        assert_eq!(patch.messages.len(), 1, "expected exactly one row");
    }

    #[tokio::test]
    async fn load_chat_patch_reads_only_the_selected_local_session() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");

        let mutation = r#"mutation {
            first_request: create_AgentRequest(input: {
                request_id: "req-selected",
                agent_did: "did:test:agent",
                behavior_id: "default",
                session_id: "sess-selected",
                content: "selected",
                status: "processing",
                lifecycle_state: "processing",
                created_at: "2026-07-24T00:00:00Z"
            }) { _docID }
            first_response: create_AgentResponse(input: {
                response_key: "req-selected",
                request_id: "req-selected",
                agent_did: "did:test:agent",
                behavior_id: "default",
                session_id: "sess-selected",
                content: "partial",
                reasoning: "",
                status: "streaming",
                error_message: "",
                token_count: 1,
                progress_seq: 1,
                created_at: "2026-07-24T00:00:00Z"
            }) { _docID }
            second_request: create_AgentRequest(input: {
                request_id: "req-unrelated",
                agent_did: "did:test:agent",
                behavior_id: "default",
                session_id: "sess-unrelated",
                content: "unrelated",
                status: "completed",
                lifecycle_state: "completed",
                created_at: "2026-07-24T00:00:00Z"
            }) { _docID }
        }"#;
        let response = node.execute(mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let patch = load_chat_patch(node.as_ref(), "req-selected")
            .await
            .expect("selected local chat patch");
        assert_eq!(patch.requests.len(), 1);
        assert_eq!(patch.requests[0].request_id, "req-selected");
        assert_eq!(patch.responses.len(), 1);
        assert_eq!(patch.responses[0].content.as_deref(), Some("partial"));
        assert!(
            patch
                .requests
                .iter()
                .all(|row| row.session_id.as_deref() == Some("sess-selected")),
            "unrelated session leaked into selected patch"
        );
    }

    #[tokio::test]
    async fn transcript_pages_bound_defradb_rows_and_use_stable_sequence_cursors() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");

        let fields = (1..=600)
            .map(|sequence| {
                format!(
                    r#"m{sequence}: create_AgentMessage(input: {{
                        message_key: "paged:{sequence}",
                        session_id: "paged",
                        sequence: {sequence},
                        role: "assistant",
                        content: "row {sequence}",
                        timestamp: "2026-08-25T00:00:00Z"
                    }}) {{ _docID }}"#
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let response = node.execute(&format!("mutation {{ {fields} }}")).await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let tip = load_session_transcript_page(node.as_ref(), "paged", None, None, None, Some(10))
            .await
            .expect("tip page");
        assert_eq!(tip.query_count, 2);
        assert_eq!(tip.message_query_limit, 11);
        assert_eq!(tip.tool_call_query_limit, 321);
        assert_eq!(tip.queried_rows, 11);
        assert!(!tip.source_exhausted);
        let tip_sequences = tip
            .store
            .messages
            .iter()
            .filter_map(|row| row.sequence)
            .collect::<Vec<_>>();
        assert_eq!(tip_sequences.iter().copied().min(), Some(591));
        assert_eq!(tip_sequences.iter().copied().max(), Some(600));

        let older = load_session_transcript_page(
            node.as_ref(),
            "paged",
            None,
            None,
            Some("paged:591"),
            Some(10),
        )
        .await
        .expect("older page");
        assert_eq!(older.query_count, 3);
        assert_eq!(older.queried_rows, 11);
        assert!(older.has_newer);
        assert!(older
            .store
            .messages
            .iter()
            .all(|row| row.sequence.is_some_and(|sequence| sequence < 591)));
        assert!(tip_sequences.iter().all(|sequence| older
            .store
            .messages
            .iter()
            .all(|row| row.sequence != Some(*sequence))));

        let tool_cursor = tool_group_cursor_sequence("tools-42");
        assert_eq!(tool_cursor, Some(42));

        let tool_fields = (1..=321)
            .map(|sequence| {
                format!(
                    r#"t{sequence}: create_AgentToolCall(input: {{
                        tool_call_key: "tool-heavy:{sequence}",
                        session_id: "tool-heavy",
                        message_sequence: {sequence},
                        tool_name: "bounded_tool",
                        tool_call_id: "call-{sequence}",
                        status: "completed",
                        lifecycle_state: "completed"
                    }}) {{ _docID }}"#
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let response = node.execute(&format!("mutation {{ {tool_fields} }}")).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let tool_page =
            load_session_transcript_page(node.as_ref(), "tool-heavy", None, None, None, Some(40))
                .await
                .expect("many bounded tool groups remain pageable");
        assert_eq!(tool_page.store.tool_calls.len(), 320);
        assert_eq!(tool_page.queried_rows, 321);
        assert!(!tool_page.source_exhausted);

        let message_fields = (1..=10)
            .map(|sequence| {
                format!(
                    r#"wm{sequence}: create_AgentMessage(input: {{
                        message_key: "tool-window:{sequence}",
                        session_id: "tool-window",
                        sequence: {sequence},
                        role: "assistant",
                        content: "window row {sequence}"
                    }}) {{ _docID }}"#
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let window_tool_fields = (1..=400)
            .map(|index| {
                let sequence = ((index - 1) / 40) + 1;
                format!(
                    r#"wt{index}: create_AgentToolCall(input: {{
                        tool_call_key: "tool-window:{index}",
                        session_id: "tool-window",
                        message_sequence: {sequence},
                        tool_name: "window_tool",
                        tool_call_id: "window-call-{index}"
                    }}) {{ _docID }}"#
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let response = node
            .execute(&format!(
                "mutation {{ {message_fields} {window_tool_fields} }}"
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let window_tip =
            load_session_transcript_page(node.as_ref(), "tool-window", None, None, None, Some(10))
                .await
                .expect("tool-window tip");
        assert_eq!(window_tip.store.tool_calls.len(), 320);
        assert!(window_tip
            .store
            .messages
            .iter()
            .all(|row| row.sequence.is_some_and(|sequence| sequence > 2)));
        assert!(!window_tip.source_exhausted);
        let window_older = load_session_transcript_page(
            node.as_ref(),
            "tool-window",
            None,
            None,
            Some("tool-window:3"),
            Some(10),
        )
        .await
        .expect("tool-window older page");
        assert_eq!(
            window_older
                .store
                .messages
                .iter()
                .filter_map(|row| row.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(window_older.store.tool_calls.len(), 80);
        assert!(window_older.source_exhausted);
    }

    #[tokio::test]
    async fn transcript_pages_preserve_scope_and_equal_sequence_groups() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");

        let response = node
            .execute(
                r#"mutation {
                    kept: create_AgentMessage(input: {
                        message_key: "scope:kept", session_id: "shared-session",
                        agent_did: "did:test:selected", requester_did: "did:test:local",
                        sequence: 3, role: "assistant", content: "kept"
                    }) { _docID }
                    wrongAgent: create_AgentMessage(input: {
                        message_key: "scope:wrong-agent", session_id: "shared-session",
                        agent_did: "did:test:other", requester_did: "did:test:local",
                        sequence: 4, role: "assistant", content: "wrong agent"
                    }) { _docID }
                    wrongRequester: create_AgentMessage(input: {
                        message_key: "scope:wrong-requester", session_id: "shared-session",
                        agent_did: "did:test:selected", requester_did: "did:test:other-requester",
                        sequence: 5, role: "assistant", content: "wrong requester"
                    }) { _docID }
                    keptTool: create_AgentToolCall(input: {
                        tool_call_key: "scope:kept-tool", session_id: "shared-session",
                        agent_did: "did:test:selected", requester_did: "did:test:local",
                        message_sequence: 3, tool_name: "kept"
                    }) { _docID }
                    wrongTool: create_AgentToolCall(input: {
                        tool_call_key: "scope:wrong-tool", session_id: "shared-session",
                        agent_did: "did:test:other", requester_did: "did:test:local",
                        message_sequence: 4, tool_name: "wrong"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let scoped = load_session_transcript_page(
            node.as_ref(),
            "shared-session",
            Some("did:test:selected"),
            Some("did:test:local"),
            None,
            Some(10),
        )
        .await
        .expect("scoped page");
        assert_eq!(scoped.store.messages.len(), 1);
        assert_eq!(scoped.store.messages[0].message_key, "scope:kept");
        assert_eq!(scoped.store.tool_calls.len(), 1);
        assert_eq!(scoped.store.tool_calls[0].tool_call_key, "scope:kept-tool");

        let response = node
            .execute(
                r#"mutation {
                    first: create_AgentMessage(input: {
                        message_key: "equal:a", session_id: "equal",
                        sequence: 2, role: "assistant", content: "a"
                    }) { _docID }
                    second: create_AgentMessage(input: {
                        message_key: "equal:b", session_id: "equal",
                        sequence: 2, role: "assistant", content: "b"
                    }) { _docID }
                    older: create_AgentMessage(input: {
                        message_key: "equal:older", session_id: "equal",
                        sequence: 1, role: "assistant", content: "older"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let equal_tip =
            load_session_transcript_page(node.as_ref(), "equal", None, None, None, Some(2))
                .await
                .expect("equal-sequence tip");
        assert_eq!(equal_tip.store.messages.len(), 2);
        assert!(equal_tip
            .store
            .messages
            .iter()
            .all(|row| row.sequence == Some(2)));
        let equal_older = load_session_transcript_page(
            node.as_ref(),
            "equal",
            None,
            None,
            Some("equal:a"),
            Some(2),
        )
        .await
        .expect("equal-sequence older page");
        assert_eq!(equal_older.store.messages.len(), 1);
        assert_eq!(equal_older.store.messages[0].message_key, "equal:older");
    }

    #[tokio::test]
    async fn transcript_pages_reject_unrepresentable_null_sequences_truthfully() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");
        let response = node
            .execute(
                r#"mutation {
                    create_AgentMessage(input: {
                        message_key: "legacy:null", session_id: "legacy",
                        role: "assistant", content: "legacy"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let error =
            load_session_transcript_page(node.as_ref(), "legacy", None, None, None, Some(10))
                .await
                .expect_err("null sequence must not be silently skipped");
        assert!(error
            .to_string()
            .contains("bounded pagination cannot represent that schema state losslessly"));
    }

    #[tokio::test]
    async fn fetch_doc_patch_returns_empty_store_for_no_matches() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");

        let patch = fetch_doc_patch(node.as_ref(), AGENT_MESSAGE_NAME, &["never-existed"])
            .await
            .expect("fetch_doc_patch");
        assert_eq!(patch.messages.len(), 0);
    }

    #[tokio::test]
    async fn fetch_doc_patch_empty_input_is_no_op() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");

        let patch = fetch_doc_patch(node.as_ref(), AGENT_MESSAGE_NAME, &[])
            .await
            .expect("fetch_doc_patch");
        assert_eq!(patch.row_count(), 0);
    }

    #[tokio::test]
    async fn fetch_doc_patch_unknown_collection_errors() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");

        let result = fetch_doc_patch(node.as_ref(), "NotARealCollection", &["x"]).await;
        assert!(result.is_err());
    }

    #[test]
    fn doc_patch_support_excludes_pairing_control_collections() {
        assert!(supports_doc_patch_collection(INFERENCE_BACKEND_NAME));
        assert!(supports_doc_patch_collection(TOOL_SERVICE_REGISTRY_NAME));
        assert!(!supports_doc_patch_collection("PeerPairingApplied"));
        assert!(!supports_doc_patch_collection("BearerPairingReady"));
    }

    #[tokio::test]
    async fn load_agent_runtimes_hydrates_executor_capacity_and_queue_depth() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");

        let response = node
            .execute(
                r#"mutation {
                    create_AgentRuntime(input: {
                        agent_did: "did:key:runtime-capacity",
                        behavior_executor_capacity: 7,
                        behavior_executor_queue_depth: 3
                    }) { agent_did }
                }"#,
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let runtimes = load_agent_runtimes(node.as_ref())
            .await
            .expect("load agent runtimes");
        let runtime = runtimes
            .iter()
            .find(|row| row.agent_did == "did:key:runtime-capacity")
            .expect("created runtime");
        assert_eq!(runtime.behavior_executor_capacity, Some(7));
        assert_eq!(runtime.behavior_executor_queue_depth, Some(3));
    }

    #[tokio::test]
    async fn load_agent_tool_calls_hydrates_subagent_projection_fields() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");

        let response = node
            .execute(
                r#"mutation {
                    create_AgentToolCall(input: {
                        tool_call_key: "session-1:spawn-1",
                        request_id: "parent-1",
                        session_id: "session-1",
                        message_sequence: 1,
                        tool_name: "spawn_subagent",
                        tool_call_id: "spawn-1",
                        args: "{}",
                        result: "",
                        status: "called",
                        lifecycle_state: "running",
                        child_request_id: "child-1",
                        await_mode: "background",
                        started_at: "2026-07-29T00:00:00Z"
                    }) { tool_call_key }
                }"#,
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let tool_calls = load_agent_tool_calls(node.as_ref())
            .await
            .expect("load agent tool calls");
        let tool_call = tool_calls
            .iter()
            .find(|row| row.tool_call_key == "session-1:spawn-1")
            .expect("created tool call");
        assert_eq!(tool_call.child_request_id.as_deref(), Some("child-1"));
        assert_eq!(tool_call.await_mode.as_deref(), Some("background"));
    }

    #[tokio::test]
    async fn load_agent_scoped_snapshot_excludes_other_agents() {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");

        let mutation = r#"mutation {
            alpha: create_AgentConversation(input: {
                session_id: "alpha-1",
                agent_did: "did:alpha",
                behavior_id: "default",
                title: "alpha",
                title_source: "user",
                preview_text: "",
                status: "active",
                created_at: "2026-05-07T00:00:00Z",
                updated_at: "2026-05-07T00:00:00Z",
                latest_request_id: ""
            }) { _docID }
            beta: create_AgentConversation(input: {
                session_id: "beta-1",
                agent_did: "did:beta",
                behavior_id: "default",
                title: "beta",
                title_source: "user",
                preview_text: "",
                status: "active",
                created_at: "2026-05-07T00:00:00Z",
                updated_at: "2026-05-07T00:00:00Z",
                latest_request_id: ""
            }) { _docID }
        }"#;
        let response = node.execute(mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let goal_mutation = r#"mutation {
            alpha: create_Goal(input: {
                goal_id: "alpha-goal",
                session_id: "alpha-goal-only",
                agent_did: "did:alpha",
                objective: "goal-only session",
                status: "active",
                created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
            beta: create_Goal(input: {
                goal_id: "beta-goal",
                session_id: "beta-goal-only",
                agent_did: "did:beta",
                objective: "other agent",
                status: "active",
                created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
        }"#;
        let response = node.execute(goal_mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let store = load_agent_scoped_snapshot(node.as_ref(), "did:alpha")
            .await
            .expect("load_agent_scoped_snapshot");

        let dids: Vec<&str> = store
            .conversations
            .iter()
            .filter_map(|c| c.agent_did.as_deref())
            .collect();
        assert!(
            dids.iter().all(|d| *d == "did:alpha"),
            "expected only did:alpha conversations; got {dids:?}"
        );
        assert_eq!(store.goals.len(), 1);
        assert_eq!(store.goals[0].session_id, "alpha-goal-only");
    }
}
