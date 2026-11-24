//! A session's interrupt marker (Lean `SessionQueue.InterruptGeneration`,
//! policy `Identity.SessionInterrupt`).

use anyhow::{Context, Result};
use gents_protocol::document_query::{
    fields::agent_session_interrupt as fields, Filter, Query, Selection,
};
use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::escape_graphql_string;

const COLLECTION: &str = gents_protocol::schemas::AGENT_SESSION_INTERRUPT_NAME;

/// The marker as a reader saw it by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptMarker {
    pub doc_id: String,
    pub generation: i64,
    pub interrupted_at: Option<String>,
}

/// The interrupt's only write: one past the generation it read
/// (`InterruptGeneration.bump`).
pub fn bump(current: i64) -> i64 {
    current + 1
}

/// A running request is interrupted once the marker passes the generation its
/// claim recorded (`InterruptGeneration.runningInterrupted`). A request that
/// recorded nothing is never compared.
pub fn running_interrupted(recorded: Option<i64>, current: i64) -> bool {
    recorded.is_some_and(|recorded| recorded < current)
}

/// A wake whose publication recorded an older generation than the marker
/// holds when it is claimed was pending at an interrupt's commit
/// (`InterruptGeneration.wakeAtClaim`).
pub fn wake_drained_at_claim(recorded: Option<i64>, current: i64) -> bool {
    running_interrupted(recorded, current)
}

/// Creates the marker of a session this transaction has just created, as the
/// session's creator, at generation 0, with the session as its parent.
pub async fn create_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session_doc_id: &str,
    node_did: &str,
    session_id: &str,
) -> Result<String> {
    let created = txn
        .execute_client_signed_create(
            "mutation($input: AgentSessionInterruptMutationInputArg!) { create_AgentSessionInterrupt(input: $input) { _docID } }",
            &serde_json::json!({"input": {
                "session_doc_id": session_doc_id, "node_did": node_did,
                "session_id": session_id, "generation": 0,
            }}),
            gents_protocol::schemas::AGENT_SESSION_INTERRUPT,
        )
        .await?;
    let doc_id = gents_store::graphql::created_doc_id(&created, COLLECTION)?;
    txn.add_resource_relationship(COLLECTION, &doc_id, "parent", "sessions", session_doc_id)
        .await?;
    Ok(doc_id)
}

/// Reads the session's marker by id. The marker is found by its session,
/// whose id it never changes, and then read by its own id, so the read is
/// validated at commit (`TransactionFence.pointRead`): a transaction that saw
/// a generation an interrupt has since bumped conflicts and runs again.
/// `None` when the actor reads no marker of the session.
pub async fn read_in_txn(
    txn: &ConfigApplyTxn<'_>,
    session_doc_id: &str,
) -> Result<Option<InterruptMarker>> {
    let located = txn
        .run(
            &Query::select(Selection::doc_id())
                .filter(Filter::eq(fields::SESSION_DOC_ID, session_doc_id))
                .limit(2)
                .render(),
        )
        .await?;
    let rows = located
        .pointer(&format!("/data/{COLLECTION}"))
        .and_then(serde_json::Value::as_array)
        .context("interrupt marker lookup omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "session has more than one interrupt marker");
    let Some(doc_id) = rows.first().and_then(|row| row["_docID"].as_str()) else {
        return Ok(None);
    };
    let read = txn
        .execute(&format!(
            r#"{{ {COLLECTION}(docID: "{}") {{ _docID session_doc_id generation interrupted_at }} }}"#,
            escape_graphql_string(doc_id)
        ))
        .await?;
    let row = read
        .pointer(&format!("/data/{COLLECTION}/0"))
        .context("interrupt marker disappeared between its lookup and its read")?;
    anyhow::ensure!(
        row["session_doc_id"].as_str() == Some(session_doc_id),
        "interrupt marker names another session"
    );
    Ok(Some(InterruptMarker {
        doc_id: doc_id.to_owned(),
        generation: row["generation"]
            .as_i64()
            .context("interrupt marker has no generation")?,
        interrupted_at: row["interrupted_at"].as_str().map(str::to_owned),
    }))
}

/// Reads by id the marker of the session in the exact scope
/// `(node_did, session_id, requester_did)`; a request whose requester is not
/// its session's has no marker to record or compare.
pub async fn read_for_session_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<InterruptMarker>> {
    let Some(session) = gents_store::config_client::agent_session::load_agent_session_row_in_txn(
        txn,
        node_did,
        session_id,
        requester_did,
    )
    .await?
    else {
        return Ok(None);
    };
    read_in_txn(txn, &session.doc_id).await
}

/// Reads by id the marker of the session `(node_did, session_id)`, whatever
/// its requester.
pub async fn read_for_session_owner_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<InterruptMarker>> {
    let Some(session) = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn, node_did, session_id,
    )
    .await?
    else {
        return Ok(None);
    };
    read_in_txn(txn, &session.doc_id).await
}

/// Writes one past `marker.generation`; the marker is the only document the
/// interrupt writes. `None` when the actor's update wrote nothing: document
/// ACP refused it.
pub async fn bump_in_txn(
    txn: &ConfigApplyTxn<'_>,
    marker: &InterruptMarker,
    at: &str,
) -> Result<Option<InterruptMarker>> {
    let generation = bump(marker.generation);
    let response = txn
        .execute_client_signed_update(
            &format!(
                r#"mutation($input: AgentSessionInterruptMutationInputArg!) {{ update_AgentSessionInterrupt(docID: "{}", input: $input) {{ _docID }} }}"#,
                escape_graphql_string(&marker.doc_id)
            ),
            &serde_json::json!({"input": {"generation": generation, "interrupted_at": at}}),
            gents_protocol::schemas::AGENT_SESSION_INTERRUPT,
        )
        .await?;
    let rows = response["data"]["update_AgentSessionInterrupt"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if rows.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        rows.len() == 1 && rows[0]["_docID"] == marker.doc_id.as_str(),
        "interrupt marker update wrote another document"
    );
    Ok(Some(InterruptMarker {
        doc_id: marker.doc_id.clone(),
        generation,
        interrupted_at: Some(at.to_owned()),
    }))
}

