use anyhow::{Context, Result};
use async_trait::async_trait;
use defra_node::EmbeddedNode;
use gents_protocol::graphql::{execute_graphql_async, GraphqlRequestOptions};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::Value;

use super::retry::log_mutation_timing;
use crate::graphql::escape_graphql_string;

const DEFAULT_BATCH_MUTATION_SIZE: usize = 50;

#[derive(Debug, Clone)]
pub struct GraphqlExecuteResponse {
    pub data: Option<Value>,
    pub errors: Vec<Value>,
}

impl GraphqlExecuteResponse {
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    fn from_http_value(value: Value) -> Self {
        let data = value.get("data").cloned();
        let errors = value
            .get("errors")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Self { data, errors }
    }

    fn from_embedded(response: defra_node::QueryResponse) -> Self {
        let errors = response
            .errors
            .into_iter()
            .map(|error| {
                serde_json::to_value(error)
                    .unwrap_or_else(|_| Value::String("GraphQL error".to_string()))
            })
            .collect();
        Self {
            data: response.data,
            errors,
        }
    }
}

#[async_trait]
pub trait GraphqlExecutor: Send + Sync {
    async fn execute_graphql(&self, query: &str) -> Result<GraphqlExecuteResponse>;
}

#[async_trait]
impl GraphqlExecutor for EmbeddedNode {
    async fn execute_graphql(&self, query: &str) -> Result<GraphqlExecuteResponse> {
        Ok(GraphqlExecuteResponse::from_embedded(
            crate::graphql::graphql_response_with_transaction_retry(
                self,
                query,
                "session fork GraphQL",
            )
            .await,
        ))
    }
}

#[derive(Debug, Clone)]
pub struct HttpGraphqlExecutor {
    endpoint: String,
    options: GraphqlRequestOptions,
}

impl HttpGraphqlExecutor {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            options: GraphqlRequestOptions::default(),
        }
    }

    pub fn with_options(endpoint: impl Into<String>, options: GraphqlRequestOptions) -> Self {
        Self {
            endpoint: endpoint.into(),
            options,
        }
    }
}

#[async_trait]
impl GraphqlExecutor for HttpGraphqlExecutor {
    async fn execute_graphql(&self, query: &str) -> Result<GraphqlExecuteResponse> {
        let value = execute_graphql_async(&self.endpoint, query, self.options).await?;
        Ok(GraphqlExecuteResponse::from_http_value(value))
    }
}

#[derive(Debug, Clone)]
struct ForkParentConversation {
    behavior_id: Option<String>,
    agent_did: Option<String>,
    agent_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ForkParams<'a> {
    pub source_session_id: &'a str,
    pub fork_at_user_turn: u32,
    pub caller_agent_did: &'a str,
    pub target_behavior_id: Option<&'a str>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ForkOutcome {
    pub session_id: String,
    pub copied_messages: u32,
    pub copied_tool_calls: u32,
    pub copied_tool_results: u32,
    pub copied_compaction_entries: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ForkError {
    #[error("fork source not found: session_id={0}")]
    ForkSourceNotFound(String),
    #[error("fork source's agent_did does not match caller")]
    ForkNotSameAgent,
    #[error("fork source has an active runtime AgentRequest and is busy")]
    ForkSourceBusy,
    #[error("fork_at_user_turn={0} is out of range (parent has only {1} user messages)")]
    ForkAtUserTurnOutOfRange(u32, u32),
    #[error("target behavior not found: {0}")]
    ForkBehaviorNotFound(String),
    #[error("target behavior {0} is not owned by principal {1}")]
    ForkBehaviorNotOwnedByPrincipal(String, String),
    #[error("fork copy step failed: {0}")]
    ForkCopyFailed(#[from] anyhow::Error),
}

async fn load_parent_conversation(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
) -> Result<Option<ForkParentConversation>> {
    let escaped = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{
                    session_id: {{ _eq: "{escaped}" }}
                }},
                limit: 1
            ) {{
                behavior_id
                agent_did
                agent_name
            }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "loading conversation document for session_id={}: {}",
            source_session_id,
            render_graphql_errors(&resp)
        );
    }

    let mut rows = graphql_rows(&resp, "AgentConversation");
    Ok(rows.pop().map(|row| ForkParentConversation {
        behavior_id: row
            .get("behavior_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        agent_did: row
            .get("agent_did")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        agent_name: row
            .get("agent_name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }))
}

async fn verify_source_idle(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
) -> Result<bool> {
    let escaped = escape_graphql_string(source_session_id);
    let active_runtime_states = RequestLifecycleState::active_runtime_graphql_list();
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{escaped}" }},
                    lifecycle_state: {{ _in: {active_runtime_states} }}
                }},
                limit: 1
            ) {{ request_id }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "verify_source_idle query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "AgentRequest");
    Ok(rows.is_empty())
}

pub async fn fork(node: &EmbeddedNode, params: ForkParams<'_>) -> Result<ForkOutcome, ForkError> {
    fork_with_executor(node, params).await
}

pub async fn fork_via_http(
    graphql_endpoint: &str,
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    let executor = HttpGraphqlExecutor::new(graphql_endpoint);
    fork_with_executor(&executor, params).await
}

async fn fork_with_executor(
    executor: &(impl GraphqlExecutor + ?Sized),
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    let parent = load_parent_conversation(executor, params.source_session_id)
        .await
        .map_err(ForkError::ForkCopyFailed)?
        .ok_or_else(|| ForkError::ForkSourceNotFound(params.source_session_id.to_string()))?;

    let parent_agent_did = parent.agent_did.as_deref().unwrap_or("");
    if parent_agent_did.is_empty() {
        return Err(ForkError::ForkSourceNotFound(
            params.source_session_id.to_string(),
        ));
    }
    if parent_agent_did != params.caller_agent_did {
        return Err(ForkError::ForkNotSameAgent);
    }

    if !verify_source_idle(executor, params.source_session_id)
        .await
        .map_err(ForkError::ForkCopyFailed)?
    {
        return Err(ForkError::ForkSourceBusy);
    }

    let (cut_seq, cut_ts) =
        match compute_cut(executor, params.source_session_id, params.fork_at_user_turn)
            .await
            .map_err(ForkError::ForkCopyFailed)?
        {
            Ok((seq, ts)) => (seq, ts),
            Err(total_user_msgs) => {
                return Err(ForkError::ForkAtUserTurnOutOfRange(
                    params.fork_at_user_turn,
                    total_user_msgs,
                ));
            }
        };

    let resolved_behavior_id = if let Some(target) = params.target_behavior_id {
        if let Some(err) = resolve_target_behavior(executor, target, parent_agent_did)
            .await
            .map_err(ForkError::ForkCopyFailed)?
        {
            return Err(err);
        }
        target.to_string()
    } else {
        parent.behavior_id.clone().unwrap_or_default()
    };

    let child_session_id = uuid::Uuid::new_v4().to_string();
    let copied_messages = copy_messages(
        executor,
        params.source_session_id,
        &child_session_id,
        parent_agent_did,
        cut_seq,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    let copied_tool_calls = copy_tool_calls(
        executor,
        params.source_session_id,
        &child_session_id,
        parent_agent_did,
        cut_seq,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    let parent_agent_name = parent.agent_name.as_deref().unwrap_or("");
    let copied_tool_results = copy_tool_results(
        executor,
        params.source_session_id,
        &child_session_id,
        &cut_ts,
        parent_agent_did,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    let copied_compaction_entries = copy_compaction_entries(
        executor,
        params.source_session_id,
        &child_session_id,
        parent_agent_did,
        &cut_ts,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    create_child_session_and_conversation(
        executor,
        &child_session_id,
        &resolved_behavior_id,
        params.source_session_id,
        params.fork_at_user_turn,
        parent_agent_did,
        parent_agent_name,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    Ok(ForkOutcome {
        session_id: child_session_id,
        copied_messages,
        copied_tool_calls,
        copied_tool_results,
        copied_compaction_entries,
    })
}

async fn resolve_target_behavior(
    executor: &(impl GraphqlExecutor + ?Sized),
    target_behavior_id: &str,
    parent_agent_did: &str,
) -> Result<Option<ForkError>> {
    let escaped = escape_graphql_string(target_behavior_id);
    let query = format!(
        r#"{{
            AgentBehavior(filter: {{ behavior_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ agent_did }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "resolve_target_behavior query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "AgentBehavior");
    if rows.is_empty() {
        return Ok(Some(ForkError::ForkBehaviorNotFound(
            target_behavior_id.to_string(),
        )));
    }
    let behavior_did = rows[0]
        .get("agent_did")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if behavior_did != parent_agent_did {
        return Ok(Some(ForkError::ForkBehaviorNotOwnedByPrincipal(
            target_behavior_id.to_string(),
            parent_agent_did.to_string(),
        )));
    }
    Ok(None)
}

async fn compute_cut(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
    fork_at_user_turn: u32,
) -> Result<std::result::Result<(u32, String), u32>> {
    let escaped = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{escaped}" }},
                    role: {{ _eq: "user" }}
                }},
                order: {{ sequence: ASC }}
            ) {{ sequence timestamp }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!("compute_cut query failed: {}", render_graphql_errors(&resp));
    }
    let rows = graphql_rows(&resp, "AgentMessage");
    let total_user_msgs = rows.len() as u32;
    if fork_at_user_turn > total_user_msgs {
        return Ok(Err(total_user_msgs));
    }
    if fork_at_user_turn == total_user_msgs {
        return compute_end_cut(executor, source_session_id).await.map(Ok);
    }
    let row = &rows[fork_at_user_turn as usize];
    let seq = row
        .get("sequence")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("sequence missing"))? as u32;
    let ts = row
        .get("timestamp")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("timestamp missing"))?
        .to_string();
    Ok(Ok((seq, ts)))
}

async fn compute_end_cut(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
) -> Result<(u32, String)> {
    let escaped = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped}" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "compute_end_cut query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let max_sequence = graphql_rows(&resp, "AgentMessage")
        .first()
        .and_then(|row| row.get("sequence"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cut_seq = u32::try_from(max_sequence.saturating_add(1))
        .context("message sequence exceeds u32 during fork end cut")?;
    Ok((cut_seq, "9999-12-31T23:59:59Z".to_string()))
}

async fn copy_messages(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
    child_session_id: &str,
    agent_did: &str,
    cut_seq: u32,
) -> Result<u32> {
    let escaped_source = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{escaped_source}" }},
                    sequence: {{ _lt: {cut_seq} }}
                }},
                order: {{ sequence: ASC }}
            ) {{ requester_did sequence role content reasoning timestamp }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "copy_messages query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "AgentMessage");
    let child_session_escaped = escape_graphql_string(child_session_id);
    let agent_did_escaped = escape_graphql_string(agent_did);
    let mut mutation_fields = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let sequence = row
            .get("sequence")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("sequence missing"))?;
        let role = row.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content = row.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let reasoning = row.get("reasoning").and_then(|v| v.as_str()).unwrap_or("");
        let timestamp = row.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let requester_did = row.get("requester_did").and_then(|v| v.as_str());
        let message_key = format!("{child_session_escaped}:{sequence}");
        mutation_fields.push(format!(
            r#"message_{index}: create_AgentMessage(input: {{
                    message_key: "{message_key}",
                    session_id: "{child_session_escaped}",
                    agent_did: "{agent_did_escaped}",
                    request_id: "",
                    request_doc_id: "",
                    requester_did: {requester_did},
                    sequence: {sequence},
                    role: "{role_escaped}",
                    content: "{content_escaped}",
                    reasoning: "{reasoning_escaped}",
                    timestamp: "{timestamp_escaped}"
                }}) {{ _docID }}
            "#,
            role_escaped = escape_graphql_string(role),
            content_escaped = escape_graphql_string(content),
            reasoning_escaped = escape_graphql_string(reasoning),
            timestamp_escaped = escape_graphql_string(timestamp),
            requester_did = nullable_string_literal(requester_did),
        ));
    }
    execute_batch_mutation_with_retry(executor, &mutation_fields, "fork::copy_messages").await?;
    Ok(mutation_fields.len() as u32)
}

async fn copy_tool_calls(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
    child_session_id: &str,
    agent_did: &str,
    cut_seq: u32,
) -> Result<u32> {
    let escaped_source = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_source}" }},
                    message_sequence: {{ _lt: {cut_seq} }}
                }},
                order: {{ message_sequence: ASC }}
            ) {{
                requester_did message_sequence tool_name tool_call_id args result status lifecycle_state
                started_at completed_at selected_service_id selected_tool_name tool_failure_class
                denial_reason denied_argv denied_command denied_argument denied_subcommand
                denied_prefix policy_mode policy_network
                cancel_cause latency_ms
            }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "copy_tool_calls query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "AgentToolCall");
    let child_session_escaped = escape_graphql_string(child_session_id);
    let agent_did_escaped = escape_graphql_string(agent_did);
    let mut mutation_fields = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let message_sequence = row
            .get("message_sequence")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("message_sequence missing"))?;
        let tool_name = row.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        let tool_call_id = row
            .get("tool_call_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args = row.get("args").and_then(|v| v.as_str()).unwrap_or("");
        let result = row.get("result").and_then(|v| v.as_str()).unwrap_or("");
        let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let lifecycle_state = row.get("lifecycle_state").and_then(|v| v.as_str());
        let started_at = row.get("started_at").and_then(|v| v.as_str()).unwrap_or("");
        let completed_at = row
            .get("completed_at")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let selected_service_id = row.get("selected_service_id").and_then(|v| v.as_str());
        let selected_tool_name = row.get("selected_tool_name").and_then(|v| v.as_str());
        let tool_failure_class = row.get("tool_failure_class").and_then(|v| v.as_str());
        let denial_reason = row.get("denial_reason").and_then(|v| v.as_str());
        let denied_argv = row.get("denied_argv").and_then(json_string_array);
        let denied_command = row.get("denied_command").and_then(|v| v.as_str());
        let denied_argument = row.get("denied_argument").and_then(|v| v.as_str());
        let denied_subcommand = row.get("denied_subcommand").and_then(|v| v.as_str());
        let denied_prefix = row.get("denied_prefix").and_then(json_string_array);
        let policy_mode = row.get("policy_mode").and_then(|v| v.as_str());
        let policy_network = row.get("policy_network").and_then(|v| v.as_str());
        let cancel_cause = row.get("cancel_cause").and_then(|v| v.as_str());
        let latency_ms = row.get("latency_ms").and_then(json_i64);
        let requester_did = row.get("requester_did").and_then(|v| v.as_str());
        let tool_call_id_escaped = escape_graphql_string(tool_call_id);
        let tool_call_key = format!("{child_session_escaped}:{tool_call_id_escaped}");
        mutation_fields.push(format!(
            r#"tool_call_{index}: create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    session_id: "{child_session_escaped}",
                    agent_did: "{agent_did_escaped}",
                    request_id: "",
                    request_doc_id: "",
                    requester_did: {requester_did},
                    message_sequence: {message_sequence},
                    tool_name: "{tool_name_escaped}",
                    tool_call_id: "{tool_call_id_escaped}",
                    args: "{args_escaped}",
                    result: "{result_escaped}",
                    status: "{status_escaped}",
                    lifecycle_state: {lifecycle_state},
                    started_at: "{started_at_escaped}",
                    completed_at: "{completed_at_escaped}",
                    selected_service_id: {selected_service_id},
                    selected_tool_name: {selected_tool_name},
                    tool_failure_class: {tool_failure_class},
                    denial_reason: {denial_reason},
                    denied_argv: {denied_argv},
                    denied_command: {denied_command},
                    denied_argument: {denied_argument},
                    denied_subcommand: {denied_subcommand},
                    denied_prefix: {denied_prefix},
                    policy_mode: {policy_mode},
                    policy_network: {policy_network},
                    cancel_cause: {cancel_cause},
                    latency_ms: {latency_ms}
                }}) {{ _docID }}
            "#,
            tool_name_escaped = escape_graphql_string(tool_name),
            args_escaped = escape_graphql_string(args),
            result_escaped = escape_graphql_string(result),
            status_escaped = escape_graphql_string(status),
            lifecycle_state = nullable_string_literal(lifecycle_state),
            started_at_escaped = escape_graphql_string(started_at),
            completed_at_escaped = escape_graphql_string(completed_at),
            selected_service_id = nullable_string_literal(selected_service_id),
            selected_tool_name = nullable_string_literal(selected_tool_name),
            tool_failure_class = nullable_string_literal(tool_failure_class),
            denial_reason = nullable_string_literal(denial_reason),
            denied_argv = nullable_string_array_literal(denied_argv.as_deref()),
            denied_command = nullable_string_literal(denied_command),
            denied_argument = nullable_string_literal(denied_argument),
            denied_subcommand = nullable_string_literal(denied_subcommand),
            denied_prefix = nullable_string_array_literal(denied_prefix.as_deref()),
            policy_mode = nullable_string_literal(policy_mode),
            policy_network = nullable_string_literal(policy_network),
            cancel_cause = nullable_string_literal(cancel_cause),
            latency_ms = nullable_i64_literal(latency_ms),
            requester_did = nullable_string_literal(requester_did),
        ));
    }
    execute_batch_mutation_with_retry(executor, &mutation_fields, "fork::copy_tool_calls").await?;
    Ok(mutation_fields.len() as u32)
}

async fn copy_tool_results(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
    child_session_id: &str,
    cut_ts: &str,
    child_agent_did: &str,
) -> Result<u32> {
    let escaped_source = escape_graphql_string(source_session_id);
    let escaped_cut_ts = escape_graphql_string(cut_ts);
    let query = format!(
        r#"{{
            AgentToolResult(
                filter: {{
                    session_id: {{ _eq: "{escaped_source}" }},
                    created_at: {{ _lt: "{escaped_cut_ts}" }}
                }},
                order: {{ created_at: ASC }}
            ) {{ tool_name tool_input output_text truncated truncation_metadata conversation_doc_id created_at }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "copy_tool_results query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "AgentToolResult");
    let child_session_escaped = escape_graphql_string(child_session_id);
    let child_agent_did_escaped = escape_graphql_string(child_agent_did);
    let mut mutation_fields = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let tool_name = row.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        let tool_input = row.get("tool_input").and_then(|v| v.as_str()).unwrap_or("");
        let output_text = row
            .get("output_text")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let truncated = row
            .get("truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let truncation_metadata = row
            .get("truncation_metadata")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let conversation_doc_id = row
            .get("conversation_doc_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let created_at = row.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        mutation_fields.push(format!(
            r#"tool_result_{index}: create_AgentToolResult(input: {{
                    tool_call_doc_id: "",
                    agent_did: "{child_agent_did_escaped}",
                    session_id: "{child_session_escaped}",
                    tool_name: "{tool_name_escaped}",
                    tool_input: "{tool_input_escaped}",
                    output_text: "{output_text_escaped}",
                    truncated: {truncated},
                    truncation_metadata: "{truncation_metadata_escaped}",
                    conversation_doc_id: "{conversation_doc_id_escaped}",
                    created_at: "{created_at_escaped}"
                }}) {{ _docID }}
            "#,
            tool_name_escaped = escape_graphql_string(tool_name),
            tool_input_escaped = escape_graphql_string(tool_input),
            output_text_escaped = escape_graphql_string(output_text),
            truncation_metadata_escaped = escape_graphql_string(truncation_metadata),
            conversation_doc_id_escaped = escape_graphql_string(conversation_doc_id),
            created_at_escaped = escape_graphql_string(created_at),
        ));
    }
    execute_batch_mutation_with_retry(executor, &mutation_fields, "fork::copy_tool_results")
        .await?;
    Ok(mutation_fields.len() as u32)
}

async fn copy_compaction_entries(
    executor: &(impl GraphqlExecutor + ?Sized),
    source_session_id: &str,
    child_session_id: &str,
    agent_did: &str,
    cut_ts: &str,
) -> Result<u32> {
    let escaped_source = escape_graphql_string(source_session_id);
    let escaped_cut_ts = escape_graphql_string(cut_ts);
    let query = format!(
        r#"{{
            CompactionEntry(
                filter: {{
                    session_id: {{ _eq: "{escaped_source}" }},
                    created_at: {{ _lt: "{escaped_cut_ts}" }}
                }},
                order: {{ sequence: ASC }}
            ) {{
                sequence summary files_read files_modified messages_compacted original_tokens compacted_tokens created_at
            }}
        }}"#
    );
    let resp = executor.execute_graphql(&query).await?;
    if resp.has_errors() {
        anyhow::bail!(
            "copy_compaction_entries query failed: {}",
            render_graphql_errors(&resp)
        );
    }
    let rows = graphql_rows(&resp, "CompactionEntry");
    let child_session_escaped = escape_graphql_string(child_session_id);
    let agent_did_escaped = escape_graphql_string(agent_did);
    let mut mutation_fields = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let sequence = row
            .get("sequence")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("compaction sequence missing"))?;
        let summary = row.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        let files_read = row
            .get("files_read")
            .and_then(|v| v.as_str())
            .unwrap_or("[]");
        let files_modified = row
            .get("files_modified")
            .and_then(|v| v.as_str())
            .unwrap_or("[]");
        let messages_compacted = row
            .get("messages_compacted")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let original_tokens = row
            .get("original_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let compacted_tokens = row
            .get("compacted_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let created_at = row.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let compaction_key = format!("{child_session_escaped}:{sequence}");
        mutation_fields.push(format!(
            r#"compaction_{index}: create_CompactionEntry(input: {{
                    compaction_key: "{compaction_key}",
                    session_id: "{child_session_escaped}",
                    agent_did: "{agent_did_escaped}",
                    request_id: "",
                    request_doc_id: "",
                    sequence: {sequence},
                    summary: "{summary_escaped}",
                    files_read: "{files_read_escaped}",
                    files_modified: "{files_modified_escaped}",
                    messages_compacted: {messages_compacted},
                    original_tokens: {original_tokens},
                    compacted_tokens: {compacted_tokens},
                    created_at: "{created_at_escaped}"
                }}) {{ _docID }}
            "#,
            summary_escaped = escape_graphql_string(summary),
            files_read_escaped = escape_graphql_string(files_read),
            files_modified_escaped = escape_graphql_string(files_modified),
            created_at_escaped = escape_graphql_string(created_at),
        ));
    }
    execute_batch_mutation_with_retry(executor, &mutation_fields, "fork::copy_compaction_entries")
        .await?;
    Ok(mutation_fields.len() as u32)
}

async fn create_child_session_and_conversation(
    executor: &(impl GraphqlExecutor + ?Sized),
    child_session_id: &str,
    behavior_id: &str,
    source_session_id: &str,
    fork_at_user_turn: u32,
    parent_agent_did: &str,
    parent_agent_name: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let child_session_escaped = escape_graphql_string(child_session_id);
    let behavior_id_escaped = escape_graphql_string(behavior_id);
    let forked_from_escaped = escape_graphql_string(source_session_id);
    let now_escaped = escape_graphql_string(&now);
    let agent_did_escaped = escape_graphql_string(parent_agent_did);
    let agent_name_escaped = escape_graphql_string(parent_agent_name);

    let session_mutation = format!(
        r#"mutation {{
            create_AgentSession(input: {{
                session_id: "{child_session_escaped}",
                agent_name: "{agent_name_escaped}",
                agent_did: "{agent_did_escaped}",
                behavior_id: "{behavior_id_escaped}",
                started: "{now_escaped}",
                status: "active"
            }}) {{ _docID }}
        }}"#
    );
    execute_mutation_with_retry(executor, &session_mutation, "fork::create_session").await?;

    let conv_mutation = format!(
        r#"mutation {{
            create_AgentConversation(input: {{
                session_id: "{child_session_escaped}",
                agent_name: "{agent_name_escaped}",
                agent_did: "{agent_did_escaped}",
                behavior_id: "{behavior_id_escaped}",
                title: "Forked conversation",
                preview_text: "",
                status: "active",
                created_at: "{now_escaped}",
                updated_at: "{now_escaped}",
                latest_request_id: "",
                forked_from_session_id: "{forked_from_escaped}",
                fork_at_user_turn: {fork_at_user_turn},
                forked_at: "{now_escaped}"
            }}) {{ _docID }}
        }}"#
    );
    execute_mutation_with_retry(executor, &conv_mutation, "fork::create_conversation").await?;
    Ok(())
}

async fn execute_mutation_with_retry(
    executor: &(impl GraphqlExecutor + ?Sized),
    mutation: &str,
    operation: &str,
) -> Result<GraphqlExecuteResponse> {
    let started = std::time::Instant::now();
    let response = executor.execute_graphql(mutation).await?;
    log_mutation_timing(operation, started.elapsed());
    if response.has_errors() {
        anyhow::bail!("{operation} failed: {}", render_graphql_errors(&response));
    }
    Ok(response)
}

async fn execute_batch_mutation_with_retry(
    executor: &(impl GraphqlExecutor + ?Sized),
    mutation_fields: &[String],
    operation: &str,
) -> Result<()> {
    if mutation_fields.is_empty() {
        return Ok(());
    }

    for fields in mutation_fields.chunks(DEFAULT_BATCH_MUTATION_SIZE) {
        let mutation = build_batch_mutation(fields);
        execute_mutation_with_retry(executor, &mutation, operation).await?;
    }

    Ok(())
}

fn build_batch_mutation(fields: &[String]) -> String {
    format!("mutation {{\n{}\n}}", fields.join("\n"))
}

fn graphql_rows(response: &GraphqlExecuteResponse, collection_name: &str) -> Vec<Value> {
    response
        .data
        .as_ref()
        .and_then(|data| data.get(collection_name))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn render_graphql_errors(response: &GraphqlExecuteResponse) -> String {
    Value::Array(response.errors.clone()).to_string()
}

fn nullable_string_literal(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .unwrap_or_else(|| "null".to_string())
}

fn nullable_string_array_literal(value: Option<&[String]>) -> String {
    value
        .map(|values| {
            let values = values
                .iter()
                .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{values}]")
        })
        .unwrap_or_else(|| "null".to_string())
}

fn nullable_i64_literal(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn json_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

fn json_string_array(value: &serde_json::Value) -> Option<Vec<String>> {
    Some(
        value
            .as_array()?
            .iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect(),
    )
}
