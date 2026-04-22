use anyhow::{anyhow, bail, Context, Result};
use defra_agent_protocol::row::{
    AgentBehaviorRow, AgentConversationRow, AgentMessageRow, AgentPrincipalRow, AgentRequestRow,
    AgentResponseRow, AgentRuntimeRow, AgentSessionRow, AgentToolCallRow, AgentToolResultRow,
    CompactionEntryRow, InferenceBackendRow, InferenceProfileRow, ScheduleRow, TaskRow,
    ToolSelectionRow, ToolServiceRegistryRow,
};
use defra_node::EmbeddedNode;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::store::{ClientStore, ClientStoreRows};

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
        tool_selections: load_tool_selections(node).await?,
        inference_backends: load_inference_backends(node).await?,
        inference_profiles: load_inference_profiles(node).await?,
        tool_service_registries: load_tool_service_registries(node).await?,
    }))
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
        "AgentRequest",
        "query { AgentRequest { request_id agent_did behavior_id session_id retry_parent_request retry_root_request superseded_by_request content status lifecycle_state backend_id execution_origin failure_reason created_at claimed_at deadline retry_count max_retries } }",
    )
    .await
}

pub async fn load_agent_responses(node: &EmbeddedNode) -> Result<Vec<AgentResponseRow>> {
    load_rows(
        node,
        "AgentResponse",
        "query { AgentResponse { response_key request_id agent_did behavior_id session_id content reasoning status error_message token_count progress_seq materialized_message_sequence materialized_at created_at completed_at } }",
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
        "query { AgentToolCall { tool_call_key session_id message_sequence tool_name tool_call_id args result status started_at completed_at } }",
    )
    .await
}

pub async fn load_agent_tool_results(node: &EmbeddedNode) -> Result<Vec<AgentToolResultRow>> {
    load_rows(
        node,
        "AgentToolResult",
        "query { AgentToolResult { agent_did session_id tool_name tool_input output_text truncated truncation_metadata conversation_doc_id created_at } }",
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

pub async fn load_tool_selections(node: &EmbeddedNode) -> Result<Vec<ToolSelectionRow>> {
    load_rows(
        node,
        "ToolSelection",
        "query { ToolSelection { selection_id agent_did display_name enable_file_tools file_tools_mode enable_bash bash_mode cli_tool_names enable_meta_tools delegate_to } }",
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
                        target: "defra_agent_desktop::query",
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
