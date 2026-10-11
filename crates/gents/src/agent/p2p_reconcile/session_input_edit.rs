//! Runtime-owned application of signed pending-input commands.
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::session_input_edit::{
    SessionInputEdit, SessionInputEditOutcome, SessionInputEditReceipt, SESSION_INPUT_EDIT_VERSION,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::enrollment_store::GraphqlEnrollmentStore;
use super::templates::{conjunctive_string_eq, decode_pairing_filters};
use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::graphql::escape_graphql_string;
use crate::identity::NodeIdentity;

#[derive(Deserialize)]
struct CommandRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    command_id: String,
    requester_did: String,
    requester_peer_id: Option<String>,
    node_did: String,
    session_id: String,
    intent_json: String,
    receipt_json: Option<String>,
}

const FIELDS: &str = "_docID command_id requester_did requester_peer_id node_did session_id intent_json receipt_json";

async fn verified_receipt(
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
                    &receipt.signer_did,
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

async fn admission_rejection(
    txn: &ConfigApplyTxn<'_>,
    store: &GraphqlEnrollmentStore,
    identity: &dyn NodeIdentity,
    command: &SessionInputEdit,
) -> Result<Option<String>> {
    if let Err(error) = command.validate_shape() {
        return Ok(Some(error.to_string()));
    }
    if command.node_did != identity.did() {
        return Ok(Some("edit targets another node".into()));
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
        return Ok(Some("invalid edit author signature".into()));
    }
    let now = Utc::now();
    if now < DateTime::parse_from_rfc3339(&command.issued_at)?
        || now >= DateTime::parse_from_rfc3339(&command.expires_at)?
    {
        return Ok(Some("edit intent is outside its validity interval".into()));
    }
    let local = command.requester_peer_id.is_none() && command.requester_did == command.node_did;
    let mut expected_admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
            &command.requester_did,
        );
    if !local {
        let Some(peer) = command.requester_peer_id.as_deref() else {
            return Ok(Some("remote edit lacks requester peer identity".into()));
        };
        let projection = store.load_projection_in_txn(txn).await?;
        let Some(fence) = super::enrollment_reconcile::exact_authorization_fence(
            &projection,
            &command.requester_did,
            peer,
        )?
        else {
            return Ok(Some("requester enrollment is not current".into()));
        };
        if fence.owner_node != command.node_did {
            return Ok(Some("enrollment targets another node".into()));
        }
        expected_admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::enrollment(
                &command.requester_did,
                fence.request_id,
                fence.request_digest,
                fence.admin_did,
                fence.authorization_sequence,
                fence.authorization_expires_at,
            );
        for collection in ["PeerPairingDesired", "PeerPairingApplied"] {
            txn.execute(&format!(
                r#"{{ _documentArrivals(collection: "{collection}", limit: 1) {{ head }} }}"#
            ))
            .await?;
        }
        let peer = escape_graphql_string(peer);
        let response = txn.execute(&format!(r#"{{
            PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer}" }}, source: {{ _eq: "enrollment" }} }}) {{ node_did }}
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer}" }} }}) {{ replicator_filter }}
        }}"#)).await?;
        let desired = response["data"]["PeerPairingDesired"]
            .as_array()
            .context("missing desired routes")?;
        let applied = response["data"]["PeerPairingApplied"]
            .as_array()
            .context("missing applied routes")?;
        let route = desired
            .iter()
            .any(|row| row["node_did"].as_str() == Some(command.node_did.as_str()))
            && applied.iter().any(|row| {
                row["replicator_filter"]
                    .as_str()
                    .and_then(|raw| decode_pairing_filters(raw).ok())
                    .and_then(|filters| filters.get("AgentSessionInputEdit").cloned())
                    .is_some_and(|filter| {
                        conjunctive_string_eq(&filter, "requester_did")
                            == Some(command.requester_did.as_str())
                            && conjunctive_string_eq(&filter, "node_did")
                                == Some(command.node_did.as_str())
                    })
            });
        if !route {
            return Ok(Some("requester edit route is not applied".into()));
        }
    }
    for replacement in &command.replacements {
        let mut actual = replacement.admission.clone();
        actual.signature.clear();
        if actual != expected_admission {
            return Ok(Some(
                "replacement admission does not match current author authority".into(),
            ));
        }
    }
    txn.execute(r#"{ _documentArrivals(collection: "AgentSession", limit: 1) { head } }"#)
        .await?;
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

pub(crate) async fn process_command(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    doc_id: &str,
) -> Result<()> {
    let store = GraphqlEnrollmentStore::new(node.clone(), identity.clone());
    ConfigAccess::transact_local_idempotent(node, None, IdempotentTransactionRetry::Standard,
        "queue.apply_signed_edit", |txn| {
        let store = &store;
        Box::pin(async move {
            let response = txn.execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {FIELDS} }} }}"#, escape_graphql_string(doc_id))).await?;
            let rows: Vec<CommandRow> = serde_json::from_value(response["data"]["AgentSessionInputEdit"].clone())?;
            let [row] = rows.as_slice() else { anyhow::bail!("edit command is missing or ambiguous"); };
            let command: SessionInputEdit = serde_json::from_str(&row.intent_json)?;
            txn.execute(r#"{ _documentArrivals(collection: "AgentSessionInputEdit", limit: 1) { head } }"#).await?;
            // Replicated unique-field collisions retain both documents, but an
            // indexed equality lookup exposes only its winning document.
            let duplicates = txn.execute(r#"{ AgentSessionInputEdit { _docID command_id } }"#).await?;
            let duplicates = duplicates["data"]["AgentSessionInputEdit"].as_array().context("missing command identity query")?;
            let mut matching = duplicates.iter().filter(|candidate| candidate["command_id"].as_str() == Some(command.command_id.as_str()));
            anyhow::ensure!(matching.next().is_some_and(|candidate| candidate["_docID"].as_str() == Some(row.doc_id.as_str())) && matching.next().is_none(), "conflicting edit command identity");
            anyhow::ensure!(row.command_id == command.command_id && row.node_did == command.node_did
                && row.requester_did == command.requester_did && row.requester_peer_id == command.requester_peer_id
                && row.session_id == command.session_id && row.node_did == identity.did(), "edit immutable scope disagrees with signed intent");
            if verified_receipt(identity.as_ref(), &command, row.receipt_json.as_deref()).await?.is_some() { return Ok(()); }
            let rejection = admission_rejection(txn, store, identity.as_ref(), &command).await?;
            let (outcome, reason, applied) = if let Some(reason) = rejection {
                (SessionInputEditOutcome::Rejected, reason, None)
            } else {
                match crate::lifecycle::queue::validate_pending_user_edit_in_txn(txn, &command.node_did, &command.session_id,
                    &command.requester_did, &command.edit, &command.replacements).await? {
                    Err(reason) => (SessionInputEditOutcome::Rejected, reason, None),
                    Ok(validated) => {
                        let receipt = crate::lifecycle::queue::apply_pending_user_edit_in_txn(txn, validated).await?;
                        (SessionInputEditOutcome::Applied, String::new(), Some(receipt))
                    }
                }
            };
            let mut receipt = SessionInputEditReceipt { version: SESSION_INPUT_EDIT_VERSION,
                command_id: command.command_id.clone(), command_digest: command.computed_digest(),
                requester_did: command.requester_did.clone(), node_did: command.node_did.clone(), session_id: command.session_id.clone(),
                outcome, reason, request_doc_ids: applied.as_ref().map(|r|r.request_doc_ids.clone()).unwrap_or_default(),
                request_ids: applied.map(|r|r.request_ids).unwrap_or_default(), processed_at: Utc::now().to_rfc3339(),
                signer_did: identity.did().to_owned(), signature: Vec::new() };
            receipt.signature = identity.sign(&receipt.signing_payload()).await?;
            receipt.validate_shape()?;
            let written = txn.execute(&format!(r#"mutation {{ update_AgentSessionInputEdit(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#,
                escape_graphql_string(&row.doc_id), escape_graphql_string(&serde_json::to_string(&receipt)?))).await?;
            anyhow::ensure!(written["data"]["update_AgentSessionInputEdit"].as_array().is_some_and(|rows| rows.len() == 1 && rows[0]["_docID"].as_str() == Some(row.doc_id.as_str())), "edit receipt write lost its command");
            Ok(())
        })
    }).await
}

pub async fn run_session_input_edit_reconciler(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    let collection_id = node
        .get_collection("AgentSessionInputEdit")?
        .context("AgentSessionInputEdit schema missing")?
        .collection_id;
    let mut changes = node.subscribe_document_changes();
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    let mut subscription_open = true;
    loop {
        let response = crate::graphql::graphql_with_transaction_retry(&node,
            &format!(r#"{{ AgentSessionInputEdit(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ _docID }} }}"#, escape_graphql_string(identity.did())),
            "queue.signed_edits").await;
        let rows = match response.and_then(|response| {
            crate::graphql::rows::<serde_json::Value>(&response, "AgentSessionInputEdit")
        }) {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "pending input edit sweep failed");
                Vec::new()
            }
        };
        for row in rows {
            if cancel.is_cancelled() {
                return Ok(());
            }
            if let Some(id) = row["_docID"].as_str() {
                if let Err(error) = process_command(&node, &identity, id).await {
                    tracing::warn!(command_doc_id = id, %error, "pending input edit could not be processed");
                }
            }
        }
        loop {
            let relevant = tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                _ = interval.tick() => true,
                event = changes.recv(), if subscription_open => {
                    match event {
                        Some(event) => event.resync_required || event.changes.iter().any(|change| change.collection_id == collection_id),
                        None => { subscription_open = false; false }
                    }
                }
            };
            if relevant {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests;
