use super::query::{load_session_document, load_session_document_optional};
use super::retry::{execute_query_timed, log_mutation_timing, retry_operation};
use super::*;

pub async fn create_session(
    node: &EmbeddedNode,
    agent_name: &str,
    agent_did: &str,
) -> Result<String> {
    let session_id = uuid::Uuid::new_v4().to_string();
    create_session_with_id(node, &session_id, agent_name, agent_did).await?;
    Ok(session_id)
}

pub(crate) async fn create_session_with_id(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    agent_did: &str,
) -> Result<()> {
    create_session_with_behavior_id(node, session_id, agent_name, agent_did, agent_name).await
}

pub async fn create_session_with_behavior_id(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    agent_did: &str,
    behavior_id: &str,
) -> Result<()> {
    create_session_with_behavior_id_and_requester_did(
        node,
        session_id,
        agent_name,
        agent_did,
        behavior_id,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn create_session_with_behavior_id_and_requester_did(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    agent_did: &str,
    behavior_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_name = escape_graphql_string(agent_name);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let requester_did_field = super::requester_did_create_field(requester_did);

    let created = retry_operation("create_session", || async {
        let now = chrono::Utc::now().to_rfc3339();
        let existing = load_session_document_optional(node, session_id).await?;
        let created = existing.is_none();
        if let Some(existing) = existing.as_ref() {
            validate_session_binding(existing, agent_did, requester_did)?;
        }
        let started = existing
            .as_ref()
            .map(|session| session.started.clone())
            .unwrap_or_else(|| now.clone());
        let resolved_behavior_id =
            resolve_behavior_id(existing.as_ref(), behavior_id, "AgentSession")?;
        let escaped_started = escape_graphql_string(&started);
        let escaped_behavior_id = escape_graphql_string(&resolved_behavior_id);

        let (mutation_field, expected_doc_id, mutation) = if let Some(existing) = existing.as_ref()
        {
            let escaped_doc_id = escape_graphql_string(&existing.doc_id);
            (
                "update_AgentSession",
                Some(existing.doc_id.as_str()),
                format!(
                    r#"mutation {{
                        update_AgentSession(
                            filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                            input: {{
                                status: "active"
                            }}
                        ) {{ _docID }}
                    }}"#
                ),
            )
        } else {
            (
                "create_AgentSession",
                None,
                format!(
                    r#"mutation {{
                    create_AgentSession(input: {{
                        session_id: "{escaped_session_id}",
                        agent_name: "{escaped_agent_name}",
                        agent_did: "{escaped_agent_did}",
                        {requester_did_field}
                        behavior_id: "{escaped_behavior_id}",
                        started: "{escaped_started}",
                        status: "active"
                    }}) {{ _docID }}
                }}"#
                ),
            )
        };

        let started_at = std::time::Instant::now();
        let resp = node.execute(&mutation).await;
        log_mutation_timing("create_session", started_at.elapsed());

        if resp.has_errors() {
            anyhow::bail!("create_session mutation failed: {:?}", resp.errors);
        }

        let returned_doc_id = exact_mutation_doc_id(resp.data.as_ref(), mutation_field)?;
        if let Some(expected_doc_id) = expected_doc_id {
            if returned_doc_id != expected_doc_id {
                anyhow::bail!(
                    "AgentSession exact update returned _docID={returned_doc_id}, expected {expected_doc_id}"
                );
            }
        }

        // Re-enumerate after the write.  A concurrent create in a legacy
        // unindexed collection must surface as a logical conflict rather than
        // allowing this successful mutation to establish an arbitrary winner.
        let verified = load_session_document(node, session_id).await?;
        validate_session_binding(&verified, agent_did, requester_did)?;
        if verified.doc_id != returned_doc_id {
            anyhow::bail!(
                "AgentSession write verification selected _docID={}, mutation returned {returned_doc_id}",
                verified.doc_id
            );
        }
        Ok(created)
    })
    .await?;

    let log_message = if created {
        "session created"
    } else {
        "session ensured"
    };
    tracing::info!(
        session_id = %session_id,
        agent = %agent_name,
        behavior_id = %behavior_id,
        created,
        "{log_message}"
    );
    Ok(())
}

pub async fn ensure_session_with_behavior_id(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    agent_did: &str,
    behavior_id: &str,
) -> Result<()> {
    create_session_with_behavior_id(node, session_id, agent_name, agent_did, behavior_id).await
}

#[allow(clippy::too_many_arguments)]
/// Ensure a session exists with immutable owner, behavior, and requester bindings.
///
/// Callers that create requests with a non-null `requester_did` must use the
/// same requester here so the session spine and every request attributed to it
/// carry one consistent principal boundary.
pub async fn ensure_session_with_behavior_id_and_requester_did(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
    agent_did: &str,
    behavior_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    create_session_with_behavior_id_and_requester_did(
        node,
        session_id,
        agent_name,
        agent_did,
        behavior_id,
        requester_did,
    )
    .await
}

pub(crate) async fn max_sequence(node: &EmbeddedNode, session_id: &str) -> Result<u32> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "max_sequence").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading max sequence for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    Ok(resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .and_then(|message| message.get("sequence"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32)
}

pub async fn close_session(node: &EmbeddedNode, session_id: &str) -> Result<()> {
    retry_operation("close_session", || async {
        let session = load_session_document(node, session_id).await?;

        let now = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let escaped_started = escape_graphql_string(&session.started);
        let escaped_doc_id = escape_graphql_string(&session.doc_id);
        let mutation = format!(
            r#"mutation {{
                update_AgentSession(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        started: "{escaped_started}",
                        status: "completed",
                        ended: "{now}"
                    }}
                ) {{ _docID }}
            }}"#,
        );

        let started_at = std::time::Instant::now();
        let resp = node.execute(&mutation).await;
        log_mutation_timing("close_session", started_at.elapsed());

        if resp.has_errors() {
            anyhow::bail!("close_session mutation failed: {:?}", resp.errors);
        }

        let returned_doc_id = exact_mutation_doc_id(resp.data.as_ref(), "update_AgentSession")?;
        if returned_doc_id != session.doc_id {
            anyhow::bail!(
                "AgentSession exact close returned _docID={returned_doc_id}, expected {}",
                session.doc_id
            );
        }
        let verified = load_session_document(node, session_id).await?;
        if verified.doc_id != session.doc_id || verified.status.as_deref() != Some("completed") {
            anyhow::bail!(
                "AgentSession close verification failed for session_id={session_id}: expected _docID={} status=completed, observed _docID={} status={:?}",
                session.doc_id,
                verified.doc_id,
                verified.status
            );
        }
        Ok(())
    })
    .await?;

    tracing::info!(session_id = %session_id, "session closed");
    Ok(())
}

fn validate_session_binding(
    existing: &super::rows::SessionDocument,
    expected_agent_did: &str,
    expected_requester_did: Option<&str>,
) -> Result<()> {
    let existing_agent_did = normalize_optional_string(existing.agent_did.as_deref());
    let expected_agent_did = normalize_optional_string(Some(expected_agent_did));
    if existing_agent_did != expected_agent_did {
        anyhow::bail!(
            "AgentSession immutable owner mismatch for session_id={}: _docID={} existing agent_did={existing_agent_did:?} expected={expected_agent_did:?}",
            existing.session_id,
            existing.doc_id
        );
    }
    let existing_requester_did = normalize_optional_string(existing.requester_did.as_deref());
    let expected_requester_did = normalize_optional_string(expected_requester_did);
    if existing_requester_did != expected_requester_did {
        anyhow::bail!(
            "AgentSession immutable requester mismatch for session_id={}: _docID={} existing requester_did={existing_requester_did:?} expected={expected_requester_did:?}",
            existing.session_id,
            existing.doc_id
        );
    }
    Ok(())
}

fn exact_mutation_doc_id(data: Option<&serde_json::Value>, field: &str) -> Result<String> {
    let add_field = field
        .strip_prefix("create_")
        .map(|collection| format!("add_{collection}"));
    let Some(value) = data.and_then(|data| {
        data.get(field)
            .or_else(|| add_field.as_deref().and_then(|field| data.get(field)))
    }) else {
        anyhow::bail!("{field} returned no result");
    };
    let document_ids = if let Some(doc_id) = value.get("_docID").and_then(serde_json::Value::as_str)
    {
        vec![doc_id.to_string()]
    } else {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("_docID").and_then(serde_json::Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    };
    match document_ids.as_slice() {
        [doc_id] if !doc_id.trim().is_empty() => Ok(doc_id.clone()),
        _ => anyhow::bail!("{field} returned non-exact _docIDs={document_ids:?}"),
    }
}

fn resolve_behavior_id(
    existing: Option<&super::rows::SessionDocument>,
    requested_behavior_id: &str,
    collection_name: &str,
) -> Result<String> {
    let existing_behavior_id =
        existing.and_then(|session| normalize_optional_string(session.behavior_id.as_deref()));
    let requested_behavior_id = normalize_optional_string(Some(requested_behavior_id));

    match (existing_behavior_id, requested_behavior_id) {
        (Some(existing), Some(requested)) if existing != requested => anyhow::bail!(
            "{collection_name} session behavior mismatch: existing={existing} requested={requested}"
        ),
        (Some(existing), _) => Ok(existing.to_string()),
        (None, Some(requested)) => Ok(requested.to_string()),
        (None, None) => Ok(String::new()),
    }
}

fn normalize_optional_string(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}
