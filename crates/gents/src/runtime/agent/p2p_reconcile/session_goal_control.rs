use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::goal_control::{
    GoalControlOutcome, SessionGoalControl, SessionGoalControlReceipt, SESSION_GOAL_CONTROL_VERSION,
};

use super::session_commands::CommandRow;
use crate::store::config_client::{ConfigAccess, IdempotentTransactionRetry};
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::NodeIdentity;

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
                &receipt.agent_did,
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
        if initial.agent_did != command.agent_did
            || initial.agent_doc_id.as_deref() != Some(command.agent_doc_id.as_str())
            || initial.session_doc_id != command.session_doc_id
        {
            return Ok(Some("goal request selects a different agent".into()));
        }
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
    agent_identity: &dyn NodeIdentity,
) -> Result<Option<DateTime<Utc>>> {
    process_command_with_clock(node, identity, doc_id, agent_identity, Utc::now).await
}

#[cfg(test)]
pub(crate) async fn process_command_at(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    doc_id: &str,
    actor: &::identity::Did,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    let agent = crate::load_agent(node, actor.as_str())
        .await?
        .context("test goal Agent is unavailable")?;
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: agent.agent_did,
        creation_nonce: agent.creation_nonce,
    };
    let agent_identity =
        crate::store::identity::resolve_agent_identity(identity.as_ref(), &binding)?;
    process_command_with_clock(node, identity, doc_id, agent_identity.as_ref(), move || now).await
}

async fn process_command_with_clock(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn NodeIdentity>,
    doc_id: &str,
    agent_identity: &dyn NodeIdentity,
    clock: impl Fn() -> DateTime<Utc> + Send + Sync,
) -> Result<Option<DateTime<Utc>>> {
    let actor = ::identity::Did::new(agent_identity.did())?;
    let access = ConfigAccess::local_managed_agent(node.clone(), actor.clone(), agent_identity)?;
    let (deferred, pending_receipt) = access.transact_idempotent(IdempotentTransactionRetry::Standard,
        "goal.apply_signed_control", |txn| {
        let clock = &clock;
        let actor = &actor;
        Box::pin(async move {
            let response = txn.execute(&format!(r#"{{ AgentSessionGoalControl(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID command_id requester_did requester_peer_id node_did agent_did session_id session_doc_id intent_json }} AgentSessionGoalReceipt(filter: {{ command_doc_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID receipt_json relationships_pending }} }}"#, escape_graphql_string(doc_id), escape_graphql_string(doc_id))).await?;
            let rows: Vec<CommandRow> = serde_json::from_value(response["data"]["AgentSessionGoalControl"].clone())?;
            let [row] = rows.as_slice() else {
                anyhow::ensure!(rows.is_empty(), "goal control command is ambiguous");
                return Ok((None, None));
            };
            if row.node_did != identity.did() { return Ok((None, None)); }
            let command: SessionGoalControl = serde_json::from_str(&row.intent_json)?;
            anyhow::ensure!(row.command_id == command.command_id && row.requester_did == command.requester_did
                && row.requester_peer_id == command.requester_peer_id && row.node_did == command.node_did
                && row.session_id == command.session_id && row.agent_did.as_deref() == Some(command.agent_did.as_str())
                && row.session_doc_id == command.session_doc_id && command.agent_did == actor.as_str(),
                "goal control immutable scope disagrees with signed intent");
            let owners = txn.document_owners("AgentSessionGoalControl", std::slice::from_ref(&row.doc_id)).await?;
            let owner = owners.first().and_then(|owner| owner.owner.as_ref())
                .context("goal command native ownership is unavailable")?;
            anyhow::ensure!(owner.as_str() == command.requester_did,
                "goal command author differs from its native owner");
            crate::store::graphql::fence_arrivals(txn, "AgentSessionGoalControl").await?;
            let twins = txn.execute(&format!(r#"{{ AgentSessionGoalControl(filter: {{ node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }} }}) {{ _docID command_id }} }}"#,
                escape_graphql_string(&row.node_did), escape_graphql_string(&row.requester_did))).await?;
            let twins = twins["data"]["AgentSessionGoalControl"].as_array().context("missing goal command identity query")?;
            if twins.iter().any(|twin| twin["command_id"].as_str() == Some(command.command_id.as_str()) && twin["_docID"].as_str() != Some(row.doc_id.as_str())) {
                tracing::debug!(command_doc_id = doc_id, command_id = %command.command_id, "goal control identity collides; leaving it unanswered");
                return Ok((None, None));
            }
            let receipts = response["data"]["AgentSessionGoalReceipt"].as_array().context("goal receipt observation omitted rows")?;
            anyhow::ensure!(receipts.len() <= 1, "goal receipt identity is ambiguous");
            let raw_receipt = receipts.first().and_then(|receipt| receipt["receipt_json"].as_str());
            if has_verified_receipt(identity.as_ref(), &row.intent_json, raw_receipt).await? {
                let pending = receipts.first().filter(|receipt| receipt["relationships_pending"] == true)
                    .map(|receipt| receipt["_docID"].as_str().context("pending goal receipt lacks document ID"))
                    .transpose()?
                    .map(|receipt| (receipt.to_owned(), command.requester_did.clone()));
                return Ok((None, pending));
            }
            if let Some(reason) = intrinsic_rejection(identity.as_ref(), &command).await? {
                tracing::debug!(command_doc_id = doc_id, reason, "goal command provenance is unavailable");
                return Ok((None, None));
            }
            let now = clock();
            let issued = DateTime::parse_from_rfc3339(&command.issued_at)?.with_timezone(&Utc);
            if now < issued { return Ok((Some(issued), None)); }
            crate::store::graphql::fence_arrivals(txn, "AgentSession").await?;
            crate::store::graphql::fence_arrivals(txn, "Agent").await?;
            let Some(session) = crate::session::load_agent_session_owner_in_txn(txn, &command.node_did, &command.session_id).await? else {
                return Ok((None, None));
            };
            let Some((agent_doc, agent)) = crate::store::config_client::read_desired_state_record_in_txn(
                txn, crate::Collection::Agent, &command.node_did, &command.agent_did,
            ).await? else {
                return Ok((None, None));
            };
            let session_doc = command.session_doc_id.as_ref().expect("validated session document");
            let targets_match = session.doc_id.as_str() == session_doc.as_str()
                && session.session.agent_did == command.agent_did
                && agent_doc == command.agent_doc_id;
            if session.doc_id.as_str() != session_doc.as_str() || agent_doc != command.agent_doc_id {
                let targets = txn.execute(&format!(r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} Agent(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
                    escape_graphql_string(session_doc), escape_graphql_string(&command.agent_doc_id))).await?;
                for (collection, expected) in [("AgentSession", session_doc), ("Agent", &command.agent_doc_id)] {
                    let rows = targets["data"][collection].as_array().context("goal target visibility query omitted rows")?;
                    if rows.len() != 1 || rows[0]["_docID"].as_str() != Some(expected.as_str()) {
                        return Ok((None, None));
                    }
                }
            }
            let subject = ::identity::Did::new(&command.requester_did)?;
            let named = |permission: &str| defra_node::DocumentAccessMode::Named {
                permission: permission.to_owned(),
                node_permission: acp::nac::NodePermission::DocumentUpdate,
            };
            let session_permission = txn.observe_document_access(&subject, "AgentSession", std::slice::from_ref(session_doc), named("update")).await?;
            let agent_permission = txn.observe_document_access(&subject, "Agent", std::slice::from_ref(&command.agent_doc_id), named("invoke")).await?;
            use defra_node::DocumentAccessDecision::{Allowed, Denied};
            let [session_permission @ (Allowed | Denied)] = session_permission.as_slice() else {
                return Ok((None, None));
            };
            let [agent_permission @ (Allowed | Denied)] = agent_permission.as_slice() else {
                return Ok((None, None));
            };
            let denied = !targets_match || agent["enabled"].as_bool() == Some(false)
                || *session_permission == Denied || *agent_permission == Denied;
            let outcome = if denied {
                GoalControlOutcome::Rejected { reason: "permission denied or session unavailable".into() }
            } else if now >= DateTime::parse_from_rfc3339(&command.expires_at)? {
                GoalControlOutcome::Rejected { reason: "goal control has expired".into() }
            } else if matches!(command.operation, gents_protocol::goal_control::GoalOperation::Create { .. }) && command.native_evidence.is_none() {
                tracing::debug!(command_doc_id = doc_id, "goal creation remains pending without its native client graphs");
                return Ok((None, None));
            } else {
                match crate::goal::prepare_goal_control(txn, &command).await? {
                    Err(reason) => GoalControlOutcome::Rejected { reason },
                    Ok(mut prepared) => {
                        if matches!(command.operation, gents_protocol::goal_control::GoalOperation::Create { .. }) {
                            match crate::goal::attach_client_goal_graphs(txn, &command, &mut prepared).await? {
                                Ok(()) => crate::goal::stage_goal_control(txn, agent_identity, prepared).await?,
                                Err(reason) => GoalControlOutcome::Rejected { reason },
                            }
                        } else {
                            crate::goal::stage_goal_control(txn, agent_identity, prepared).await?
                        }
                    },
                }
            };
            let mut receipt = SessionGoalControlReceipt {
                version: SESSION_GOAL_CONTROL_VERSION, command_id: command.command_id.clone(),
                command_digest: command.computed_digest()?, requester_did: command.requester_did.clone(),
                node_did: command.node_did.clone(), agent_did: command.agent_did.clone(), session_id: command.session_id.clone(), outcome,
                processed_at: clock().to_rfc3339(), signature: Vec::new(),
            };
            receipt.signature = agent_identity.sign(&receipt.signing_payload()?).await?;
            receipt.validate_for(&command)?;
            let written = txn.execute_with_variables(
                "mutation($input: AgentSessionGoalReceiptMutationInputArg!) { create_AgentSessionGoalReceipt(input: $input) { _docID } }",
                &serde_json::json!({"input": {"command_doc_id": row.doc_id, "command_id": command.command_id, "receipt_json": serde_json::to_string(&receipt)?, "relationships_pending": true}}),
            ).await?;
            let receipt_doc = crate::store::graphql::created_doc_id(&written, "AgentSessionGoalReceipt")?;
            Ok((None, Some((receipt_doc, command.requester_did))))
        })
    }).await?;
    if let Some((receipt_doc, requester_did)) = pending_receipt {
        access
            .add_resource_relationship(
                "AgentSessionGoalReceipt",
                &receipt_doc,
                "parent",
                "goal_controls",
                doc_id,
            )
            .await?;
        access
            .add_actor_relationship(
                "AgentSessionGoalReceipt",
                &receipt_doc,
                "reader",
                &::identity::Did::new(&requester_did)?,
            )
            .await?;
        let updated = access.write("goal.receipt_relationships_published", &format!(
            r#"mutation {{ update_AgentSessionGoalReceipt(docID: "{}", input: {{ relationships_pending: false }}) {{ _docID }} }}"#,
            escape_graphql_string(&receipt_doc),
        )).await?;
        anyhow::ensure!(
            updated["data"]["update_AgentSessionGoalReceipt"]
                .as_array()
                .is_some_and(|rows| rows.len() == 1),
            "pending goal receipt was not updated"
        );
    }
    Ok(deferred)
}
