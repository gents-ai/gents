use super::retry::execute_query_timed;
use super::rows::AgentMessageRow;
use super::*;
use gents_protocol::transcript::decode_persisted_message;
use std::sync::Arc;

type TranscriptAppendLock = Arc<tokio::sync::Mutex<()>>;

fn transcript_append_lock(session_id: &str) -> TranscriptAppendLock {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>,
        >,
    > = std::sync::OnceLock::new();

    let mut locks = LOCKS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(lock) = locks.get(session_id).and_then(std::sync::Weak::upgrade) {
        return lock;
    }

    // Session ownership confines transcript writers to one deployment. The
    // per-session lock serializes that deployment's owned loop and independent
    // background-completion tasks without serializing unrelated agents.
    locks.retain(|_, lock| lock.strong_count() > 0);
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(session_id.to_string(), Arc::downgrade(&lock));
    lock
}

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
        node, session_id, agent_did, None, sequence, role, content, reasoning,
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
    escaped_message_key: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let escaped = escape_graphql_string(content);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let requester_did_field = super::requester_did_create_field(requester_did);
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
) -> Result<u32> {
    let append_lock = transcript_append_lock(session_id);
    let _append_guard = append_lock.lock().await;
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_message_with_key_and_requester_did(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
    request_id: Option<&str>,
    message_key: &str,
    preferred_sequence: Option<u32>,
) -> Result<u32> {
    let append_lock = transcript_append_lock(session_id);
    let _append_guard = append_lock.lock().await;
    if let Some(sequence) = message_sequence_for_key(node, message_key).await? {
        return Ok(sequence);
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
            Some(message_key),
        )
        .await
        {
            Ok(()) => return Ok(sequence),
            Err(error) if attempts < 5 => {
                if let Some(existing) = message_sequence_for_key(node, message_key).await? {
                    return Ok(existing);
                }
                tracing::debug!(
                    session_id = %session_id,
                    sequence,
                    error = %error,
                    "keyed AgentMessage append failed; retrying with refreshed sequence"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
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
                    session_id: {{ _eq: "{escaped_session_id}" }}
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
        .and_then(serde_json::Value::as_array)
        .is_some_and(|rows| !rows.is_empty()))
}

async fn message_sequence_for_key(node: &EmbeddedNode, message_key: &str) -> Result<Option<u32>> {
    let escaped_message_key = escape_graphql_string(message_key);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ message_key: {{ _eq: "{escaped_message_key}" }} }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );
    let response = execute_query_timed(node, &query, "message_sequence_for_key").await;
    if response.has_errors() {
        anyhow::bail!(
            "loading AgentMessage sequence for message_key={}: {:?}",
            message_key,
            response.errors
        );
    }
    let rows: Vec<ToolCallSequenceRow2> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows.first().map(|row| row.sequence))
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
    // A background spawn can terminalize before the owned loop persists its
    // immediate handle/receipt. Reserve one result position for each such call
    // so its completion notification cannot overtake the receipt. Foreground
    // calls are deliberately excluded: their results are allocated as they
    // complete, including the result of a blocking wait after an independently
    // appended background-completion notification.
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
    message_key: Option<&str>,
) -> Result<()> {
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
    let message_key = message_key
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("{session_id}:{sequence}"));
    let message_key = escape_graphql_string(&message_key);

    let mutation = format!(
        r#"mutation {{
            create_AgentMessage(input: {{
                message_key: "{message_key}",
                session_id: "{escaped_session_id}",
                agent_did: "{escaped_agent_did}",
                {requester_did_field}
                request_id: "{escaped_request_id}",
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
