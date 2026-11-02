use anyhow::{Context, Result};
use chrono::Utc;
use gents_protocol::row::AgentRequestRow;

use super::{
    agent_id_for_write, ensure_peer_chat_ready_at, find_request_in_scope,
    observe_submitted_request, ClientCore,
};
use crate::client::mutations::{self, SubmitRequestOptions, SubmittedRequest};

fn request_in_scope(
    rows: &[AgentRequestRow],
    request_id: &str,
    node_did: Option<&str>,
) -> Result<AgentRequestRow> {
    find_request_in_scope(rows, request_id, node_did)?
        .with_context(|| format!("request {request_id} not found"))
}

impl ClientCore {
    pub async fn submit_request(
        &self,
        session_id: &str,
        node_did: &str,
        content: &str,
        agent_id: Option<&str>,
    ) -> Result<SubmittedRequest> {
        self.submit_request_with_options(
            session_id,
            node_did,
            content,
            agent_id,
            SubmitRequestOptions::default(),
        )
        .await
    }

    pub async fn submit_request_with_options(
        &self,
        session_id: &str,
        node_did: &str,
        content: &str,
        agent_id: Option<&str>,
        mut options: SubmitRequestOptions,
    ) -> Result<SubmittedRequest> {
        if let Some(intent) = options.caller_intent.as_ref() {
            anyhow::ensure!(
                intent.node_did == node_did,
                "caller submission belongs to another node"
            );
            if let Some(receipt) = self.recover_caller_submission(intent).await? {
                return Ok(receipt);
            }
        }
        let snapshot = self.store.snapshot();
        let peer_record = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer_record.as_ref(), Utc::now())?;
        let (signer, admission, requester_did) = self
            .request_authority(node_did, peer_record.as_ref())
            .await?;
        self.ensure_sender_owns_session(node_did, session_id, &requester_did)?;
        // Queue preparation captures assignment intent before signing; a later
        // replacement must not change an already prepared unassigned message.
        if options.input.queue.is_none() {
            options.input = self
                .prepare_goal_input(node_did, session_id, options.input)
                .await?;
        }
        crate::local_runtime::ensure_request_input_supported(peer_record.as_ref(), &options.input)
            .await?;
        let agent_id = agent_id_for_write(agent_id);
        match mutations::submit_request(
            &self.node,
            snapshot.as_ref(),
            &self.session_queue_access(node_did),
            session_id,
            node_did,
            &requester_did,
            signer.as_ref(),
            admission,
            content,
            agent_id.as_deref(),
            options,
        )
        .await
        {
            Ok(result) => {
                let result =
                    observe_submitted_request(&self.store, result, self.refresh_store()).await;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_submit",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.submit_failure(error)),
        }
    }

    /// Refuse a send into a session another requester owns before writing it.
    /// Core refuses such a request at claim time, and a refused request is
    /// never observed under the session's owner, so the send would otherwise
    /// wait for an observation that doesn't come. A session's owner is written
    /// once, so the snapshot's row is current; a session the snapshot doesn't
    /// hold is left to core's admission.
    fn ensure_sender_owns_session(
        &self,
        node_did: &str,
        session_id: &str,
        sender_did: &str,
    ) -> Result<()> {
        let store = self.store.snapshot();
        let Some(session) = store
            .sessions
            .iter()
            .find(|row| row.session_id == session_id && row.node_did == node_did)
        else {
            return Ok(());
        };
        match crate::client::query::session_unsendable_reason(session, sender_did) {
            Some(reason) => Err(crate::error::BridgeError::new(
                crate::error::BridgeErrorCode::SessionNotOwned,
                reason,
            )
            .into()),
            None => Ok(()),
        }
    }

    pub async fn resend_request(&self, stale_request_id: &str) -> Result<SubmittedRequest> {
        let selected_node_did = self.selected_node_did();
        self.resend_request_in_scope(stale_request_id, selected_node_did.as_deref())
            .await
    }

    pub fn request_in_scope(
        &self,
        request_id: &str,
        node_did: Option<&str>,
    ) -> Result<AgentRequestRow> {
        request_in_scope(&self.store.snapshot().requests, request_id, node_did)
    }

    pub async fn resend_request_in_scope(
        &self,
        stale_request_id: &str,
        node_did: Option<&str>,
    ) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        let stale = request_in_scope(&snapshot.requests, stale_request_id, node_did)?;
        let node_did = stale
            .node_did
            .as_deref()
            .context("stale request has no node_did")?;
        let peer_record = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer_record.as_ref(), Utc::now())?;
        let (signer, admission, requester_did) = self
            .request_authority(node_did, peer_record.as_ref())
            .await?;
        let result = mutations::resend_request(
            &self.node,
            snapshot.as_ref(),
            &self.session_queue_access(node_did),
            stale_request_id,
            node_did,
            &requester_did,
            signer.as_ref(),
            admission,
        )
        .await;
        match result {
            Ok(result) => {
                let result =
                    observe_submitted_request(&self.store, result, self.refresh_store()).await;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_resend",
                    row_id = %result.request_id,
                    stale_request_id = %stale_request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("resend request", error)),
        }
    }

    pub async fn interrupt_request(&self, request_id: &str) -> Result<()> {
        let snapshot = self.store.snapshot();
        let selected_node_did = self.selected_node_did();
        let mut requests = snapshot
            .requests
            .iter()
            .filter(|request| request.request_id == request_id)
            .filter(|request| {
                selected_node_did
                    .as_deref()
                    .is_none_or(|selected| request.node_did.as_deref() == Some(selected))
            });
        let request = requests
            .next()
            .with_context(|| format!("request {request_id} is absent from the selected node"))?;
        anyhow::ensure!(
            requests.next().is_none(),
            "request {request_id} is ambiguous across the selected node scope"
        );
        let node_did = request
            .node_did
            .as_deref()
            .context("request has no node_did")?;
        let peer_record = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer_record.as_ref(), Utc::now())?;
        match mutations::interrupt_request(&self.node, request_id).await {
            Ok(()) => {
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_interrupt",
                    row_id = %request_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("interrupt request", error)),
        }
    }

    pub async fn retry_request(&self, parent: &AgentRequestRow) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        let node_did = parent
            .node_did
            .as_deref()
            .context("retry parent has no node_did")?;
        let peer_record = self
            .peer_record_for_chat_write(node_did, Utc::now())
            .await?;
        ensure_peer_chat_ready_at(node_did, peer_record.as_ref(), Utc::now())?;
        crate::local_runtime::ensure_request_input_supported(
            peer_record.as_ref(),
            &parent.input.clone().unwrap_or_default(),
        )
        .await?;
        let (signer, admission, requester_did) = self
            .request_authority(node_did, peer_record.as_ref())
            .await?;
        let result = mutations::retry_request(
            &self.node,
            snapshot.as_ref(),
            parent,
            &requester_did,
            signer.as_ref(),
            admission,
        )
        .await;
        match result {
            Ok(result) => {
                let result =
                    observe_submitted_request(&self.store, result, self.refresh_store()).await;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_retry",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("retry request", error)),
        }
    }
}
