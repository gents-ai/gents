use anyhow::{Context, Result};
use gents_protocol::session::{decode_session_row, SessionOwnerRow, SESSION_SCOPE_FIELDS};

use crate::store::graphql::escape_graphql_string;

pub async fn load_agent_session_row_in_txn(
    txn: &super::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<SessionOwnerRow>> {
    Ok(load_agent_session_owner_in_txn(txn, node_did, session_id)
        .await?
        .filter(|row| row.session.requester_did.as_deref() == requester_did))
}

pub async fn load_agent_session_owner_in_txn(
    txn: &super::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<SessionOwnerRow>> {
    Ok(
        match read_agent_session_owner_in_txn(txn, node_did, session_id).await? {
            AgentSessionRowRead::Present(row) => Some(*row),
            AgentSessionRowRead::Absent => None,
        },
    )
}

/// One session-row query's outcome (Lean `SessionObservation.SessionRead`).
/// Document ACP filters unreadable rows without an error, so `Absent` means
/// only that no row is readable by the actor.
#[derive(Debug)]
pub enum AgentSessionRowRead {
    Absent,
    Present(Box<SessionOwnerRow>),
}

pub async fn read_agent_session_owner_in_txn(
    txn: &super::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<AgentSessionRowRead> {
    let scope = format!(
        r#"node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id),
    );
    let query = format!(
        r#"{{
            AgentSession(filter: {{ {scope} }}, limit: 2) {{
                {SESSION_SCOPE_FIELDS}
            }}
        }}"#
    );
    decode_agent_session_read(&txn.execute(&query).await?, session_id)
}

/// Decodes the session-row query's response. DefraDB answers a filtered
/// collection with an empty list, so a response without the row list is a
/// malformed read and fails rather than classify as a state.
pub fn decode_agent_session_read(
    response: &serde_json::Value,
    session_id: &str,
) -> Result<AgentSessionRowRead> {
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentSession"))
        .and_then(serde_json::Value::as_array)
        .context("AgentSession query omitted rows")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "duplicate AgentSession rows for session_id={session_id}"
    );
    Ok(match rows.first() {
        Some(row) => AgentSessionRowRead::Present(Box::new(decode_session_row(row)?)),
        None => AgentSessionRowRead::Absent,
    })
}
