//! Owner chat sends whose commit outcome is unknown.
//!
//! The signed request of an ambiguous owner write is retained in memory so a
//! later reconciliation replays identical bytes and never mints a new identity.
//! After a restart the payload is gone; the desktop UI keeps the identity, and
//! reconciliation then falls back to proving presence or absence by request id.

use anyhow::{Context, Result};
use chrono::Utc;

use super::super::super::mutations::{
    resolve_unresolved_submission, submitted_request_exists, SubmissionOutcomeUnknown,
    SubmittedRequest,
};
use super::{observe_submitted_request, ClientCore};

impl ClientCore {
    /// Keep the signed payload of an unknown-outcome send for identical replay.
    pub(super) fn retain_unresolved_submission(&self, error: anyhow::Error) -> anyhow::Error {
        if let Some(unknown) = error.downcast_ref::<SubmissionOutcomeUnknown>() {
            self.unresolved_submissions
                .lock()
                .expect("unresolved submission lock poisoned")
                .insert(unknown.request_id.clone(), unknown.pending.clone());
        }
        error
    }

    /// Reconcile an earlier send whose owner outcome was unknown.
    ///
    /// Returns `Ok(Some(_))` once the owner holds the original request,
    /// `Ok(None)` when the payload is no longer retained and the owner proves
    /// it never received the request (a fresh send is then safe), and
    /// `SubmissionOutcomeUnknown` again when the outcome still cannot be
    /// determined.
    pub async fn resolve_chat_submission(
        &self,
        node_did: &str,
        session_id: &str,
        request_id: &str,
    ) -> Result<Option<SubmittedRequest>> {
        let retained = self
            .unresolved_submissions
            .lock()
            .expect("unresolved submission lock poisoned")
            .get(request_id)
            .cloned();
        let resolved = match retained {
            Some(pending) => {
                anyhow::ensure!(
                    pending.node_did == node_did && pending.session_id == session_id,
                    "unresolved submission {request_id} belongs to another session"
                );
                let access = self.session_queue_access(&pending.node_did);
                match resolve_unresolved_submission(&access, &pending).await {
                    Ok(receipt) => {
                        self.unresolved_submissions
                            .lock()
                            .expect("unresolved submission lock poisoned")
                            .remove(request_id);
                        Some(receipt)
                    }
                    Err(error) => {
                        return Err(self.record_mutation_error("resolve chat submission", error))
                    }
                }
            }
            None if self
                .store
                .snapshot()
                .requests
                .iter()
                .any(|row| row.request_id == request_id) =>
            {
                Some(SubmittedRequest {
                    request_id: request_id.to_string(),
                    session_id: session_id.to_string(),
                    node_did: node_did.to_string(),
                    agent_id: None,
                })
            }
            None => {
                let peer_record = self
                    .peer_record_for_chat_write(node_did, Utc::now())
                    .await?;
                let (_, _, requester_did) = self
                    .request_authority(node_did, peer_record.as_ref())
                    .await?;
                let present = submitted_request_exists(
                    &self.session_queue_access(node_did),
                    node_did,
                    session_id,
                    &requester_did,
                    request_id,
                )
                .await
                .with_context(|| {
                    format!("checking whether earlier send {request_id} reached its owner")
                })?;
                present.then(|| SubmittedRequest {
                    request_id: request_id.to_string(),
                    session_id: session_id.to_string(),
                    node_did: node_did.to_string(),
                    agent_id: None,
                })
            }
        };
        let Some(receipt) = resolved else {
            return Ok(None);
        };
        let receipt = observe_submitted_request(&self.store, receipt, self.refresh_store()).await;
        self.clear_mutation_error();
        Ok(Some(receipt))
    }
}
