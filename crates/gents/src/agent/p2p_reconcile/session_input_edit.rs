//! Runtime-owned application of signed pending-input commands.
use std::collections::HashMap;
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

/// Checks that hold for the command's lifetime, independent of clock and authority.
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
            crate::graphql::fence_arrivals(txn, collection).await?;
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
    crate::graphql::fence_arrivals(txn, "AgentSession").await?;
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
            crate::graphql::fence_arrivals(txn, "AgentSessionInputEdit").await?;
            // Replicated unique-field collisions retain both documents, but an
            // equality lookup on the unique field exposes only its winner; the
            // non-unique scope index exposes every physical twin.
            let twins = txn.execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }} }}) {{ _docID command_id }} }}"#,
                escape_graphql_string(&row.node_did), escape_graphql_string(&row.requester_did))).await?;
            let twins = twins["data"]["AgentSessionInputEdit"].as_array().context("missing command identity query")?;
            if twins.iter().any(|twin| twin["command_id"].as_str() == Some(command.command_id.as_str()) && twin["_docID"].as_str() != Some(row.doc_id.as_str())) {
                tracing::debug!(command_doc_id = doc_id, command_id = %command.command_id, "edit command identity collides; leaving it unanswered");
                return Ok(None);
            }
            let (outcome, reason, request_doc_ids) = match intrinsic_rejection(identity.as_ref(), &command).await {
                Some(reason) => (SessionInputEditOutcome::Rejected, reason, Vec::new()),
                None => match crate::lifecycle::queue::persisted_replacements_in_txn(txn, &command.node_did, &command.replacements).await? {
                    crate::lifecycle::queue::PersistedReplacements::Exact(ids) => {
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
                            None => match crate::lifecycle::queue::validate_pending_user_edit_in_txn(txn, &command.node_did, &command.session_id,
                                &command.requester_did, &command.edit, &command.replacements).await? {
                                Err(reason) => (SessionInputEditOutcome::Rejected, reason, Vec::new()),
                                Ok(validated) => (SessionInputEditOutcome::Applied, String::new(),
                                    crate::lifecycle::queue::apply_pending_user_edit_in_txn(txn, validated).await?.request_doc_ids),
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

#[derive(Clone, Copy)]
enum Sweep {
    /// Commands without any receipt; cheap enough for every tick.
    Unanswered,
    /// Also commands whose receipt does not verify. A replica can overwrite a
    /// valid receipt, and only its change notification names that command, so
    /// this runs whenever notifications may have been lost: at startup, on
    /// resync, and on every tick once the subscription has closed.
    Unverified,
}

async fn unanswered_commands(
    node: &EmbeddedNode,
    identity: &dyn NodeIdentity,
    sweep: Sweep,
) -> Result<Vec<String>> {
    let receipt_filter = match sweep {
        Sweep::Unanswered => ", receipt_json: { _eq: null }",
        Sweep::Unverified => "",
    };
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentSessionInputEdit(filter: {{ node_did: {{ _eq: "{}" }}{receipt_filter} }}) {{ _docID intent_json receipt_json }} }}"#,
            escape_graphql_string(identity.did())
        ),
        "queue.signed_edits",
    )
    .await?;
    #[derive(Deserialize)]
    struct Row {
        #[serde(rename = "_docID")]
        doc_id: String,
        intent_json: String,
        receipt_json: Option<String>,
    }
    let mut unanswered = Vec::new();
    for row in crate::graphql::rows::<Row>(&response, "AgentSessionInputEdit")? {
        let answered = match serde_json::from_str::<SessionInputEdit>(&row.intent_json) {
            Ok(command) => verified_receipt(identity, &command, row.receipt_json.as_deref())
                .await?
                .is_some(),
            Err(_) => row.receipt_json.is_some(),
        };
        if !answered {
            unanswered.push(row.doc_id);
        }
    }
    Ok(unanswered)
}

/// Sweeps unanswered commands, processes each changed command, and retries each
/// command deferred until its issuance at that instant.
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
    let mut deferred: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut ids = Vec::new();
    // Discovered commands and full sweeps that failed are owed to the next
    // tick: unanswered-only ticks cannot rediscover a command whose invalid
    // receipt is non-null.
    let mut failed = Vec::new();
    let mut full_sweep_owed = false;
    let mut sweep = Some(Sweep::Unverified);
    loop {
        if let Some(sweep) = sweep {
            match unanswered_commands(&node, identity.as_ref(), sweep).await {
                Ok(unanswered) => {
                    full_sweep_owed &= matches!(sweep, Sweep::Unanswered);
                    ids.extend(unanswered);
                }
                Err(error) => {
                    full_sweep_owed |= matches!(sweep, Sweep::Unverified);
                    tracing::warn!(%error, "pending input edit sweep failed");
                }
            }
        }
        ids.sort();
        ids.dedup();
        for id in ids.drain(..) {
            if cancel.is_cancelled() {
                return Ok(());
            }
            match process_command(&node, &identity, &id).await {
                Ok(Some(issued_at)) => {
                    deferred.insert(id, issued_at);
                }
                Ok(None) => {
                    deferred.remove(&id);
                }
                Err(error) => {
                    deferred.remove(&id);
                    tracing::warn!(command_doc_id = id, %error, "pending input edit could not be processed");
                    failed.push(id);
                }
            }
        }
        let retry = deferred
            .values()
            .min()
            .map(|at| (*at - Utc::now()).to_std().unwrap_or_default());
        sweep = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                ids.append(&mut failed);
                Some(if subscription_open && !full_sweep_owed { Sweep::Unanswered } else { Sweep::Unverified })
            }
            _ = tokio::time::sleep(retry.unwrap_or_default()), if retry.is_some() => {
                let now = Utc::now();
                ids.extend(deferred.iter().filter(|(_, at)| **at <= now).map(|(id, _)| id.clone()));
                None
            }
            event = changes.recv(), if subscription_open => match event {
                Some(event) => {
                    ids.extend(event.changes.into_iter()
                        .filter(|change| change.collection_id == collection_id)
                        .map(|change| change.doc_id));
                    event.resync_required.then_some(Sweep::Unverified)
                }
                None => {
                    subscription_open = false;
                    Some(Sweep::Unverified)
                }
            },
        };
    }
}

#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod tests;
