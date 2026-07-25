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
use serde_json::{json, Value};

use super::peer_directory::PeerRecord;
use super::store::{ClientStore, ClientStoreRows};

const REMOTE_SNAPSHOT_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

const AGENT_PRINCIPAL_FIELDS: &str =
    "agent_did display_name default_behavior_id enabled created_at created_by";
const AGENT_BEHAVIOR_FIELDS: &str = "behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled skill_refs skill_excludes created_at";
const AGENT_RUNTIME_FIELDS: &str = "agent_did process_state reconcile_phase active_generation router_generation default_behavior_id runnable_behavior_count unavailable_behavior_count behavior_executor_capacity behavior_executor_queue_depth last_reconcile_result last_reconcile_error last_reconcile_completed_at updated_at";
const AGENT_CONVERSATION_FIELDS: &str = "session_id agent_name agent_did requester_did behavior_id title title_source preview_text status created_at updated_at latest_request_id";
const AGENT_REQUEST_FIELDS: &str = "request_id agent_did requester_did behavior_id session_id retry_parent_request retry_root_request superseded_by_request content temperature top_p top_k max_tokens metadata status lifecycle_state backend_id execution_origin caused_by_trigger_id caused_by_trigger_kind caused_by_parent_request_id failure_reason terminalized_at terminal_redrive_attempts created_at claimed_at deadline retry_count max_retries interrupt_requested_at valid_until";
const AGENT_RESPONSE_FIELDS: &str = "response_key request_id agent_did requester_did behavior_id session_id content reasoning status error_message token_count progress_seq materialized_message_sequence materialized_at created_at completed_at interrupted_at";
const AGENT_MESSAGE_FIELDS: &str =
    "message_key session_id requester_did sequence role content reasoning timestamp";
const AGENT_SESSION_FIELDS: &str =
    "session_id agent_name requester_did behavior_id started ended status";
const GOAL_FIELDS: &str = "goal_id session_id agent_did objective status token_budget tokens_used active_time_seconds active_started_at consecutive_blocked_audits last_blocked_request_id last_blocked_reason last_continued_from_request_id continuation_sequence wrapup_requested wrapup_completed infrastructure_retry_count last_failure completion_evidence created_at updated_at";
const AGENT_TOOL_CALL_FIELDS: &str = "tool_call_key session_id request_id requester_did message_sequence tool_name tool_call_id args result status lifecycle_state cancel_policy workflow_group_id workflow_role deadline_at cancel_cause started_at completed_at selected_service_id selected_tool_name tool_failure_class denial_reason denied_argv denied_command denied_argument denied_subcommand denied_prefix policy_mode policy_network latency_ms partial_output_tail partial_output_seq";
const AGENT_TOOL_RESULT_FIELDS: &str = "agent_did requester_did session_id tool_name tool_input output_text truncated truncation_metadata conversation_doc_id created_at discarded_because_interrupted";
const COMPACTION_ENTRY_FIELDS: &str = "compaction_key session_id requester_did sequence summary files_read files_modified messages_compacted original_tokens compacted_tokens created_at";
const TASK_FIELDS: &str = "task_id name description behavior_id prompt_template enabled output_schema_ref created_at updated_at";
const SKILL_FIELDS: &str = "skill_id agent_did scope name description instructions tool_refs display_name interface_json enabled created_at";
const SCHEDULE_FIELDS: &str = "schedule_id task_id interval_secs cron timezone missed_run_policy enabled concurrency next_run_at last_attempt_at last_status last_error fire_count created_at updated_at";
const EVENT_TRIGGER_FIELDS: &str = "trigger_id task_id source_collection event_kind filter enabled concurrency created_at updated_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count";
const TOOL_SELECTION_FIELDS: &str = "selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode command_execution_policy command_allowed_argv_prefixes command_forbidden_argv_prefixes command_network_mode cli_tool_names enable_meta_tools allowed_mcp_service_ids delegate_to backgroundable_tool_names enable_memory enable_session_history_tool enable_context_budget enable_defra_query defra_query_collections subagent_targets subagent_spawn_enabled orchestration_enabled subagent_steering_enabled subagent_background_enabled subagent_allow_cross_deployment cross_deployment_spawn_timeout_seconds tool_policy_version write_tools subagent_default_await_mode enable_self_config self_config_categories self_config_no_lockout self_config_dry_run";
const INFERENCE_BACKEND_FIELDS: &str = "backend_id name provider_kind openai_wire_api endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status";
const INFERENCE_PROFILE_FIELDS: &str = "profile_id display_name context_window max_output_tokens max_turns temperature top_p top_k min_p frequency_penalty presence_penalty repetition_penalty stream_batch_ms stream_liveness_timeout_secs deadline_duration_secs retry_max_transport retry_backoff_ms retry_max_resample retry_allow_repair retry_interactive_max";
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
    let mut remote_loads = Vec::new();

    for peer in peers {
        let Some(graphql) = peer
            .graphql
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };

        let peer = peer.clone();
        let graphql = graphql.to_string();
        let requester_did = requester_did.to_string();
        remote_loads.push(tokio::spawn(async move {
            let result =
                load_full_snapshot_from_graphql(&graphql, &peer.agent_did, &requester_did).await;
            (peer, graphql, result)
        }));
    }

    for remote_load in remote_loads {
        match remote_load.await {
            Ok((peer, graphql, Ok(mut remote))) => {
                remote.stamp_source_agent_did(&peer.agent_did);
                let remote_count = remote.row_count();
                append_rows(&mut rows, remote.to_rows());
                tracing::info!(
                    target: "gents_desktop_core::query",
                    peer_id = %peer.peer_id,
                    label = %peer.label,
                    graphql,
                    rows = remote_count,
                    "desktop loaded remote GraphQL snapshot"
                );
            }
            Ok((peer, graphql, Err(error))) => {
                tracing::warn!(
                    target: "gents_desktop_core::query",
                    peer_id = %peer.peer_id,
                    label = %peer.label,
                    graphql,
                    error = %error,
                    "desktop could not load remote GraphQL snapshot"
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "gents_desktop_core::query",
                    error = %error,
                    "desktop remote GraphQL snapshot task failed"
                );
            }
        }
    }

    Ok(ClientStore::from_rows(rows))
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
        "query { EventTrigger { trigger_id task_id source_collection event_kind filter enabled concurrency created_at updated_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count } }",
    )
    .await
}

pub async fn load_tool_selections(node: &EmbeddedNode) -> Result<Vec<ToolSelectionRow>> {
    load_rows(
        node,
        "ToolSelection",
        "query { ToolSelection { selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode command_execution_policy command_allowed_argv_prefixes command_forbidden_argv_prefixes command_network_mode cli_tool_names enable_meta_tools allowed_mcp_service_ids delegate_to backgroundable_tool_names enable_memory enable_session_history_tool enable_context_budget enable_defra_query defra_query_collections subagent_targets subagent_spawn_enabled orchestration_enabled subagent_steering_enabled subagent_background_enabled subagent_allow_cross_deployment cross_deployment_spawn_timeout_seconds tool_policy_version write_tools subagent_default_await_mode enable_self_config self_config_categories self_config_no_lockout self_config_dry_run } }",
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
        "query { InferenceProfile { profile_id display_name context_window max_output_tokens max_turns temperature top_p top_k min_p frequency_penalty presence_penalty repetition_penalty stream_batch_ms stream_liveness_timeout_secs deadline_duration_secs retry_max_transport retry_backoff_ms retry_max_resample retry_allow_repair retry_interactive_max } }",
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

pub async fn load_full_snapshot_from_graphql(
    graphql: &str,
    agent_did: &str,
    requester_did: &str,
) -> Result<ClientStore> {
    let client = reqwest::Client::builder()
        .timeout(REMOTE_SNAPSHOT_HTTP_TIMEOUT)
        .build()
        .context("building remote GraphQL snapshot HTTP client")?;
    let data = execute_remote_snapshot_query(&client, graphql, agent_did, requester_did).await?;

    Ok(ClientStore::from_rows(ClientStoreRows {
        agent_principals: parse_remote_rows(&data, "AgentPrincipal")?,
        behaviors: parse_remote_rows(&data, "AgentBehavior")?,
        runtimes: parse_remote_rows(&data, "AgentRuntime")?,
        conversations: parse_remote_rows(&data, "AgentConversation")?,
        requests: parse_remote_rows(&data, "AgentRequest")?,
        responses: parse_remote_rows(&data, "AgentResponse")?,
        messages: parse_remote_rows(&data, "AgentMessage")?,
        sessions: parse_remote_rows(&data, "AgentSession")?,
        goals: parse_remote_rows(&data, "Goal")?,
        tool_calls: parse_remote_rows(&data, "AgentToolCall")?,
        tool_results: parse_remote_rows(&data, "AgentToolResult")?,
        compaction_entries: parse_remote_rows(&data, "CompactionEntry")?,
        tasks: parse_remote_rows(&data, "Task")?,
        schedules: parse_remote_rows(&data, "Schedule")?,
        event_triggers: parse_remote_rows(&data, "EventTrigger")?,
        skills: parse_remote_rows(&data, "Skill")?,
        tool_selections: parse_remote_rows(&data, "ToolSelection")?,
        inference_backends: parse_remote_rows(&data, "InferenceBackend")?,
        inference_profiles: parse_remote_rows(&data, "InferenceProfile")?,
        tool_service_registries: parse_remote_rows(&data, "ToolServiceRegistry")?,
        ..ClientStoreRows::default()
    }))
}

pub async fn load_chat_patch_from_graphql(graphql: &str, request_id: &str) -> Result<ClientStore> {
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return Ok(ClientStore::default());
    }

    let client = reqwest::Client::builder()
        .timeout(REMOTE_SNAPSHOT_HTTP_TIMEOUT)
        .build()
        .context("building remote GraphQL chat patch HTTP client")?;
    let lookup_query = remote_request_lookup_query(request_id);
    let lookup_data = execute_remote_graphql_query(
        &client,
        graphql,
        &lookup_query,
        "remote GraphQL request lookup",
    )
    .await?;
    let request_rows: Vec<AgentRequestRow> = parse_remote_rows(&lookup_data, "AgentRequest")?;
    let Some(session_id) = request_rows
        .first()
        .and_then(|row| row.session_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    else {
        return Ok(ClientStore::from_rows(ClientStoreRows {
            requests: request_rows,
            responses: parse_remote_rows(&lookup_data, "AgentResponse")?,
            ..ClientStoreRows::default()
        }));
    };

    let patch_query = remote_chat_patch_query(&session_id);
    let data =
        execute_remote_graphql_query(&client, graphql, &patch_query, "remote GraphQL chat patch")
            .await?;

    chat_patch_from_data(&data)
}

/// Load only the selected request's conversation slice from the embedded
/// replica. This is the bounded polling fallback for a dropped/coalesced
/// observer event; it does not reload every conversation for the agent.
pub async fn load_chat_patch(node: &EmbeddedNode, request_id: &str) -> Result<ClientStore> {
    let request_id = request_id.trim();
    if request_id.is_empty() {
        return Ok(ClientStore::default());
    }

    let lookup_query = remote_request_lookup_query(request_id);
    let lookup_data =
        execute_local_graphql_query(node, &lookup_query, "local request lookup").await?;
    let request_rows: Vec<AgentRequestRow> = parse_remote_rows(&lookup_data, "AgentRequest")?;
    let Some(session_id) = request_rows
        .first()
        .and_then(|row| row.session_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    else {
        return Ok(ClientStore::from_rows(ClientStoreRows {
            requests: request_rows,
            responses: parse_remote_rows(&lookup_data, "AgentResponse")?,
            ..ClientStoreRows::default()
        }));
    };

    let patch_query = remote_chat_patch_query(&session_id);
    let data = execute_local_graphql_query(node, &patch_query, "local chat patch").await?;
    chat_patch_from_data(&data)
}

fn chat_patch_from_data(data: &Value) -> Result<ClientStore> {
    Ok(ClientStore::from_rows(ClientStoreRows {
        conversations: parse_remote_rows(&data, "AgentConversation")?,
        requests: parse_remote_rows(&data, "AgentRequest")?,
        responses: parse_remote_rows(&data, "AgentResponse")?,
        messages: parse_remote_rows(&data, "AgentMessage")?,
        sessions: parse_remote_rows(&data, "AgentSession")?,
        goals: parse_remote_rows(&data, "Goal")?,
        tool_calls: parse_remote_rows(&data, "AgentToolCall")?,
        tool_results: parse_remote_rows(&data, "AgentToolResult")?,
        compaction_entries: parse_remote_rows(&data, "CompactionEntry")?,
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

#[derive(Debug, Deserialize)]
struct RemoteGraphqlResponse {
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    errors: Option<Value>,
}

async fn execute_remote_snapshot_query(
    client: &reqwest::Client,
    graphql: &str,
    agent_did: &str,
    requester_did: &str,
) -> Result<Value> {
    let query = remote_snapshot_query(agent_did, requester_did);
    execute_remote_graphql_query(client, graphql, &query, "snapshot").await
}

async fn execute_remote_graphql_query(
    client: &reqwest::Client,
    graphql: &str,
    query: &str,
    operation: &str,
) -> Result<Value> {
    let response = client
        .post(graphql)
        .json(&json!({ "query": query }))
        .send()
        .await
        .with_context(|| format!("sending {operation} query to {graphql}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .with_context(|| format!("reading {operation} response from {graphql}"))?;
    if !status.is_success() {
        bail!(
            "{operation} query to {graphql} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }

    let response: RemoteGraphqlResponse = serde_json::from_slice(&body)
        .with_context(|| format!("decoding {operation} response from {graphql}"))?;
    if let Some(errors) = response
        .errors
        .as_ref()
        .filter(|errors| !errors_is_empty(errors))
    {
        bail!("{operation} query returned errors: {errors}");
    }
    response
        .data
        .context(format!("{operation} query returned no data"))
}

fn parse_remote_rows<T>(data: &Value, root: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let rows = data
        .get(root)
        .ok_or_else(|| anyhow!("remote GraphQL snapshot missing root field {root}"))?;
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
                        "skipping malformed remote row"
                    ),
                }
            }
            Ok(parsed)
        }
        other => Err(anyhow!(
            "remote GraphQL snapshot for {root} returned non-array payload: {other}"
        )),
    }
}

fn errors_is_empty(errors: &Value) -> bool {
    match errors {
        Value::Null => true,
        Value::Array(errors) => errors.is_empty(),
        _ => false,
    }
}

fn append_rows(target: &mut ClientStoreRows, mut incoming: ClientStoreRows) {
    target
        .agent_principals
        .append(&mut incoming.agent_principals);
    target.behaviors.append(&mut incoming.behaviors);
    target.runtimes.append(&mut incoming.runtimes);
    target.conversations.append(&mut incoming.conversations);
    target.requests.append(&mut incoming.requests);
    target.responses.append(&mut incoming.responses);
    target.messages.append(&mut incoming.messages);
    target.sessions.append(&mut incoming.sessions);
    target.goals.append(&mut incoming.goals);
    target.tool_calls.append(&mut incoming.tool_calls);
    target.tool_results.append(&mut incoming.tool_results);
    target
        .compaction_entries
        .append(&mut incoming.compaction_entries);
    target
        .message_source_agent_dids
        .append(&mut incoming.message_source_agent_dids);
    target
        .session_source_agent_dids
        .append(&mut incoming.session_source_agent_dids);
    target
        .tool_call_source_agent_dids
        .append(&mut incoming.tool_call_source_agent_dids);
    target
        .tool_result_source_agent_dids
        .append(&mut incoming.tool_result_source_agent_dids);
    target
        .compaction_entry_source_agent_dids
        .append(&mut incoming.compaction_entry_source_agent_dids);
    target.tasks.append(&mut incoming.tasks);
    target.schedules.append(&mut incoming.schedules);
    target.event_triggers.append(&mut incoming.event_triggers);
    target
        .task_source_agent_dids
        .append(&mut incoming.task_source_agent_dids);
    target
        .schedule_source_agent_dids
        .append(&mut incoming.schedule_source_agent_dids);
    target
        .event_trigger_source_agent_dids
        .append(&mut incoming.event_trigger_source_agent_dids);
    target.skills.append(&mut incoming.skills);
    target
        .skill_source_agent_dids
        .append(&mut incoming.skill_source_agent_dids);
    target.tool_selections.append(&mut incoming.tool_selections);
    target
        .inference_backends
        .append(&mut incoming.inference_backends);
    target
        .inference_profiles
        .append(&mut incoming.inference_profiles);
    target
        .tool_service_registries
        .append(&mut incoming.tool_service_registries);
    target
        .inference_backend_source_agent_dids
        .append(&mut incoming.inference_backend_source_agent_dids);
    target
        .inference_profile_source_agent_dids
        .append(&mut incoming.inference_profile_source_agent_dids);
    target
        .tool_service_registry_source_agent_dids
        .append(&mut incoming.tool_service_registry_source_agent_dids);
}

fn remote_request_lookup_query(request_id: &str) -> String {
    let request_id = escape_graphql_string(request_id);
    format!(
        r#"
query DesktopRemoteRequestLookup {{
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
  AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_MESSAGE_FIELDS} }}
  AgentSession(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_SESSION_FIELDS} }}
  Goal(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {GOAL_FIELDS} }}
  AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_TOOL_CALL_FIELDS} }}
  AgentToolResult(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_TOOL_RESULT_FIELDS} }}
  CompactionEntry(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {COMPACTION_ENTRY_FIELDS} }}
}}
"#
    )
}

fn remote_snapshot_query(agent_did: &str, requester_did: &str) -> String {
    let agent_did = escape_graphql_string(agent_did);
    let requester_did = escape_graphql_string(requester_did);
    format!(
        r#"
query DesktopRemoteSnapshot {{
  AgentPrincipal(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ {AGENT_PRINCIPAL_FIELDS} }}
  AgentBehavior(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ {AGENT_BEHAVIOR_FIELDS} }}
  AgentRuntime(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ {AGENT_RUNTIME_FIELDS} }}
  AgentConversation(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{ {AGENT_CONVERSATION_FIELDS} }}
  AgentRequest(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{ {AGENT_REQUEST_FIELDS} }}
  AgentResponse(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{ {AGENT_RESPONSE_FIELDS} }}
  AgentMessage(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{ {AGENT_MESSAGE_FIELDS} }}
  AgentSession(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{ {AGENT_SESSION_FIELDS} }}
  Goal(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ {GOAL_FIELDS} }}
  AgentToolCall(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{ {AGENT_TOOL_CALL_FIELDS} }}
  AgentToolResult(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{ {AGENT_TOOL_RESULT_FIELDS} }}
  CompactionEntry(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{ {COMPACTION_ENTRY_FIELDS} }}
  Task {{ task_id name description behavior_id prompt_template enabled output_schema_ref created_at updated_at }}
  Schedule {{ schedule_id task_id interval_secs cron timezone missed_run_policy enabled concurrency next_run_at last_attempt_at last_status last_error fire_count created_at updated_at }}
  EventTrigger {{ trigger_id task_id source_collection event_kind filter enabled concurrency created_at updated_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count }}
  Skill(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ {SKILL_FIELDS} }}
  ToolSelection(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ {TOOL_SELECTION_FIELDS} }}
  InferenceBackend {{ backend_id name provider_kind openai_wire_api endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status }}
  InferenceProfile {{ profile_id display_name context_window max_output_tokens max_turns temperature top_p top_k min_p frequency_penalty presence_penalty repetition_penalty stream_batch_ms stream_liveness_timeout_secs deadline_duration_secs retry_max_transport retry_backoff_ms retry_max_resample retry_allow_repair retry_interactive_max }}
  ToolServiceRegistry {{ service_id display_name description hostname tailscale_ip lan_ip mcp_port mcp_path status version updated_at }}
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
    use std::sync::Arc;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    async fn remote_snapshot_hydrates_from_requester_scoped_query() {
        let server = MockServer::start().await;
        let data = json!({
            "AgentPrincipal": [],
            "AgentBehavior": [],
            "AgentRuntime": [],
            "AgentConversation": [],
            "AgentRequest": [],
            "AgentResponse": [],
            "AgentMessage": [],
            "AgentSession": [],
            "Goal": [],
            "AgentToolCall": [],
            "AgentToolResult": [],
            "CompactionEntry": [],
            "Task": [],
            "Schedule": [],
            "EventTrigger": [],
            "Skill": [],
            "ToolSelection": [],
            "InferenceBackend": [],
            "InferenceProfile": [],
            "ToolServiceRegistry": []
        });
        Mock::given(method("POST"))
            .and(body_string_contains("did:test:agent"))
            .and(body_string_contains("did:test:requester"))
            .and(body_string_contains("requester_did"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": data })))
            .expect(1)
            .mount(&server)
            .await;

        let snapshot =
            load_full_snapshot_from_graphql(&server.uri(), "did:test:agent", "did:test:requester")
                .await
                .expect("requester-scoped remote snapshot");

        assert_eq!(snapshot.row_count(), 0);
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

    #[test]
    fn remote_tool_call_queries_include_local_field_set() {
        let chat_patch = remote_chat_patch_query("sess-1");
        let remote_snapshot = remote_snapshot_query("did:test:agent", "did:test:requester");
        for field in AGENT_TOOL_CALL_FIELDS.split_whitespace() {
            assert!(
                chat_patch.contains(field),
                "remote chat patch missing AgentToolCall field {field}"
            );
            assert!(
                remote_snapshot.contains(field),
                "remote snapshot missing AgentToolCall field {field}"
            );
        }
    }

    #[test]
    fn remote_goal_queries_include_local_field_set() {
        let chat_patch = remote_chat_patch_query("sess-1");
        let remote_snapshot = remote_snapshot_query("did:test:agent", "did:test:requester");
        for field in GOAL_FIELDS.split_whitespace() {
            assert!(
                chat_patch.contains(field),
                "remote chat patch missing Goal field {field}"
            );
            assert!(
                remote_snapshot.contains(field),
                "remote snapshot missing Goal field {field}"
            );
        }
    }

    #[test]
    fn remote_runtime_query_includes_local_field_set() {
        let remote_snapshot = remote_snapshot_query("did:test:agent", "did:test:requester");
        for field in AGENT_RUNTIME_FIELDS.split_whitespace() {
            assert!(
                remote_snapshot.contains(field),
                "remote snapshot missing AgentRuntime field {field}"
            );
        }
    }

    #[test]
    fn remote_snapshot_scopes_conversation_rows_to_agent_and_requester() {
        let query = remote_snapshot_query(
            r#"did:test:agent"with-quote"#,
            r#"did:test:requester"with-quote"#,
        );

        assert!(query.contains(
            r#"agent_did: { _eq: "did:test:agent\"with-quote" }, requester_did: { _eq: "did:test:requester\"with-quote" }"#
        ));
        assert!(query.contains(
            r#"AgentMessage(filter: { agent_did: { _eq: "did:test:agent\"with-quote" }, requester_did: { _eq: "did:test:requester\"with-quote" } })"#
        ));
        assert!(!query.contains("AgentConversation {"));
        assert!(!query.contains("AgentRequest {"));
    }
}
