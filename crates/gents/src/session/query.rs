use super::rows::{ConversationDocument, SessionDocument};
use super::*;

pub(super) async fn load_session_document(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<SessionDocument> {
    load_session_document_optional(node, session_id)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "loading session for completion: no AgentSession for session_id={session_id}"
            )
        })
}

pub(super) async fn load_session_document_optional(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Option<SessionDocument>> {
    load_agent_session_exact(node, session_id).await
}

pub async fn load_agent_session_exact(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Option<SessionDocument>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }}
                }}
            ) {{
                _docID
                session_id
                agent_name
                agent_did
                requester_did
                behavior_id
                started
                ended
                status
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading session for completion session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    let rows: Vec<SessionDocument> =
        match resp.data.as_ref().and_then(|data| data.get("AgentSession")) {
            Some(value) => serde_json::from_value(value.clone())?,
            None => Vec::new(),
        };
    for row in &rows {
        if row.session_id != session_id {
            anyhow::bail!(
                "AgentSession logical key mismatch: queried session_id={session_id} but _docID={} returned session_id={}",
                row.doc_id,
                row.session_id
            );
        }
    }
    Ok(super::resolve_exact_logical_match(
        "AgentSession",
        "session_id",
        session_id,
        rows,
        |row| row.doc_id.as_str(),
    )?)
}

/// Whether any `AgentResponse` in this session is still streaming.
///
/// Backs the session-scope resolution of the modelled `safeToReduce` gate: a
/// live response means a turn is still being written into this session's
/// transcript, and compaction must not summarize a half-written turn. See
/// `boundary.compaction.safe-to-reduce-session-scope`.
pub(crate) async fn session_has_live_response(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<bool> {
    session_has_other_live_response(node, session_id, None).await
}

/// Whether this session has a streaming response other than the current
/// owned-loop request. At a completion-turn boundary the current response is
/// necessarily still marked `streaming`, but every message the loop is about
/// to compact has finished streaming and been yielded to persistence. A
/// different live response can still be half-written and closes the modelled
/// `safeToReduce` gate.
pub(crate) async fn session_has_other_live_response(
    node: &EmbeddedNode,
    session_id: &str,
    current_request_id: Option<&str>,
) -> Result<bool> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    status: {{ _eq: "streaming" }}
                }},
                limit: 2
            ) {{
                response_key
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading live responses for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(rows.iter().any(|row| {
        let response_key = row.get("response_key").and_then(|value| value.as_str());
        match current_request_id {
            Some(current_request_id) => response_key != Some(current_request_id),
            None => true,
        }
    }))
}

pub(crate) async fn load_session_behavior_id(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Option<String>> {
    Ok(load_session_document_optional(node, session_id)
        .await?
        .and_then(|session| {
            session.behavior_id.and_then(|behavior_id| {
                let trimmed = behavior_id.trim().to_string();
                (!trimmed.is_empty()).then_some(trimmed)
            })
        }))
}

/// Load every `AgentConversation` doc for a session, canonical doc first.
///
/// Duplicates exist in the wild (#693): `session_id` is unique-indexed in the
/// current schema, but DefraDB cannot add an index to an already-created
/// collection, so stores whose collection predates the unique index carry
/// duplicate rows permanently — and replication can mint them. A write
/// addressed by `filter: { session_id }` matches every duplicate, and DefraDB
/// refuses it (`cannot upsert multiple matching documents`), which is why every
/// conversation write must address a single `_docID`.
///
/// The canonical doc is the one live surfaces read and recovery repairs. It is
/// chosen by an explicit total order (newest `updated_at`, then richest, then
/// greatest `_docID`) rather than by scan order: DefraDB returns duplicates in
/// docID order, not recency order. `Recovery.canonical_perm_invariant`
/// (proofs/Proofs/Recovery/Sweeps/Conversation.lean) proves this choice does not
/// depend on the order rows come back in.
pub(super) async fn load_conversation_documents_ranked(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Vec<ConversationDocument>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }}
                }}
            ) {{
                _docID
                title
                title_source
                preview_text
                status
                latest_request_id
                behavior_id
                created_at
                updated_at
                agent_did
                agent_name
                forked_from_session_id
                fork_at_user_turn
                forked_at
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading conversation documents for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    let mut rows: Vec<ConversationDocument> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentConversation"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    rows.sort_by(|left, right| conversation_rank(right).cmp(&conversation_rank(left)));
    Ok(rows)
}

/// Ranking key mirroring Lean `Recovery.docRank`: `(updated_at, richness,
/// doc_id)`, compared lexicographically. `doc_id` is the store's primary key, so
/// distinct docs never tie and the greatest element is unique.
fn conversation_rank(doc: &ConversationDocument) -> (String, usize, String) {
    let richness = [
        doc.title.trim(),
        doc.preview_text.trim(),
        doc.latest_request_id.trim(),
    ]
    .iter()
    .filter(|field| !field.is_empty())
    .count();
    (doc.updated_at.clone(), richness, doc.doc_id.clone())
}

pub(super) async fn load_conversation_document(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Option<ConversationDocument>> {
    Ok(load_conversation_documents_ranked(node, session_id)
        .await?
        .into_iter()
        .next())
}

pub(super) async fn load_recent_conversation_titles(
    node: &EmbeddedNode,
    agent_did: &str,
    exclude_session_id: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_session_id = escape_graphql_string(exclude_session_id);
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    session_id: {{ _ne: "{escaped_session_id}" }}
                }},
                order: {{ updated_at: DESC }},
                limit: {limit}
            ) {{
                title
                title_source
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading recent conversation titles for agent_did={}: {:?}",
            agent_did,
            resp.errors
        );
    }

    let rows: Vec<ConversationDocument> = match resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentConversation"))
    {
        Some(value) => serde_json::from_value(value.clone())?,
        None => Vec::new(),
    };

    Ok(rows
        .into_iter()
        .filter(|row| row.title_source.as_deref() != Some("placeholder"))
        .filter_map(|row| {
            let trimmed = row.title.trim();
            (!trimmed.is_empty()).then_some(trimmed.to_string())
        })
        .collect())
}
