use super::retry::execute_query_timed;
use super::rows::AgentMessageRow;
use super::*;
use anyhow::Context;
use gents_protocol::transcript::decode_persisted_message;
use serde_json::Value;

/// Injective key for a sequence within one canonical session scope.
/// Explicit caller-owned keys keep their own vocabulary (steering, receipts).
pub fn sequence_message_key(
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    sequence: u32,
) -> String {
    format!(
        "message:{}",
        serde_json::to_string(&(agent_did, session_id, requester_did, sequence))
            .expect("session scope serializes")
    )
}

pub async fn load_history(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<Message>> {
    load_history_through_sequence(node, session_id, agent_did, requester_did, None).await
}

pub(crate) async fn load_history_through_sequence(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
) -> Result<Vec<Message>> {
    Ok(load_sequenced_history_projection(
        node,
        session_id,
        agent_did,
        requester_did,
        through_sequence,
        None,
        None,
    )
    .await?
    .into_iter()
    .map(|row| row.message)
    .collect())
}

pub(crate) async fn load_sequenced_history_for_request(
    node: &EmbeddedNode,
    request: &crate::watcher::AgentRequest,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
) -> Result<Vec<SequencedMessage>> {
    load_sequenced_history_projection(
        node,
        &request.session_id,
        &request.agent_did,
        request.requester_did.as_deref(),
        through_sequence,
        after_sequence,
        Some((&request.request_id, Message::user(request.content.clone()))),
    )
    .await
}

#[cfg(test)]
pub(super) async fn load_history_projection(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    current_input: Option<(&str, Message)>,
) -> Result<Vec<Message>> {
    Ok(load_sequenced_history_projection(
        node,
        session_id,
        agent_did,
        requester_did,
        through_sequence,
        None,
        current_input,
    )
    .await?
    .into_iter()
    .map(|row| row.message)
    .collect())
}

pub(super) async fn load_sequenced_history_projection(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    through_sequence: Option<u32>,
    after_sequence: Option<u32>,
    current_input: Option<(&str, Message)>,
) -> Result<Vec<SequencedMessage>> {
    let scope = super::query::session_scope_filter(agent_did, session_id, requester_did);
    let mut sequence_bounds = Vec::new();
    if let Some(sequence) = after_sequence {
        sequence_bounds.push(format!("_gt: {sequence}"));
    }
    if let Some(sequence) = through_sequence {
        sequence_bounds.push(format!("_le: {sequence}"));
    }
    let sequence_filter = (!sequence_bounds.is_empty())
        .then(|| format!(", sequence: {{ {} }}", sequence_bounds.join(", ")))
        .unwrap_or_default();
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ {scope}{sequence_filter} }},
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

    let resp = execute_query_timed(node, &query, "load_history").await?;
    if resp.has_errors() {
        anyhow::bail!(
            "loading history for session_id={} through_sequence={:?}: {:?}",
            session_id,
            through_sequence,
            resp.errors
        );
    }

    let messages: Vec<AgentMessageRow> = serde_json::from_value(
        resp.data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .context("history query omitted message rows")?
            .clone(),
    )?;

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
        history.push(SequencedMessage {
            sequence: msg.sequence,
            message: decoded,
        });
    }

    tracing::Span::current().record("history_message_count", history.len() as i64);
    tracing::debug!(session_id = %session_id, ?through_sequence, ?after_sequence, current_request_id = current_input.as_ref().map(|value| value.0), count = history.len(), "loaded history");
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
    let message_key = escape_graphql_string(&sequence_message_key(
        agent_did,
        session_id,
        requester_did,
        sequence,
    ));
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

    let scope = super::query::session_scope_filter(agent_did, session_id, requester_did);

    // `agent_did` is only written in the `add` branch: it is the immutable scope
    // key, stamped once at create. The `update` branch must not rewrite it.
    let mutation = format!(
        r#"mutation {{
            upsert_AgentMessage(
                filter: {{ {scope}, message_key: {{ _eq: "{escaped_message_key}" }} }},
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

    crate::config_client::ConfigAccess::write_local(node, "session.save_message", &mutation)
        .await?;
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
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "session.append_message",
        |txn| {
            Box::pin(async move {
                append_message_in_txn(
                    txn,
                    session_id,
                    agent_did,
                    requester_did,
                    role,
                    content,
                    reasoning,
                    request_id,
                    request_doc_id,
                    None,
                    None,
                )
                .await
                .map(|(sequence, _)| sequence)
            })
        },
    )
    .await
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
    crate::config_client::ConfigAccess::transact_local_idempotent(
        node,
        None,
        crate::config_client::IdempotentTransactionRetry::FiveAttempts,
        "session.append_message_once",
        |txn| {
            Box::pin(async move {
                append_message_in_txn(
                    txn,
                    session_id,
                    agent_did,
                    requester_did,
                    role,
                    content,
                    reasoning,
                    request_id,
                    request_doc_id,
                    Some(message_key),
                    preferred_sequence,
                )
                .await
            })
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn append_message_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    role: &str,
    content: &str,
    reasoning: Option<&str>,
    request_id: Option<&str>,
    request_doc_id: Option<&str>,
    message_key: Option<&str>,
    preferred_sequence: Option<u32>,
) -> Result<(u32, bool)> {
    if let Some(key) = message_key {
        if let Some(sequence) =
            message_sequence_for_key(txn, session_id, agent_did, requester_did, key).await?
        {
            return Ok((sequence, false));
        }
    }
    let sequence = match preferred_sequence {
        Some(sequence)
            if !message_sequence_exists(txn, session_id, agent_did, requester_did, sequence)
                .await? =>
        {
            sequence
        }
        _ => next_append_sequence(txn, session_id, agent_did, requester_did).await?,
    };
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
    let response = txn.execute(&mutation).await?;
    let response = defra_node::QueryResponse::success(
        response
            .get("data")
            .context("message create omitted data")?
            .clone(),
    );
    anyhow::ensure!(
        crate::graphql::single_mutation_document(&response, "create_AgentMessage")?.is_some(),
        "message create returned no document"
    );
    Ok((sequence, true))
}

async fn message_sequence_exists(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    sequence: u32,
) -> Result<bool> {
    let scope = super::query::session_scope_filter(agent_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    {scope},
                    sequence: {{ _eq: {sequence} }}
                }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    Ok(response
        .get("data")
        .and_then(|data| data.get("AgentMessage"))
        .and_then(Value::as_array)
        .context("message sequence query omitted rows")?
        .is_empty()
        == false)
}

async fn message_sequence_for_key(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    message_key: &str,
) -> Result<Option<u32>> {
    let scope = super::query::session_scope_filter(agent_did, session_id, requester_did);
    let escaped_message_key = escape_graphql_string(message_key);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    {scope},
                    message_key: {{ _eq: "{escaped_message_key}" }}
                }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentMessage"))
        .and_then(Value::as_array)
        .context("message key query omitted rows")?;
    rows.first()
        .map(|row| {
            u32::try_from(
                row.get("sequence")
                    .and_then(Value::as_u64)
                    .context("message key has invalid sequence")?,
            )
            .context("message sequence exceeds u32")
        })
        .transpose()
}

/// #497: durable request-scoped dedup. Return the sequence of an already-persisted
/// message for `(session_id, request_id, content)`, if one exists. Used to keep
/// the turn-1 user prompt + `<context>` message exactly-once across daemon retry
/// attempts (each attempt builds a fresh hook, so in-memory turn counting cannot
/// prevent a duplicate row after a transient failure before the first token).
pub(crate) async fn message_sequence_for_request_content(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    request_id: &str,
    content: &str,
) -> Result<Option<u32>> {
    if request_id.is_empty() {
        return Ok(None);
    }
    let scope = super::query::session_scope_filter(agent_did, session_id, requester_did);
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_content = escape_graphql_string(content);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{
                    {scope},
                    request_id: {{ _eq: "{escaped_request_id}" }},
                    content: {{ _eq: "{escaped_content}" }}
                }},
                order: {{ sequence: ASC }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "message_sequence_for_request_content").await?;
    if resp.has_errors() {
        anyhow::bail!(
            "dedup lookup for session_id={} request_id={}: {:?}",
            session_id,
            request_id,
            resp.errors
        );
    }

    let rows: Vec<MessageSequenceRow> = serde_json::from_value(
        resp.data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .context("request message query omitted rows")?
            .clone(),
    )?;
    Ok(rows.first().map(|row| row.sequence))
}

#[derive(Deserialize)]
struct MessageSequenceRow {
    sequence: u32,
}

async fn next_append_sequence(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<u32> {
    let message_max =
        super::sessions::max_sequence_in_txn(txn, session_id, agent_did, requester_did).await?;
    let tool_call_reserved_max =
        max_tool_call_reserved_sequence(txn, session_id, agent_did, requester_did).await?;
    message_max
        .max(tool_call_reserved_max)
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("message sequence exhausted"))
}

#[derive(Deserialize)]
struct ToolCallSequenceRow {
    message_sequence: u32,
}

async fn max_tool_call_reserved_sequence(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<u32> {
    let scope = super::query::session_scope_filter(agent_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    {scope}
                    await_mode: {{ _eq: "background" }}
                }}
            ) {{ message_sequence }}
        }}"#
    );

    let resp = txn.execute(&query).await?;

    let rows: Vec<ToolCallSequenceRow> = serde_json::from_value(
        resp.get("data")
            .and_then(|data| data.get("AgentToolCall"))
            .context("tool reservation query omitted rows")?
            .clone(),
    )?;
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
    let message_key = message_key.map(escape_graphql_string).unwrap_or_else(|| {
        escape_graphql_string(&sequence_message_key(
            agent_did,
            session_id,
            requester_did,
            sequence,
        ))
    });

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

/// Attach transcript materialization only to the active physical request in
/// its exact session scope. A missing or ambiguous response aborts the write.
pub(crate) async fn mark_response_materialized(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
    sequence: u32,
) -> Result<()> {
    anyhow::ensure!(
        !request_doc_id.is_empty(),
        "response materialization requires a request document"
    );
    let scope = super::query::session_scope_filter(agent_did, session_id, requester_did);
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            update_AgentResponse(
                filter: {{ {scope}, request_doc_id: {{ _eq: "{}" }} }},
                input: {{
                    materialized_message_sequence: {sequence},
                    materialized_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#,
        escape_graphql_string(request_doc_id),
    );
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "session.mark_response_materialized",
        |txn| {
            let mutation = mutation.clone();
            Box::pin(async move {
                let response = txn.execute(&mutation).await?;
                let rows = response
                    .get("data")
                    .and_then(|data| data.get("update_AgentResponse"))
                    .and_then(Value::as_array)
                    .context("response materialization omitted mutation rows")?;
                anyhow::ensure!(
                    rows.len() == 1,
                    "response materialization requires exactly one scoped response; found {}",
                    rows.len()
                );
                Ok(())
            })
        },
    )
    .await
}
