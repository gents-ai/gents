use super::rows::SessionOwnerRow;
use super::*;

/// Fields for one canonical `AgentSession` read. Nested values are DefraDB
/// JSON scalar columns: read the bare field name, never an object subselection.
pub const AGENT_SESSION_FIELDS: &str = "session_id node_did requester_did agent_id \
created_at closed_at stopped_at title provenance observation tags _docID";

pub(crate) async fn session_stopped_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<bool> {
    let scope = session_scope_filter(node_did, session_id, requester_did);
    let response = txn
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ {scope} }}, limit: 2) {{ stopped_at }} }}"#
        ))
        .await?;
    let rows = response
        .pointer("/data/AgentSession")
        .and_then(serde_json::Value::as_array)
        .ok_or(SessionError::StopLatchRowsOmitted)?;
    if rows.len() > 1 {
        return Err(SessionError::StopLatchAmbiguous);
    }
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    match row
        .get("stopped_at")
        .ok_or(SessionError::StopLatchOmitted)?
    {
        serde_json::Value::Null => Ok(false),
        serde_json::Value::String(value) if !value.trim().is_empty() => Ok(true),
        _ => Err(SessionError::StopLatchMalformed),
    }
}

pub(crate) async fn session_is_stopped(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<bool> {
    crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "session.stop_latch",
        |txn| {
            Box::pin(session_stopped_in_txn(
                txn,
                node_did,
                session_id,
                requester_did,
            ))
        },
    )
    .await
}

fn parse_timestamp(field: &'static str, value: &str) -> Result<(), SessionError> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|source| SessionError::InvalidTimestamp { field, source })
}

pub(super) fn validate_agent_session(
    session: &gents_protocol::session::AgentSession,
) -> Result<(), SessionError> {
    for (name, value) in [
        ("session_id", session.session_id.as_str()),
        ("node_did", session.node_did.as_str()),
        ("agent_id", session.agent_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(SessionError::BlankField { name });
        }
    }
    if let Some(requester) = session.requester_did.as_deref() {
        if requester.trim().is_empty() {
            return Err(SessionError::BlankRequester);
        }
    }
    parse_timestamp("created_at", &session.created_at)?;
    if let Some(closed_at) = session.closed_at.as_deref() {
        parse_timestamp("closed_at", closed_at)?;
    }
    if let Some(stopped_at) = session.stopped_at.as_deref() {
        parse_timestamp("stopped_at", stopped_at)?;
    }
    if let Some(observation) = session.observation.as_ref() {
        parse_timestamp("last_activity_at", &observation.last_activity_at)?;
        if let Some(preview) = observation.preview.as_deref() {
            if preview.chars().count() > 240 {
                return Err(SessionError::PreviewTooLong);
            }
        }
    }
    if let Some(title) = session.title.as_ref() {
        if title.text.trim().is_empty() {
            return Err(SessionError::BlankTitle);
        }
    }
    Ok(())
}

pub fn decode_session_row(row: &serde_json::Value) -> Result<SessionOwnerRow, SessionError> {
    let doc_id = row
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(SessionError::MissingDocId)?
        .to_string();
    let mut document = row.clone();
    document
        .as_object_mut()
        .ok_or(SessionError::RowNotAnObject)?
        .remove("_docID");
    let session: gents_protocol::session::AgentSession =
        serde_json::from_value(document).map_err(SessionError::DecodeRow)?;
    validate_agent_session(&session)?;
    Ok(SessionOwnerRow { doc_id, session })
}

pub fn session_scope_filter(
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> String {
    let requester = match requester_did {
        Some(did) => format!("\"{}\"", escape_graphql_string(did)),
        None => "null".to_string(),
    };
    format!(
        r#"node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id)
    )
}

pub fn public_request_filter(scope: &str) -> String {
    let purpose = format!(
        r#"purpose: {{ _eq: "{}" }}"#,
        gents_protocol::request_admission::RequestPurpose::PUBLIC.as_str()
    );
    if scope.is_empty() {
        purpose
    } else {
        format!("{scope}, {purpose}")
    }
}

pub(super) async fn load_agent_session_row(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<SessionOwnerRow>> {
    let mut rows = load_agent_session_rows(node, node_did, session_id, requester_did).await?;
    if rows.len() > 1 {
        return Err(SessionError::DuplicateAgentSessionRows);
    }
    Ok(rows.pop())
}

pub(super) async fn load_agent_session_rows(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<SessionOwnerRow>> {
    let scope = session_scope_filter(node_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{ {scope} }}
            ) {{ {AGENT_SESSION_FIELDS} }}
        }}"#
    );

    let resp =
        crate::graphql::graphql_with_transaction_retry(node, &query, "load_agent_session_rows")
            .await?;

    let rows = crate::graphql::rows::<serde_json::Value>(&resp, "AgentSession")?;
    rows.iter().map(decode_session_row).collect()
}

pub(super) async fn load_agent_session(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<gents_protocol::session::AgentSession>> {
    Ok(
        load_agent_session_row(node, node_did, session_id, requester_did)
            .await?
            .map(|row| row.session),
    )
}

pub(super) async fn require_agent_session(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<gents_protocol::session::AgentSession> {
    load_agent_session(node, node_did, session_id, requester_did)
        .await?
        .ok_or_else(|| SessionError::AgentSessionMissing {
            session_id: session_id.to_owned(),
        })
}

/// The requester that owns the existing session `(node_did, session_id)`, or
/// `None` when no session exists yet. Runtime deliveries into an existing
/// session adopt it (Lean `Enrollment.runtimeRequesterScope`).
pub(crate) async fn load_session_requester_scope(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
) -> Result<Option<String>> {
    let query = format!(
        r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ {AGENT_SESSION_FIELDS} }} }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id)
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "load session requester scope",
    )
    .await?;
    let rows = crate::graphql::rows::<serde_json::Value>(&response, "AgentSession")?;
    if rows.len() > 1 {
        return Err(SessionError::DuplicateAgentSessionRows);
    }
    Ok(match rows.first() {
        Some(row) => decode_session_row(row)?.session.requester_did,
        None => None,
    })
}

pub(crate) async fn require_session(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    require_agent_session(node, node_did, session_id, requester_did)
        .await
        .map(|_| ())
}

pub(crate) async fn load_session_agent_id(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<String>> {
    Ok(
        load_agent_session(node, node_did, session_id, requester_did)
            .await?
            .map(|session| session.agent_id),
    )
}
