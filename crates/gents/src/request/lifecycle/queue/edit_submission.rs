use super::*;
use crate::store::config_client::ConfigAccess;
use crate::store::identity::NodeIdentity;
use gents_protocol::request_admission::AgentRequestAdmissionRecord;
use gents_protocol::session_input_edit::{
    PendingQueueEdit, SessionInputEdit, SessionInputEditOutcome, SessionInputEditReceipt,
    SESSION_INPUT_EDIT_VERSION,
};

const COMMAND_LIFETIME: chrono::Duration = chrono::Duration::minutes(5);
const RECEIPT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
const RECEIPT_POLL: std::time::Duration = std::time::Duration::from_millis(250);

#[allow(clippy::too_many_arguments)]
pub async fn submit_pending_user_edit(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    requester_peer_id: Option<&str>,
    node_did: &str,
    session_id: &str,
    edit: PendingQueueEdit,
) -> Result<SessionInputEditReceipt> {
    let requester_did =
        crate::session::client_requester_scope(access, node_did, session_id, signer.did()).await?;
    let replacements = super::prepare_pending_user_edit(
        access,
        signer,
        admission,
        node_did,
        session_id,
        &requester_did,
        &edit,
    )
    .await?;
    submit_prepared_edit(
        access,
        signer,
        requester_peer_id,
        node_did,
        session_id,
        edit,
        replacements,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn submit_pending_user_steer(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    admission: AgentRequestAdmissionRecord,
    requester_peer_id: Option<&str>,
    node_did: &str,
    session_id: &str,
    edit: PendingQueueEdit,
    request_doc_id: &str,
    request_id: Option<&str>,
) -> Result<SessionInputEditReceipt> {
    anyhow::ensure!(
        edit.messages
            .first()
            .is_some_and(|message| message.request_doc_id == request_doc_id),
        "The steered message must lead its selected queue group."
    );
    let deliveries = edit
        .messages
        .iter()
        .map(|message| {
            if message.request_doc_id == request_doc_id {
                gents_protocol::request_input::QueueDelivery::Steer
            } else {
                gents_protocol::request_input::QueueDelivery::Queue
            }
        })
        .collect::<Vec<_>>();
    let requester_did =
        crate::session::client_requester_scope(access, node_did, session_id, signer.did()).await?;
    let replacements = super::management::prepare_pending_user_edit_with_deliveries(
        access,
        signer,
        admission,
        node_did,
        session_id,
        &requester_did,
        &edit,
        Some(&deliveries),
        request_id,
    )
    .await?;
    submit_prepared_edit(
        access,
        signer,
        requester_peer_id,
        node_did,
        session_id,
        edit,
        replacements,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn submit_prepared_edit(
    access: &ConfigAccess,
    signer: &dyn NodeIdentity,
    requester_peer_id: Option<&str>,
    node_did: &str,
    session_id: &str,
    edit: PendingQueueEdit,
    replacements: Vec<gents_protocol::request_admission::AgentRequestCreate>,
) -> Result<SessionInputEditReceipt> {
    let requester_did = signer.did();
    anyhow::ensure!(
        access.actor_did().as_deref() == Some(requester_did),
        "edit actor differs from signer"
    );
    let (session_doc_id, agent_did, agent_doc_id) = access
        .transact_readonly("queue.edit.targets", |txn| {
            Box::pin(async move {
                let session =
                    crate::session::load_agent_session_owner_in_txn(txn, node_did, session_id)
                        .await?
                        .context("edit session unavailable")?;
                let (agent_doc, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Agent,
                    node_did,
                    &session.session.agent_did,
                )
                .await?
                .context("edit Agent unavailable")?;
                Ok::<_, anyhow::Error>((session.doc_id, session.session.agent_did, agent_doc))
            })
        })
        .await?;
    let mut replacement_evidence = Vec::with_capacity(replacements.len());
    for create in &replacements {
        anyhow::ensure!(
            create.session_doc_id.as_deref() == Some(session_doc_id.as_str())
                && create.agent_doc_id.as_deref() == Some(agent_doc_id.as_str())
                && create.agent_did == agent_did
                && create.admission.signer_did == requester_did,
            "replacement physical targets or signer changed"
        );
        replacement_evidence.push(
            crate::store::config_client::prepare_request_evidence(
                signer,
                crate::request::lifecycle::client_request_input(create)?,
            )
            .await?,
        );
    }
    let now = chrono::Utc::now();
    let mut command = SessionInputEdit {
        version: SESSION_INPUT_EDIT_VERSION,
        command_id: uuid::Uuid::new_v4().to_string(),
        requester_did: requester_did.to_owned(),
        requester_peer_id: requester_peer_id
            .filter(|_| requester_did != node_did)
            .map(str::to_owned),
        node_did: node_did.to_owned(),
        session_id: session_id.to_owned(),
        session_doc_id,
        agent_did,
        agent_doc_id,
        issued_at: now.to_rfc3339(),
        expires_at: (now + COMMAND_LIFETIME).to_rfc3339(),
        edit,
        replacements,
        replacement_evidence,
        signature: Vec::new(),
    };
    command.signature = signer.sign(&command.signing_payload()).await?;
    command.validate_shape()?;
    let input = serde_json::json!({"command_id":command.command_id, "requester_did":command.requester_did,
        "requester_peer_id":command.requester_peer_id, "node_did":command.node_did,
        "session_id":command.session_id, "intent_json":serde_json::to_string(&command)?});
    let doc_id = access.transact_write_with_finality("queue.submit_pending_edit", |txn| {
        let input = &input;
        let command = &command;
        Box::pin(async move {
            let response = txn.execute_client_signed_create(
                "mutation($input: AgentSessionInputEditMutationInputArg!) { create_AgentSessionInputEdit(input: $input) { _docID } }",
                &serde_json::json!({"input":input}), gents_protocol::schemas::AGENT_SESSION_INPUT_EDIT,
            ).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "AgentSessionInputEdit")?;
            txn.add_resource_relationship("AgentSessionInputEdit", &doc, "parent", "sessions", &command.session_doc_id).await?;
            txn.add_actor_relationship("AgentSessionInputEdit", &doc, "working_agent", &identity::Did::new(&command.agent_did)?).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await.map_err(|failure| {
        if failure.unconfirmed {
            failure.error.context(format!("Pending edit {} may have been saved; inspect it before resending", command.command_id))
        } else { failure.error }
    })?;
    match tokio::time::timeout(RECEIPT_WAIT, answered(access, &doc_id, &command)).await {
        Ok(receipt) => receipt,
        Err(_) => anyhow::bail!(
            "Pending edit {} was saved and is waiting for the runtime; its outcome is not yet confirmed.",
            command.command_id
        ),
    }
}

async fn answered(
    access: &ConfigAccess,
    doc_id: &str,
    command: &SessionInputEdit,
) -> Result<SessionInputEditReceipt> {
    let query = format!(
        r#"{{ AgentSessionInputEdit(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ receipt_json }} }}"#,
        escape_graphql_string(doc_id)
    );
    loop {
        let response = access.execute(&query).await?;
        let receipt = response["data"]["AgentSessionInputEdit"][0]["receipt_json"]
            .as_str()
            .and_then(|raw| serde_json::from_str::<SessionInputEditReceipt>(raw).ok())
            .filter(|receipt| {
                receipt.validate_for(command).is_ok()
                    && crate::store::identity::verify_did_signature(
                        &command.node_did,
                        &receipt.signing_payload(),
                        &receipt.signature,
                    )
                    .unwrap_or(false)
            });
        match receipt {
            Some(receipt) if receipt.outcome == SessionInputEditOutcome::Rejected => {
                anyhow::bail!("Runtime rejected pending edit: {}", receipt.reason)
            }
            Some(receipt) if effect_visible(access, command, &receipt).await? => {
                return Ok(receipt)
            }
            _ => tokio::time::sleep(RECEIPT_POLL).await,
        }
    }
}

/// The local replica may show a receipt before the requests it names.
async fn effect_visible(
    access: &ConfigAccess,
    command: &SessionInputEdit,
    receipt: &SessionInputEditReceipt,
) -> Result<bool> {
    let ids = crate::store::graphql::graphql_string_list_literal(
        command
            .edit
            .selected_request_doc_ids
            .iter()
            .chain(&receipt.request_doc_ids)
            .map(String::as_str),
    );
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(docIDs: {ids}) {{ _docID request_id lifecycle_state }} }}"#
        ))
        .await?;
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    let state = |id: &String| {
        rows.iter()
            .find(|row| row.doc_id.as_ref() == Some(id))
            .and_then(|row| row.lifecycle_state)
    };
    Ok(command
        .edit
        .selected_request_doc_ids
        .iter()
        .all(|id| state(id).is_some_and(|state| state != RequestLifecycleState::Pending))
        && receipt.request_doc_ids.iter().all(|id| state(id).is_some()))
}
