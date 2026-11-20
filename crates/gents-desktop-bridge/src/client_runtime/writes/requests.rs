use anyhow::{Context, Result};
use gents_observe::client_projection::query::find_request_in_scope;
use gents_protocol::operations::request::{
    MessageSendBody, MessageSendInput, MessageSendRoute, ReissueKind, RequestRef,
    RequestReissueInput,
};
use gents_protocol::row::AgentRequestRow;

use super::{ensure_runtime_connection, observe_submitted_request, ClientRuntime};
use gents::client_operations::SubmittedRequest;
use gents_store::client_error::request_operation_failure;

fn request_in_scope(
    rows: &[AgentRequestRow],
    request_id: &str,
    node_did: &str,
) -> Result<AgentRequestRow> {
    find_request_in_scope(rows, request_id, Some(node_did))?
        .with_context(|| format!("request {request_id} not found"))
}

impl ClientRuntime {
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
        if let (Some(request_id), Some(session_id)) = (&route.request_id, &route.session_id) {
            let acknowledged = gents_store::client_journal::outbox::lookup(
                &self.node,
                self.node_identity.did(),
                &route.node_did,
                request_id,
            )
            .await?
            .is_some_and(|record| !record.pending);
            let observed = self
                .stores
                .snapshot(&route.node_did)
                .requests
                .iter()
                .any(|row| {
                    row.request_id == *request_id
                        && row.session_id.as_ref() == Some(session_id)
                        && row.node_did.as_ref() == Some(&route.node_did)
                });
            let connected = self.stores.runtime_connection(&route.node_did)
                == Some(
                    gents_observe::client_projection::observe::RuntimeConnectionStatus::Connected,
                );
            if acknowledged || observed || connected {
                let access = self.session_queue_access(&route.node_did)?;
                if let Some(receipt) = gents_session::operations::request::recover_send(
                    &access,
                    &self.node_identity,
                    &route,
                )
                .await?
                {
                    return Ok(SubmittedRequest {
                        request_id: receipt.request_id,
                        session_id: receipt.session_id,
                        node_did: receipt.node_did,
                        agent_did: receipt.agent_did,
                    });
                }
            }
        }
        let body = message.await?;
        let steer = body.delivery == gents_protocol::operations::request::DeliveryChoice::Steer;
        let input = MessageSendInput {
            node_did: route.node_did,
            session_id: route.session_id,
            request_id: route.request_id,
            agent_did: body.agent_did,
            content: body.content,
            input: body.input,
            delivery: body.delivery,
            valid_until: body.valid_until,
            mailbox_item_id: body.mailbox_item_id,
        };
        let record = self.retain_chat(input.clone()).await?;
        if !record.pending {
            return Ok(SubmittedRequest {
                request_id: record.submission_id.clone(),
                node_did: record.node_did.clone(),
                session_id: record
                    .authored()?
                    .session_id()
                    .context("the retained submission has no session")?
                    .to_owned(),
                agent_did: input.agent_did,
            });
        }
        self.require_reachable(&record)?;
        let _delivery = self.delivery_lock.lock().await;
        if !steer {
            while let Some(next) = gents_store::client_journal::outbox::next(
                &self.node,
                self.node_identity.did(),
                &record.node_did,
                None,
            )
            .await?
            {
                let receipt = self.deliver_authored_record(&next).await?;
                if next.submission_id == record.submission_id {
                    return Ok(receipt);
                }
            }
        }
        self.deliver_authored_record(&record).await
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
        _parent_input: Option<&gents_protocol::request_input::RequestInput>,
    ) -> Result<SubmittedRequest> {
        let node_did = parent.node_did.clone();
        let peer_record = self.connection_for_write(&node_did)?;
        ensure_runtime_connection(&node_did, peer_record.as_ref())?;
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
                    SubmittedRequest {
                        request_id: receipt.request_id,
                        session_id: receipt.session_id,
                        node_did: receipt.node_did,
                        agent_did: receipt.agent_did,
                    },
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
