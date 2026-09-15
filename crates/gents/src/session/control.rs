//! Attach runtime-signed control continuations to their existing session without
//! relabeling either request authority or the session's exact requester scope.
use super::*;
use crate::config_client::ConfigApplyTxn;
use crate::request_admission::{
    verify_request_receipt_signature, verify_runtime_local_control_receipt, SIGNED_REQUEST_FIELDS,
};
use anyhow::Context;
use gents_protocol::row::AgentRequestRow;

fn rejected(session_id: &str, reason: impl ToString) -> anyhow::Error {
    crate::lifecycle::ClaimAdmissionError::SessionScopeMismatch {
        session_id: session_id.to_owned(),
        reason: reason.to_string(),
    }
    .into()
}

async fn load_request(txn: &ConfigApplyTxn<'_>, doc_id: &str) -> Result<AgentRequestRow> {
    let response = txn.execute(&format!(
        "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{}\" }} }} ) {{ {SIGNED_REQUEST_FIELDS} }} }}",
        escape_graphql_string(doc_id)
    )).await?;
    let mut rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    // Missing replicated dependencies remain retryable; invalid durable edges do not.
    anyhow::ensure!(
        rows.len() == 1,
        "control ancestry request {doc_id} is unavailable or ambiguous"
    );
    Ok(rows.remove(0))
}

/// This is a projection guard, not execution admission or ACP. Only exact,
/// authenticated local-control edges can cross a requester scope. User latest
/// observations remain exact-requester projections; background activity uses its
/// existing all-requester observation path. No session fields are written here.
pub(crate) async fn preserve_control_session_in_txn(
    txn: &ConfigApplyTxn<'_>,
    request: &crate::AgentRequest,
) -> Result<bool> {
    let response = txn.execute(&format!(
        "{{ AgentSession(filter: {{ agent_did: {{ _eq: \"{}\" }}, session_id: {{ _eq: \"{}\" }} }}) {{ {AGENT_SESSION_FIELDS} }} }}",
        escape_graphql_string(&request.agent_did), escape_graphql_string(&request.session_id)
    )).await?;
    let rows = response["data"]["AgentSession"]
        .as_array()
        .context("session query omitted rows")?;
    if rows.is_empty() {
        return Ok(false);
    }
    if rows.len() != 1 {
        return Err(rejected(&request.session_id, "ambiguous session owner"));
    }
    let owner = decode_session_row(&rows[0])?.session;
    if owner.requester_did == request.requester_did {
        return Ok(false);
    }
    if owner.behavior_id != request.behavior_id {
        return Err(rejected(
            &request.session_id,
            "control behavior differs from session owner",
        ));
    }
    let mut child = load_request(txn, &request.doc_id).await?;
    if child.request_id != request.request_id || child.requester_did != request.requester_did {
        return Err(rejected(
            &request.session_id,
            "claimed request differs from its physical control receipt",
        ));
    }
    let mut visited = std::collections::HashSet::new();
    loop {
        let doc_id = child
            .doc_id
            .as_deref()
            .context("control request omitted document ID")?;
        if !visited.insert(doc_id.to_owned()) {
            return Err(rejected(&request.session_id, "cyclic control ancestry"));
        }
        if child.agent_did.as_deref() != Some(owner.agent_did.as_str())
            || child.session_id.as_deref() != Some(owner.session_id.as_str())
            || child.behavior_id.as_deref() != Some(owner.behavior_id.as_str())
        {
            return Err(rejected(
                &request.session_id,
                "control ancestor is outside session owner/behavior",
            ));
        }
        verify_request_receipt_signature(&child).map_err(|e| rejected(&request.session_id, e))?;
        if child.requester_did == owner.requester_did {
            if child.admission_signer_did.as_deref()
                != owner
                    .requester_did
                    .as_deref()
                    .or(Some(owner.agent_did.as_str()))
            {
                return Err(rejected(
                    &request.session_id,
                    "session ancestor signer differs from its requester",
                ));
            }
            return Ok(true);
        }
        let parent_id = child
            .caused_by_parent_request_id
            .as_deref()
            .ok_or_else(|| {
                rejected(
                    &request.session_id,
                    "request has no authenticated session ancestry",
                )
            })?;
        verify_runtime_local_control_receipt(&child, &owner.agent_did, parent_id)
            .map_err(|e| rejected(&request.session_id, e))?;
        let parent_doc = child
            .caused_by_parent_request_doc_id
            .as_deref()
            .ok_or_else(|| rejected(&request.session_id, "control parent document is absent"))?;
        let parent = load_request(txn, parent_doc).await?;
        if parent.request_id != parent_id || parent.doc_id.as_deref() != Some(parent_doc) {
            return Err(rejected(
                &request.session_id,
                "control physical parent binding differs",
            ));
        }
        child = parent;
    }
}
