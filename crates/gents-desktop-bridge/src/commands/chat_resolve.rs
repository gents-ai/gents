//! Chat sends whose owner outcome is not confirmed.
//!
//! An ambiguous owner write may already have committed, so the result reports
//! `deliveryUnknown` with the original request identity instead of an error
//! that invites a fresh resend. A later send carrying `resolveRequestId`
//! reconciles that exact request rather than minting a new one.

use anyhow::Result;
use gents_desktop_core::client::{
    ClientCore, DeliveryUnconfirmed, SubmissionOutcomeUnknown, SubmittedRequest,
};

use crate::types::{ChatSendOutcome, ChatSendRequest, ChatSendResult};

/// The earlier send this request asks to reconcile, if any.
pub(super) fn resolve_target(request: &ChatSendRequest) -> Option<&str> {
    request
        .resolve_request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Report a submission, turning an unknown owner outcome into a typed result.
pub(super) fn sent(submitted: Result<SubmittedRequest>) -> Result<ChatSendResult> {
    let error = match submitted {
        Ok(receipt) => return Ok(receipt.into()),
        Err(error) => error,
    };
    let error = match error.downcast::<SubmissionOutcomeUnknown>() {
        Ok(unknown) => {
            return Ok(ChatSendResult::with_outcome(
                unknown.pending.result,
                ChatSendOutcome::DeliveryUnknown,
            ))
        }
        Err(error) => error,
    };
    match error.downcast::<DeliveryUnconfirmed>() {
        Ok(unconfirmed) => Ok(ChatSendResult::with_outcome(
            SubmittedRequest {
                request_id: unconfirmed.request_id,
                session_id: unconfirmed.session_id,
                node_did: unconfirmed.node_did,
                agent_id: None,
            },
            ChatSendOutcome::DeliveryUnknown,
        )),
        Err(error) => Err(error),
    }
}

/// Confirm or replay the original signed request instead of sending a new one.
pub(super) async fn resolve_chat_submission(
    core: &ClientCore,
    node_did: &str,
    request: &ChatSendRequest,
    request_id: &str,
) -> Result<ChatSendResult> {
    let session_id = request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("resolving an earlier send requires its session_id"))?;
    core.verify_submission_attachments(
        node_did,
        session_id,
        request_id,
        request.attachments.as_deref().unwrap_or_default(),
    )
    .await?;
    match core
        .resolve_chat_submission(node_did, session_id, request_id, &request.content)
        .await
    {
        Ok(Some(receipt)) => Ok(receipt.into()),
        Ok(None) => Ok(ChatSendResult::with_outcome(
            SubmittedRequest {
                request_id: request_id.to_string(),
                session_id: session_id.to_string(),
                node_did: node_did.to_string(),
                agent_id: None,
            },
            ChatSendOutcome::NotDelivered,
        )),
        Err(error) => sent(Err(error)),
    }
}
