use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::graphql::escape_graphql_string;
use super::query::load_conversation_document;
use super::retry::execute_mutation_with_retry;

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
    #[error("fork source has a non-terminal AgentRequest and is busy")]
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

async fn verify_source_idle(node: &EmbeddedNode, source_session_id: &str) -> Result<bool> {
    let escaped = escape_graphql_string(source_session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{escaped}" }},
                    lifecycle_state: {{ _in: ["pending", "claimed", "processing", "inputRequired"] }}
                }},
                limit: 1
            ) {{ request_id }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("verify_source_idle query failed: {:?}", resp.errors);
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(rows.is_empty())
}

pub async fn fork(
    node: &EmbeddedNode,
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    // Step 1: load parent conversation (validates existence).
    let parent = load_conversation_document(node, params.source_session_id)
        .await
        .map_err(ForkError::ForkCopyFailed)?
        .ok_or_else(|| ForkError::ForkSourceNotFound(params.source_session_id.to_string()))?;

    // Step 1b: reject callers that are not the same principal as the parent conversation.
    let parent_agent_did = parent.agent_did.as_deref().unwrap_or("");
    if parent_agent_did != params.caller_agent_did {
        return Err(ForkError::ForkNotSameAgent);
    }

    // Step 1c: reject busy sources before doing any copy work.
    if !verify_source_idle(node, params.source_session_id)
        .await
        .map_err(ForkError::ForkCopyFailed)?
    {
        return Err(ForkError::ForkSourceBusy);
    }

    // Step 2: compute cut_seq from the Nth user message.
    let (cut_seq, cut_ts) = compute_cut(node, params.source_session_id, params.fork_at_user_turn)
        .await
        .map_err(ForkError::ForkCopyFailed)?
        .ok_or_else(|| ForkError::ForkAtUserTurnOutOfRange(params.fork_at_user_turn, 0))?;

    // Step 3: resolve child behavior (inherit parent, or swap to validated target).
    let resolved_behavior_id = if let Some(target) = params.target_behavior_id {
        if let Some(err) = resolve_target_behavior(node, target, parent_agent_did)
            .await
            .map_err(ForkError::ForkCopyFailed)?
        {
            return Err(err);
        }
        target.to_string()
    } else {
        parent.behavior_id.clone().unwrap_or_default()
    };

    // Step 4 & 5: copy messages, create child session + conversation.
    let child_session_id = uuid::Uuid::new_v4().to_string();
    let copied_messages = copy_messages(
        node,
        params.source_session_id,
        &child_session_id,
        cut_seq,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    let copied_tool_calls = copy_tool_calls(
        node,
        params.source_session_id,
        &child_session_id,
        cut_seq,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    // Look up child agent_did from parent to pass into copy_tool_results.
    let child_agent_did = parent.agent_did.clone().unwrap_or_default();
    let copied_tool_results = copy_tool_results(
        node,
        params.source_session_id,
        &child_session_id,
        &cut_ts,
        &child_agent_did,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    let copied_compaction_entries = copy_compaction_entries(
        node,
        params.source_session_id,
        &child_session_id,
        &cut_ts,
    )
    .await
    .map_err(ForkError::ForkCopyFailed)?;

    create_child_session_and_conversation(
        node,
        &child_session_id,
        &resolved_behavior_id,
        params.source_session_id,
        params.fork_at_user_turn,
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
    node: &EmbeddedNode,
    target_behavior_id: &str,
    parent_agent_did: &str,
) -> Result<Option<ForkError>> {
    let escaped = escape_graphql_string(target_behavior_id);
    let query = format!(
        r#"{{
            AgentBehavior(filter: {{ behavior_id: {{ _eq: "{escaped}" }} }}, limit: 1) {{ agent_did }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("resolve_target_behavior query failed: {:?}", resp.errors);
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentBehavior"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        return Ok(Some(ForkError::ForkBehaviorNotFound(target_behavior_id.to_string())));
    }
    let behavior_did = rows[0].get("agent_did").and_then(|v| v.as_str()).unwrap_or("");
    if behavior_did != parent_agent_did {
        return Ok(Some(ForkError::ForkBehaviorNotOwnedByPrincipal(
            target_behavior_id.to_string(),
            parent_agent_did.to_string(),
        )));
    }
    Ok(None)
}

async fn compute_cut(
    node: &EmbeddedNode,
    source_session_id: &str,
    fork_at_user_turn: u32,
) -> Result<Option<(u32, String)>> {
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
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("compute_cut query failed: {:?}", resp.errors);
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if (fork_at_user_turn as usize) >= rows.len() {
        return Ok(None);
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
    Ok(Some((seq, ts)))
}

async fn copy_messages(
    node: &EmbeddedNode,
    source_session_id: &str,
    child_session_id: &str,
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
            ) {{ sequence role content timestamp }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("copy_messages query failed: {:?}", resp.errors);
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut count = 0u32;
    let child_session_escaped = escape_graphql_string(child_session_id);
    for row in &rows {
        let sequence = row
            .get("sequence")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("sequence missing"))?;
        let role = row.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content = row.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let timestamp = row.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let message_key = format!("{child_session_escaped}:{sequence}");
        let mutation = format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "{message_key}",
                    session_id: "{child_session_escaped}",
                    sequence: {sequence},
                    role: "{role_escaped}",
                    content: "{content_escaped}",
                    timestamp: "{timestamp_escaped}"
                }}) {{ _docID }}
            }}"#,
            role_escaped = escape_graphql_string(role),
            content_escaped = escape_graphql_string(content),
            timestamp_escaped = escape_graphql_string(timestamp),
        );
        execute_mutation_with_retry(node, &mutation, "fork::copy_message").await?;
        count += 1;
    }
    Ok(count)
}

async fn copy_tool_calls(
    node: &EmbeddedNode,
    source_session_id: &str,
    child_session_id: &str,
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
                message_sequence tool_name tool_call_id args result status started_at completed_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("copy_tool_calls query failed: {:?}", resp.errors);
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut count = 0u32;
    let child_session_escaped = escape_graphql_string(child_session_id);
    for row in &rows {
        let message_sequence = row.get("message_sequence").and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("message_sequence missing"))?;
        let tool_name = row.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        let tool_call_id = row.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("");
        let args = row.get("args").and_then(|v| v.as_str()).unwrap_or("");
        let result = row.get("result").and_then(|v| v.as_str()).unwrap_or("");
        let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("");
        let started_at = row.get("started_at").and_then(|v| v.as_str()).unwrap_or("");
        let completed_at = row.get("completed_at").and_then(|v| v.as_str()).unwrap_or("");
        let tool_call_id_escaped = escape_graphql_string(tool_call_id);
        let tool_call_key = format!("{child_session_escaped}:{tool_call_id_escaped}");
        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    session_id: "{child_session_escaped}",
                    message_sequence: {message_sequence},
                    tool_name: "{tool_name_escaped}",
                    tool_call_id: "{tool_call_id_escaped}",
                    args: "{args_escaped}",
                    result: "{result_escaped}",
                    status: "{status_escaped}",
                    started_at: "{started_at_escaped}",
                    completed_at: "{completed_at_escaped}"
                }}) {{ _docID }}
            }}"#,
            tool_name_escaped = escape_graphql_string(tool_name),
            args_escaped = escape_graphql_string(args),
            result_escaped = escape_graphql_string(result),
            status_escaped = escape_graphql_string(status),
            started_at_escaped = escape_graphql_string(started_at),
            completed_at_escaped = escape_graphql_string(completed_at),
        );
        execute_mutation_with_retry(node, &mutation, "fork::copy_tool_call").await?;
        count += 1;
    }
    Ok(count)
}

async fn copy_tool_results(
    node: &EmbeddedNode,
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
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("copy_tool_results query failed: {:?}", resp.errors);
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolResult"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut count = 0u32;
    let child_session_escaped = escape_graphql_string(child_session_id);
    let child_agent_did_escaped = escape_graphql_string(child_agent_did);
    for row in &rows {
        let tool_name = row.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        let tool_input = row.get("tool_input").and_then(|v| v.as_str()).unwrap_or("");
        let output_text = row.get("output_text").and_then(|v| v.as_str()).unwrap_or("");
        let truncated = row.get("truncated").and_then(|v| v.as_bool()).unwrap_or(false);
        let truncation_metadata = row.get("truncation_metadata").and_then(|v| v.as_str()).unwrap_or("");
        let conversation_doc_id = row.get("conversation_doc_id").and_then(|v| v.as_str()).unwrap_or("");
        let created_at = row.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let mutation = format!(
            r#"mutation {{
                create_AgentToolResult(input: {{
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
            }}"#,
            tool_name_escaped = escape_graphql_string(tool_name),
            tool_input_escaped = escape_graphql_string(tool_input),
            output_text_escaped = escape_graphql_string(output_text),
            truncation_metadata_escaped = escape_graphql_string(truncation_metadata),
            conversation_doc_id_escaped = escape_graphql_string(conversation_doc_id),
            created_at_escaped = escape_graphql_string(created_at),
        );
        execute_mutation_with_retry(node, &mutation, "fork::copy_tool_result").await?;
        count += 1;
    }
    Ok(count)
}

async fn copy_compaction_entries(
    node: &EmbeddedNode,
    source_session_id: &str,
    child_session_id: &str,
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
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("copy_compaction_entries query failed: {:?}", resp.errors);
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("CompactionEntry"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut count = 0u32;
    let child_session_escaped = escape_graphql_string(child_session_id);
    for row in &rows {
        let sequence = row.get("sequence").and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("compaction sequence missing"))?;
        let summary = row.get("summary").and_then(|v| v.as_str()).unwrap_or("");
        let files_read = row.get("files_read").and_then(|v| v.as_str()).unwrap_or("[]");
        let files_modified = row.get("files_modified").and_then(|v| v.as_str()).unwrap_or("[]");
        let messages_compacted = row.get("messages_compacted").and_then(|v| v.as_u64()).unwrap_or(0);
        let original_tokens = row.get("original_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let compacted_tokens = row.get("compacted_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let created_at = row.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let compaction_key = format!("{child_session_escaped}:{sequence}");
        let mutation = format!(
            r#"mutation {{
                create_CompactionEntry(input: {{
                    compaction_key: "{compaction_key}",
                    session_id: "{child_session_escaped}",
                    sequence: {sequence},
                    summary: "{summary_escaped}",
                    files_read: "{files_read_escaped}",
                    files_modified: "{files_modified_escaped}",
                    messages_compacted: {messages_compacted},
                    original_tokens: {original_tokens},
                    compacted_tokens: {compacted_tokens},
                    created_at: "{created_at_escaped}"
                }}) {{ _docID }}
            }}"#,
            summary_escaped = escape_graphql_string(summary),
            files_read_escaped = escape_graphql_string(files_read),
            files_modified_escaped = escape_graphql_string(files_modified),
            created_at_escaped = escape_graphql_string(created_at),
        );
        execute_mutation_with_retry(node, &mutation, "fork::copy_compaction_entry").await?;
        count += 1;
    }
    Ok(count)
}

async fn create_child_session_and_conversation(
    node: &EmbeddedNode,
    child_session_id: &str,
    behavior_id: &str,
    source_session_id: &str,
    fork_at_user_turn: u32,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let child_session_escaped = escape_graphql_string(child_session_id);
    let behavior_id_escaped = escape_graphql_string(behavior_id);
    let forked_from_escaped = escape_graphql_string(source_session_id);
    let now_escaped = escape_graphql_string(&now);

    let session_mutation = format!(
        r#"mutation {{
            create_AgentSession(input: {{
                session_id: "{child_session_escaped}",
                agent_name: "",
                behavior_id: "{behavior_id_escaped}",
                started: "{now_escaped}",
                status: "active"
            }}) {{ _docID }}
        }}"#
    );
    execute_mutation_with_retry(node, &session_mutation, "fork::create_session").await?;

    // We need agent_did on the child conversation. Borrow from the parent for now
    // (future patch: carry in ForkParams or resolve via principal).
    let parent_conv_query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{forked_from_escaped}" }} }},
                limit: 1
            ) {{ agent_did agent_name }}
        }}"#
    );
    let parent_resp = node.execute(&parent_conv_query).await;
    if parent_resp.has_errors() {
        anyhow::bail!("fork::create_conversation query failed: {:?}", parent_resp.errors);
    }
    let parent_row = parent_resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentConversation"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("parent AgentConversation missing in child-create path"))?;
    let agent_did_escaped = escape_graphql_string(
        parent_row.get("agent_did").and_then(|v| v.as_str()).unwrap_or(""),
    );
    let agent_name_escaped = escape_graphql_string(
        parent_row.get("agent_name").and_then(|v| v.as_str()).unwrap_or(""),
    );

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
    execute_mutation_with_retry(node, &conv_mutation, "fork::create_conversation").await?;
    Ok(())
}
