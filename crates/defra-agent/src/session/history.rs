use super::retry::execute_query_timed;
use super::rows::AgentMessageRow;
use super::*;
use defra_agent_protocol::transcript::decode_persisted_message;

pub async fn load_history(node: &EmbeddedNode, session_id: &str) -> Result<Vec<Message>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                sequence
                role
                content
                timestamp
            }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "load_history").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading history for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    let messages: Vec<AgentMessageRow> =
        match resp.data.as_ref().and_then(|data| data.get("AgentMessage")) {
            Some(value) => serde_json::from_value(value.clone())?,
            None => Vec::new(),
        };

    let mut history = Vec::with_capacity(messages.len());
    for msg in messages {
        history.push(decode_persisted_message(
            msg.role.as_str(),
            msg.content.as_str(),
        ));
    }

    tracing::Span::current().record("history_message_count", history.len() as i64);
    tracing::debug!(session_id = %session_id, count = history.len(), "loaded history");
    Ok(history)
}

pub(crate) async fn save_message(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    sequence: u32,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
) -> Result<()> {
    let escaped_session_id = escape_graphql_string(session_id);
    let message_key = format!("{escaped_session_id}:{sequence}");
    save_message_inner(
        node,
        session_id,
        agent_did,
        sequence,
        role,
        content,
        reasoning,
        &message_key,
    )
    .await
}

pub(crate) async fn save_message_with_key(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    sequence: u32,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
    message_key: &str,
) -> Result<()> {
    let escaped_message_key = escape_graphql_string(message_key);
    save_message_inner(
        node,
        session_id,
        agent_did,
        sequence,
        role,
        content,
        reasoning,
        &escaped_message_key,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn save_message_inner(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    sequence: u32,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
    escaped_message_key: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped = escape_graphql_string(content);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_role = escape_graphql_string(role);
    // #492: persist the durable reasoning copy alongside content. Empty/absent
    // reasoning is written as "" so the field round-trips deterministically.
    let escaped_reasoning = escape_graphql_string(reasoning.unwrap_or(""));

    // `agent_did` is only written in the `add` branch: it is the immutable scope
    // key, stamped once at create. The `update` branch must not rewrite it.
    let mutation = format!(
        r#"mutation {{
            upsert_AgentMessage(
                filter: {{ message_key: {{ _eq: "{escaped_message_key}" }} }},
                add: {{
                    message_key: "{escaped_message_key}",
                    session_id: "{escaped_session_id}",
                    agent_did: "{escaped_agent_did}",
                    sequence: {sequence},
                    role: "{escaped_role}",
                    content: "{escaped}",
                    reasoning: "{escaped_reasoning}",
                    timestamp: "{now}"
                }},
                update: {{
                    content: "{escaped}",
                    reasoning: "{escaped_reasoning}",
                    timestamp: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );

    super::retry::execute_mutation_with_retry(node, &mutation, "save_message").await?;
    Ok(())
}

pub(crate) async fn append_message(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
) -> Result<u32> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let sequence = next_append_sequence(node, session_id).await?;
        match create_message(
            node, session_id, agent_did, sequence, role, content, reasoning,
        )
        .await
        {
            Ok(()) => return Ok(sequence),
            Err(error) if attempts < 5 => {
                tracing::debug!(
                    session_id = %session_id,
                    sequence,
                    error = %error,
                    "append_message create failed; retrying with refreshed sequence"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn next_append_sequence(node: &EmbeddedNode, session_id: &str) -> Result<u32> {
    let message_max = super::sessions::max_sequence(node, session_id).await?;
    let tool_call_reserved_max = max_tool_call_reserved_sequence(node, session_id).await?;
    Ok(message_max.max(tool_call_reserved_max) + 1)
}

#[derive(Deserialize)]
struct ToolCallSequenceRow {
    message_sequence: u32,
}

async fn max_tool_call_reserved_sequence(node: &EmbeddedNode, session_id: &str) -> Result<u32> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}
            ) {{ message_sequence }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "max_tool_call_reserved_sequence").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading tool-call message sequences for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    let rows: Vec<ToolCallSequenceRow> = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let mut counts = std::collections::BTreeMap::<u32, u32>::new();
    for row in rows {
        *counts.entry(row.message_sequence).or_default() += 1;
    }
    Ok(counts
        .into_iter()
        .map(|(sequence, count)| sequence + count)
        .max()
        .unwrap_or(0))
}

async fn create_message(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    sequence: u32,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped = escape_graphql_string(content);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_role = escape_graphql_string(role);
    // #492: durable reasoning copy written at materialize time (see save_message).
    let escaped_reasoning = escape_graphql_string(reasoning.unwrap_or(""));
    let message_key = format!("{escaped_session_id}:{sequence}");

    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{escaped_session_id}",
                agent_did: "{escaped_agent_did}",
                sequence: {sequence},
                role: "{escaped_role}",
                content: "{escaped}",
                reasoning: "{escaped_reasoning}",
                timestamp: "{now}"
            }}) {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        anyhow::bail!("append AgentMessage failed: {:?}", resp.errors);
    }
    Ok(())
}

pub(crate) async fn mark_response_materialized(
    node: &EmbeddedNode,
    request_id: &str,
    sequence: u32,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_request_id = escape_graphql_string(request_id);
    let mutation = format!(
        r#"mutation {{
            update_AgentResponse(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                input: {{
                    materialized_message_sequence: {sequence},
                    materialized_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );

    super::retry::execute_mutation_with_retry(node, &mutation, "mark_response_materialized")
        .await?;
    Ok(())
}
