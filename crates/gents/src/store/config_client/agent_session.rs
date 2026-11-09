use anyhow::Result;
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
    match read_agent_session_owner_in_txn(txn, node_did, session_id).await? {
        AgentSessionRowRead::Present(row) => Ok(Some(*row)),
        AgentSessionRowRead::Absent => Ok(None),
        AgentSessionRowRead::Omitted => anyhow::bail!("AgentSession query omitted rows"),
    }
}

/// One session-row query's outcome. Document ACP filters unreadable rows
/// without an error, so `Absent` means only that no row is readable by the
/// actor. `Omitted` is a successful response without the collection's row
/// list, which proves neither (Lean `SessionObservation.SessionRead`).
#[derive(Debug)]
pub enum AgentSessionRowRead {
    Absent,
    Present(Box<SessionOwnerRow>),
    Omitted,
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
    let response = txn.execute(&query).await?;
    let Some(rows) = response
        .get("data")
        .and_then(|data| data.get("AgentSession"))
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(AgentSessionRowRead::Omitted);
    };
    anyhow::ensure!(
        rows.len() <= 1,
        "duplicate AgentSession rows for session_id={session_id}"
    );
    Ok(match rows.first() {
        Some(row) => AgentSessionRowRead::Present(Box::new(decode_session_row(row)?)),
        None => AgentSessionRowRead::Absent,
    })
}
