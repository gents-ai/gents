use super::rows::SessionOwnerRow;
use super::*;
use anyhow::Context;

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
        .context("session stop-latch query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "session stop-latch scope is ambiguous");
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    match row
        .get("stopped_at")
        .context("session stop-latch query omitted stopped_at")?
    {
        serde_json::Value::Null => Ok(false),
        serde_json::Value::String(value) if !value.trim().is_empty() => Ok(true),
        _ => anyhow::bail!("session stopped_at is malformed"),
    }
}

pub(crate) async fn session_is_stopped(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<bool> {
    let scope = session_scope_filter(node_did, session_id, requester_did);
    let query = format!(r#"{{ AgentSession(filter: {{ {scope} }}, limit: 2) {{ stopped_at }} }}"#);
    let response =
        crate::graphql::graphql_with_transaction_retry(node, &query, "load session stop latch")
            .await?;
    let rows = crate::graphql::rows::<serde_json::Value>(&response, "AgentSession")?;
    anyhow::ensure!(rows.len() <= 1, "session stop-latch scope is ambiguous");
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    match row
        .get("stopped_at")
        .context("session stop-latch query omitted stopped_at")?
    {
        serde_json::Value::Null => Ok(false),
        serde_json::Value::String(value) if !value.trim().is_empty() => Ok(true),
        _ => anyhow::bail!("session stopped_at is malformed"),
    }
}

pub(super) fn validate_agent_session(
    session: &gents_protocol::session::AgentSession,
) -> Result<()> {
    for (name, value) in [
        ("session_id", session.session_id.as_str()),
        ("node_did", session.node_did.as_str()),
        ("agent_id", session.agent_id.as_str()),
    ] {
        anyhow::ensure!(!value.trim().is_empty(), "session {name} must be nonblank");
    }
    if let Some(requester) = session.requester_did.as_deref() {
        anyhow::ensure!(
            !requester.trim().is_empty(),
            "session requester_did must be nonblank when present"
        );
    }
    chrono::DateTime::parse_from_rfc3339(&session.created_at)
        .context("session created_at is invalid")?;
    if let Some(closed_at) = session.closed_at.as_deref() {
        chrono::DateTime::parse_from_rfc3339(closed_at).context("session closed_at is invalid")?;
    }
    if let Some(stopped_at) = session.stopped_at.as_deref() {
        chrono::DateTime::parse_from_rfc3339(stopped_at)
            .context("session stopped_at is invalid")?;
    }
    if let Some(observation) = session.observation.as_ref() {
        chrono::DateTime::parse_from_rfc3339(&observation.last_activity_at)
            .context("session last_activity_at is invalid")?;
        if let Some(preview) = observation.preview.as_deref() {
            anyhow::ensure!(
                preview.chars().count() <= 240,
                "session preview exceeds the 240-character bound"
            );
        }
    }
    if let Some(title) = session.title.as_ref() {
        anyhow::ensure!(
            !title.text.trim().is_empty(),
            "session title text must be nonblank"
        );
    }
    Ok(())
}

pub fn decode_session_row(row: &serde_json::Value) -> Result<SessionOwnerRow> {
    let doc_id = row
        .get("_docID")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .context("AgentSession row omitted _docID")?
        .to_string();
    let mut document = row.clone();
    document
        .as_object_mut()
        .context("AgentSession row is not an object")?
        .remove("_docID");
    let session: gents_protocol::session::AgentSession =
        serde_json::from_value(document).context("decoding canonical AgentSession")?;
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
    load_agent_session_rows(node, node_did, session_id, requester_did)
        .await
        .and_then(|mut rows| {
            anyhow::ensure!(rows.len() <= 1, "duplicate AgentSession rows for one label");
            Ok(rows.pop())
        })
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
        .ok_or_else(|| {
            anyhow::anyhow!(
                "loading session for completion: no AgentSession for session_id={session_id}"
            )
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
    anyhow::ensure!(rows.len() <= 1, "duplicate AgentSession rows for one label");
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
