use anyhow::{Context, Result};
use gents_protocol::operations::request::{
    MessageSendBody, MessageSendInput, MessageSendRoute, ReissueKind, ReissueReceipt, RequestRef,
    RequestReissueInput,
};
use gents_protocol::row::AgentRequestRow;

use super::{
    acknowledge_submitted_request, ensure_peer_chat_ready, find_request_in_scope,
    observe_submitted_request, ClientCore,
};
use crate::client::mutations::SubmittedRequest;
use crate::error::request_operation_failure;

fn request_in_scope(
    rows: &[AgentRequestRow],
    request_id: &str,
    node_did: &str,
) -> Result<AgentRequestRow> {
    find_request_in_scope(rows, request_id, Some(node_did))?
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
        let (route, body) = input.into_parts();
        self.send_message_with(route, std::future::ready(Ok(body)))
            .await
    }

    pub async fn send_message_with<M>(
        &self,
        route: MessageSendRoute,
        message: M,
    ) -> Result<SubmittedRequest>
    where
        M: std::future::Future<Output = Result<MessageSendBody>>,
    {
        let node_did = route.node_did.trim().to_owned();
        let peer_record = self.peer_record_for_chat_write(&node_did)?;
        ensure_peer_chat_ready(&node_did, peer_record.as_ref())?;
        let (signer, _, _) = self
            .request_authority(&node_did, peer_record.as_ref())
            .await?;
        let access = self.session_queue_access(&node_did)?;
        let message = async {
            let body = message
                .await
                .map_err(gents_protocol::operations::request::RequestOperationError::passthrough)?;
            crate::local_runtime::ensure_request_input_supported(peer_record.as_ref(), &body.input)
                .await
                .map_err(gents_protocol::operations::request::RequestOperationError::passthrough)?;
            Ok(body)
        };
        match gents::operations::request::send_with(&access, signer.as_ref(), route, message).await
        {
            Ok(receipt) => {
                let result = SubmittedRequest {
                    request_id: receipt.request_id,
                    session_id: receipt.session_id,
                    node_did: receipt.node_did,
                    agent_did: receipt.agent_did,
                };
                acknowledge_submitted_request(&self.stores.ensure(&node_did), &result);
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    action = "chat_submit",
                    row_id = %result.request_id,
                    "desktop write saved"
                );
                Ok(result)
            }
            Err(error) => {
                Err(self.record_mutation_error("submit request", request_operation_failure(error)))
            }
        }
    }

    /// The request as `node_did`'s store shows it.
    pub fn request_in_scope(&self, request_id: &str, node_did: &str) -> Result<AgentRequestRow> {
        request_in_scope(
            &self.stores.snapshot(node_did).requests,
            request_id,
            node_did,
        )
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
                    &self.stores.ensure(&node_did),
                    receipt.into(),
                    self.refresh_node(&node_did),
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
