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
/// claim recorded (`InterruptGeneration.runningInterrupted`). Every claim
/// records one (`claim_record`); only a pending wake has recorded nothing.
pub fn running_interrupted(recorded: Option<i64>, current: i64) -> bool {
    recorded.is_some_and(|recorded| recorded < current)
}

/// How a request in `state` has settled (`InterruptGeneration.Settlement`).
pub(crate) fn settlement(
    state: Option<gents_protocol::request_lifecycle::RequestLifecycleState>,
) -> Settlement {
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    match state {
        Some(RequestLifecycleState::Interrupted) => Settlement::Interrupted,
        Some(state) if state.is_terminal() => Settlement::Otherwise,
        _ => Settlement::Unsettled,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settlement {
    Unsettled,
    Interrupted,
    Otherwise,
}

/// A wake whose publication recorded an older generation than the marker
/// holds when it is claimed was pending at an interrupt's commit
/// (`InterruptGeneration.wakeAtClaim`).
pub fn wake_drained_at_claim(recorded: Option<i64>, current: i64) -> bool {
    running_interrupted(recorded, current)
}

/// Creates the marker of a session this transaction has just created, as the
/// session's creator, at generation 0, and stages its `parent` relationship to
/// the session in the same transaction, so whoever reads or updates the
/// session reads or updates its marker (`Identity.SessionInterrupt`).
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
    anyhow::ensure!(
        rows.len() <= 1,
        "session has more than one interrupt marker"
    );
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

/// What a reader of a session's marker finds (`InterruptGeneration.MarkerRead`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMarker {
    Found(InterruptMarker),
    NoSession,
    Unreadable,
}

/// A session the caller reads whose marker it cannot read. The marker is
/// created with its session and related to it in that transaction, so this is
/// a broken session: nothing in it can be interrupted, and nothing runs in it.
#[derive(Debug, thiserror::Error)]
#[error("session {session_id} has no interrupt marker this caller can read, so no interrupt could reach its requests and none of them runs; check the session with agent_session_read")]
pub struct UnreadableMarker {
    pub session_id: String,
}

/// Reads the marker of the session `(node_did, session_id)` (unique), whoever
/// requested it: every request of the session, an Agent's auxiliary request in
/// a User's session included, records and compares that session's generation.
pub async fn session_marker_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<SessionMarker> {
    let Some(session) = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
        txn, node_did, session_id,
    )
    .await?
    else {
        return Ok(SessionMarker::NoSession);
    };
    Ok(match read_in_txn(txn, &session.doc_id).await? {
        Some(marker) => SessionMarker::Found(marker),
        None => SessionMarker::Unreadable,
    })
}

/// [`session_marker_in_txn`] for a reader that has nothing to do without a
/// session: `None` when no session is readable, [`UnreadableMarker`] when the
/// session is and its marker is not.
pub async fn read_for_session_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Option<InterruptMarker>> {
    match session_marker_in_txn(txn, node_did, session_id).await? {
        SessionMarker::Found(marker) => Ok(Some(marker)),
        SessionMarker::NoSession => Ok(None),
        SessionMarker::Unreadable => Err(UnreadableMarker {
            session_id: session_id.to_owned(),
        }
        .into()),
    }
}

/// The generation a claim records, or `None` when it refuses the request
/// (`InterruptGeneration.claimRecord`).
pub fn claim_record(read: &SessionMarker, creates_session: bool) -> Option<i64> {
    match read {
        SessionMarker::Found(marker) => Some(marker.generation),
        SessionMarker::NoSession => creates_session.then_some(0),
        SessionMarker::Unreadable => None,
    }
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

#[cfg(test)]
mod tests {
    use super::{claim_record, InterruptMarker, SessionMarker};

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Read {
        Found,
        NoSession,
        Unreadable,
    }

    #[test]
    fn a_claim_records_the_generation_lean_decides() {
        use gents_lean_vocab::families::interrupt_generation_claim_claim as family;
        family::replay::<Read>(|input| {
            let read = match input.read {
                Read::Found => SessionMarker::Found(InterruptMarker {
                    doc_id: "marker".into(),
                    generation: i64::try_from(input.generation).expect("small generation"),
                    interrupted_at: None,
                }),
                Read::NoSession => SessionMarker::NoSession,
                Read::Unreadable => SessionMarker::Unreadable,
            };
            family::Output {
                recorded: claim_record(&read, input.creates_session)
                    .map(|generation| u64::try_from(generation).expect("generations count up")),
            }
        });
    }
}
