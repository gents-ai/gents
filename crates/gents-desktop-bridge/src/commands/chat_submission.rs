use anyhow::{Context, Result};
use gents_desktop_core::client::{
    ClientCore, SubmissionIntent, SubmissionReference, SubmittedRequest,
};

use crate::types::{
    ChatSendRequest, ChatSendResult, ChatSubmissionReceipt, ChatSubmissionRecovery,
};

/// Durable caller bindings use sorted object keys and omit absent options,
/// so wire-property or Rust-field order cannot change an unresolved operation.
pub(super) fn intent(
    core: &ClientCore,
    request: &ChatSendRequest,
) -> Result<Option<SubmissionIntent>> {
    let (request_id, client_did) =
        match (request.request_id.as_deref(), request.client_did.as_deref()) {
            (None, None) => return Ok(None),
            (Some(request_id), Some(client_did)) => (request_id, client_did),
            _ => anyhow::bail!(
                "caller submission requires its original request identity and client DID"
            ),
        };
    anyhow::ensure!(
        client_did == core.node_identity().did(),
        "caller submission belongs to another client identity"
    );
    anyhow::ensure!(
        request.resolve_request_id.is_none(),
        "a caller-bound submission uses its original command for recovery"
    );
    let mut command = serde_json::to_value(request)?;
    command.sort_all_objects();
    Ok(Some(core.caller_submission_intent(
        request.node_did.trim(),
        request_id,
        &serde_json::to_vec(&command)?,
    )?))
}

fn receipt(intent: &SubmissionIntent, reference: SubmissionReference) -> ChatSubmissionReceipt {
    ChatSubmissionReceipt {
        node_did: reference.request.node_did,
        request_id: reference.request.request_id,
        session_id: reference.request.session_id,
        agent_id: reference.request.agent_id,
        envelope_hash: intent.envelope_hash.clone(),
        signed_payload_hash: reference.signed_payload_hash,
    }
}

pub(super) async fn sent(
    core: &ClientCore,
    intent: &SubmissionIntent,
    submitted: Result<SubmittedRequest>,
) -> Result<ChatSendResult> {
    let mut result = super::chat_resolve::sent(submitted)?;
    let reference = core
        .caller_submission_reference(intent)
        .await?
        .context("caller submission reply has no retained receipt")?;
    anyhow::ensure!(
        reference.request.request_id == result.request_id
            && reference.request.session_id == result.session_id
            && reference.request.node_did == result.node_did
            && reference.request.agent_id == result.agent_id,
        "caller submission reply crossed its retained scope"
    );
    result.submission_receipt = Some(receipt(intent, reference));
    Ok(result)
}

pub async fn recover_chat_submission(
    core: &ClientCore,
    request: ChatSendRequest,
) -> Result<ChatSubmissionRecovery> {
    let intent = intent(core, &request)?
        .context("recovery requires the original caller request identity")?;
    match core.recover_caller_submission(&intent).await {
        Ok(None) => Ok(ChatSubmissionRecovery::Unconfirmed {
            node_did: intent.node_did,
            request_id: intent.request_id,
            requested_session_id: request.session_id,
        }),
        recovered => Ok(ChatSubmissionRecovery::Prepared {
            result: Box::new(
                sent(
                    core,
                    &intent,
                    recovered.and_then(|result| {
                        result.context("retained submission reply omitted its result")
                    }),
                )
                .await?,
            ),
        }),
    }
}

pub async fn acknowledge_chat_submission(
    core: &ClientCore,
    receipt: ChatSubmissionReceipt,
) -> Result<()> {
    let intent = SubmissionIntent {
        client_did: core.node_identity().did().to_owned(),
        node_did: receipt.node_did.clone(),
        request_id: receipt.request_id.clone(),
        envelope_hash: receipt.envelope_hash,
    };
    let reference = SubmissionReference {
        request: SubmittedRequest {
            node_did: receipt.node_did,
            request_id: receipt.request_id,
            session_id: receipt.session_id,
            agent_id: receipt.agent_id,
        },
        signed_payload_hash: receipt.signed_payload_hash,
    };
    core.acknowledge_caller_submission(&intent, &reference)
        .await
}

#[cfg(test)]
#[path = "chat_submission/tests.rs"]
mod tests;
