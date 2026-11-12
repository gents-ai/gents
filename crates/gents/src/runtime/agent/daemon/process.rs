use std::time::Duration;

use anyhow::Result;

use self::finish::OwnedWorkEnd;
use super::owned_work::OwnedWorkOutcome;
use super::terminal::request_failure_reason;
use super::{
    permits_natural_finish, request, verify_request_at_claim_boundary, AgentDaemon,
    RequestInterruptionSource,
};
use crate::inference::llm::rig_compat::ProviderModel;
use crate::observe::runtime_trace::record_current_failure_class;
use crate::request::lifecycle::RequestLifecycle;
use crate::request::streaming::DefraStreamWriter;
use crate::runtime::agent::worker_capacity::RequestGuard;
use gents_protocol::row::AgentRequest;

mod claim;
mod finish;
mod sealed;

impl<M> AgentDaemon<M>
where
    M: ProviderModel,
{
    #[cfg(test)]
    pub(in crate::runtime::agent) async fn process_request(
        &mut self,
        request: AgentRequest,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        Box::pin(self.process_request_with_capacity(request, None, shutdown)).await
    }

    /// `capacity` is the post-dequeue worker permit; this call owns it until
    /// it returns, bound to the claim's execution generation once one exists.
    pub(in crate::runtime::agent) async fn process_request_with_capacity(
        &mut self,
        request: AgentRequest,
        mut capacity: Option<RequestGuard>,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        // Publication and terminal selection belong to this one execution.
        let Ok(_service_work) = self
            .background_execution_registry
            .service_admission()
            .admit()
        else {
            return Ok(());
        };
        let stream_writer = DefraStreamWriter::new_managed(
            self.node.clone(),
            self.agent_config.agent_identity.as_ref(),
            Duration::from_millis(self.agent_config.stream_batch_ms),
        )?;
        let Some((request, client_claim)) = verify_request_at_claim_boundary(
            &self.request_admission,
            self.node.clone(),
            self.agent_config.agent_identity.as_ref(),
            request,
        )
        .await
        else {
            return Ok(());
        };
        let execution_origin = crate::request::lifecycle::ExecutionOrigin::from_persisted(
            request.execution_origin.as_deref(),
        )
        .expect("fresh request admission requires a canonical execution_origin");
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            self.node.clone(),
            request.clone(),
            self.agent_config.deadline_duration.as_secs(),
            execution_origin,
            self.agent_config.backend_id.clone().unwrap_or_default(),
        );
        lifecycle.bind_managed_agent(self.agent_config.agent_identity.as_ref())?;
        lifecycle.set_client_claim_admission(client_claim);
        lifecycle.set_steering_admission(
            self.request_admission.clone(),
            self.agent_config.agent_identity.clone(),
        );
        lifecycle.set_execution_lease_duration(self.agent_config.stream_liveness_timeout);
        lifecycle.set_configured_max_total_tokens(self.agent_config.max_total_tokens);
        let parked = if request.execution_generation.is_some() {
            match lifecycle.is_waiting_for_backend().await {
                Ok(parked) => parked,
                Err(error) => {
                    tracing::warn!(request_id = %request.request_id, %error, "could not inspect request recovery state");
                    return Ok(());
                }
            }
        } else {
            false
        };
        if !parked {
            lifecycle.set_fold_admitted(self.verified_fold_admissions(&request).await);
        }

        if !self
            .claim_request(
                &request,
                &mut lifecycle,
                &stream_writer,
                &mut capacity,
                parked,
            )
            .await
        {
            return Ok(());
        }

        let (interrupt_tx, mut interrupt_rx) =
            tokio::sync::watch::channel::<Option<crate::request::interrupt::InterruptIntent>>(None);
        let observer = tokio_util::task::AbortOnDropHandle::new(
            crate::request::interrupt::spawn_request_interrupt_observer(
                self.node.clone(),
                lifecycle.access()?,
                request.doc_id.clone(),
                interrupt_tx,
                shutdown.clone(),
            ),
        );

        if self
            .reject_unroutable_agent(&request, &mut lifecycle, &stream_writer)
            .await
        {
            return Ok(());
        }

        let sealed = request::prepare_unless_interrupted(
            &mut interrupt_rx,
            crate::tools::workspace::writer_request_already_sealed_with_identity(
                &self.node,
                self.agent_config.agent_identity.as_ref(),
                &request,
            ),
        )
        .await;
        let Some(sealed) = sealed else {
            self.finalize_interrupt_before_work(&mut lifecycle, &stream_writer, &request)
                .await?;
            return Ok(());
        };
        match sealed {
            // Sealed work is never rerun: host task-hook commands are not
            // replayed and remaining cleanup belongs to task hook recovery.
            Ok(true) => {
                return self
                    .complete_already_sealed(&request, &mut lifecycle, &stream_writer)
                    .await;
            }
            Ok(false) => {}
            Err(error) => {
                record_current_failure_class(&error);
                tracing::error!(
                    request_id = %request.request_id,
                    error = %crate::request::failure_text::BoundedErrorChain(error.as_ref()),
                    "failed to inspect writer workspace seal state"
                );
                self.finalize_failure_before_work(
                    &mut lifecycle,
                    &stream_writer,
                    &request_failure_reason(&error),
                    &request,
                )
                .await;
                return Ok(());
            }
        }

        let prepared_hooks = request::prepare_unless_interrupted(
            &mut interrupt_rx,
            self.prepare_task_hooks(&request, lifecycle.resumed_from_backend()),
        )
        .await;
        let Some(prepared_hooks) = prepared_hooks else {
            self.finalize_interrupt_before_work(&mut lifecycle, &stream_writer, &request)
                .await?;
            return Ok(());
        };
        let (hooks, hook_cwd, hook_record, task_owned, recorded_before) = match prepared_hooks {
            Ok(prepared) => prepared,
            Err(error) => {
                record_current_failure_class(&error);
                tracing::error!(
                    agent_did = %self.agent_config.agent_did,
                    request_id = %request.request_id,
                    error = %format!("{error:#}"),
                    "refusing to run a request whose task hooks cannot be prepared"
                );
                self.finalize_failure_before_work(
                    &mut lifecycle,
                    &stream_writer,
                    &format!("{error:#}"),
                    &request,
                )
                .await;
                return Ok(());
            }
        };

        lifecycle.allow_natural_finish(permits_natural_finish(&request, task_owned));

        let ownership_lost = lifecycle.ownership_lost();
        let hook_cancellation = crate::request::task_hooks::TaskHookCancellation::default();
        let hook_cancellation_follower = (!hooks.is_empty()).then(|| {
            hook_cancellation.follow(
                interrupt_rx.clone(),
                shutdown.clone(),
                ownership_lost.clone(),
            )
        });
        let hook_exec =
            crate::request::task_hooks::ManagedTaskHookExec::new(hook_cwd, hook_cancellation)
                .with_record(hook_record.clone())
                .with_execution_lease(
                    self.node.clone(),
                    request.doc_id.clone(),
                    crate::request::lifecycle::RequestExecutionLease::new(
                        lifecycle.execution_generation()?.to_owned(),
                        lifecycle.actor_did()?,
                    ),
                );

        let mut owned_work = None;
        let run = crate::request::task_hooks::run_task_hooks_with_before(
            &hooks,
            &hook_exec,
            recorded_before,
            || async {
                if let Some(record) = &hook_record {
                    if let Err(error) = record.work_started().await {
                        let outcome = OwnedWorkOutcome::observed(
                            crate::request::task_hooks::TaskAgentResult::Failure,
                            Some(format!("could not record task hook state: {error}")),
                            true,
                        );
                        let observation = outcome.observation();
                        owned_work = Some(outcome);
                        return observation;
                    }
                }
                let outcome = Box::pin(self.observe_owned_work(
                    &mut lifecycle,
                    &stream_writer,
                    &request,
                    shutdown,
                    interrupt_rx,
                    hook_record.as_ref(),
                ))
                .await;
                let observation = outcome.observation();
                owned_work = Some(outcome);
                observation
            },
        )
        .await;
        observer.abort();
        if let Some(follower) = hook_cancellation_follower {
            follower.abort();
        }
        let parked = run.final_outcome() == crate::request::task_hooks::TaskHookOutcome::Parked;
        if !parked {
            if let Some(record) = &hook_record {
                record.release().await;
            }
        }
        if parked {
            if let Some(OwnedWorkOutcome::UncertainParking(error)) = owned_work {
                return Err(error.context("provider recovery could not confirm request ownership"));
            }
            return Ok(());
        }

        // A revoked execution's terminal was written by the revoking owner.
        if ownership_lost.is_cancelled() {
            tracing::warn!(request_id = %request.request_id, "task hooks stopped after execution ownership loss");
            return Ok(());
        }

        let skipped_before_work = owned_work.is_none();
        let (work_reason, release_writer_binding, interruption_source) = match owned_work {
            None => (None, true, RequestInterruptionSource::TaskHook),
            Some(OwnedWorkOutcome::Interrupted(source)) => (None, false, source),
            Some(OwnedWorkOutcome::Parked) => return Ok(()),
            Some(OwnedWorkOutcome::UncertainParking(error)) => return Err(error),
            Some(OwnedWorkOutcome::OwnershipLost) => return Ok(()),
            Some(OwnedWorkOutcome::DrainFailed(error)) => return Err(error),
            Some(OwnedWorkOutcome::Observed {
                reason,
                release_writer_binding,
                ..
            }) => (
                reason,
                release_writer_binding,
                RequestInterruptionSource::TaskHook,
            ),
        };

        self.finish_task_hooks(
            &request,
            &mut lifecycle,
            &stream_writer,
            &run,
            OwnedWorkEnd {
                reason: work_reason,
                release_writer_binding,
                interruption_source,
                skipped_before_work,
            },
        )
        .await
    }
}
