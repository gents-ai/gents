use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::session::{AgentSession, SessionArchiveOutcome, SessionArchiveReceipt};
use std::sync::Arc;

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;

const ARCHIVE_REVISION_LIMIT: u64 = i64::MAX as u64;

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Applied,
    Unchanged,
    Conflict,
    Exhausted,
}

fn apply_transition(
    mut session: AgentSession,
    expected_revision: u64,
    archived: bool,
    now: &str,
) -> (Decision, AgentSession) {
    let decision = if session.archived_at.is_some() == archived {
        Decision::Unchanged
    } else if session.archive_revision != expected_revision {
        Decision::Conflict
    } else if session.archive_revision >= ARCHIVE_REVISION_LIMIT {
        Decision::Exhausted
    } else {
        session.archive_revision += 1;
        session.archived_at = archived.then(|| now.to_owned());
        Decision::Applied
    };
    (decision, session)
}

/// Shared presentation metadata uses the caller's native DID/ACP transaction.
/// DefraDB owns read/write authorization and transaction validation; archive
/// scope fields select the row and never confer authority. The revision fences
/// observations in the same native store; cross-node convergence remains owned
/// by DefraDB's branchable document merge rather than a global session lease.
pub async fn set_session_archived(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    actor: &dyn NodeIdentity,
    expected_revision: u64,
    archived: bool,
) -> Result<SessionArchiveReceipt> {
    set_session_archived_on(
        &ConfigAccess::local(Arc::clone(node)),
        node_did,
        session_id,
        actor,
        expected_revision,
        archived,
    )
    .await
}

/// HTTP access uses its endpoint's authenticated DID; embedded access retains
/// the explicitly supplied actor. Scope identifiers confer no authorization.
pub async fn set_session_archived_on(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    actor: &dyn NodeIdentity,
    expected_revision: u64,
    archived: bool,
) -> Result<SessionArchiveReceipt> {
    anyhow::ensure!(
        !node_did.trim().is_empty() && !session_id.trim().is_empty(),
        "session archive requires exact node and session identities"
    );
    anyhow::ensure!(
        expected_revision <= ARCHIVE_REVISION_LIMIT,
        "invalid session archive revision"
    );
    let now = chrono::Utc::now().to_rfc3339();
    match access {
        ConfigAccess::Local(local) => {
            ConfigAccess::transact_local_write(
                local.node(),
                Some(::identity::Did::new(actor.did())?),
                "session.archive",
                |txn| {
                    Box::pin(apply_in_txn(
                        txn,
                        node_did,
                        session_id,
                        expected_revision,
                        archived,
                        &now,
                    ))
                },
            )
            .await
        }
        ConfigAccess::Graphql(_) => {
            access
                .transact_write("session.archive", |txn| {
                    Box::pin(apply_in_txn(
                        txn,
                        node_did,
                        session_id,
                        expected_revision,
                        archived,
                        &now,
                    ))
                })
                .await
        }
    }
}

async fn apply_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    expected_revision: u64,
    archived: bool,
    now: &str,
) -> Result<SessionArchiveReceipt> {
    let filter = format!(
        r#"node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(session_id)
    );
    let rows = txn
        .cursor_rows("AgentSession", super::AGENT_SESSION_FIELDS, &filter)
        .await?;
    anyhow::ensure!(rows.len() <= 1, "session archive scope is ambiguous");
    let row = super::decode_session_row(
        rows.first()
            .context("session archive target is absent or unreadable")?,
    )?;
    super::query::validate_agent_session(&row.session)?;
    let (decision, session) = apply_transition(row.session, expected_revision, archived, now);
    let outcome = match decision {
        Decision::Applied => {
            let archived_at = session
                .archived_at
                .as_ref()
                .map(|value| format!("\"{}\"", escape_graphql_string(value)))
                .unwrap_or_else(|| "null".into());
            let response = txn
                .execute(&format!(
                    r#"mutation {{ update_AgentSession(
            filter: {{ _docID: {{ _eq: "{}" }}, {filter} }},
            input: {{ archived_at: {archived_at}, archive_revision: {} }}
        ) {{ _docID }} }}"#,
                    escape_graphql_string(&row.doc_id),
                    session.archive_revision
                ))
                .await?;
            let documents = crate::graphql::mutation_documents(&response, "update_AgentSession")?
                .context("session archive response omitted its selected document")?;
            anyhow::ensure!(
                documents.len() == 1
                    && documents[0]
                        .get("_docID")
                        .and_then(serde_json::Value::as_str)
                        == Some(row.doc_id.as_str()),
                "session archive did not commit to the selected document"
            );
            SessionArchiveOutcome::Applied
        }
        Decision::Unchanged => SessionArchiveOutcome::Unchanged,
        Decision::Conflict => SessionArchiveOutcome::Conflict,
        Decision::Exhausted => anyhow::bail!("session archive revision is exhausted"),
    };
    Ok(SessionArchiveReceipt {
        node_did: node_did.to_owned(),
        session_id: session_id.to_owned(),
        archived_at: session.archived_at,
        revision: session.archive_revision.to_string(),
        outcome,
    })
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
