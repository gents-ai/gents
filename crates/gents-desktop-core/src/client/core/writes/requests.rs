use anyhow::{Context, Result};
use gents_protocol::operations::request::{
    MessageSendInput, ReissueKind, ReissueReceipt, RequestRef, RequestReissueInput,
};
use gents_protocol::row::AgentRequestRow;

use super::{ensure_peer_chat_ready, find_request_in_scope, observe_submitted_request, ClientCore};
use crate::client::mutations::SubmittedRequest;
use crate::error::request_operation_failure;

fn request_in_scope(
    rows: &[AgentRequestRow],
    request_id: &str,
    node_did: Option<&str>,
) -> Result<AgentRequestRow> {
    find_request_in_scope(rows, request_id, node_did)?
        .with_context(|| format!("request {request_id} not found"))
}

impl From<ReissueReceipt> for SubmittedRequest {
    fn from(receipt: ReissueReceipt) -> Self {
        Self {
            request_id: receipt.request_id,
            session_id: receipt.session_id,
            node_did: receipt.node_did,
            agent_did: receipt.agent_did,
        }
    }
}

impl ClientCore {
    pub async fn submit_request(
        &self,
        session_id: &str,
        node_did: &str,
        content: &str,
        agent_did: Option<&str>,
    ) -> Result<SubmittedRequest> {
        self.send_message(MessageSendInput {
            node_did: node_did.to_owned(),
            session_id: Some(session_id.to_owned()),
            agent_did: agent_did.map(str::to_owned),
            content: content.to_owned(),
            ..MessageSendInput::default()
        })
        .await
    }

    /// The optimistic send's one signed create (Lean `ClientOptimisticSend`):
    /// `input.request_id` is the identity the client picked, and a retry with
    /// the same identity is the same request.
    pub async fn send_message(&self, input: MessageSendInput) -> Result<SubmittedRequest> {
        let node_did = input.node_did.trim().to_owned();
        let peer_record = self.peer_record_for_chat_write(&node_did)?;
        ensure_peer_chat_ready(&node_did, peer_record.as_ref())?;
        crate::local_runtime::ensure_request_input_supported(peer_record.as_ref(), &input.input)
            .await?;
        let (signer, _, _) = self
            .request_authority(&node_did, peer_record.as_ref())
            .await?;
        let access = self.session_queue_access(&node_did)?;
        match gents::operations::request::send(&access, signer.as_ref(), input).await {
            Ok(receipt) => {
                let result = observe_submitted_request(
                    &self.store,
                    SubmittedRequest {
                        request_id: receipt.request_id,
                        session_id: receipt.session_id,
                        node_did: receipt.node_did,
                        agent_did: receipt.agent_did,
                    },
                    self.refresh_written_node(&node_did),
                )
                .await;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_submit",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => Err(self.record_mutation_error(
                "submit request",
                request_operation_failure(error),
            )),
        }
    }

    pub fn request_in_scope(
        &self,
        request_id: &str,
        node_did: Option<&str>,
    ) -> Result<AgentRequestRow> {
        request_in_scope(&self.store.snapshot().requests, request_id, node_did)
    }

    pub async fn retry_request(&self, parent: &AgentRequestRow) -> Result<SubmittedRequest> {
        let node_did = parent
            .node_did
            .as_deref()
            .context("retry parent has no node_did")?;
        let parent_ref = RequestRef {
            node_did: node_did.to_owned(),
            requester_did: parent.requester_did.clone(),
            request_id: parent.request_id.clone(),
        };
        self.reissue(
            ReissueKind::Retry,
            parent_ref,
            Some(&parent.input.clone().unwrap_or_default()),
        )
        .await
    }

    async fn reissue(
        &self,
        kind: ReissueKind,
        parent: RequestRef,
        parent_input: Option<&gents_protocol::request_input::RequestInput>,
    ) -> Result<SubmittedRequest> {
        let node_did = parent.node_did.clone();
        let peer_record = self.peer_record_for_chat_write(&node_did)?;
        ensure_peer_chat_ready(&node_did, peer_record.as_ref())?;
        if let Some(input) = parent_input {
            crate::local_runtime::ensure_request_input_supported(peer_record.as_ref(), input)
                .await?;
        }
        let (signer, _, _) = self
            .request_authority(&node_did, peer_record.as_ref())
            .await?;
        let parent_request_id = parent.request_id.clone();
        let result = gents_session::operations::request::reissue(
            &self.session_queue_access(&node_did)?,
            signer.as_ref(),
            RequestReissueInput {
                kind,
                parent,
                request_id: None,
            },
        )
        .await;
        let operation = format!("{} request", kind.as_str());
        match result {
            Ok(receipt) => {
                let result = observe_submitted_request(
                    &self.store,
                    receipt.into(),
                    self.refresh_written_node(&node_did),
                )
                .await;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = %format!("chat_{}", kind.as_str()),
                    row_id = %result.request_id,
                    parent_request_id = %parent_request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => {
                Err(self.record_mutation_error(&operation, request_operation_failure(error)))
            }
        }
    }
}
