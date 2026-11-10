use anyhow::{Context, Result};
use gents_protocol::row::AgentRequestRow;

use super::{
    acknowledge_submitted_request, agent_did_for_write, ensure_peer_chat_ready,
    find_request_in_scope, observe_submitted_request, ClientCore,
};
use crate::client::mutations::{self, SendMessage, SubmitRequestOptions, SubmittedRequest};

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
            agent_did,
            SubmitRequestOptions::default(),
            std::future::ready(Ok(SendMessage {
                content: content.to_owned(),
                ..SendMessage::default()
            })),
        )
        .await
    }

    /// The optimistic send's one signed create (Lean `ClientOptimisticSend`):
    /// `options.request_id` is the identity the client picked, and a retry
    /// with the same identity is the same request. Its first step asks the
    /// runtime for the request it already holds under that identity (Lean
    /// `ClientOptimisticSend.held_resend_never_refused`); that needs only the
    /// route to the runtime and the requester scope. A held send confirms
    /// before `message` is prepared and before any check a new send must pass,
    /// so it confirms even when its question has closed or its folder is gone.
    /// `message` brings the request's input; `options.input` is not read.
    pub async fn submit_request_with_options<M>(
        &self,
        session_id: &str,
        node_did: &str,
        agent_did: Option<&str>,
        mut options: SubmitRequestOptions,
        message: M,
    ) -> Result<SubmittedRequest>
    where
        M: std::future::Future<Output = Result<SendMessage>>,
    {
        let peer_record = self.peer_record_for_chat_write(node_did)?;
        ensure_peer_chat_ready(node_did, peer_record.as_ref())?;
        let access = self.session_queue_access(node_did)?;
        let requester_did = gents::session::client_requester_scope(
            &access,
            node_did,
            session_id,
            self.node_identity.did(),
        )
        .await?;
        if let Some(request_id) = options.request_id.as_deref() {
            let scope = gents::request::lifecycle::ClientRequestScope {
                node_did,
                session_id,
                requester_did: &requester_did,
                request_id,
            };
            let held = mutations::held_request(&access, scope)
                .await
                .map_err(|error| self.record_mutation_error("submit request", error))?;
            if let Some(held) = held {
                acknowledge_submitted_request(&self.store, &held);
                self.clear_mutation_error();
                return Ok(held);
            }
        }
        let SendMessage { content, input } = message.await?;
        options.input = input;
        let snapshot = self.store.snapshot();
        let (signer, admission, _) = self
            .request_authority(node_did, peer_record.as_ref())
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
        match mutations::submit_new_request(
            snapshot.as_ref(),
            &access,
            session_id,
            node_did,
            &requester_did,
            signer.as_ref(),
            admission,
            &content,
            agent_did.as_deref(),
            options,
        )
        .await
        {
            Ok(result) => {
                acknowledge_submitted_request(&self.store, &result);
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_submit",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error("submit request", error)),
        }
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
        // The store need not hold the stale request; with a node scope the
        // resend reads it from that runtime by id.
        let held = find_request_in_scope(&snapshot.requests, stale_request_id, node_did)?;
        let node_did = match held.as_ref() {
            Some(stale) => stale
                .node_did
                .as_deref()
                .context("stale request has no node_did")?,
            None => node_did.with_context(|| format!("request {stale_request_id} not found"))?,
        };
        let peer_record = self.peer_record_for_chat_write(node_did)?;
        ensure_peer_chat_ready(node_did, peer_record.as_ref())?;
        let (signer, admission, _) = self
            .request_authority(node_did, peer_record.as_ref())
            .await?;
        let requester_did = held
            .as_ref()
            .map(|stale| {
                stale
                    .requester_did
                    .as_deref()
                    .context("stale request has no session requester provenance")
            })
            .transpose()?;
        let result = mutations::resend_request(
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
                let node = result.node_did.clone();
                let result = observe_submitted_request(
                    &self.store,
                    result,
                    self.refresh_written_node(&node),
                )
                .await;
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

    pub async fn retry_request(&self, parent: &AgentRequestRow) -> Result<SubmittedRequest> {
        let snapshot = self.store.snapshot();
        let node_did = parent
            .node_did
            .as_deref()
            .context("retry parent has no node_did")?;
        let peer_record = self.peer_record_for_chat_write(node_did)?;
        ensure_peer_chat_ready(node_did, peer_record.as_ref())?;
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
                let node = result.node_did.clone();
                let result = observe_submitted_request(
                    &self.store,
                    result,
                    self.refresh_written_node(&node),
                )
                .await;
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
