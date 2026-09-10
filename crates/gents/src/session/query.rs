use super::rows::SessionOwnerRow;
use super::*;
use anyhow::Context;

/// Fields for one canonical `AgentSession` read. Nested values (title,
/// provenance, observation) are DefraDB JSON scalar columns: read the bare
/// field name, never an object subselection, and decode through the canonical
/// protocol owners.
pub const AGENT_SESSION_FIELDS: &str = "session_id agent_did requester_did behavior_id \
created_at closed_at title provenance observation tags _docID";

/// Validate one decoded canonical session at the document boundary: required
/// identifiers are present, timestamps are RFC3339, and a
/// present title carries nonblank text.
pub(super) fn validate_agent_session(
    session: &gents_protocol::session::AgentSession,
) -> Result<()> {
    for (name, value) in [
        ("session_id", session.session_id.as_str()),
        ("agent_did", session.agent_did.as_str()),
        ("behavior_id", session.behavior_id.as_str()),
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
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> String {
    let requester = match requester_did {
        Some(did) => format!("\"{}\"", escape_graphql_string(did)),
        None => "null".to_string(),
    };
    format!(
        r#"agent_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }}"#,
        escape_graphql_string(agent_did),
        escape_graphql_string(session_id)
    )
}

/// Load the exact durable session document under its logical label.
/// Duplicate rows for one label are a data error, never a silent pick.
pub(super) async fn load_agent_session_row(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<SessionOwnerRow>> {
    load_agent_session_rows(node, agent_did, session_id, requester_did)
        .await
        .and_then(|mut rows| {
            anyhow::ensure!(rows.len() <= 1, "duplicate AgentSession rows for one label");
            Ok(rows.pop())
        })
}

pub(super) async fn load_agent_session_rows(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<SessionOwnerRow>> {
    let scope = session_scope_filter(agent_did, session_id, requester_did);
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{ {scope} }}
            ) {{ {AGENT_SESSION_FIELDS} }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("loading session session_id={session_id}: {:?}", resp.errors);
    }

    let rows = crate::graphql::rows::<serde_json::Value>(&resp, "AgentSession")?;
    rows.iter().map(decode_session_row).collect()
}

pub(super) async fn load_agent_session(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<gents_protocol::session::AgentSession>> {
    Ok(
        load_agent_session_row(node, agent_did, session_id, requester_did)
            .await?
            .map(|row| row.session),
    )
}

pub(super) async fn require_agent_session(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<gents_protocol::session::AgentSession> {
    load_agent_session(node, agent_did, session_id, requester_did)
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "loading session for completion: no AgentSession for session_id={session_id}"
            )
        })
}

pub(crate) async fn require_session(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    require_agent_session(node, agent_did, session_id, requester_did)
        .await
        .map(|_| ())
}

/// Whether any `AgentResponse` in this session is still streaming.
///
/// Backs the session-scope resolution of the modelled `safeToReduce` gate: a
/// live response means a turn is still being written into this session's
/// transcript, and compaction must not summarize a half-written turn. See
/// `boundary.compaction.safe-to-reduce-session-scope`.
pub(crate) async fn session_has_live_response(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<bool> {
    session_has_other_live_response(node, agent_did, session_id, requester_did, None).await
}

/// At a completion-turn boundary, the current physical request's response is
/// still streaming but its yielded messages are durable. Another live response
/// in the same canonical session scope keeps the reduction gate closed.
pub(crate) async fn session_has_other_live_response(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    current_request_doc_id: Option<&str>,
) -> Result<bool> {
    let scope = session_scope_filter(agent_did, session_id, requester_did);
    let query = format!(
        r#"{{ AgentResponse(
            filter: {{ {scope}, status: {{ _eq: "streaming" }} }}, limit: 2
        ) {{ request_doc_id }} }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading live responses for session_id={session_id}: {:?}",
            resp.errors
        );
    }
    let rows = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(serde_json::Value::as_array)
        .context("live response query omitted rows")?;
    // Two streaming rows cannot both be the one owned response. Keep the gate
    // closed even for malformed duplicate physical bindings; a bounded read
    // must not hide a third, unrelated live response.
    if rows.len() > 1 {
        return Ok(true);
    }
    Ok(rows.iter().any(|row| match current_request_doc_id {
        Some(current) => {
            row.get("request_doc_id")
                .and_then(serde_json::Value::as_str)
                != Some(current)
        }
        None => true,
    }))
}

pub(crate) async fn load_session_behavior_id(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<String>> {
    Ok(
        load_agent_session(node, agent_did, session_id, requester_did)
            .await?
            .map(|session| session.behavior_id),
    )
}

#[cfg(test)]
mod session_decoder_tests {
    use super::*;

    #[test]
    fn requester_absence_is_an_exact_scope_and_labels_are_escaped() {
        let absent = session_scope_filter("owner", "session", None);
        assert!(absent.contains("requester_did: { _eq: null }"));
        let selected = session_scope_filter("owner", "session", Some("requester"));
        assert!(selected.contains(r#"requester_did: { _eq: "requester" }"#));
        assert_ne!(absent, selected);
        let quoted = session_scope_filter("owner\"", "session", None);
        assert!(quoted.contains(r#"owner\""#));
    }

    #[test]
    fn database_identity_is_separate_from_strict_session_payload() {
        let mut row = serde_json::json!({
            "_docID":"physical-session", "session_id":"session", "agent_did":"owner",
            "behavior_id":"behavior", "created_at":"2026-09-09T22:30:00.123456+00:00",
            "observation":{"last_activity_at":"2026-09-09T22:31:00.654321Z"}
        });
        let decoded = decode_session_row(&row).unwrap();
        assert_eq!(decoded.doc_id, "physical-session");
        assert_eq!(
            decoded.session.created_at,
            "2026-09-09T22:30:00.123456+00:00"
        );
        assert_eq!(decoded.session.requester_did, None);
        row["retired_field"] = serde_json::json!(true);
        assert!(decode_session_row(&row).is_err());
        row.as_object_mut().unwrap().remove("retired_field");
        row["created_at"] = serde_json::json!("invalid");
        assert!(decode_session_row(&row).is_err());
    }
}
