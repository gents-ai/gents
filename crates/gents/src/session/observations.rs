use super::query::{decode_session_row, load_agent_session, validate_agent_session};
use super::sessions::{
    load_agent_session_row_in_txn, patch_session_in_txn, touch_activity_value, touched_observation,
    SESSION_SCOPE_FIELDS,
};
use super::*;
use anyhow::Context;

/// Authoritative request fact read from `AgentRequest` rows for observation
/// advancement. The pure mirror of Lean `AgentSession.RequestFact`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRequestFact {
    pub agent_did: String,
    pub session_id: String,
    /// Exact requester scope; `None` is its own scope, not a wildcard.
    pub requester_did: Option<String>,
    pub behavior_id: String,
    pub created_at: String,
    pub observed: gents_protocol::session::SessionRequestObservation,
}

/// Canonical newest ordering: `created_at` then logical `request_id`, both
/// descending. The physical `_docID` is exact identity for refresh/retry and
/// is never an extra sort key.
fn request_fact_newer(a: &SessionRequestFact, b: &SessionRequestFact) -> bool {
    let (a_created, b_created) = match (
        chrono::DateTime::parse_from_rfc3339(&a.created_at),
        chrono::DateTime::parse_from_rfc3339(&b.created_at),
    ) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return false,
    };
    a_created > b_created
        || (a_created == b_created && a.observed.request_id > b.observed.request_id)
}

/// Whether `incoming` is the canonical latest request across `rows` under the
/// session's exact requester scope, including the currently observed row.
/// Duplicate logical roots fail instead of a limit-1 silent pick.
fn incoming_is_canonical_latest(
    session: &gents_protocol::session::AgentSession,
    rows: &[SessionRequestFact],
    incoming: &SessionRequestFact,
) -> Result<bool> {
    anyhow::ensure!(
        rows.len()
            == rows
                .iter()
                .map(|row| row.observed.request_id.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
        "session observation selection found duplicate request_id roots for \
         session_id={}",
        session.session_id
    );
    anyhow::ensure!(
        rows.iter().any(|row| *row == *incoming),
        "incoming request row is missing from the scoped request facts"
    );
    Ok(rows
        .iter()
        .all(|row| *row == *incoming || !request_fact_newer(row, incoming)))
}

fn scoped_request_filter(
    agent_did: &str,
    session_id: &str,
    requester_scope: Option<Option<&str>>,
) -> String {
    let agent_did = escape_graphql_string(agent_did);
    let session_id = escape_graphql_string(session_id);
    let requester_filter = if requester_scope.is_none() {
        String::new()
    } else if let Some(Some(requester_did)) = requester_scope {
        format!(
            r#", requester_did: {{ _eq: "{}" }}"#,
            escape_graphql_string(requester_did)
        )
    } else {
        // Exact absent scope: requester_did must be null.
        r#", requester_did: { _eq: null }"#.to_string()
    };
    format!(
        r#"filter: {{ agent_did: {{ _eq: "{agent_did}" }}, session_id: {{ _eq: "{session_id}" }}{requester_filter} }}"#
    )
}

/// Read the complete scoped request facts (created_at + identity + state)
/// inside the caller's transaction. `all_requesters` selects the explicit
/// background/all-requester scope; the ordinary route never widens.
pub(crate) async fn load_scoped_request_facts_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session: &gents_protocol::session::AgentSession,
    all_requesters: bool,
) -> Result<Vec<SessionRequestFact>> {
    load_request_facts_in_txn(
        txn,
        &session.agent_did,
        &session.session_id,
        if all_requesters {
            None
        } else {
            Some(session.requester_did.as_deref())
        },
    )
    .await
}

async fn load_request_facts_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    session_id: &str,
    requester_scope: Option<Option<&str>>,
) -> Result<Vec<SessionRequestFact>> {
    let query = format!(
        r#"{{
            AgentRequest({}) {{
                _docID request_id agent_did session_id requester_did behavior_id created_at lifecycle_state
            }}
        }}"#,
        scoped_request_filter(agent_did, session_id, requester_scope)
    );
    let response = txn.execute(&query).await?;
    let rows: Vec<serde_json::Value> = serde_json::from_value(
        response
            .get("data")
            .and_then(|data| data.get("AgentRequest"))
            .cloned()
            .context("request facts query omitted AgentRequest rows")?,
    )
    .context("decoding request fact rows")?;
    rows.iter()
        .map(|row| {
            chrono::DateTime::parse_from_rfc3339(
                row.get("created_at")
                    .and_then(serde_json::Value::as_str)
                    .context("request row omitted created_at")?,
            )
            .context("invalid request created_at")?;
            Ok(SessionRequestFact {
                agent_did: row
                    .get("agent_did")
                    .and_then(serde_json::Value::as_str)
                    .context("request row omitted agent_did")?
                    .to_string(),
                session_id: row
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .context("request row omitted session_id")?
                    .to_string(),
                requester_did: row
                    .get("requester_did")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                behavior_id: row
                    .get("behavior_id")
                    .and_then(serde_json::Value::as_str)
                    .context("request row omitted behavior_id")?
                    .to_string(),
                created_at: row
                    .get("created_at")
                    .and_then(serde_json::Value::as_str)
                    .context("request row omitted created_at")?
                    .to_string(),
                observed: gents_protocol::session::SessionRequestObservation {
                    request_doc_id: row
                        .get("_docID")
                        .and_then(serde_json::Value::as_str)
                        .context("request row omitted _docID")?
                        .to_string(),
                    request_id: row
                        .get("request_id")
                        .and_then(serde_json::Value::as_str)
                        .context("request row omitted request_id")?
                        .to_string(),
                    lifecycle_state: serde_json::from_value(
                        row.get("lifecycle_state").cloned().unwrap_or_default(),
                    )
                    .context("decoding request lifecycle_state")?,
                },
            })
        })
        .collect()
}

/// Read the actual request head, never the session's presentation cache.
/// Outer `None` explicitly selects all requesters; `Some(None)` is the exact
/// absent requester scope. Ordering uses parsed time then logical request ID.
pub async fn load_latest_request_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    session_id: &str,
    requester_scope: Option<Option<&str>>,
) -> Result<Option<SessionRequestFact>> {
    let rows = load_request_facts_in_txn(txn, agent_did, session_id, requester_scope).await?;
    let mut identities = std::collections::BTreeSet::new();
    let mut latest = None;
    for row in rows {
        anyhow::ensure!(
            identities.insert(row.observed.request_id.clone()),
            "request-head selection found duplicate logical request roots"
        );
        if latest
            .as_ref()
            .is_none_or(|old| request_fact_newer(&row, old))
        {
            latest = Some(row);
        }
    }
    Ok(latest)
}

/// Validate that `incoming` matches the session owner's scope and behavior,
/// mirroring the Lean `advance` guard.
fn validate_incoming_scope(
    session: &gents_protocol::session::AgentSession,
    incoming: &SessionRequestFact,
) -> Result<()> {
    anyhow::ensure!(
        incoming.agent_did == session.agent_did
            && incoming.session_id == session.session_id
            && incoming.behavior_id == session.behavior_id
            && incoming.requester_did == session.requester_did,
        "incoming request does not match the session owner/session/requester/behavior scope"
    );
    Ok(())
}

fn normalize_preview(content: &str) -> String {
    content
        .chars()
        .take(240)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Publish the physical `_docID`, logical `request_id`, observed state and
/// normalized preview as one atomic observation patch inside the caller's
/// transaction. The incoming request must match the session owner's exact
/// scope, be present in the complete scoped request facts (including the
/// currently observed row), and be the canonical latest by
/// (created_at DESC, request_id DESC); anything else is a no-op rejection.
pub(crate) async fn advance_session_request_observation_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    incoming: &SessionRequestFact,
    preview_content: &str,
    now: &str,
) -> Result<bool> {
    let session = load_agent_session_row_in_txn(
        txn,
        &incoming.agent_did,
        &incoming.session_id,
        incoming.requester_did.as_deref(),
    )
    .await?
    .context("advancing observation: session disappeared")?;
    validate_agent_session(&session.session)?;
    validate_incoming_scope(&session.session, incoming)?;
    let rows = load_scoped_request_facts_in_txn(txn, &session.session, false).await?;
    if !incoming_is_canonical_latest(&session.session, &rows, incoming)? {
        return Ok(false);
    }
    let activity = touch_activity_value(&session, now);
    let observation = gents_protocol::session::SessionObservation {
        last_activity_at: activity.clone(),
        preview: Some(normalize_preview(preview_content)),
        latest_request: Some(incoming.observed.clone()),
    };
    patch_session_in_txn(
        txn,
        &session.doc_id,
        serde_json::json!({"observation": observation}),
    )
    .await?;
    tracing::debug!(
        session_id = %incoming.session_id,
        request_id = %incoming.observed.request_id,
        "advanced session request observation"
    );
    Ok(true)
}

/// Lifecycle/preview refresh inside the caller's transaction: reread the
/// authoritative row by its exact physical `_docID` **and** logical
/// `request_id`, validate the session scope/behavior and stored-observation
/// identity, and copy the observed state from the authoritative row. Event
/// lifecycle payloads have no influence; a missing or mismatched row is a
/// no-op, never an older-local-request fallback.
pub(crate) async fn refresh_session_request_observation_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    request_doc_id: &str,
    request_id: &str,
    now: &str,
) -> Result<bool> {
    let Some(session) =
        load_agent_session_row_in_txn(txn, agent_did, session_id, requester_did).await?
    else {
        return Ok(false);
    };
    validate_agent_session(&session.session)?;
    let Some(observation) = session.session.observation.as_ref() else {
        return Ok(false);
    };
    let Some(latest) = observation.latest_request.as_ref() else {
        return Ok(false);
    };
    if latest.request_doc_id != request_doc_id || latest.request_id != request_id {
        return Ok(false);
    }
    let escaped_doc_id = escape_graphql_string(request_doc_id);
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    _docID: {{ _eq: "{escaped_doc_id}" }},
                    request_id: {{ _eq: "{escaped_request_id}" }}
                }},
                limit: 2
            ) {{
                _docID request_id agent_did session_id requester_did behavior_id created_at lifecycle_state
            }}
        }}"#
    );
    let response = txn.execute_local_response(&query).await?;
    let rows = crate::graphql::rows::<serde_json::Value>(&response, "AgentRequest")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "session observation refresh found duplicate authoritative request rows"
    );
    let Some(row) = rows.first() else {
        // Missing exact rows remain unknown; never fall back to an older
        // local request.
        return Ok(false);
    };
    let current = gents_protocol::session::SessionRequestObservation {
        request_doc_id: row
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .context("request row omitted _docID")?
            .to_string(),
        request_id: row
            .get("request_id")
            .and_then(serde_json::Value::as_str)
            .context("request row omitted request_id")?
            .to_string(),
        lifecycle_state: serde_json::from_value(
            row.get("lifecycle_state").cloned().unwrap_or_default(),
        )
        .context("decoding request lifecycle_state")?,
    };
    let scope_ok = row.get("agent_did").and_then(serde_json::Value::as_str)
        == Some(session.session.agent_did.as_str())
        && row.get("session_id").and_then(serde_json::Value::as_str)
            == Some(session.session.session_id.as_str())
        && row.get("requester_did").and_then(serde_json::Value::as_str)
            == session.session.requester_did.as_deref()
        && row.get("behavior_id").and_then(serde_json::Value::as_str)
            == Some(session.session.behavior_id.as_str());
    if !scope_ok {
        return Ok(false);
    }
    let activity = touch_activity_value(&session, now);
    let next = gents_protocol::session::SessionObservation {
        last_activity_at: activity,
        preview: observation.preview.clone(),
        latest_request: Some(current),
    };
    patch_session_in_txn(
        txn,
        &session.doc_id,
        serde_json::json!({"observation": next}),
    )
    .await?;
    Ok(true)
}

pub(crate) async fn update_session_title_with_source(
    node: &EmbeddedNode,
    agent_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    title: &str,
    source: gents_protocol::session::SessionTitleSource,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    chrono::DateTime::parse_from_rfc3339(&now).context("invalid title update timestamp")?;
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "session.update_title",
        move |txn| {
            let title = title.to_string();
            let now = now.clone();
            Box::pin(async move {
                apply_title_in_txn(
                    txn,
                    agent_did,
                    requester_did,
                    session_id,
                    Some(&title),
                    source,
                    &now,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        },
    )
    .await
}

/// In-transaction title replacement: generated titles replace only an absent
/// or placeholder title; an explicit user rename (including clear) always applies.
/// Automatic task/placeholder titles never overwrite an existing title. Identity, creation
/// time, closure, provenance, tags, preview and the latest request identity
/// are preserved; activity advances monotonically.
pub async fn apply_title_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    agent_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    title: Option<&str>,
    source: gents_protocol::session::SessionTitleSource,
    now: &str,
) -> Result<()> {
    chrono::DateTime::parse_from_rfc3339(now).context("invalid title update timestamp")?;
    let Some(row) =
        load_agent_session_row_in_txn(txn, agent_did, session_id, requester_did).await?
    else {
        anyhow::bail!("updating title: no AgentSession for session_id={session_id}");
    };
    let session = &row.session;
    let applies = match (session.title.as_ref(), source) {
        (_, gents_protocol::session::SessionTitleSource::User) => true,
        (None, _) => true,
        (
            Some(gents_protocol::session::SessionTitle {
                source: gents_protocol::session::SessionTitleSource::Placeholder,
                ..
            }),
            gents_protocol::session::SessionTitleSource::Generated,
        ) => true,
        _ => false,
    };
    if !applies {
        return Ok(());
    }
    let title = title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|text| gents_protocol::session::SessionTitle {
            text: text.to_string(),
            source,
        });
    patch_session_in_txn(
        txn,
        &row.doc_id,
        serde_json::json!({
            "title": title,
            "observation": touched_observation(&row, now),
        }),
    )
    .await?;
    Ok(())
}

pub(crate) async fn load_recent_titles_for_agent(
    node: &EmbeddedNode,
    agent_did: &str,
    exclude_session_id: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_session_id = escape_graphql_string(exclude_session_id);
    // Activity lives in the observation JSON. Rank the complete owner scope
    // before limiting; a creation-time window can omit recently active sessions.
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    session_id: {{ _ne: "{escaped_session_id}" }}
                }}
            ) {{
                {SESSION_SCOPE_FIELDS}
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading recent titles for agent_did={}: {:?}",
            agent_did,
            resp.errors
        );
    }

    let rows = crate::graphql::rows::<serde_json::Value>(&resp, "AgentSession")?;
    let mut sessions: Vec<gents_protocol::session::AgentSession> = rows
        .iter()
        .map(|row| decode_session_row(row).map(|row| row.session))
        .collect::<Result<_>>()?;
    sessions.sort_by(|a, b| {
        let key = |session: &gents_protocol::session::AgentSession| {
            let time = session
                .observation
                .as_ref()
                .map(|observation| observation.last_activity_at.as_str())
                .unwrap_or(session.created_at.as_str());
            chrono::DateTime::parse_from_rfc3339(time).expect("session decoder validated timestamp")
        };
        key(b)
            .cmp(&key(a))
            .then_with(|| b.session_id.cmp(&a.session_id))
    });
    Ok(sessions
        .into_iter()
        .filter(|session| {
            !matches!(
                session.title.as_ref().map(|title| title.source),
                Some(gents_protocol::session::SessionTitleSource::Placeholder)
            )
        })
        .filter_map(|session| {
            let trimmed = session
                .title
                .as_ref()
                .map(|title| title.text.trim().to_string())
                .unwrap_or_default();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .take(limit)
        .collect())
}

pub(crate) async fn session_needs_generated_title(
    node: &EmbeddedNode,
    agent_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> Result<bool> {
    let Some(session) = load_agent_session(node, agent_did, session_id, requester_did).await?
    else {
        return Ok(false);
    };
    validate_agent_session(&session)?;

    Ok(match session.title.as_ref() {
        None => true,
        Some(title) => {
            title.source == gents_protocol::session::SessionTitleSource::Placeholder
                || title.text.trim().is_empty()
        }
    })
}

pub(crate) fn derive_session_preview(content: &str) -> String {
    truncate_chars(&normalize_conversation_text(content), 240)
}

fn normalize_conversation_text(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod observation_refresh_tests {
    use super::*;
    use crate::config_client::{ConfigAccess, IdempotentTransactionRetry};
    use gents_protocol::request_lifecycle::RequestLifecycleState;

    #[tokio::test]
    async fn request_head_uses_exact_scope_and_timestamp_then_logical_id() {
        let temp = tempfile::tempdir().unwrap();
        let node = defra_node::EmbeddedNode::builder()
            .data_path(temp.path())
            .build()
            .await
            .unwrap();
        crate::ensure_runtime_schemas(&node).await.unwrap();
        ConfigAccess::transact_local_idempotent(
            &node, None, IdempotentTransactionRetry::Standard, "test.session.request_head",
            |txn| Box::pin(async move {
                for (id, agent, requester, time) in [
                    ("a", "did:test:head", None, "2030-01-01T01:00:00+01:00"),
                    ("z", "did:test:head", None, "2030-01-01T00:00:00Z"),
                    ("remote", "did:test:head", Some("did:test:requester"), "2030-01-01T00:00:01Z"),
                    ("foreign", "did:test:foreign", None, "2040-01-01T00:00:00Z"),
                ] {
                    txn.execute_with_variables(
                        "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                        &serde_json::json!({"input": {"request_id": id, "agent_did": agent,
                            "requester_did": requester, "session_id": "head-session", "behavior_id": "head-behavior",
                            "content": "prompt", "created_at": time, "lifecycle_state": "pending"}}),
                    ).await?;
                }
                let local = load_latest_request_in_txn(&txn, "did:test:head", "head-session", Some(None)).await?.unwrap();
                assert_eq!(local.observed.request_id, "z");
                let all = load_latest_request_in_txn(&txn, "did:test:head", "head-session", None).await?.unwrap();
                assert_eq!(all.observed.request_id, "remote");
                let exact = load_latest_request_in_txn(&txn, "did:test:head", "head-session", Some(Some("did:test:requester"))).await?.unwrap();
                assert_eq!(exact, all);
                assert!(load_latest_request_in_txn(&txn, "did:test:head", "head-session", Some(Some("did:test:absent"))).await?.is_none());
                Ok(())
            }),
        ).await.unwrap();
    }

    #[tokio::test]
    async fn refresh_accepts_null_requester_and_rejects_changed_session_scope() {
        let temp = tempfile::tempdir().unwrap();
        let node = defra_node::EmbeddedNode::builder()
            .data_path(temp.path())
            .build()
            .await
            .unwrap();
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let response = node
            .execute(
                r#"mutation {
            create_AgentRequest(input: {
                request_id: "refresh-request", agent_did: "did:test:refresh",
                session_id: "refresh-session", behavior_id: "refresh-behavior",
                content: "prompt", created_at: "2030-01-01T00:00:00Z",
                lifecycle_state: "processing"
            }) { _docID }
        }"#,
            )
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        ConfigAccess::transact_local_idempotent(
            &node, None, IdempotentTransactionRetry::Standard, "test.session.refresh",
            |txn| Box::pin(async move {
                let now = "2030-01-01T00:00:01Z";
                crate::session::ensure_session_in_txn(
                    &txn, "refresh-session", "did:test:refresh", "refresh-behavior",
                    None, None, None, now,
                ).await?;
                let owner = load_agent_session_row_in_txn(
                    &txn, "did:test:refresh", "refresh-session", None,
                ).await?.unwrap();
                let facts = load_scoped_request_facts_in_txn(&txn, &owner.session, false).await?;
                assert_eq!(facts.len(), 1);
                let fact = &facts[0];
                assert!(advance_session_request_observation_in_txn(&txn, fact, "prompt", now).await?);
                let doc_id = escape_graphql_string(&fact.observed.request_doc_id);
                txn.execute_local_response(&format!(r#"mutation {{
                    update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                        input: {{ lifecycle_state: "completed" }}) {{ _docID }}
                }}"#)).await?;
                assert!(refresh_session_request_observation_in_txn(
                    &txn, "did:test:refresh", None, "refresh-session",
                    &fact.observed.request_doc_id, "refresh-request", now,
                ).await?);
                patch_session_in_txn(&txn, &owner.doc_id, serde_json::json!({
                    "closed_at": "2030-01-01T00:00:02Z", "tags": ["review"]
                })).await?;
                assert!(crate::session::reopen_session_in_txn(
                    &txn, "refresh-session", "did:test:refresh", None,
                    "2030-01-01T00:00:03Z",
                ).await?);
                let reopened = load_agent_session_row_in_txn(
                    &txn, "did:test:refresh", "refresh-session", None,
                ).await?.unwrap().session;
                assert!(reopened.closed_at.is_none());
                assert_eq!(reopened.created_at, now);
                assert_eq!(reopened.tags, vec!["review"]);
                assert_eq!(reopened.observation.unwrap().latest_request.unwrap().lifecycle_state,
                    RequestLifecycleState::Completed);
                // A malformed/stale observation must not import state from a
                // different immutable request scope, even when its logical
                // request label matches.
                txn.execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &serde_json::json!({"input": {
                        "request_id": "refresh-request",
                        "agent_did": "did:test:refresh",
                        "session_id": "other-session",
                        "behavior_id": "refresh-behavior",
                        "content": "foreign prompt",
                        "created_at": "2030-01-01T00:00:04Z",
                        "lifecycle_state": "failed"
                    }}),
                ).await?;
                let foreign = txn.execute_local_response(r#"{
                    AgentRequest(filter: {
                        request_id: {_eq: "refresh-request"},
                        session_id: {_eq: "other-session"}
                    }) {_docID}
                }"#).await?;
                let foreign_doc_id = foreign.data.as_ref()
                    .and_then(|data| data.get("AgentRequest"))
                    .and_then(serde_json::Value::as_array)
                    .and_then(|rows| rows.first())
                    .and_then(|row| row.get("_docID"))
                    .and_then(serde_json::Value::as_str)
                    .context("foreign request omitted _docID")?;
                patch_session_in_txn(&txn, &owner.doc_id, serde_json::json!({
                    "observation": {
                        "last_activity_at": "2030-01-01T00:00:03Z",
                        "preview": "prompt",
                        "latest_request": {
                            "request_doc_id": foreign_doc_id,
                            "request_id": "refresh-request",
                            "lifecycle_state": "completed"
                        }
                    }
                })).await?;
                assert!(!refresh_session_request_observation_in_txn(
                    &txn, "did:test:refresh", None, "refresh-session",
                    foreign_doc_id, "refresh-request", now,
                ).await?);
                let session = load_agent_session_row_in_txn(
                    &txn, "did:test:refresh", "refresh-session", None,
                ).await?.unwrap().session;
                assert_eq!(session.observation.unwrap().latest_request.unwrap().lifecycle_state,
                    RequestLifecycleState::Completed);
                Ok(())
            }),
        ).await.unwrap();
    }
}
