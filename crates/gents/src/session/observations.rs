use super::query::{decode_session_row, load_agent_session, validate_agent_session};
use super::sessions::{
    load_agent_session_row_in_txn, patch_session_in_txn, touch_activity_value, touched_observation,
    SESSION_SCOPE_FIELDS,
};
use super::*;
use anyhow::Context;
use std::collections::BTreeMap;

const REQUEST_FACT_SESSION_BATCH_SIZE: usize =
    crate::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRequestFact {
    pub node_did: String,
    pub session_id: String,
    /// Exact requester scope; `None` is its own scope, not a wildcard.
    pub requester_did: Option<String>,
    pub agent_id: String,
    pub created_at: String,
    pub hop: u32,
    pub observed: gents_protocol::session::SessionRequestObservation,
}

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
        rows.contains(incoming),
        "incoming request row is missing from the scoped request facts"
    );
    Ok(rows
        .iter()
        .all(|row| *row == *incoming || !request_fact_newer(row, incoming)))
}

fn scoped_request_filter(
    node_did: &str,
    session_id: &str,
    requester_scope: Option<Option<&str>>,
) -> String {
    let node_did = escape_graphql_string(node_did);
    let session_id = escape_graphql_string(session_id);
    let requester_filter = requester_filter(requester_scope);
    format!(
        r#"filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _eq: "{session_id}" }}, purpose: {{ _eq: "normal" }}{requester_filter} }}"#
    )
}

fn scoped_request_sessions_filter(
    node_did: &str,
    session_ids: &[String],
    requester_scope: Option<Option<&str>>,
) -> String {
    let node_did = escape_graphql_string(node_did);
    let session_ids =
        crate::graphql::graphql_string_list_literal(session_ids.iter().map(String::as_str));
    let requester_filter = requester_filter(requester_scope);
    format!(
        r#"filter: {{ node_did: {{ _eq: "{node_did}" }}, session_id: {{ _in: {session_ids} }}, purpose: {{ _eq: "normal" }}{requester_filter} }}"#
    )
}

fn requester_filter(requester_scope: Option<Option<&str>>) -> String {
    let requester_filter = if requester_scope.is_none() {
        String::new()
    } else if let Some(Some(requester_did)) = requester_scope {
        format!(
            r#", requester_did: {{ _eq: "{}" }}"#,
            escape_graphql_string(requester_did)
        )
    } else {
        r#", requester_did: { _eq: null }"#.to_string()
    };
    requester_filter
}

pub(crate) async fn load_scoped_request_facts_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session: &gents_protocol::session::AgentSession,
    all_requesters: bool,
) -> Result<Vec<SessionRequestFact>> {
    load_request_facts_in_txn(
        txn,
        &session.node_did,
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
    node_did: &str,
    session_id: &str,
    requester_scope: Option<Option<&str>>,
) -> Result<Vec<SessionRequestFact>> {
    let query = request_fact_query(&scoped_request_filter(
        node_did,
        session_id,
        requester_scope,
    ));
    let response = txn.execute(&query).await?;
    decode_request_facts(&response)
}

fn request_fact_query(filter: &str) -> String {
    format!(
        r#"{{
            AgentRequest({filter}) {{
                _docID request_id node_did session_id requester_did agent_id created_at lifecycle_state
                request_hop
            }}
        }}"#
    )
}

fn decode_request_facts(response: &serde_json::Value) -> Result<Vec<SessionRequestFact>> {
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .context("request facts query omitted AgentRequest rows")?
        .as_array()
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
                node_did: row
                    .get("node_did")
                    .and_then(serde_json::Value::as_str)
                    .context("request row omitted node_did")?
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
                agent_id: row
                    .get("agent_id")
                    .and_then(serde_json::Value::as_str)
                    .context("request row omitted agent_id")?
                    .to_string(),
                created_at: row
                    .get("created_at")
                    .and_then(serde_json::Value::as_str)
                    .context("request row omitted created_at")?
                    .to_string(),
                hop: row
                    .get("request_hop")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|hop| u32::try_from(hop).ok())
                    .unwrap_or(0),
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

/// Lean `CausalHop.sessionCurrentHop`: the hop of the session's latest
/// request under the canonical order. `created_at` has whole-second
/// precision, so the requests sharing the latest second have no order between
/// them; the current hop is the highest hop among them, which errs toward
/// refusing a continuation, never toward running one below a refusal.
pub(crate) fn session_current_hop(facts: &[SessionRequestFact]) -> u32 {
    let at =
        |fact: &SessionRequestFact| chrono::DateTime::parse_from_rfc3339(&fact.created_at).ok();
    let Some(latest) = facts.iter().map(at).max() else {
        return 0;
    };
    facts
        .iter()
        .filter(|fact| at(fact) == latest)
        .map(|fact| fact.hop)
        .max()
        .unwrap_or(0)
}

pub(crate) async fn load_session_current_hop_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<u32> {
    Ok(session_current_hop(
        &load_request_facts_in_txn(txn, node_did, session_id, None).await?,
    ))
}

pub(crate) async fn load_session_current_hop(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
) -> Result<u32> {
    let (node_did, session_id) = (node_did.to_owned(), session_id.to_owned());
    crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "session.current_hop",
        |txn| {
            let (node_did, session_id) = (node_did.clone(), session_id.clone());
            Box::pin(
                async move { load_session_current_hop_in_txn(txn, &node_did, &session_id).await },
            )
        },
    )
    .await
}

pub async fn load_latest_request_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_scope: Option<Option<&str>>,
) -> Result<Option<SessionRequestFact>> {
    let rows = load_request_facts_in_txn(txn, node_did, session_id, requester_scope).await?;
    latest_request_fact(rows)
}

pub(crate) async fn load_latest_requests_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_ids: &[String],
    requester_scope: Option<Option<&str>>,
) -> Result<BTreeMap<String, SessionRequestFact>> {
    let mut latest_by_session = BTreeMap::new();
    for batch in session_ids.chunks(REQUEST_FACT_SESSION_BATCH_SIZE) {
        let query = request_fact_query(&scoped_request_sessions_filter(
            node_did,
            batch,
            requester_scope,
        ));
        #[cfg(test)]
        record_request_fact_batch_query();
        let rows = decode_request_facts(&txn.execute(&query).await?)?;
        let mut rows_by_session = BTreeMap::<String, Vec<SessionRequestFact>>::new();
        for row in rows {
            rows_by_session
                .entry(row.session_id.clone())
                .or_default()
                .push(row);
        }
        for (session_id, rows) in rows_by_session {
            if let Some(latest) = latest_request_fact(rows)? {
                latest_by_session.insert(session_id, latest);
            }
        }
    }
    Ok(latest_by_session)
}

fn latest_request_fact(rows: Vec<SessionRequestFact>) -> Result<Option<SessionRequestFact>> {
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

#[cfg(test)]
tokio::task_local! {
    static REQUEST_FACT_BATCH_QUERIES: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

#[cfg(test)]
fn record_request_fact_batch_query() {
    let _ = REQUEST_FACT_BATCH_QUERIES
        .try_with(|queries| queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
}

#[cfg(test)]
async fn count_request_fact_batch_queries<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    let queries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = REQUEST_FACT_BATCH_QUERIES
        .scope(queries.clone(), future)
        .await;
    (output, queries.load(std::sync::atomic::Ordering::Relaxed))
}

fn validate_incoming_scope(
    session: &gents_protocol::session::AgentSession,
    incoming: &SessionRequestFact,
) -> Result<()> {
    anyhow::ensure!(
        incoming.node_did == session.node_did
            && incoming.session_id == session.session_id
            && incoming.agent_id == session.agent_id
            && incoming.requester_did == session.requester_did,
        "incoming request does not match the session owner/session/requester/agent scope"
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

pub(crate) async fn advance_session_request_observation_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    incoming: &SessionRequestFact,
    preview_content: &str,
    now: &str,
) -> Result<bool> {
    advance_observation_in_txn(
        txn,
        incoming,
        preview_content,
        now,
        ObservationBoundary::Claim,
    )
    .await
}

/// Event target for the refusal projection's skip observations. A scoped test
/// subscriber reads these back, so the target stays in
/// `test_support::CAPTURED_EVENT_TARGETS`.
pub(crate) const REFUSAL_PROJECTION_EVENT_TARGET: &str = "gents::refusal_projection";

/// Refusal-boundary variant: a row this owner cannot read — an invalid
/// scoped request fact, duplicate `request_id` roots, an incoming row missing
/// from its session scope — warns and leaves the observation untouched
/// instead of erroring, so the refusal sharing this transaction persists.
/// Write failures still propagate; they roll the refusal back with every
/// other mutation it owns.
pub(crate) async fn advance_refused_request_observation_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    incoming: &SessionRequestFact,
    preview_content: &str,
    now: &str,
) -> Result<bool> {
    advance_observation_in_txn(
        txn,
        incoming,
        preview_content,
        now,
        ObservationBoundary::Refusal,
    )
    .await
}

#[derive(Clone, Copy)]
enum ObservationBoundary {
    Claim,
    Refusal,
}

async fn advance_observation_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    incoming: &SessionRequestFact,
    preview_content: &str,
    now: &str,
    boundary: ObservationBoundary,
) -> Result<bool> {
    let read = async {
        let session = load_agent_session_row_in_txn(
            txn,
            &incoming.node_did,
            &incoming.session_id,
            incoming.requester_did.as_deref(),
        )
        .await?
        .context("advancing observation: session disappeared")?;
        validate_agent_session(&session.session)?;
        validate_incoming_scope(&session.session, incoming)?;
        let rows = load_scoped_request_facts_in_txn(txn, &session.session, false).await?;
        let canonical = incoming_is_canonical_latest(&session.session, &rows, incoming)?;
        Ok((session, canonical))
    }
    .await;
    let (session, canonical) = match read {
        Ok(read) => read,
        Err(error) if matches!(boundary, ObservationBoundary::Refusal) => {
            tracing::warn!(
                target: REFUSAL_PROJECTION_EVENT_TARGET,
                session_id = %incoming.session_id,
                request_id = %incoming.observed.request_id,
                error = %error,
                "refused request leaves its session observation untouched; the refusal itself \
                 persists in this transaction"
            );
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    if !canonical {
        return Ok(false);
    }
    let activity = touch_activity_value(&session, now);
    let observation = gents_protocol::session::SessionObservation {
        reply_preview: None,
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

/// A terminal boundary supplies `Some`, including `Some(None)` for no readable
/// final text. Ordinary activity supplies `None` and preserves the last reply.
pub(crate) async fn refresh_session_request_observation_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    request_doc_id: &str,
    request_id: &str,
    reply_preview: Option<Option<&str>>,
    now: &str,
) -> Result<bool> {
    let Some(session) =
        load_agent_session_row_in_txn(txn, node_did, session_id, requester_did).await?
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
                    request_id: {{ _eq: "{escaped_request_id}" }},
                    purpose: {{ _eq: "normal" }}
                }},
                limit: 2
            ) {{
                _docID request_id node_did session_id requester_did agent_id created_at lifecycle_state
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
    let scope_ok = row.get("node_did").and_then(serde_json::Value::as_str)
        == Some(session.session.node_did.as_str())
        && row.get("session_id").and_then(serde_json::Value::as_str)
            == Some(session.session.session_id.as_str())
        && row.get("requester_did").and_then(serde_json::Value::as_str)
            == session.session.requester_did.as_deref()
        && row.get("agent_id").and_then(serde_json::Value::as_str)
            == Some(session.session.agent_id.as_str());
    if !scope_ok {
        return Ok(false);
    }
    let activity = touch_activity_value(&session, now);
    let next = gents_protocol::session::SessionObservation {
        reply_preview: reply_preview.map_or_else(
            || observation.reply_preview.clone(),
            |reply| super::preview::observe_reply_preview(observation, &current, reply),
        ),
        last_activity_at: activity,
        preview: observation.preview.clone(),
        latest_request: Some(current),
    };
    txn.contend_with_request_siblings();
    patch_session_in_txn(
        txn,
        &session.doc_id,
        serde_json::json!({"observation": next}),
    )
    .await?;
    Ok(true)
}

pub(crate) async fn update_session_title_with_source(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    title: &str,
    source: gents_protocol::session::SessionTitleSource,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    chrono::DateTime::parse_from_rfc3339(&now).context("invalid title update timestamp")?;
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "session.update_title",
        move |txn| {
            let title = title.to_string();
            let now = now.clone();
            Box::pin(async move {
                apply_title_in_txn(
                    txn,
                    node_did,
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

pub async fn apply_title_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    title: Option<&str>,
    source: gents_protocol::session::SessionTitleSource,
    now: &str,
) -> Result<()> {
    chrono::DateTime::parse_from_rfc3339(now).context("invalid title update timestamp")?;
    let Some(row) = load_agent_session_row_in_txn(txn, node_did, session_id, requester_did).await?
    else {
        anyhow::bail!("updating title: no AgentSession for session_id={session_id}");
    };
    let session = &row.session;
    let applies = matches!(
        (session.title.as_ref(), source),
        (_, gents_protocol::session::SessionTitleSource::User)
            | (None, _)
            | (
                Some(gents_protocol::session::SessionTitle {
                    source: gents_protocol::session::SessionTitleSource::Placeholder,
                    ..
                }),
                gents_protocol::session::SessionTitleSource::Generated,
            )
    );
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
    node_did: &str,
    exclude_session_id: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let escaped_node_did = escape_graphql_string(node_did);
    let escaped_session_id = escape_graphql_string(exclude_session_id);
    // Activity lives in the observation JSON. Rank the complete owner scope
    // before limiting; a creation-time window can omit recently active sessions.
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{
                    node_did: {{ _eq: "{escaped_node_did}" }},
                    session_id: {{ _ne: "{escaped_session_id}" }}
                }}
            ) {{
                {SESSION_SCOPE_FIELDS}
            }}
        }}"#
    );

    let resp = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "load_recent_titles_for_agent",
    )
    .await?;

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
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> Result<bool> {
    let Some(session) = load_agent_session(node, node_did, session_id, requester_did).await? else {
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
        let node = std::sync::Arc::new(
            crate::test_support::node_builder()
                .data_path(temp.path())
                .build()
                .await
                .unwrap(),
        );
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
                        &serde_json::json!({"input": {"request_id": id, "purpose": "normal", "node_did": agent,
                            "requester_did": requester, "session_id": "head-session", "agent_id": "head-agent",
                            "content": "prompt", "created_at": time, "lifecycle_state": "pending"}}),
                    ).await?;
                }
                let local = load_latest_request_in_txn(txn, "did:test:head", "head-session", Some(None)).await?.unwrap();
                assert_eq!(local.observed.request_id, "z");
                let all = load_latest_request_in_txn(txn, "did:test:head", "head-session", None).await?.unwrap();
                assert_eq!(all.observed.request_id, "remote");
                let exact = load_latest_request_in_txn(txn, "did:test:head", "head-session", Some(Some("did:test:requester"))).await?.unwrap();
                assert_eq!(exact, all);
                assert!(load_latest_request_in_txn(txn, "did:test:head", "head-session", Some(Some("did:test:absent"))).await?.is_none());
                Ok(())
            }),
        ).await.unwrap();
    }

    #[tokio::test]
    async fn batched_request_heads_preserve_scope_order_and_bound_query_count() {
        let temp = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            crate::test_support::node_builder()
                .data_path(temp.path())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let session_ids = (0..REQUEST_FACT_SESSION_BATCH_SIZE + 1)
            .map(|index| format!("batch-session-{index:02}"))
            .collect::<Vec<_>>();

        ConfigAccess::transact_local_idempotent(
            &node,
            None,
            IdempotentTransactionRetry::Standard,
            "test.session.batched_request_heads",
            |txn| {
                let session_ids = session_ids.clone();
                Box::pin(async move {
                    for (index, session_id) in session_ids.iter().enumerate() {
                        let request_id = format!("request-{index:02}");
                        txn.execute_with_variables(
                            "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                            &serde_json::json!({"input": {
                                "request_id": request_id,
                                "purpose": "normal",
                                "node_did": "did:test:batch-heads",
                                "session_id": session_id,
                                "agent_id": "batch-agent",
                                "content": "prompt",
                                "created_at": "2030-01-01T00:00:00Z",
                                "lifecycle_state": "pending"
                            }}),
                        ).await?;
                    }
                    for (request_id, purpose, node_did, requester_did, created_at) in [
                        ("a-tie", "normal", "did:test:batch-heads", None, "2030-01-01T01:00:00+01:00"),
                        ("z-tie", "normal", "did:test:batch-heads", None, "2030-01-01T00:00:00Z"),
                        ("remote-head", "normal", "did:test:batch-heads", Some("did:test:requester"), "2030-01-01T00:00:01Z"),
                        ("foreign-node", "normal", "did:test:other-node", None, "2040-01-01T00:00:00Z"),
                        ("title-audit", "title-audit", "did:test:batch-heads", None, "2050-01-01T00:00:00Z"),
                    ] {
                        txn.execute_with_variables(
                            "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                            &serde_json::json!({"input": {
                                "request_id": request_id,
                                "purpose": purpose,
                                "node_did": node_did,
                                "requester_did": requester_did,
                                "session_id": session_ids[0],
                                "agent_id": "batch-agent",
                                "content": "prompt",
                                "created_at": created_at,
                                "lifecycle_state": "pending"
                            }}),
                        ).await?;
                    }

                    let (heads, batch_queries) = count_request_fact_batch_queries(
                        load_latest_requests_in_txn(
                            txn,
                            "did:test:batch-heads",
                            &session_ids,
                            None,
                        ),
                    )
                    .await;
                    let heads = heads?;
                    assert_eq!(batch_queries, 2);
                    assert_eq!(heads.len(), session_ids.len());
                    assert_eq!(heads[&session_ids[0]].observed.request_id, "remote-head");
                    assert_eq!(heads[&session_ids[1]].observed.request_id, "request-01");

                    let absent_requester = load_latest_requests_in_txn(
                        txn,
                        "did:test:batch-heads",
                        &session_ids[..1],
                        Some(None),
                    )
                    .await?;
                    assert_eq!(absent_requester[&session_ids[0]].observed.request_id, "z-tie");
                    let exact_requester = load_latest_requests_in_txn(
                        txn,
                        "did:test:batch-heads",
                        &session_ids[..1],
                        Some(Some("did:test:requester")),
                    )
                    .await?;
                    assert_eq!(exact_requester[&session_ids[0]].observed.request_id, "remote-head");
                    Ok(())
                })
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn refresh_accepts_null_requester_and_rejects_changed_session_scope() {
        let temp = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            crate::test_support::node_builder()
                .data_path(temp.path())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let response = node
            .execute(
                r#"mutation {
            create_AgentRequest(input: {
                request_id: "refresh-request", purpose: "normal", node_did: "did:test:refresh",
                session_id: "refresh-session", agent_id: "refresh-agent",
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
                    txn, "refresh-session", "did:test:refresh", "refresh-agent",
                    None, None, None, now,
                ).await?;
                let owner = load_agent_session_row_in_txn(
                    txn, "did:test:refresh", "refresh-session", None,
                ).await?.unwrap();
                let facts = load_scoped_request_facts_in_txn(txn, &owner.session, false).await?;
                assert_eq!(facts.len(), 1);
                let fact = &facts[0];
                assert!(advance_session_request_observation_in_txn(txn, fact, "prompt", now).await?);
                let doc_id = escape_graphql_string(&fact.observed.request_doc_id);
                txn.execute_local_response(&format!(r#"mutation {{
                    update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                        input: {{ lifecycle_state: "completed" }}) {{ _docID }}
                }}"#)).await?;
                assert!(refresh_session_request_observation_in_txn(
                    txn, "did:test:refresh", None, "refresh-session",
                    &fact.observed.request_doc_id, "refresh-request", Some(Some("Finished")), now,
                ).await?);
                patch_session_in_txn(txn, &owner.doc_id, serde_json::json!({
                    "closed_at": "2030-01-01T00:00:02Z", "tags": ["review"]
                })).await?;
                assert!(crate::session::reopen_session_in_txn(
                    txn, "refresh-session", "did:test:refresh", None,
                    "2030-01-01T00:00:03Z",
                ).await?);
                let reopened = load_agent_session_row_in_txn(
                    txn, "did:test:refresh", "refresh-session", None,
                ).await?.unwrap().session;
                assert!(reopened.closed_at.is_none());
                assert_eq!(reopened.created_at, now);
                assert_eq!(reopened.tags, vec!["review"]);
                assert_eq!(reopened.observation.as_ref().unwrap().reply_preview.as_deref(), Some("Finished"));
                assert_eq!(reopened.observation.unwrap().latest_request.unwrap().lifecycle_state,
                    RequestLifecycleState::Completed);
                // A malformed/stale observation must not import state from a
                // different immutable request scope, even when its logical
                // request label matches.
                txn.execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &serde_json::json!({"input": {
                        "request_id": "refresh-request",
                        "purpose": "normal",
                        "node_did": "did:test:refresh",
                        "session_id": "other-session",
                        "agent_id": "refresh-agent",
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
                patch_session_in_txn(txn, &owner.doc_id, serde_json::json!({
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
                    txn, "did:test:refresh", None, "refresh-session",
                    foreign_doc_id, "refresh-request", Some(Some("Wrong scope")), now,
                ).await?);
                let session = load_agent_session_row_in_txn(
                    txn, "did:test:refresh", "refresh-session", None,
                ).await?.unwrap().session;
                assert_eq!(session.observation.as_ref().unwrap().reply_preview.as_deref(), Some("Finished"));
                assert_eq!(session.observation.unwrap().latest_request.unwrap().lifecycle_state,
                    RequestLifecycleState::Completed);
                Ok(())
            }),
        ).await.unwrap();
    }
}

#[cfg(test)]
mod session_hop_tests {
    use super::{session_current_hop, SessionRequestFact};

    fn fact(request_id: &str, created_at: &str, hop: u32) -> SessionRequestFact {
        SessionRequestFact {
            node_did: "did:test:agent".into(),
            session_id: "session".into(),
            requester_did: None,
            agent_id: "general".into(),
            created_at: created_at.into(),
            hop,
            observed: gents_protocol::session::SessionRequestObservation {
                request_doc_id: format!("doc-{request_id}"),
                request_id: request_id.into(),
                lifecycle_state: gents_protocol::request_lifecycle::RequestLifecycleState::Pending,
            },
        }
    }

    /// Lean `CausalHop.same_second_tie_takes_the_highest_hop`: a refused
    /// over-bound wake and a native wake written in the same second read as
    /// the refused hop, whatever their request ids.
    #[test]
    fn a_same_second_tie_takes_the_highest_hop() {
        let facts = [
            fact("parent", "2026-09-27T05:12:04Z", 0),
            fact("a-refused-wake", "2026-09-27T05:12:05Z", 9),
            fact("z-native-wake", "2026-09-27T05:12:05Z", 3),
        ];
        assert_eq!(session_current_hop(&facts), 9);
        let reversed = [
            fact("parent", "2026-09-27T05:12:04Z", 0),
            fact("z-refused-wake", "2026-09-27T05:12:05Z", 9),
            fact("a-native-wake", "2026-09-27T05:12:05Z", 3),
        ];
        assert_eq!(session_current_hop(&reversed), 9);
        // An earlier second never counts, however high its hop.
        let later_root = [
            fact("refused-wake", "2026-09-27T05:12:04Z", 9),
            fact("user-root", "2026-09-27T05:12:05Z", 0),
        ];
        assert_eq!(session_current_hop(&later_root), 0);
        assert_eq!(session_current_hop(&[]), 0);
    }
}
