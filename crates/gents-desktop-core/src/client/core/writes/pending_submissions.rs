//! Owner chat sends whose commit outcome is unknown.
//!
//! The signed request of an ambiguous owner write is retained locally so a
//! later reconciliation replays identical bytes and never mints a new identity.
//! A restart still permits exact replay. Older sends with no retained payload
//! remain unresolved when the owner cannot prove delivery.

use anyhow::Result;

use super::super::super::mutations::{
    reconcile_unretained_submission, resolve_unresolved_submission, SubmissionAttemptError,
    SubmissionError, SubmittedRequest,
};
use super::super::super::mutations::{
    submission_delivery, submission_journal, SubmissionIntent, SubmissionReference,
};
use super::{observe_submitted_request, ClientCore};

impl ClientCore {
    /// Bind the complete authored command before deriving session, queue or
    /// Goal inputs. The actor is the existing local node DID, never wire input.
    pub fn caller_submission_intent(
        &self,
        node_did: &str,
        request_id: &str,
        authored_command: &[u8],
    ) -> Result<SubmissionIntent> {
        anyhow::ensure!(
            !node_did.trim().is_empty() && !request_id.trim().is_empty(),
            "caller submission requires a node and canonical request identity"
        );
        Ok(SubmissionIntent::new(
            self.node_identity().did().to_owned(),
            node_did.to_owned(),
            request_id.to_owned(),
            authored_command,
        ))
    }

    /// Recovery retrieves only the original authored binding; it cannot supply
    /// replacement input or reinterpret a record owned by another runtime.
    pub async fn retained_caller_submission_intent(
        &self,
        node_did: &str,
        request_id: &str,
    ) -> Result<Option<SubmissionIntent>> {
        anyhow::ensure!(
            !node_did.trim().is_empty() && !request_id.trim().is_empty(),
            "caller recovery requires a node and canonical request identity"
        );
        Ok(submission_journal::caller::lookup(
            &self.node,
            self.node_identity().did(),
            node_did,
            request_id,
        )
        .await?
        .map(|record| record.intent))
    }

    fn ensure_caller_submission_actor(&self, intent: &SubmissionIntent) -> Result<()> {
        anyhow::ensure!(
            intent.client_did == self.node_identity().did(),
            "caller submission belongs to another client identity"
        );
        Ok(())
    }

    /// Missing preparation is not proof that a prior outer call did not run.
    /// Callers resolving an ambiguous RPC must preserve their original identity
    /// even when this returns `None`; only a new authored operation may prepare.
    /// A late recovery may belong to another chat, so it preserves selection.
    pub async fn recover_caller_submission(
        &self,
        intent: &SubmissionIntent,
    ) -> std::result::Result<Option<SubmittedRequest>, SubmissionError> {
        self.ensure_caller_submission_actor(intent)?;
        let Some(record) = submission_journal::caller::recover(&self.node, intent).await? else {
            return Ok(None);
        };
        let access = if record.needs_reconciliation() {
            let now = chrono::Utc::now();
            let peer = self.peer_record_for_chat_write(&intent.node_did, now).await;
            match peer {
                Ok(Some(peer))
                    if super::ensure_peer_chat_ready_at(&intent.node_did, Some(&peer), now)
                        .is_ok() =>
                {
                    let capability =
                        gents::store::config_client::ClientSigningCapability::from_ed25519_key(
                            self.node_identity.private_key_bytes(),
                        )
                        .map_err(anyhow::Error::from)?;
                    crate::local_runtime::operator_endpoint(&peer, &capability).map(gents::store::config_client::ConfigAccess::Graphql)
                }
                _ => None,
            }
        } else {
            None
        };
        let receipt = submission_delivery::reconcile(&self.node, access.as_ref(), record).await?;
        self.store.request_resync(Some(&receipt.node_did));
        Ok(Some(receipt))
    }

    pub async fn caller_submission_reference(
        &self,
        intent: &SubmissionIntent,
    ) -> Result<Option<SubmissionReference>> {
        self.ensure_caller_submission_actor(intent)?;
        Ok(submission_journal::caller::recover(&self.node, intent)
            .await?
            .map(|record| record.reference))
    }

    pub async fn acknowledge_caller_submission(
        &self,
        intent: &SubmissionIntent,
        reference: &SubmissionReference,
    ) -> Result<()> {
        self.ensure_caller_submission_actor(intent)?;
        submission_journal::caller::update(&self.node, intent, reference, None).await?;
        Ok(())
    }

    pub async fn verify_submission_attachments(
        &self,
        node_did: &str,
        session_id: &str,
        request_id: &str,
        attachments: &[gents_protocol::request_input::RequestAttachment],
    ) -> Result<()> {
        let retained = super::super::super::mutations::submission_journal::load(
            &self.node,
            self.node_identity.did(),
            request_id,
        )
        .await?;
        if let Some(pending) = retained {
            anyhow::ensure!(
                pending.node_did == node_did && pending.session_id == session_id,
                "The retained request belongs to another session"
            );
            anyhow::ensure!(
                pending.create.input.attachments == attachments,
                "The retained request contains different attachments"
            );
        } else if let Ok(row) = self.request_in_scope(request_id, Some(node_did)) {
            anyhow::ensure!(
                row.session_id.as_deref() == Some(session_id),
                "The retained request belongs to another session"
            );
            anyhow::ensure!(
                row.input
                    .as_ref()
                    .map(|i| i.attachments.as_slice())
                    .unwrap_or_default()
                    == attachments,
                "The retained request contains different attachments"
            );
        } else {
            anyhow::ensure!(attachments.is_empty(), "Attachment delivery is still unknown; reconnect to recover the original request before retrying");
        }
        Ok(())
    }

    /// Map a submit failure, keeping the signed payload of an unknown-outcome
    /// send for identical replay. An unknown outcome is not recorded as a
    /// mutation failure: the request may have been delivered.
    pub(super) fn submit_failure(&self, error: SubmissionError) -> SubmissionError {
        match &error {
            SubmissionError::Cleanup(_) => {
                self.record_mutation_error_message("submission cleanup", &error)
            }
            SubmissionError::Unknown(_) => {}
            _ => self.record_mutation_error_message("submit request", &error),
        }
        error
    }

    /// Reconcile an earlier send whose owner outcome was unknown.
    ///
    /// Returns `Ok(Some(_))` once the owner holds the original request, and
    /// `Ok(None)` only when the owner definitely rejected the retained,
    /// byte-identical signed request. `SubmissionOutcomeUnknown` (payload
    /// retained) or `DeliveryUnconfirmed` (payload no longer retained) is
    /// returned when the outcome still cannot be determined.
    pub async fn resolve_chat_submission(
        &self,
        node_did: &str,
        session_id: &str,
        request_id: &str,
        content: &str,
    ) -> std::result::Result<Option<SubmittedRequest>, SubmissionError> {
        let retained = super::super::super::mutations::submission_journal::load(
            &self.node,
            self.node_identity.did(),
            request_id,
        )
        .await?;
        let resolved = match retained {
            Some(pending) => {
                if pending.node_did != node_did || pending.session_id != session_id {
                    return Err(anyhow::anyhow!(
                        "unresolved submission {request_id} belongs to another session"
                    )
                    .into());
                }
                // Mailbox answers carry no composer content; they are keyed by
                // their mailbox item instead.
                if !pending.accepts_composer_content(content) {
                    return Err(anyhow::anyhow!(
                        "unresolved submission {request_id} is a different message"
                    )
                    .into());
                }
                let access = self.session_queue_access(&pending.node_did)?;
                match resolve_unresolved_submission(&access, &pending).await {
                    Ok(receipt) => {
                        let cleanup = super::super::super::mutations::submission_journal::forget(
                            &self.node,
                            self.node_identity.did(),
                            request_id,
                        )
                        .await;
                        Some(super::super::super::mutations::report_submission_cleanup(
                            &pending,
                            Ok(receipt),
                            cleanup,
                        )?)
                    }
                    Err(SubmissionAttemptError::Unknown(error)) => return Err(error.into()),
                    // The receipt proved absence and the owner definitely
                    // rejected the identical replay, so it holds nothing with
                    // this identity.
                    Err(SubmissionAttemptError::Rejected(error)) => {
                        let cleanup = super::super::super::mutations::submission_journal::forget(
                            &self.node,
                            self.node_identity.did(),
                            request_id,
                        )
                        .await;
                        return Err(super::super::super::mutations::report_submission_rejection(
                            &pending,
                            error.into(),
                            cleanup,
                        ));
                    }
                }
            }
            None if self.store.snapshot().requests.iter().any(|row| {
                row.request_id == request_id
                    && row.node_did.as_deref() == Some(node_did)
                    && row.session_id.as_deref() == Some(session_id)
            }) =>
            {
                Some(SubmittedRequest {
                    request_id: request_id.to_string(),
                    session_id: session_id.to_string(),
                    node_did: node_did.to_string(),
                    agent_did: None,
                })
            }
            None => Some(
                reconcile_unretained_submission(
                    &self.session_queue_access(node_did)?,
                    node_did,
                    session_id,
                    request_id,
                )
                .await?,
            ),
        };
        let Some(receipt) = resolved else {
            return Ok(None);
        };
        let receipt = observe_submitted_request(&self.store, receipt, self.refresh_store()).await;
        self.clear_mutation_error();
        Ok(Some(receipt))
    }
}
