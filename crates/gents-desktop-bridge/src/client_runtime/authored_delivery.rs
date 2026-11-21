use super::ClientRuntime;
use anyhow::Result;
use gents_observe::client_projection::observe::RuntimeConnectionStatus;
use gents_protocol::{
    authored_submission::AuthoredSubmission,
    operations::request::{MessageSendInput, RequestOperationError},
};
use gents_store::client_journal::outbox;
use std::sync::{atomic::Ordering, Arc};

#[derive(Debug, thiserror::Error)]
#[error("Your submission is saved on this client and will be sent when this runtime reconnects.")]
pub struct SavedForDelivery {
    pub node_did: String,
    pub submission_id: String,
    pub session_id: String,
    pub agent_did: Option<String>,
}

impl SavedForDelivery {
    pub fn in_error<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        gents_protocol::error_chain::find_source::<Self>(error)
    }
}

impl ClientRuntime {
    pub fn start_authored_delivery(self: &Arc<Self>) {
        if self.delivery_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let owner = Arc::downgrade(self);
        let mut updates = self.store_updates();
        let mut connections = self.runtime_connection_updates();
        let cancel = self.delivery_cancel.clone();
        tokio::spawn(async move {
            let mut observed: rapidhash::RapidHashMap<
                String,
                Option<(RuntimeConnectionStatus, u64)>,
            > = Default::default();
            loop {
                let Some(core) = owner.upgrade() else {
                    break;
                };
                for connection in core.runtime_connections() {
                    let observation = core
                        .stores()
                        .runtime_connection_observation(&connection.node_did);
                    let status = observation.map(|(status, _)| status);
                    let changed = observed.insert(connection.node_did.clone(), observation)
                        != Some(observation);
                    if changed
                        && matches!(
                            status,
                            Some(
                                RuntimeConnectionStatus::Connected
                                    | RuntimeConnectionStatus::Refused
                            )
                        )
                        && connection.access_request.is_some()
                    {
                        if let Err(error) = core.runtime_access_status(&connection.node_did).await {
                            tracing::debug!(node_did = %connection.node_did, error = %format!("{error:#}"), "client access status is unavailable");
                        }
                    }
                    if changed && status == Some(RuntimeConnectionStatus::Connected) {
                        if let Some(endpoint) =
                            core.operator_observation_graphql(&connection.node_did)
                        {
                            core.owed
                                .settle(&connection, endpoint, core.stores().clone());
                        }
                        if let Err(error) = core.deliver_authored_for(&connection.node_did).await {
                            tracing::warn!(node_did = %connection.node_did, error = %format!("{error:#}"), "authored delivery remains retained");
                        }
                    }
                }
                drop(core);
                tokio::select! {
                    () = cancel.cancelled() => break,
                    result = updates.changed() => if result.is_err() { break; },
                    result = connections.changed() => if result.is_err() { break; },
                }
            }
        });
    }

    pub async fn deliver_authored_for(&self, node_did: &str) -> Result<()> {
        let _delivery = self.delivery_lock.lock().await;
        while let Some(record) =
            outbox::next(&self.node, self.node_identity.did(), node_did, None).await?
        {
            tokio::select! {
                () = self.delivery_cancel.cancelled() => return Ok(()),
                result = self.deliver_authored_record(&record) => { result?; }
            }
        }
        Ok(())
    }

    pub(super) async fn deliver_authored_record(
        &self,
        record: &outbox::Record,
    ) -> Result<gents::client_operations::SubmittedRequest> {
        let authored = record.authored()?;
        match authored {
            AuthoredSubmission::Chat(input) => {
                let access = self.session_queue_access(&record.node_did)?;
                let receipt = if let Some(prepared) = &record.prepared {
                    let create = serde_json::from_str(prepared)?;
                    gents_session::operations::request::send_prepared(
                        &access,
                        &self.node_identity,
                        &create,
                    )
                    .await?
                } else {
                    let (route, body) = input.into_parts();
                    let prepared_access = access.clone();
                    gents_session::operations::request::send_retaining(
                        &access,
                        &self.node_identity,
                        route,
                        |node, session| async move {
                            let mut body = body;
                            if body.input.goal_assignment.is_none() {
                                body.input = gents_automation::goal::prepare_client_goal_input(
                                    &prepared_access,
                                    &self.node_identity,
                                    &node,
                                    &session,
                                    body.input,
                                )
                                .await
                                .map_err(RequestOperationError::passthrough)?;
                            }
                            Ok(body)
                        },
                        |create| async move {
                            let bytes = serde_json::to_string(&create)
                                .map_err(RequestOperationError::passthrough_error)?;
                            outbox::prepare(&self.node, record, &bytes)
                                .await
                                .map_err(RequestOperationError::passthrough_error)?;
                            Ok(())
                        },
                    )
                    .await?
                };
                anyhow::ensure!(
                    receipt.node_did == record.node_did
                        && receipt.request_id == record.submission_id,
                    "runtime receipt crossed the authored binding"
                );
                outbox::acknowledge(&self.node, record, true).await?;
                let receipt = gents::client_operations::SubmittedRequest {
                    request_id: receipt.request_id,
                    session_id: receipt.session_id,
                    node_did: receipt.node_did,
                    agent_did: receipt.agent_did,
                };
                self.stores
                    .ensure(&record.node_did)
                    .set_focused_request_id(Some(receipt.request_id.clone()));
                self.stores.ensure(&record.node_did).request_resync();
                Ok(receipt)
            }
            AuthoredSubmission::Goal(input) => {
                let receipt = self.publish_authored_goal(record).await?;
                anyhow::ensure!(
                    receipt.command_id == record.submission_id,
                    "Goal receipt crossed the authored binding"
                );
                outbox::acknowledge(&self.node, record, true).await?;
                self.stores.ensure(&record.node_did).request_resync();
                Ok(gents::client_operations::SubmittedRequest {
                    request_id: record.submission_id.clone(),
                    session_id: input.session_id,
                    node_did: input.node_did,
                    agent_did: Some(receipt.agent_did),
                })
            }
        }
    }

    pub(super) async fn retain_chat(&self, mut input: MessageSendInput) -> Result<outbox::Record> {
        if !input.input.has_authored_input(&input.content) {
            return Err(RequestOperationError::ContentRequired.into());
        }
        let nonblank = |value: Option<String>| {
            value
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let request_id =
            nonblank(input.request_id.take()).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        input.session_id = Some(gents_protocol::authored_submission::authored_session_id(
            &request_id,
            nonblank(input.session_id.take()).as_deref(),
        ));
        input.request_id = Some(request_id);
        super::writes::ensure_runtime_connection(
            &input.node_did,
            self.connection_for_write(&input.node_did)?.as_ref(),
        )?;
        outbox::reserve(
            &self.node,
            self.node_identity.did(),
            &AuthoredSubmission::Chat(Box::new(input)),
        )
        .await
        .map_err(Into::into)
    }

    pub(super) fn require_reachable(&self, record: &outbox::Record) -> Result<()> {
        if matches!(
            self.stores.runtime_connection(&record.node_did),
            None | Some(
                RuntimeConnectionStatus::Connecting
                    | RuntimeConnectionStatus::Disconnected
                    | RuntimeConnectionStatus::NoEndpoint
            )
        ) {
            let authored = record.authored()?;
            let agent_did = match &authored {
                AuthoredSubmission::Chat(input) => input.agent_did.clone(),
                AuthoredSubmission::Goal(input) => input.agent_did.clone(),
            };
            return Err(SavedForDelivery {
                agent_did,
                node_did: record.node_did.clone(),
                submission_id: record.submission_id.clone(),
                session_id: record
                    .authored()?
                    .session_id()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("the retained submission has no session"))?,
            }
            .into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod native;
