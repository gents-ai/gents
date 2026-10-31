use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::session_input_edit::{
    SessionInputEdit, SessionInputEditOutcome, SessionInputEditReceipt, SESSION_INPUT_EDIT_VERSION,
};

use super::enrollment_store::GraphqlEnrollmentStore;
#[cfg(test)]
use super::session_commands::run_session_command_reconciler;
use super::session_commands::{CommandRow, FIELDS};
use crate::store::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;

pub(super) async fn verified_receipt(
    identity: &dyn NodeIdentity,
    command: &SessionInputEdit,
    raw: Option<&str>,
) -> Result<Option<SessionInputEditReceipt>> {
    let Some(receipt) =
        raw.and_then(|raw| serde_json::from_str::<SessionInputEditReceipt>(raw).ok())
    else {
        return Ok(None);
    };
    if receipt.validate_for(command).is_err()
        || !matches!(
            identity
                .verify(
                    &receipt.node_did,
                    &receipt.signing_payload(),
                    &receipt.signature,
                )
                .await,
            Ok(true)
        )
    {
        return Ok(None);
    }
    Ok(Some(receipt))
}

async fn intrinsic_rejection(
    identity: &dyn NodeIdentity,
    command: &SessionInputEdit,
) -> Option<String> {
    if let Err(error) = command.validate_shape() {
        return Some(error.to_string());
    }
    if command.node_did != identity.did() {
        return Some("edit targets another node".into());
    }
    if !matches!(
        identity
            .verify(
                &command.requester_did,
                &command.signing_payload(),
                &command.signature,
            )
            .await,
        Ok(true)
    ) {
        return Some("invalid edit author signature".into());
    }
    None
}

async fn authority_rejection(
    txn: &ConfigApplyTxn<'_>,
    store: &GraphqlEnrollmentStore,
    command: &SessionInputEdit,
) -> Result<Option<String>> {
    let expected_admission = match super::session_command_authority::current_admission(
        txn,
        store,
        super::session_command_authority::AuthorScope {
            requester_did: &command.requester_did,
            requester_peer_id: command.requester_peer_id.as_deref(),
            node_did: &command.node_did,
        },
        "AgentSessionInputEdit",
    )
    .await?
    {
        Ok(admission) => admission,
        Err(reason) => return Ok(Some(reason)),
    };
    for replacement in &command.replacements {
        let mut actual = replacement.admission.clone();
        actual.signature.clear();
        if actual != expected_admission {
            return Ok(Some(
                "replacement admission does not match current author authority".into(),
            ));
        }
    }
    crate::store::graphql::fence_arrivals(txn, "AgentSession").await?;
    let response = txn.execute(&format!(r#"{{ AgentSession(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }} }}) {{ requester_did }} }}"#,
        escape_graphql_string(&command.node_did), escape_graphql_string(&command.session_id))).await?;
    let sessions = response["data"]["AgentSession"]
        .as_array()
        .context("missing session scope")?;
    if sessions.len() != 1
        || sessions[0]["requester_did"].as_str() != Some(command.requester_did.as_str())
    {
        return Ok(Some("session ownership does not match edit author".into()));
    }
    Ok(None)
}

async fn signed_receipt(
    identity: &dyn NodeIdentity,
    command: &SessionInputEdit,
    outcome: SessionInputEditOutcome,
    reason: String,
    request_doc_ids: Vec<String>,
) -> Result<SessionInputEditReceipt> {
    let request_ids = if outcome == SessionInputEditOutcome::Applied {
        command
            .replacements
            .iter()
            .map(|create| create.request_id.clone())
            .collect()
    } else {
        Vec::new()
    };
    let mut receipt = SessionInputEditReceipt {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: command.command_id.clone(),
        command_digest: command.computed_digest(),
        requester_did: command.requester_did.clone(),
        node_did: command.node_did.clone(),
        session_id: command.session_id.clone(),
        outcome,
        reason,
        request_doc_ids,
        request_ids,
        processed_at: Utc::now().to_rfc3339(),
        signature: Vec::new(),
    };
    receipt.signature = identity.sign(&receipt.signing_payload()).await?;
    receipt.validate_shape()?;
    Ok(receipt)
}

/// Applies or answers one command. Returns its `issued_at` when the command is
/// not yet valid on this clock, so the caller can retry it then.
pub(crate) async fn process_command(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    doc_id: &str,
) -> Result<Option<DateTime<Utc>>> {
    let store = GraphqlEnrollmentStore::new(node.clone(), identity.clone());
    ConfigAccess::transact_local_idempotent(node, None, IdempotentTransactionRetry::Standard,
        "queue.apply_signed_edit", |txn| {
        let store = &store;
        Box::pin(async move {
            let response = txn.execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {FIELDS} }} }}"#, escape_graphql_string(doc_id))).await?;
            let rows: Vec<CommandRow> = serde_json::from_value(response["data"]["AgentSessionInputEdit"].clone())?;
            let [row] = rows.as_slice() else { anyhow::bail!("edit command is missing or ambiguous"); };
            if row.node_did != identity.did() {
                return Ok(None);
            }
            let command: SessionInputEdit = serde_json::from_str(&row.intent_json)?;
            anyhow::ensure!(row.command_id == command.command_id && row.node_did == command.node_did
                && row.requester_did == command.requester_did && row.requester_peer_id == command.requester_peer_id
                && row.session_id == command.session_id, "edit immutable scope disagrees with signed intent");
            if verified_receipt(identity.as_ref(), &command, row.receipt_json.as_deref()).await?.is_some() { return Ok(None); }
            crate::store::graphql::fence_arrivals(txn, "AgentSessionInputEdit").await?;
            let twins = txn.execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }} }}) {{ _docID command_id }} }}"#,
                escape_graphql_string(&row.node_did), escape_graphql_string(&row.requester_did))).await?;
            let twins = twins["data"]["AgentSessionInputEdit"].as_array().context("missing command identity query")?;
            if twins.iter().any(|twin| twin["command_id"].as_str() == Some(command.command_id.as_str()) && twin["_docID"].as_str() != Some(row.doc_id.as_str())) {
                tracing::debug!(command_doc_id = doc_id, command_id = %command.command_id, "edit command identity collides; leaving it unanswered");
                return Ok(None);
            }
            let (outcome, reason, request_doc_ids) = match intrinsic_rejection(identity.as_ref(), &command).await {
                Some(reason) => (SessionInputEditOutcome::Rejected, reason, Vec::new()),
                None => match crate::request::lifecycle::queue::persisted_replacements_in_txn(txn, &command.node_did, &command.replacements).await? {
                    crate::request::lifecycle::queue::PersistedReplacements::Exact(ids) => {
                        (SessionInputEditOutcome::Applied, String::new(), ids)
                    }
                    _ => {
                        let now = Utc::now();
                        let issued_at = DateTime::parse_from_rfc3339(&command.issued_at)?.with_timezone(&Utc);
                        if now < issued_at {
                            return Ok(Some(issued_at));
                        }
                        let rejection = if now >= DateTime::parse_from_rfc3339(&command.expires_at)? {
                            Some("edit intent has expired".into())
                        } else {
                            authority_rejection(txn, store, &command).await?
                        };
                        match rejection {
                            Some(reason) => (SessionInputEditOutcome::Rejected, reason, Vec::new()),
                            None => match crate::request::lifecycle::queue::validate_pending_user_edit_in_txn(txn, &command.node_did, &command.session_id,
                                &command.requester_did, &command.edit, &command.replacements).await? {
                                Err(reason) => (SessionInputEditOutcome::Rejected, reason, Vec::new()),
                                Ok(validated) => (SessionInputEditOutcome::Applied, String::new(),
                                    crate::request::lifecycle::queue::apply_pending_user_edit_in_txn(txn, validated).await?.request_doc_ids),
                            },
                        }
                    }
                },
            };
            let receipt = signed_receipt(identity.as_ref(), &command, outcome, reason, request_doc_ids).await?;
            let written = txn.execute(&format!(r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#,
                escape_graphql_string(&row.doc_id), escape_graphql_string(&serde_json::to_string(&receipt)?))).await?;
            anyhow::ensure!(written["data"]["update_AgentSessionInputEdit"].as_array().is_some_and(|rows| rows.len() == 1 && rows[0]["_docID"].as_str() == Some(row.doc_id.as_str())), "edit receipt write lost its command");
            Ok(None)
        })
    }).await
}

#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod tests;
