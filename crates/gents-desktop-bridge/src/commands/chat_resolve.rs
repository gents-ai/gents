//! Chat sends whose owner outcome is not confirmed.
//!
//! An ambiguous owner write may already have committed, so the result reports
//! `deliveryUnknown` with the original request identity instead of an error
//! that invites a fresh resend. A later send carrying `resolveRequestId`
//! reconciles that exact request rather than minting a new one.

use anyhow::Result;
use gents_desktop_core::client::{
    ClientCore, DeliveryUnconfirmed, SubmissionNotDelivered, SubmissionOutcomeUnknown,
    SubmittedRequest,
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
    let error = match error.downcast::<SubmissionNotDelivered>() {
        Ok(rejected) => {
            return Ok(ChatSendResult::with_outcome(
                rejected.request,
                ChatSendOutcome::NotDelivered,
            ))
        }
        Err(error) => error,
    };
    let error = match error.downcast::<SubmissionOutcomeUnknown>() {
        Ok(unknown) => {
            let delivery_error = unknown
                .cause
                .as_ref()
                .filter(|cause| gents::graphql::is_storage_full(cause))
                .map(|_| crate::error::BridgeError::storage_full());
            let mut result = ChatSendResult::with_outcome(
                unknown.pending.result,
                ChatSendOutcome::DeliveryUnknown,
            );
            result.delivery_error = delivery_error;
            return Ok(result);
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

#[cfg(test)]
mod storage_failure_tests {
    use super::*;
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };

    #[test]
    fn uncertain_storage_delivery_keeps_identity_and_actionable_reason() {
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "request",
            "did:test:node",
            "did:test:node",
            "agent",
            "session",
            "retained message",
            "interactive",
            "2026-10-25T00:00:00Z",
            AgentRequestAdmissionRecord::local_self("did:test:node"),
        );
        create.admission.signature = vec![7; 64];
        let receipt = SubmittedRequest {
            request_id: "request".into(),
            session_id: "session".into(),
            node_did: "did:test:node".into(),
            agent_id: Some("agent".into()),
        };
        let pending = serde_json::from_value(serde_json::json!({
            "request_id": "request", "session_id": "session", "node_did": "did:test:node",
            "mutation": create.graphql_mutation().unwrap(), "create": create,
            "result": receipt,
        }))
        .unwrap();
        let unknown = SubmissionOutcomeUnknown {
            request_id: "request".into(),
            session_id: "session".into(),
            pending,
            cause: Some(anyhow::Error::new(
                gents_protocol::database_error::StorageFull,
            )),
        };
        let result = sent(Err(unknown.into())).unwrap();
        assert_eq!(result.request_id, "request");
        assert_eq!(result.session_id, "session");
        assert_eq!(result.outcome, Some(ChatSendOutcome::DeliveryUnknown));
        let encoded = serde_json::to_value(result).unwrap();
        assert_eq!(encoded["deliveryError"]["code"], "storageFull");
        assert_eq!(encoded["deliveryError"]["retryable"], false);
    }
}
