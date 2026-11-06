use anyhow::{Context, Result};
use chrono::Utc;
use gents_protocol::row::AgentRequestRow;

use super::{
    agent_did_for_write, ensure_peer_chat_ready_at, find_request_in_scope,
    observe_submitted_request, ClientCore,
};
use crate::client::mutations::{self, SubmissionError, SubmitRequestOptions, SubmittedRequest};

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
        agent_did: Option<&str>,
    ) -> Result<SubmittedRequest> {
        self.submit_request_with_options(
            session_id,
            node_did,
            content,
            agent_did,
            SubmitRequestOptions::default(),
        )
        .await
        .map_err(Into::into)
    }

    pub async fn submit_request_with_options(
        &self,
        session_id: &str,
        node_did: &str,
        content: &str,
        agent_did: Option<&str>,
        mut options: SubmitRequestOptions,
    ) -> std::result::Result<SubmittedRequest, SubmissionError> {
        if let Some(intent) = options.caller_intent.as_ref() {
            if intent.node_did != node_did {
                return Err(anyhow::anyhow!("caller submission belongs to another node").into());
            }
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
        let requester_did = gents::session::client_requester_scope(
            &self.session_queue_access(node_did)?,
            node_did,
            session_id,
            &requester_did,
        )
        .await?;
        // Queue preparation captures assignment intent before signing; a later
        // replacement must not change an already prepared unassigned message.
        if options.input.queue.is_none() {
            options.input = self
                .prepare_goal_input(node_did, session_id, options.input)
                .await?;
        }
        crate::local_runtime::ensure_request_input_supported(peer_record.as_ref(), &options.input)
            .await?;
        let agent_did = agent_did_for_write(agent_did);
        match mutations::submit_request(
            &self.node,
            snapshot.as_ref(),
            &self.session_queue_access(node_did)?,
            session_id,
            node_did,
            &requester_did,
            signer.as_ref(),
            admission,
            content,
            agent_did.as_deref(),
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
        let (signer, admission, _) = self
            .request_authority(node_did, peer_record.as_ref())
            .await?;
        let requester_did = stale
            .requester_did
            .as_deref()
            .context("stale request has no session requester provenance")?;
        let result = mutations::resend_request(
            &self.node,
            snapshot.as_ref(),
            &self.session_queue_access(node_did)?,
            stale_request_id,
            node_did,
            requester_did,
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
        let request_doc_id = request
            .doc_id
            .as_deref()
            .context("request has no physical identity")?;
        match gents::request::interrupt::interrupt_request_by_doc_id_with_access(
            &self.session_queue_access(node_did)?,
            request_doc_id,
            node_did,
            request.requester_did.as_deref(),
        )
        .await
        {
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
        let (signer, admission, _) = self
            .request_authority(node_did, peer_record.as_ref())
            .await?;
        let requester_did = parent
            .requester_did
            .as_deref()
            .context("retry parent has no session requester provenance")?;
        let result = mutations::retry_request(
            &self.session_queue_access(node_did)?,
            snapshot.as_ref(),
            parent,
            requester_did,
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
