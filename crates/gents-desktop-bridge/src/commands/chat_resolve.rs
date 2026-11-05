//! Chat sends whose owner outcome is not confirmed.
//!
//! An ambiguous owner write may already have committed, so the result reports
//! `deliveryUnknown` with the original request identity instead of an error
//! that invites a fresh resend. A later send carrying `resolveRequestId`
//! reconciles that exact request rather than minting a new one.

use anyhow::Result;
use gents_desktop_core::client::{
    ClientCore, DeliveryUnconfirmed, SubmissionCleanupIncomplete, SubmissionNotDelivered,
    SubmissionOutcomeUnknown, SubmittedRequest,
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
    let error = match error.downcast::<SubmissionCleanupIncomplete>() {
        Ok(incomplete) => {
            let (mut result, error) = match incomplete {
                SubmissionCleanupIncomplete::Accepted { request, cause } => (
                    ChatSendResult::from(request),
                    crate::error::BridgeError::classify_transport_error(cause),
                ),
                SubmissionCleanupIncomplete::Rejected {
                    request,
                    cause,
                    rejection,
                } => {
                    let rejection = crate::error::BridgeError::classify_transport_error(rejection);
                    let cleanup = crate::error::BridgeError::classify_transport_error(cause);
                    let mut error = if rejection.code == crate::error::BridgeErrorCode::Unknown {
                        cleanup.clone()
                    } else {
                        rejection.clone()
                    };
                    error.message = format!(
                        "{}; saved-message cleanup failed: {}",
                        rejection.message, cleanup.message
                    );
                    (
                        ChatSendResult::with_outcome(request, ChatSendOutcome::NotDelivered),
                        error,
                    )
                }
            };
            result.delivery_error = Some(error);
            return Ok(result);
        }
        Err(error) => error,
    };
    let error = match error.downcast::<SubmissionNotDelivered>() {
        Ok(SubmissionNotDelivered::Rejected { request, cause }) => {
            let mut result = ChatSendResult::with_outcome(request, ChatSendOutcome::NotDelivered);
            result.delivery_error = cause.map(crate::error::BridgeError::classify_transport_error);
            return Ok(result);
        }
        Err(error) => error,
    };
    let error = match error.downcast::<SubmissionOutcomeUnknown>() {
        Ok(unknown) => {
            let delivery_error = unknown
                .cause
                .map(crate::error::BridgeError::classify_transport_error)
                .filter(|error| {
                    matches!(
                        error.code,
                        crate::error::BridgeErrorCode::StorageFull
                            | crate::error::BridgeErrorCode::PermissionDenied
                    )
                });
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
                agent_did: None,
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
                agent_did: None,
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
    fn rejected_recovery_surfaces_the_native_denial_with_the_original_identity() {
        let request = SubmittedRequest {
            request_id: "original-request".into(),
            session_id: "session".into(),
            node_did: "node".into(),
            agent_did: None,
        };
        let cause = gents::store::config_client::ConfigClientError::Database(
            gents_protocol::database_error::GraphqlFailure::new(
                "request", serde_json::json!([{ "message": "denied", "extensions": { "code": "PERMISSION_DENIED" } }]),
            ),
        ).into();
        let result = sent(Err(SubmissionNotDelivered::Rejected {
            request,
            cause: Some(cause),
        }
        .into()))
        .unwrap();
        assert_eq!(result.request_id, "original-request");
        assert_eq!(result.outcome, Some(ChatSendOutcome::NotDelivered));
        assert_eq!(
            result.delivery_error.unwrap().code,
            crate::error::BridgeErrorCode::PermissionDenied
        );
    }

    #[test]
    fn cleanup_failure_keeps_the_confirmed_delivery_receipt() {
        for accepted in [true, false] {
            let request = SubmittedRequest {
                request_id: "original-request".into(),
                session_id: "session".into(),
                node_did: "node".into(),
                agent_did: None,
            };
            let cause = gents::store::config_client::ConfigClientError::Database(
                    gents_protocol::database_error::GraphqlFailure::new(
                        "cleanup",
                        serde_json::json!([{ "message": "denied", "extensions": { "code": "PERMISSION_DENIED" } }]),
                    ),
                ).into();
            let incomplete = if accepted {
                SubmissionCleanupIncomplete::Accepted { request, cause }
            } else {
                SubmissionCleanupIncomplete::Rejected {
                    request,
                    cause,
                    rejection: anyhow::anyhow!("request rejected"),
                }
            };
            let result = sent(Err(incomplete.into())).unwrap();
            assert_eq!(result.request_id, "original-request");
            assert_eq!(
                result.outcome,
                (!accepted).then_some(ChatSendOutcome::NotDelivered)
            );
            assert_eq!(
                result.delivery_error.unwrap().code,
                crate::error::BridgeErrorCode::PermissionDenied
            );
        }
    }

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
            agent_did: Some("agent".into()),
        };
        let pending: gents_desktop_core::client::UnresolvedSubmission =
            serde_json::from_value(serde_json::json!({
                "request_id": "request", "session_id": "session", "node_did": "did:test:node",
                "mutation": create.graphql_mutation().unwrap(), "create": create,
                "result": receipt,
            }))
            .unwrap();
        let unknown = SubmissionOutcomeUnknown {
            request_id: "request".into(),
            session_id: "session".into(),
            pending: pending.clone(),
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
        let denial = gents::store::config_client::ConfigClientError::Database(
            gents_protocol::database_error::GraphqlFailure::new(
                "receipt read", serde_json::json!([{ "message": "denied", "extensions": { "code": "PERMISSION_DENIED" } }]),
            ),
        ).into();
        let unknown = SubmissionOutcomeUnknown {
            request_id: "request".into(),
            session_id: "session".into(),
            pending: pending.clone(),
            cause: Some(denial),
        };
        let denied = sent(Err(unknown.into())).unwrap();
        assert_eq!(denied.request_id, "request");
        assert_eq!(denied.outcome, Some(ChatSendOutcome::DeliveryUnknown));
        assert_eq!(
            denied.delivery_error.unwrap().code,
            crate::error::BridgeErrorCode::PermissionDenied
        );
        let unknown = SubmissionOutcomeUnknown {
            request_id: "request".into(),
            session_id: "session".into(),
            pending,
            cause: None,
        };
        let absent = sent(Err(unknown.into())).unwrap();
        assert_eq!(absent.outcome, Some(ChatSendOutcome::DeliveryUnknown));
        assert!(absent.delivery_error.is_none());
    }
}
