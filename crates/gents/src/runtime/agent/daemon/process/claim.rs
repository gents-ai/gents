use tracing::Instrument;

use super::{
    request_failure_reason, AgentDaemon, AgentRequest, DefraStreamWriter, ProviderModel,
    RequestGuard, RequestLifecycle,
};
use crate::observe::runtime_trace::{
    record_current_claim_outcome, record_current_failure_class, record_current_request_outcome,
};
use crate::request::lifecycle::ClaimOutcome;
use crate::runtime::agent::worker_capacity::WorkerTicket;

impl<M> AgentDaemon<M>
where
    M: ProviderModel,
{
    pub(super) async fn claim_request(
        &mut self,
        request: &AgentRequest,
        lifecycle: &mut RequestLifecycle,
        stream_writer: &DefraStreamWriter,
        capacity: &mut Option<RequestGuard>,
        parked: bool,
    ) -> bool {
        let mut wake = None;
        let claim_result = async {
            if parked {
                let observed = if let Some(health) = &self.backend_health {
                    health
                        .healthy_observed_at(&self.node, &request.node_did, lifecycle.backend_id())
                        .await?
                } else {
                    None
                };
                let outcome = lifecycle.wake_for_backend(observed).await?;
                let resumed = outcome.resumed();
                wake = Some(outcome);
                Ok(if resumed {
                    ClaimOutcome::Claimed
                } else {
                    ClaimOutcome::Queued
                })
            } else {
                lifecycle.claim_with_identity().await
            }
        }
        .instrument(tracing::info_span!(
            "request.claim",
            request_id = %request.request_id,
            session_id = %request.session_id,
            node_did = %request.node_did,
            agent_did = %self.agent_config.agent_did,
        ))
        .await;

        match claim_result {
            Ok(ClaimOutcome::Claimed) => {
                record_current_claim_outcome("claimed");
                let generation = match lifecycle.execution_generation() {
                    Ok(generation) => generation.to_owned(),
                    Err(error) => {
                        record_current_request_outcome("worker_ticket_missing");
                        record_current_failure_class(&error);
                        self.finalize_failure_before_work(
                            lifecycle,
                            stream_writer,
                            &request_failure_reason(&error),
                            request,
                        )
                        .await;
                        return false;
                    }
                };
                let ticket = WorkerTicket::new(lifecycle.request().doc_id.clone(), generation);
                if let Err(error) = RequestGuard::bind_claim(capacity, ticket) {
                    let error = anyhow::Error::new(error);
                    record_current_request_outcome("worker_ticket_refused");
                    record_current_failure_class(&error);
                    self.finalize_failure_before_work(
                        lifecycle,
                        stream_writer,
                        &request_failure_reason(&error),
                        request,
                    )
                    .await;
                    return false;
                }
            }
            Ok(ClaimOutcome::Queued) => {
                record_current_claim_outcome("queued");
                record_current_request_outcome("queued");
                tracing::info!(
                    agent_did = %self.agent_config.agent_did,
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    agent_backend = lifecycle.backend_id(),
                    bound_backend = wake.as_ref().and_then(|wake| wake.bound_backend()),
                    "{}",
                    wake.as_ref().map_or(
                        "request queued behind an earlier same-session request",
                        |wake| wake.unclaimed_reason(),
                    )
                );
                return false;
            }
            Ok(ClaimOutcome::Interrupted) => {
                record_current_claim_outcome("interrupted");
                record_current_request_outcome("interrupted_pre_claim");
                tracing::info!(
                    target: super::super::TRACING_TARGET,
                    agent_did = %self.agent_config.agent_did,
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    cancellation_source = "pre_claim",
                    "request interrupted before claim"
                );
                return false;
            }
            Ok(ClaimOutcome::Expired) => {
                record_current_claim_outcome("expired");
                record_current_request_outcome("expired_pre_claim");
                tracing::info!(
                    target: super::super::TRACING_TARGET,
                    agent_did = %self.agent_config.agent_did,
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    cancellation_source = "stale_ttl",
                    "request expired (valid_until passed) before claim; marked dead"
                );
                return false;
            }
            Err(error) => {
                record_current_claim_outcome("error");
                let deterministic_rejection =
                    crate::request::lifecycle::is_claim_admission_error(&error);
                record_current_request_outcome(if deterministic_rejection {
                    "admission_rejected"
                } else {
                    "claim_error"
                });
                record_current_failure_class(&error);
                if deterministic_rejection {
                    tracing::warn!(
                        agent_did = %self.agent_config.agent_did,
                        request_id = %request.request_id,
                        error = %crate::request::failure_text::BoundedErrorChain(error.as_ref()),
                        "rejecting request with an invalid canonical admission binding"
                    );
                    if let Err(rejection_error) = lifecycle
                        .reject_admission(&request_failure_reason(&error))
                        .await
                    {
                        tracing::error!(
                            agent_did = %self.agent_config.agent_did,
                            request_id = %request.request_id,
                            error = %rejection_error,
                            "failed to persist request admission rejection"
                        );
                    }
                    return false;
                }
                tracing::warn!(
                    agent_did = %self.agent_config.agent_did,
                    request_id = %request.request_id,
                    error = %crate::request::failure_text::BoundedErrorChain(error.as_ref()),
                    "failed to claim request; leaving it pending for retry"
                );
                return false;
            }
        }
        true
    }

    pub(super) async fn reject_unroutable_agent(
        &mut self,
        request: &AgentRequest,
        lifecycle: &mut RequestLifecycle,
        stream_writer: &DefraStreamWriter,
    ) -> bool {
        let requested_agent_did = request.agent_did.as_str();
        if requested_agent_did != self.agent_config.agent_did {
            let error = anyhow::anyhow!(
                "request targets agent {} but runtime is serving agent {}",
                requested_agent_did,
                self.agent_config.agent_did
            );
            record_current_request_outcome("rejected_agent_mismatch");
            record_current_failure_class(&error);
            tracing::warn!(
                agent_did = %self.agent_config.agent_did,
                request_id = %request.request_id,
                session_id = %request.session_id,
                requested_agent_did = %requested_agent_did,
                "rejecting request for unroutable agent"
            );
            self.finalize_failure_before_work(
                lifecycle,
                stream_writer,
                &request_failure_reason(&error),
                request,
            )
            .await;
            return true;
        }
        false
    }
}
