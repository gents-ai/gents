use crate::agent::p2p_reconcile::error::P2pReconcileError;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::goal_control::{
    GoalControlOutcome, SessionGoalControl, SessionGoalControlReceipt, SESSION_GOAL_CONTROL_VERSION,
};

use super::enrollment_store::GraphqlEnrollmentStore;
use super::session_command_authority::{current_admission, AuthorScope};
use super::session_commands::{CommandRow, FIELDS};
use crate::config_client::{ConfigAccess, IdempotentTransactionRetry};
use crate::graphql::escape_graphql_string;
use crate::identity::NodeIdentity;

type Result<T, E = P2pReconcileError> = std::result::Result<T, E>;

pub(super) async fn has_verified_receipt(
    identity: &dyn NodeIdentity,
    intent: &str,
    raw: Option<&str>,
) -> Result<bool> {
    let Ok(command) = serde_json::from_str::<SessionGoalControl>(intent) else {
        return Ok(raw.is_some());
    };
    let Some(receipt) =
        raw.and_then(|raw| serde_json::from_str::<SessionGoalControlReceipt>(raw).ok())
    else {
        return Ok(false);
    };
    Ok(receipt.validate_for(&command).is_ok()
        && identity
            .verify(
                &receipt.node_did,
                &receipt.signing_payload()?,
                &receipt.signature,
            )
            .await
            .unwrap_or(false))
}

async fn intrinsic_rejection(
    identity: &dyn NodeIdentity,
    command: &SessionGoalControl,
) -> Result<Option<String>> {
    if let Err(error) = command.validate_shape() {
        return Ok(Some(error.to_string()));
    }
    if command.node_did != identity.did() {
        return Ok(Some("goal control targets another node".into()));
    }
    if !identity
        .verify(
            &command.requester_did,
            &command.signing_payload()?,
            &command.signature,
        )
        .await
        .unwrap_or(false)
    {
        return Ok(Some("invalid goal control author signature".into()));
    }
    if let Some(initial) = command.initial_request.as_deref() {
        if !identity
            .verify(
                &command.requester_did,
                &initial.signing_payload(),
                &initial.admission.signature,
            )
            .await
            .unwrap_or(false)
        {
            return Ok(Some("invalid initial goal request signature".into()));
        }
    }
    Ok(None)
}

pub(crate) async fn process_command(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    doc_id: &str,
) -> Result<Option<DateTime<Utc>>> {
    process_command_with_clock(node, identity, doc_id, Utc::now).await
}

#[cfg(test)]
pub(crate) async fn process_command_at(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    doc_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    process_command_with_clock(node, identity, doc_id, move || now).await
}

async fn process_command_with_clock(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    doc_id: &str,
    clock: impl Fn() -> DateTime<Utc> + Send + Sync,
) -> Result<Option<DateTime<Utc>>> {
    let store = GraphqlEnrollmentStore::new(node.clone(), identity.clone());
    ConfigAccess::transact_local_idempotent(node, None, IdempotentTransactionRetry::Standard,
        "goal.apply_signed_control", |txn| {
        let store = &store;
        let clock = &clock;
        Box::pin(async move {
            let response = txn.execute(&format!(r#"{{ AgentSessionGoalControl(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {FIELDS} }} }}"#, escape_graphql_string(doc_id))).await?;
            let rows: Vec<CommandRow> = serde_json::from_value(response["data"]["AgentSessionGoalControl"].clone())?;
            let [row] = rows.as_slice() else { return Err(P2pReconcileError::GoalControlCommandMissingAmbiguous); };
            if row.node_did != identity.did() { return Ok(None); }
            let command: SessionGoalControl = serde_json::from_str(&row.intent_json)?;
            if !(row.command_id == command.command_id && row.requester_did == command.requester_did
                && row.requester_peer_id == command.requester_peer_id && row.node_did == command.node_did
                && row.session_id == command.session_id) {
 return Err(P2pReconcileError::GoalControlImmutableScopeDisagreesSigned);
 }
            crate::graphql::fence_arrivals(txn, "AgentSessionGoalControl").await?;
            let twins = txn.execute(&format!(r#"{{ AgentSessionGoalControl(filter: {{ node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }} }}) {{ _docID command_id }} }}"#,
                escape_graphql_string(&row.node_did), escape_graphql_string(&row.requester_did))).await?;
            let twins = twins["data"]["AgentSessionGoalControl"].as_array().ok_or(P2pReconcileError::MissingGoalCommandIdentityQuery)?;
            if twins.iter().any(|twin| twin["command_id"].as_str() == Some(command.command_id.as_str()) && twin["_docID"].as_str() != Some(row.doc_id.as_str())) {
                tracing::debug!(command_doc_id = doc_id, command_id = %command.command_id, "goal control identity collides; leaving it unanswered");
                return Ok(None);
            }
            if has_verified_receipt(identity.as_ref(), &row.intent_json, row.receipt_json.as_deref()).await? {
                return Ok(None);
            }
            let rejection = intrinsic_rejection(identity.as_ref(), &command).await?;
            let outcome = match rejection {
                Some(reason) => GoalControlOutcome::Rejected { reason },
                None => {
                    let now = clock();
                    let issued = DateTime::parse_from_rfc3339(&command.issued_at)?.with_timezone(&Utc);
                    if now < issued { return Ok(Some(issued)); }
                    let authority = if now >= DateTime::parse_from_rfc3339(&command.expires_at)? {
                        Err("goal control has expired".into())
                    } else {
                        current_admission(txn, store, AuthorScope { requester_did: &command.requester_did,
                            requester_peer_id: command.requester_peer_id.as_deref(), node_did: &command.node_did },
                            "AgentSessionGoalControl").await?
                    };
                    match authority {
                        Err(reason) => GoalControlOutcome::Rejected { reason },
                        Ok(expected_admission) => {
                            let admission_matches = command.initial_request.as_ref().is_none_or(|initial| {
                                let mut actual = initial.admission.clone();
                                actual.signature.clear();
                                actual == expected_admission
                            });
                            if !admission_matches {
                                GoalControlOutcome::Rejected { reason: "initial request admission does not match current author authority".into() }
                            } else {
                                match crate::goal::prepare_goal_control(txn, &command).await? {
                                    Err(reason) => GoalControlOutcome::Rejected { reason },
                                    Ok(prepared) => crate::goal::stage_goal_control(txn, identity.as_ref(), prepared).await?,
                                }
                            }
                        }
                    }
                }
            };
            let mut receipt = SessionGoalControlReceipt {
                version: SESSION_GOAL_CONTROL_VERSION, command_id: command.command_id.clone(),
                command_digest: command.computed_digest()?, requester_did: command.requester_did.clone(),
                node_did: command.node_did.clone(), session_id: command.session_id.clone(), outcome,
                processed_at: clock().to_rfc3339(), signature: Vec::new(),
            };
            receipt.signature = identity.sign(&receipt.signing_payload()?).await?;
            receipt.validate_for(&command)?;
            let written = txn.execute(&format!(r#"mutation {{ update_AgentSessionGoalControl(docID: "{}", input: {{ receipt_json: "{}" }}) {{ _docID }} }}"#,
                escape_graphql_string(&row.doc_id), escape_graphql_string(&serde_json::to_string(&receipt)?))).await?;
            if !(written["data"]["update_AgentSessionGoalControl"].as_array().is_some_and(|rows| rows.len() == 1 && rows[0]["_docID"].as_str() == Some(row.doc_id.as_str()))) {
 return Err(P2pReconcileError::GoalControlReceiptLostCommand);
 }
            Ok(None)
        })
    }).await
}
