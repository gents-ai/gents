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

pub async fn fork(
    node: &EmbeddedNode,
    params: ForkParams<'_>,
) -> Result<ForkOutcome, ForkError> {
    // Step 1: load parent conversation (validates existence).
    let parent = load_conversation_document(node, params.source_session_id)
        .await
        .map_err(ForkError::ForkCopyFailed)?
        .ok_or_else(|| ForkError::ForkSourceNotFound(params.source_session_id.to_string()))?;

    // Step 2: compute cut_seq from the Nth user message.
    let (cut_seq, _cut_ts) = compute_cut(node, params.source_session_id, params.fork_at_user_turn)
        .await
        .map_err(ForkError::ForkCopyFailed)?
        .ok_or_else(|| ForkError::ForkAtUserTurnOutOfRange(params.fork_at_user_turn, 0))?;

    // Step 3: resolve child behavior (inherit parent for this task).
    let resolved_behavior_id = parent
        .behavior_id
        .clone()
        .unwrap_or_else(|| String::new());

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
        ..ForkOutcome::default()
    })
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
