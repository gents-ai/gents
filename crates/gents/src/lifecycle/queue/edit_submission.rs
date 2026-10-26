use super::*;
use crate::config_client::ConfigAccess;
use crate::identity::NodeIdentity;
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
    let requester_did = signer.did();
    let replacements = super::prepare_pending_user_edit(
        access,
        signer,
        admission,
        node_did,
        session_id,
        requester_did,
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
) -> Result<SessionInputEditReceipt> {
    if !(edit
        .messages
        .first()
        .is_some_and(|message| message.request_doc_id == request_doc_id))
    {
        return Err(QueueError::SteeredMessageMustLeadSelectedQueue);
    }
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
    let replacements = super::management::prepare_pending_user_edit_with_deliveries(
        access,
        signer,
        admission,
        node_did,
        session_id,
        signer.did(),
        &edit,
        Some(&deliveries),
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
        issued_at: now.to_rfc3339(),
        expires_at: (now + COMMAND_LIFETIME).to_rfc3339(),
        edit,
        replacements,
        signature: Vec::new(),
    };
    command.signature = signer.sign(&command.signing_payload()).await?;
    command.validate_shape()?;
    let peer = command
        .requester_peer_id
        .as_deref()
        .map(|peer| format!(r#""{}""#, escape_graphql_string(peer)))
        .unwrap_or_else(|| "null".into());
    let response = access
        .write(
            "queue.submit_pending_edit",
            &format!(
                r#"mutation {{ create_AgentSessionInputEdit(input: {{
                command_id: "{}", requester_did: "{}", requester_peer_id: {peer},
                node_did: "{}", session_id: "{}", intent_json: "{}"
            }}) {{ _docID }} }}"#,
                escape_graphql_string(&command.command_id),
                escape_graphql_string(&command.requester_did),
                escape_graphql_string(node_did),
                escape_graphql_string(session_id),
                escape_graphql_string(&serde_json::to_string(&command)?),
            ),
        )
        .await?;
    let doc_id = crate::graphql::created_doc_id(&response, "AgentSessionInputEdit")?;
    match tokio::time::timeout(RECEIPT_WAIT, answered(access, &doc_id, &command)).await {
        Ok(receipt) => Ok(receipt?),
        Err(_) => {
            return Err(QueueError::PendingEditWasSavedWaitingRuntime {
                command_id: command.command_id.to_string(),
            })
        }
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
                    && crate::identity::verify_did_signature(
                        &command.node_did,
                        &receipt.signing_payload(),
                        &receipt.signature,
                    )
                    .unwrap_or(false)
            });
        match receipt {
            Some(receipt) if receipt.outcome == SessionInputEditOutcome::Rejected => {
                return Err(QueueError::RuntimeRejectedEdit {
                    reason: receipt.reason,
                })
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
    let ids = crate::graphql::graphql_string_list_literal(
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
