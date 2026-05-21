use std::collections::HashSet;

use anyhow::{anyhow, bail, Context, Result};
use defra_agent_protocol::graphql::escape_graphql_string;
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentConversationRow, AgentMessageRow, AgentPrincipalRow, AgentRequestRow,
    AgentResponseRow, AgentRuntimeRow, AgentSessionRow, AgentToolCallRow, AgentToolResultRow,
    CompactionEntryRow, EventTriggerRow, InferenceBackendRow, InferenceProfileRow, ScheduleRow,
    TaskRow, ToolSelectionRow, ToolServiceRegistryRow,
};
use defra_agent_protocol::schemas::{
    AGENT_BEHAVIOR_NAME, AGENT_CONVERSATION_NAME, AGENT_MESSAGE_NAME, AGENT_PRINCIPAL_NAME,
    AGENT_REQUEST_NAME, AGENT_RESPONSE_NAME, AGENT_RUNTIME_NAME, AGENT_SESSION_NAME,
    AGENT_TOOL_CALL_NAME, AGENT_TOOL_RESULT_NAME, COMPACTION_ENTRY_NAME, EVENT_TRIGGER_NAME,
    INFERENCE_BACKEND_NAME, INFERENCE_PROFILE_NAME, SCHEDULE_NAME, TASK_NAME, TOOL_SELECTION_NAME,
    TOOL_SERVICE_REGISTRY_NAME,
};
use defra_node::EmbeddedNode;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use super::peer_directory::PeerRecord;
use super::store::{ClientStore, ClientStoreRows};

const REMOTE_SNAPSHOT_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

const AGENT_PRINCIPAL_FIELDS: &str =
    "agent_did display_name default_behavior_id enabled created_at created_by";
const AGENT_BEHAVIOR_FIELDS: &str = "behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled created_at";
const AGENT_RUNTIME_FIELDS: &str = "agent_did process_state reconcile_phase active_generation router_generation default_behavior_id runnable_behavior_count unavailable_behavior_count last_reconcile_result last_reconcile_error last_reconcile_completed_at updated_at";
const AGENT_CONVERSATION_FIELDS: &str = "session_id agent_name agent_did behavior_id title title_source preview_text status created_at updated_at latest_request_id";
const AGENT_REQUEST_FIELDS: &str = "request_id agent_did behavior_id session_id retry_parent_request retry_root_request superseded_by_request content status lifecycle_state backend_id execution_origin caused_by_trigger_id caused_by_trigger_kind caused_by_parent_request_id failure_reason created_at claimed_at deadline retry_count max_retries interrupt_requested_at valid_until";
const AGENT_RESPONSE_FIELDS: &str = "response_key request_id agent_did behavior_id session_id content reasoning status error_message token_count progress_seq materialized_message_sequence materialized_at created_at completed_at interrupted_at";
const AGENT_MESSAGE_FIELDS: &str = "message_key session_id sequence role content timestamp";
const AGENT_SESSION_FIELDS: &str = "session_id agent_name behavior_id started ended status";
const AGENT_TOOL_CALL_FIELDS: &str = "tool_call_key session_id request_id message_sequence tool_name tool_call_id args result status lifecycle_state cancel_policy deadline_at cancel_cause started_at completed_at selected_service_id selected_tool_name tool_failure_class latency_ms";
const AGENT_TOOL_RESULT_FIELDS: &str = "agent_did session_id tool_name tool_input output_text truncated truncation_metadata conversation_doc_id created_at discarded_because_interrupted";
const COMPACTION_ENTRY_FIELDS: &str = "compaction_key session_id sequence summary files_read files_modified messages_compacted original_tokens compacted_tokens created_at";
const TASK_FIELDS: &str = "task_id name description behavior_id prompt_template enabled output_schema_ref created_at updated_at";
const SCHEDULE_FIELDS: &str = "schedule_id task_id interval_secs enabled concurrency next_run_at last_attempt_at last_status last_error fire_count created_at updated_at";
const EVENT_TRIGGER_FIELDS: &str = "trigger_id task_id source_collection event_kind filter enabled concurrency created_at updated_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count";
const TOOL_SELECTION_FIELDS: &str = "selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode command_execution_policy command_allowed_argv_prefixes command_forbidden_argv_prefixes command_network_mode cli_tool_names enable_meta_tools allowed_mcp_service_ids delegate_to backgroundable_tool_names";
const INFERENCE_BACKEND_FIELDS: &str = "backend_id name provider_kind endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status";
const INFERENCE_PROFILE_FIELDS: &str = "profile_id display_name context_window max_output_tokens max_turns temperature stream_batch_ms deadline_duration_secs";
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
        tool_calls: load_agent_tool_calls(node).await?,
        tool_results: load_agent_tool_results(node).await?,
        compaction_entries: load_compaction_entries(node).await?,
        tasks: load_tasks(node).await?,
        schedules: load_schedules(node).await?,
        event_triggers: load_event_triggers(node).await?,
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
        remote_loads.push(tokio::spawn(async move {
            let result = load_full_snapshot_from_graphql(&graphql).await;
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
                    target: "defra_agent_desktop_core::query",
                    peer_id = %peer.peer_id,
                    label = %peer.label,
                    graphql,
                    rows = remote_count,
                    "desktop loaded remote GraphQL snapshot"
                );
            }
            Ok((peer, graphql, Err(error))) => {
                tracing::warn!(
                    target: "defra_agent_desktop_core::query",
                    peer_id = %peer.peer_id,
                    label = %peer.label,
                    graphql,
                    error = %error,
                    "desktop could not load remote GraphQL snapshot"
                );
            }
            Err(error) => {
                tracing::warn!(
                    target: "defra_agent_desktop_core::query",
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
        "query { AgentBehavior { behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled created_at } }",
    )
    .await
}

pub async fn load_agent_runtimes(node: &EmbeddedNode) -> Result<Vec<AgentRuntimeRow>> {
    load_rows(
        node,
        "AgentRuntime",
        "query { AgentRuntime { agent_did process_state reconcile_phase active_generation router_generation default_behavior_id runnable_behavior_count unavailable_behavior_count last_reconcile_result last_reconcile_error last_reconcile_completed_at updated_at } }",
    )
    .await
}

pub async fn load_agent_conversations(node: &EmbeddedNode) -> Result<Vec<AgentConversationRow>> {
    load_rows(
        node,
        "AgentConversation",
        "query { AgentConversation { session_id agent_name agent_did behavior_id title title_source preview_text status created_at updated_at latest_request_id } }",
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
        "AgentMessage",
        "query { AgentMessage { message_key session_id sequence role content timestamp } }",
    )
    .await
}

pub async fn load_agent_sessions(node: &EmbeddedNode) -> Result<Vec<AgentSessionRow>> {
    load_rows(
        node,
        "AgentSession",
        "query { AgentSession { session_id agent_name behavior_id started ended status } }",
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
        "CompactionEntry",
        "query { CompactionEntry { compaction_key session_id sequence summary files_read files_modified messages_compacted original_tokens compacted_tokens created_at } }",
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

pub async fn load_schedules(node: &EmbeddedNode) -> Result<Vec<ScheduleRow>> {
    load_rows(
        node,
        "Schedule",
        "query { Schedule { schedule_id task_id interval_secs enabled concurrency next_run_at last_attempt_at last_status last_error fire_count created_at updated_at } }",
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
        "query { ToolSelection { selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode command_execution_policy command_allowed_argv_prefixes command_forbidden_argv_prefixes command_network_mode cli_tool_names enable_meta_tools allowed_mcp_service_ids delegate_to backgroundable_tool_names } }",
    )
    .await
}

pub async fn load_inference_backends(node: &EmbeddedNode) -> Result<Vec<InferenceBackendRow>> {
    load_rows(
        node,
        "InferenceBackend",
        "query { InferenceBackend { backend_id name provider_kind endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status } }",
    )
    .await
}

pub async fn load_inference_profiles(node: &EmbeddedNode) -> Result<Vec<InferenceProfileRow>> {
    load_rows(
        node,
        "InferenceProfile",
        "query { InferenceProfile { profile_id display_name context_window max_output_tokens max_turns temperature stream_batch_ms deadline_duration_secs } }",
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

pub async fn load_full_snapshot_from_graphql(graphql: &str) -> Result<ClientStore> {
    let client = reqwest::Client::builder()
        .timeout(REMOTE_SNAPSHOT_HTTP_TIMEOUT)
        .build()
        .context("building remote GraphQL snapshot HTTP client")?;
    let data = execute_remote_snapshot_query(&client, graphql).await?;

    Ok(ClientStore::from_rows(ClientStoreRows {
        agent_principals: parse_remote_rows(&data, "AgentPrincipal")?,
        behaviors: parse_remote_rows(&data, "AgentBehavior")?,
        runtimes: parse_remote_rows(&data, "AgentRuntime")?,
        conversations: parse_remote_rows(&data, "AgentConversation")?,
        requests: parse_remote_rows(&data, "AgentRequest")?,
        responses: parse_remote_rows(&data, "AgentResponse")?,
        messages: parse_remote_rows(&data, "AgentMessage")?,
        sessions: parse_remote_rows(&data, "AgentSession")?,
        tool_calls: parse_remote_rows(&data, "AgentToolCall")?,
        tool_results: parse_remote_rows(&data, "AgentToolResult")?,
        compaction_entries: parse_remote_rows(&data, "CompactionEntry")?,
        tasks: parse_remote_rows(&data, "Task")?,
        schedules: parse_remote_rows(&data, "Schedule")?,
        event_triggers: parse_remote_rows(&data, "EventTrigger")?,
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

    Ok(ClientStore::from_rows(ClientStoreRows {
        conversations: parse_remote_rows(&data, "AgentConversation")?,
        requests: parse_remote_rows(&data, "AgentRequest")?,
        responses: parse_remote_rows(&data, "AgentResponse")?,
        messages: parse_remote_rows(&data, "AgentMessage")?,
        sessions: parse_remote_rows(&data, "AgentSession")?,
        tool_calls: parse_remote_rows(&data, "AgentToolCall")?,
        tool_results: parse_remote_rows(&data, "AgentToolResult")?,
        compaction_entries: parse_remote_rows(&data, "CompactionEntry")?,
        ..ClientStoreRows::default()
    }))
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
                        target: "defra_agent_desktop_core::query",
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

async fn execute_remote_snapshot_query(client: &reqwest::Client, graphql: &str) -> Result<Value> {
    execute_remote_graphql_query(client, graphql, REMOTE_SNAPSHOT_QUERY, "snapshot").await
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
                        target: "defra_agent_desktop_core::query",
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
  AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}, limit: 1) {{ request_id agent_did behavior_id session_id retry_parent_request retry_root_request superseded_by_request content status lifecycle_state backend_id execution_origin caused_by_trigger_id caused_by_trigger_kind caused_by_parent_request_id failure_reason created_at claimed_at deadline retry_count max_retries interrupt_requested_at valid_until }}
  AgentResponse(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ response_key request_id agent_did behavior_id session_id content reasoning status error_message token_count progress_seq materialized_message_sequence materialized_at created_at completed_at interrupted_at }}
}}
"#
    )
}

fn remote_chat_patch_query(session_id: &str) -> String {
    let session_id = escape_graphql_string(session_id);
    format!(
        r#"
query DesktopRemoteChatPatch {{
  AgentConversation(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ session_id agent_name agent_did behavior_id title title_source preview_text status created_at updated_at latest_request_id }}
  AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ request_id agent_did behavior_id session_id retry_parent_request retry_root_request superseded_by_request content status lifecycle_state backend_id execution_origin caused_by_trigger_id caused_by_trigger_kind caused_by_parent_request_id failure_reason created_at claimed_at deadline retry_count max_retries interrupt_requested_at valid_until }}
  AgentResponse(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ response_key request_id agent_did behavior_id session_id content reasoning status error_message token_count progress_seq materialized_message_sequence materialized_at created_at completed_at interrupted_at }}
  AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ message_key session_id sequence role content timestamp }}
  AgentSession(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ session_id agent_name behavior_id started ended status }}
  AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ {AGENT_TOOL_CALL_FIELDS} }}
  AgentToolResult(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ agent_did session_id tool_name tool_input output_text truncated truncation_metadata conversation_doc_id created_at discarded_because_interrupted }}
  CompactionEntry(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{ compaction_key session_id sequence summary files_read files_modified messages_compacted original_tokens compacted_tokens created_at }}
}}
"#
    )
}

const REMOTE_SNAPSHOT_QUERY: &str = r#"
query DesktopRemoteSnapshot {
  AgentPrincipal { agent_did display_name default_behavior_id enabled created_at created_by }
  AgentBehavior { behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled created_at }
  AgentRuntime { agent_did process_state reconcile_phase active_generation router_generation default_behavior_id runnable_behavior_count unavailable_behavior_count last_reconcile_result last_reconcile_error last_reconcile_completed_at updated_at }
  AgentConversation { session_id agent_name agent_did behavior_id title title_source preview_text status created_at updated_at latest_request_id }
  AgentRequest { request_id agent_did behavior_id session_id retry_parent_request retry_root_request superseded_by_request content status lifecycle_state backend_id execution_origin caused_by_trigger_id caused_by_trigger_kind caused_by_parent_request_id failure_reason created_at claimed_at deadline retry_count max_retries interrupt_requested_at valid_until }
  AgentResponse { response_key request_id agent_did behavior_id session_id content reasoning status error_message token_count progress_seq materialized_message_sequence materialized_at created_at completed_at interrupted_at }
  AgentMessage { message_key session_id sequence role content timestamp }
  AgentSession { session_id agent_name behavior_id started ended status }
  AgentToolCall { tool_call_key session_id request_id message_sequence tool_name tool_call_id args result status lifecycle_state cancel_policy deadline_at cancel_cause started_at completed_at selected_service_id selected_tool_name tool_failure_class latency_ms }
  AgentToolResult { agent_did session_id tool_name tool_input output_text truncated truncation_metadata conversation_doc_id created_at discarded_because_interrupted }
  CompactionEntry { compaction_key session_id sequence summary files_read files_modified messages_compacted original_tokens compacted_tokens created_at }
  Task { task_id name description behavior_id prompt_template enabled output_schema_ref created_at updated_at }
  Schedule { schedule_id task_id interval_secs enabled concurrency next_run_at last_attempt_at last_status last_error fire_count created_at updated_at }
  EventTrigger { trigger_id task_id source_collection event_kind filter enabled concurrency created_at updated_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count }
  ToolSelection { selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode command_execution_policy command_allowed_argv_prefixes command_forbidden_argv_prefixes command_network_mode cli_tool_names enable_meta_tools allowed_mcp_service_ids delegate_to backgroundable_tool_names }
  InferenceBackend { backend_id name provider_kind endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status }
  InferenceProfile { profile_id display_name context_window max_output_tokens max_turns temperature stream_batch_ms deadline_duration_secs }
  ToolServiceRegistry { service_id display_name description hostname tailscale_ip lan_ip mcp_port mcp_path status version updated_at }
}
"#;

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
/// collections are filtered by `agent_did`; transcript collections
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
        tool_calls,
        tool_results,
        compaction_entries,
        tasks,
        schedules,
        event_triggers,
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
    use defra_agent_protocol::schemas::AGENT_MESSAGE_NAME;
    use defra_node::NodeBuilder;
    use std::sync::Arc;

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
    }

    #[test]
    fn remote_tool_call_queries_include_local_field_set() {
        let chat_patch = remote_chat_patch_query("sess-1");
        for field in AGENT_TOOL_CALL_FIELDS.split_whitespace() {
            assert!(
                chat_patch.contains(field),
                "remote chat patch missing AgentToolCall field {field}"
            );
            assert!(
                REMOTE_SNAPSHOT_QUERY.contains(field),
                "remote snapshot missing AgentToolCall field {field}"
            );
        }
    }
}
