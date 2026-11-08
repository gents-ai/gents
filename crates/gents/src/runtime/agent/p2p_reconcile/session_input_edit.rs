use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::session_input_edit::{
    SessionInputEdit, SessionInputEditOutcome, SessionInputEditReceipt, SESSION_INPUT_EDIT_VERSION,
};

#[cfg(test)]
use super::session_commands::run_session_command_reconciler_with_agents;
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
    command: &SessionInputEdit,
) -> Result<Option<String>> {
    // P2P carries no session edit, so a command naming a requester peer
    // arrived outside the direct client-signed path and fails closed.
    if command.requester_peer_id.is_some() {
        return Ok(Some("remote session edits are not accepted".into()));
    }
    let expected_admission =
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
            &command.requester_did,
        );
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
    let Some(session) =
        crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
            txn,
            &command.node_did,
            &command.session_id,
        )
        .await?
    else {
        // Processing is reached only once the command's physical session was
        // observed as this Agent's, so an unresolved logical ID is a binding
        // mismatch, not a missing replica.
        return Ok(Some(
            "edit physical targets do not match the canonical session".into(),
        ));
    };
    let Some((agent_doc, _)) = crate::store::config_client::read_desired_state_record_in_txn(
        txn,
        crate::Collection::Agent,
        &command.node_did,
        &command.agent_did,
    )
    .await?
    else {
        anyhow::bail!("edit Agent unavailable")
    };
    if session.doc_id != command.session_doc_id
        || session.session.agent_did != command.agent_did
        || agent_doc != command.agent_doc_id
        || txn.actor_did() != Some(command.agent_did.as_str())
    {
        return Ok(Some(
            "edit physical targets do not match the canonical session".into(),
        ));
    }
    let requester = session.session.requester_did.as_deref();
    if requester.is_none()
        || command
            .replacements
            .iter()
            .any(|replacement| Some(replacement.requester_did.as_str()) != requester)
    {
        return Ok(Some(
            "replacement changed canonical session provenance".into(),
        ));
    }
    let session_id = escape_graphql_string(&command.session_doc_id);
    let response = txn.execute(&format!(
        r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{session_id}" }} }}, limit: 2) {{ _docID relationships_pending }} }}"#
    )).await?;
    let sessions = response["data"]["AgentSession"]
        .as_array()
        .context("edit session readiness unavailable")?;
    anyhow::ensure!(
        sessions.len() == 1
            && sessions[0]["_docID"].as_str() == Some(command.session_doc_id.as_str())
            && sessions[0]["relationships_pending"] == false,
        "edit session relationships unavailable"
    );
    let target = escape_graphql_string(&command.agent_doc_id);
    let response = txn.execute(&format!(r#"{{ Agent(filter: {{ _docID: {{ _eq: "{target}" }} }}, limit: 2) {{ _docID node_did agent_did enabled }} }}"#)).await?;
    let agents = response["data"]["Agent"]
        .as_array()
        .context("edit Agent observation unavailable")?;
    let [agent] = agents.as_slice() else {
        anyhow::bail!("edit Agent observation ambiguous")
    };
    if agent["_docID"].as_str() != Some(command.agent_doc_id.as_str())
        || agent["node_did"].as_str() != Some(command.node_did.as_str())
        || agent["agent_did"].as_str() != Some(command.agent_did.as_str())
        || agent["enabled"].as_bool() == Some(false)
    {
        return Ok(Some("edit is not authorized".into()));
    }
    let subject = ::identity::Did::new(&command.requester_did)?;
    let named = |permission: &str| defra_node::DocumentAccessMode::Named {
        permission: permission.to_owned(),
        node_permission: acp::nac::NodePermission::DocumentUpdate,
    };
    use defra_node::DocumentAccessDecision::{Allowed, Denied, Unavailable};
    for (collection, docs, permission) in [
        (
            "AgentSession",
            vec![command.session_doc_id.clone()],
            "update",
        ),
        ("Agent", vec![command.agent_doc_id.clone()], "invoke"),
        (
            "AgentRequest",
            command.edit.selected_request_doc_ids.clone(),
            "update",
        ),
    ] {
        let observed = txn
            .observe_document_access(&subject, collection, &docs, named(permission))
            .await?;
        if observed.len() != docs.len() || observed.contains(&Unavailable) {
            anyhow::bail!("edit authority observation unavailable");
        }
        if observed.contains(&Denied) {
            return Ok(Some("edit is not authorized".into()));
        }
        anyhow::ensure!(
            observed.iter().all(|decision| *decision == Allowed),
            "edit authority unavailable"
        );
    }
    Ok(None)
}

/// The working Agent of the physical session the signed intent names, when its
/// logical session ID does not resolve. That Agent answers the mismatch; a
/// physical session that is not observable leaves the command undecided.
async fn physical_session_agent(
    txn: &ConfigApplyTxn<'_>,
    row: &CommandRow,
) -> Result<Option<String>> {
    let Ok(command) = serde_json::from_str::<SessionInputEdit>(&row.intent_json) else {
        return Ok(None);
    };
    let response = txn
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ node_did agent_did }} }}"#,
            escape_graphql_string(&command.session_doc_id)
        ))
        .await?;
    let sessions = response["data"]["AgentSession"]
        .as_array()
        .context("edit physical session observation unavailable")?;
    let [session] = sessions.as_slice() else {
        return Ok(None);
    };
    Ok((session["node_did"].as_str() == Some(row.node_did.as_str()))
        .then(|| session["agent_did"].as_str().map(str::to_owned))
        .flatten())
}

async fn verified_graphs(
    command: &SessionInputEdit,
) -> Result<Result<Vec<Arc<db::block::builder::VerifiedClientCreate>>, String>> {
    let mut result = Vec::with_capacity(command.replacements.len());
    for (create, evidence) in command
        .replacements
        .iter()
        .zip(&command.replacement_evidence)
    {
        let input = match crate::store::config_client::client_request_input(create) {
            Ok(input) => input,
            Err(_) => return Ok(Err("invalid replacement input".into())),
        };
        let graph =
            match crate::store::config_client::verify_request_evidence(input, evidence.clone())
                .await
            {
                Ok(graph) => graph,
                Err(
                    crate::store::config_client::ConfigClientError::ClientAgentGraph(_)
                    | crate::store::config_client::ConfigClientError::Json(_),
                ) => return Ok(Err("invalid replacement evidence".into())),
                Err(error) => return Err(error.into()),
            };
        if graph.signer_did() != command.requester_did {
            return Ok(Err("replacement signer differs from edit author".into()));
        }
        result.push(Arc::new(graph));
    }
    Ok(Ok(result))
}

async fn replacements_were_applied(
    txn: &ConfigApplyTxn<'_>,
    command: &SessionInputEdit,
    docs: &[String],
) -> Result<bool> {
    for (index, old) in command.edit.selected_request_doc_ids.iter().enumerate() {
        let query = format!(
            r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{_docID lifecycle_state superseded_by_request_doc_id failure_reason}} }}"#,
            escape_graphql_string(old)
        );
        let response = txn.execute(&query).await?;
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .context("selected request observation unavailable")?;
        let [row] = rows.as_slice() else {
            return Ok(false);
        };
        let applied = match docs.get(index) {
            Some(doc) => {
                row["lifecycle_state"] == "superseded"
                    && row["superseded_by_request_doc_id"].as_str() == Some(doc)
            }
            None => {
                row["lifecycle_state"] == "interrupted"
                    && row["failure_reason"].as_str()
                        == Some(crate::request::lifecycle::REMOVED_REASON)
            }
        };
        if !applied {
            return Ok(false);
        }
    }
    Ok(true)
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
    agent_identity: &dyn NodeIdentity,
) -> Result<Option<DateTime<Utc>>> {
    let actor = ::identity::Did::new(agent_identity.did())?;
    let access = ConfigAccess::local_managed_agent(node.clone(), actor.clone(), agent_identity)?;
    access.transact_idempotent(IdempotentTransactionRetry::Standard,
        "queue.apply_signed_edit", |txn| {
        let actor = &actor;
        Box::pin(async move {
            let response = txn.execute(&format!(r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ {FIELDS} }} }}"#, escape_graphql_string(doc_id))).await?;
            let rows: Vec<CommandRow> = serde_json::from_value(response["data"]["AgentSessionInputEdit"].clone())?;
            let [row] = rows.as_slice() else {
                anyhow::ensure!(rows.is_empty(), "edit command is ambiguous");
                return Ok(None);
            };
            if row.node_did != identity.did() {
                return Ok(None);
            }
            let working_agent = match crate::store::config_client::agent_session::load_agent_session_owner_in_txn(txn, &row.node_did, &row.session_id).await? {
                Some(session) => session.session.agent_did,
                None => match physical_session_agent(txn, &row).await? {
                    Some(agent) => agent,
                    None => return Ok(None),
                },
            };
            if working_agent != actor.as_str() { return Ok(None); }
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
            let intrinsic = intrinsic_rejection(identity.as_ref(), &command).await;
            let owners = txn.document_owners("AgentSessionInputEdit", std::slice::from_ref(&row.doc_id)).await?;
            let owner = owners.first().and_then(|entry| entry.owner.as_ref())
                .context("edit command owner unavailable")?;
            anyhow::ensure!(owners.len() == 1 && owners[0].visible && owners[0].doc_id == row.doc_id,
                "edit command owner unavailable or ambiguous");
            let owner_matches = owner.as_str() == command.requester_did;
            let graphs = if intrinsic.is_none() && owner_matches {
                Some(verified_graphs(&command).await?)
            } else { None };
            let (outcome, reason, request_doc_ids) = if let Some(reason) = intrinsic {
                (SessionInputEditOutcome::Rejected, reason, Vec::new())
            } else if !owner_matches {
                (SessionInputEditOutcome::Rejected, "edit author differs from native owner".into(), Vec::new())
            } else if let Some(Err(reason)) = graphs.as_ref() {
                (SessionInputEditOutcome::Rejected, reason.clone(), Vec::new())
            } else {
                let graphs = graphs.as_ref().and_then(|result| result.as_ref().ok()).context("verified graphs unavailable")?;
                let expected: Vec<String> = graphs.iter().map(|graph| graph.doc_id().to_owned()).collect();
                match crate::request::lifecycle::queue::persisted_replacements_in_txn(txn, &command.node_did, &command.replacements, Some(&expected)).await? {
                    crate::request::lifecycle::queue::PersistedReplacements::Exact(ids) => {
                        if replacements_were_applied(txn, &command, &ids).await? {
                            (SessionInputEditOutcome::Applied, String::new(), ids)
                        } else {
                            (SessionInputEditOutcome::Rejected, "replacement state conflicts with edit".into(), Vec::new())
                        }
                    }
                    crate::request::lifecycle::queue::PersistedReplacements::Conflicting =>
                        (SessionInputEditOutcome::Rejected, "replacement identity conflicts with edit".into(), Vec::new()),
                    crate::request::lifecycle::queue::PersistedReplacements::None => {
                        if command.replacements.is_empty()
                            && replacements_were_applied(txn, &command, &[]).await? {
                            // A terminal removal does not name the command that caused it.
                            return Ok(None);
                        }
                        let now = Utc::now();
                        let issued_at = DateTime::parse_from_rfc3339(&command.issued_at)?.with_timezone(&Utc);
                        if now < issued_at { return Ok(Some(issued_at)); }
                        let rejection = if now >= DateTime::parse_from_rfc3339(&command.expires_at)? {
                            Some("edit intent has expired".into())
                        } else { authority_rejection(txn, &command).await? };
                        match rejection {
                            Some(reason) => (SessionInputEditOutcome::Rejected, reason, Vec::new()),
                            None => {
                                let session = crate::store::config_client::agent_session::load_agent_session_owner_in_txn(txn, &command.node_did, &command.session_id).await?.context("validated session disappeared")?;
                                let requester = session.session.requester_did.context("validated session lacks requester provenance")?;
                                match crate::request::lifecycle::queue::validate_pending_user_edit_in_txn(txn, &command.node_did, &command.session_id,
                                    &requester, &command.requester_did, &command.edit, &command.replacements).await? {
                                    Err(reason) => (SessionInputEditOutcome::Rejected, reason, Vec::new()),
                                    Ok(validated) => (SessionInputEditOutcome::Applied, String::new(),
                                        crate::request::lifecycle::queue::apply_pending_user_edit_in_txn(txn, validated, graphs).await?.request_doc_ids),
                                }
                            }
                        }
                    }
                }
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
async fn test_signed_replacement_fields(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    node_did: &str,
    session_id: &str,
    replacements: &[gents_protocol::request_admission::AgentRequestCreate],
) -> (String, String, String, Vec<serde_json::Value>) {
    let (session_doc, agent_did, agent_doc) = access
        .transact_readonly("test.edit.targets", |txn| {
            Box::pin(async move {
                let session =
                    crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn, node_did, session_id,
                    )
                    .await?
                    .context("test session missing")?;
                let (agent, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Agent,
                    node_did,
                    &session.session.agent_did,
                )
                .await?
                .context("test Agent missing")?;
                Ok::<_, anyhow::Error>((session.doc_id, session.session.agent_did, agent))
            })
        })
        .await
        .unwrap();
    let mut evidence = Vec::new();
    for create in replacements {
        evidence.push(
            crate::store::config_client::prepare_request_evidence(
                signer,
                crate::store::config_client::client_request_input(create).unwrap(),
            )
            .await
            .unwrap(),
        );
    }
    (session_doc, agent_did, agent_doc, evidence)
}

#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod verified_graph_tests;
