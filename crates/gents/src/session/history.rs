use super::retry::execute_query_timed;
use super::rows::AgentMessageRow;
use super::*;
use gents_protocol::transcript::decode_persisted_message;
use serde_json::Value;

pub async fn load_history(node: &EmbeddedNode, session_id: &str) -> Result<Vec<Message>> {
    load_history_through_sequence(node, session_id, None).await
}

pub(crate) async fn load_history_through_sequence(
    node: &EmbeddedNode,
    session_id: &str,
    through_sequence: Option<u32>,
) -> Result<Vec<Message>> {
    load_history_projection(node, session_id, through_sequence, None).await
}

pub(crate) async fn load_history_for_request(
    node: &EmbeddedNode,
    request: &crate::watcher::AgentRequest,
    through_sequence: Option<u32>,
) -> Result<Vec<Message>> {
    let current_input = Message::user(request.content.clone());
    load_history_projection(
        node,
        &request.session_id,
        through_sequence,
        Some((&request.request_id, current_input)),
    )
    .await
}

pub(super) async fn load_history_projection(
    node: &EmbeddedNode,
    session_id: &str,
    through_sequence: Option<u32>,
    current_input: Option<(&str, Message)>,
) -> Result<Vec<Message>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let sequence_filter = through_sequence
        .map(|sequence| format!(", sequence: {{ _le: {sequence} }}"))
        .unwrap_or_default();
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }}{sequence_filter} }},
                order: {{ sequence: ASC }}
            ) {{
                sequence
                role
                content
                timestamp
                request_id
                message_key
            }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "load_history").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading history for session_id={} through_sequence={:?}: {:?}",
            session_id,
            through_sequence,
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
        let decoded = decode_persisted_message(msg.role.as_str(), msg.content.as_str());
        if current_input.as_ref().is_some_and(|(request_id, input)| {
            msg.request_id.as_deref() == Some(request_id.as_ref())
                && (crate::lifecycle::queue::is_steering_input_message_key(&msg.message_key)
                    || decoded == *input)
        }) {
            continue;
        }
        history.push(decoded);
    }

    tracing::Span::current().record("history_message_count", history.len() as i64);
    tracing::debug!(session_id = %session_id, ?through_sequence, current_request_id = current_input.as_ref().map(|value| value.0), count = history.len(), "loaded history");
    Ok(history)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn save_message(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    sequence: u32,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
) -> Result<()> {
    save_message_with_requester_did(
        node, session_id, agent_did, None, sequence, role, content, reasoning, None, None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn save_message_with_requester_did(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    sequence: u32,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
) -> Result<()> {
    let escaped_session_id = escape_graphql_string(session_id);
    let message_key = format!("{escaped_session_id}:{sequence}");
    save_message_inner(
        node,
        session_id,
        agent_did,
        requester_did,
        sequence,
        role,
        content,
        reasoning,
        request_id,
        request_doc_id,
        &message_key,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn save_message_inner(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    sequence: u32,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
    escaped_message_key: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped = escape_graphql_string(content);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let requester_did_field = super::requester_did_create_field(requester_did);
    let request_doc_id_field = super::request_doc_id_create_field(request_doc_id);
    let escaped_request_id = escape_graphql_string(request_id.unwrap_or(""));
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
                    {requester_did_field}
                    {request_doc_id_field}
                    request_id: "{escaped_request_id}",
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_message_with_requester_did(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
) -> Result<u32> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        let sequence = next_append_sequence(node, session_id).await?;
        match create_message(
            node,
            session_id,
            agent_did,
            requester_did,
            sequence,
            role,
            content,
            reasoning,
            request_id,
            request_doc_id,
            None,
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

/// Append a message exactly once under a caller-owned stable key.
///
/// Concurrent writers can reserve the same next sequence or race on the same
/// key. A successful key winner is authoritative; losers re-read that durable
/// row and return its sequence without updating its content.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_message_once_with_key_and_requester_did(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
    message_key: &str,
    preferred_sequence: Option<u32>,
) -> Result<(u32, bool)> {
    if let Some(sequence) = message_sequence_for_key(node, session_id, message_key).await? {
        return Ok((sequence, false));
    }

    let mut attempts = 0;
    loop {
        attempts += 1;
        let sequence = match preferred_sequence {
            Some(sequence) if !message_sequence_exists(node, session_id, sequence).await? => {
                sequence
            }
            Some(_) | None => next_append_sequence(node, session_id).await?,
        };
        match create_message(
            node,
            session_id,
            agent_did,
            requester_did,
            sequence,
            role,
            content,
            reasoning,
            request_id,
            request_doc_id,
            Some(message_key),
        )
        .await
        {
            Ok(()) => return Ok((sequence, true)),
            Err(error) => {
                if let Some(existing) =
                    message_sequence_for_key(node, session_id, message_key).await?
                {
                    return Ok((existing, false));
                }
                if attempts >= 5 {
                    return Err(error);
                }
                tracing::debug!(
                    session_id,
                    message_key,
                    sequence,
                    error = %error,
                    "keyed append lost a sequence race; retrying"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

async fn message_sequence_exists(
    node: &EmbeddedNode,
    session_id: &str,
    sequence: u32,
) -> Result<bool> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    sequence: {{ _eq: {sequence} }}
                }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );
    let response = execute_query_timed(node, &query, "message_sequence_exists").await;
    if response.has_errors() {
        anyhow::bail!(
            "checking AgentMessage sequence for session_id={} sequence={}: {:?}",
            session_id,
            sequence,
            response.errors
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty()))
}

async fn message_sequence_for_key(
    node: &EmbeddedNode,
    session_id: &str,
    message_key: &str,
) -> Result<Option<u32>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_message_key = escape_graphql_string(message_key);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    message_key: {{ _eq: "{escaped_message_key}" }}
                }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );
    let response = execute_query_timed(node, &query, "message_sequence_for_key").await;
    if response.has_errors() {
        anyhow::bail!(
            "keyed AgentMessage lookup failed for session_id={} message_key={}: {:?}",
            session_id,
            message_key,
            response.errors
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("sequence"))
        .and_then(Value::as_u64)
        .and_then(|sequence| u32::try_from(sequence).ok()))
}

/// #497: durable request-scoped dedup. Return the sequence of an already-persisted
/// message for `(session_id, request_id, content)`, if one exists. Used to keep
/// the turn-1 user prompt + `<context>` message exactly-once across daemon retry
/// attempts (each attempt builds a fresh hook, so in-memory turn counting cannot
/// prevent a duplicate row after a transient failure before the first token).
pub(crate) async fn message_sequence_for_request_content(
    node: &EmbeddedNode,
    session_id: &str,
    request_id: &str,
    content: &str,
) -> Result<Option<u32>> {
    if request_id.is_empty() {
        return Ok(None);
    }
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_content = escape_graphql_string(content);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    request_id: {{ _eq: "{escaped_request_id}" }},
                    content: {{ _eq: "{escaped_content}" }}
                }},
                order: {{ sequence: ASC }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "message_sequence_for_request_content").await;
    if resp.has_errors() {
        anyhow::bail!(
            "dedup lookup for session_id={} request_id={}: {:?}",
            session_id,
            request_id,
            resp.errors
        );
    }

    let rows: Vec<ToolCallSequenceRow2> = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows.first().map(|row| row.sequence))
}

#[derive(Deserialize)]
struct ToolCallSequenceRow2 {
    sequence: u32,
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
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }}
                    await_mode: {{ _eq: "background" }}
                }}
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
    // Background spawns reserve one result position after their assistant
    // turn so an independently appended completion cannot overtake the
    // immediate receipt. Foreground results do not reserve a position: they
    // append when the owned loop observes completion.
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

#[allow(clippy::too_many_arguments)]
async fn create_message(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    sequence: u32,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
    message_key: Option<&str>,
) -> Result<()> {
    let mutation = create_message_mutation(
        session_id,
        agent_did,
        requester_did,
        sequence,
        role,
        content,
        reasoning,
        request_id,
        request_doc_id,
        message_key,
    );

    super::retry::execute_mutation_with_retry(node, &mutation, "append AgentMessage").await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn create_message_mutation(
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    sequence: u32,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
    message_key: Option<&str>,
) -> String {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped = escape_graphql_string(content);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let requester_did_field = super::requester_did_create_field(requester_did);
    let escaped_role = escape_graphql_string(role);
    // #492: durable reasoning copy written at materialize time (see save_message).
    let escaped_reasoning = escape_graphql_string(reasoning.unwrap_or(""));
    // #497: stamp the originating request id so the owned loop can durably dedup
    // the turn-1 user prompt + <context> message across retry attempts (a fresh
    // hook is built per attempt and cannot dedup in memory). Empty when the
    // write is not request-scoped (background/fork paths).
    let escaped_request_id = escape_graphql_string(request_id.unwrap_or(""));
    let request_doc_id_field = super::request_doc_id_create_field(request_doc_id);
    let message_key = message_key
        .map(escape_graphql_string)
        .unwrap_or_else(|| format!("{escaped_session_id}:{sequence}"));

    format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{escaped_session_id}",
                agent_did: "{escaped_agent_did}",
                {requester_did_field}
                request_id: "{escaped_request_id}",
                {request_doc_id_field}
                sequence: {sequence},
                role: "{escaped_role}",
                content: "{escaped}",
                reasoning: "{escaped_reasoning}",
                timestamp: "{now}"
            }}) {{ _docID }}
        }}"#
    )
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
