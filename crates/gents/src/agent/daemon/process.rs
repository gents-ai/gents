use std::time::Duration;

use anyhow::Result;
use tracing::Instrument;

use super::owned_work::OwnedWorkOutcome;
use super::terminal::{finalize_request_failure, request_failure_reason, terminalize_request};
use super::{
    permits_natural_finish, request, verify_request_at_claim_boundary, AgentDaemon,
    RequestInterruptionSource,
};
use crate::agent::worker_capacity::{RequestGuard, WorkerTicket};
use crate::lifecycle::{ClaimOutcome, RequestLifecycle, RequestTerminalOutcome};
use crate::llm::rig_compat::ProviderModel;
use crate::runtime_trace::{
    record_current_claim_outcome, record_current_failure_class, record_current_request_outcome,
};
use crate::streaming::DefraStreamWriter;
use crate::watcher::AgentRequest;

impl<M> AgentDaemon<M>
where
    M: ProviderModel,
{
    #[cfg(test)]
    pub(in crate::agent) async fn process_request(
        &mut self,
        request: AgentRequest,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        self.process_request_with_capacity(request, None, shutdown)
            .await
    }

    /// `capacity` is the post-dequeue worker permit; this call owns it until
    /// it returns, bound to the claim's execution generation once one exists.
    pub(in crate::agent) async fn process_request_with_capacity(
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
        let stream_writer = DefraStreamWriter::new(
            self.node.clone(),
            self.agent_config.node_did(),
            Duration::from_millis(self.agent_config.stream_batch_ms),
        );
        let Some(request) = verify_request_at_claim_boundary(
            &self.request_admission,
            self.node.clone(),
            &self.agent_config.agent_id,
            request,
        )
        .await
        else {
            return Ok(());
        };
        let execution_origin =
            crate::lifecycle::ExecutionOrigin::from_persisted(request.execution_origin.as_deref())
                .expect("fresh request admission requires a canonical execution_origin");
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            self.node.clone(),
            &self.agent_config.agent_id,
            self.agent_config.node_did(),
            request.clone(),
            self.agent_config.deadline_duration.as_secs(),
            execution_origin,
            self.agent_config.backend_id.clone().unwrap_or_default(),
        );
        lifecycle.set_execution_lease_duration(self.agent_config.stream_liveness_timeout);
        lifecycle.set_configured_max_total_tokens(self.agent_config.max_total_tokens);
        lifecycle.set_fold_admitted(self.verified_fold_admissions(&request).await);

        let claim_result = lifecycle
            .claim_with_identity()
            .instrument(tracing::info_span!(
                "request.claim",
                request_id = %request.request_id,
                session_id = %request.session_id,
                node_did = %request.node_did,
                agent_id = %self.agent_config.agent_id,
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
                            &mut lifecycle,
                            &stream_writer,
                            &request_failure_reason(&error),
                            &request,
                        )
                        .await;
                        return Ok(());
                    }
                };
                let ticket = WorkerTicket::new(lifecycle.request().doc_id.clone(), generation);
                if let Err(error) = RequestGuard::bind_claim(&mut capacity, ticket) {
                    let error = anyhow::Error::new(error);
                    record_current_request_outcome("worker_ticket_refused");
                    record_current_failure_class(&error);
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
            Ok(ClaimOutcome::Queued) => {
                record_current_claim_outcome("queued");
                record_current_request_outcome("queued");
                tracing::info!(
                    agent_id = %self.agent_config.agent_id,
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    "request queued behind an earlier same-session request"
                );
                return Ok(());
            }
            Ok(ClaimOutcome::Interrupted) => {
                record_current_claim_outcome("interrupted");
                record_current_request_outcome("interrupted_pre_claim");
                tracing::info!(
                    target: super::TRACING_TARGET,
                    agent_id = %self.agent_config.agent_id,
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    cancellation_source = "pre_claim",
                    "request interrupted before claim"
                );
                return Ok(());
            }
            Ok(ClaimOutcome::Expired) => {
                record_current_claim_outcome("expired");
                record_current_request_outcome("expired_pre_claim");
                tracing::info!(
                    target: super::TRACING_TARGET,
                    agent_id = %self.agent_config.agent_id,
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    cancellation_source = "stale_ttl",
                    "request expired (valid_until passed) before claim; marked dead"
                );
                return Ok(());
            }
            Err(error) => {
                record_current_claim_outcome("error");
                let deterministic_rejection = crate::lifecycle::is_claim_admission_error(&error);
                record_current_request_outcome(if deterministic_rejection {
                    "admission_rejected"
                } else {
                    "claim_error"
                });
                record_current_failure_class(&error);
                if deterministic_rejection {
                    tracing::warn!(
                        agent_id = %self.agent_config.agent_id,
                        request_id = %request.request_id,
                        error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                        "rejecting request with an invalid canonical admission binding"
                    );
                    if let Err(rejection_error) = lifecycle
                        .reject_admission(&request_failure_reason(&error))
                        .await
                    {
                        tracing::error!(
                            agent_id = %self.agent_config.agent_id,
                            request_id = %request.request_id,
                            error = %rejection_error,
                            "failed to persist request admission rejection"
                        );
                    }
                    return Ok(());
                }
                tracing::warn!(
                    agent_id = %self.agent_config.agent_id,
                    request_id = %request.request_id,
                    error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                    "failed to claim request; leaving it pending for retry"
                );
                return Ok(());
            }
        }

        let (interrupt_tx, mut interrupt_rx) =
            tokio::sync::watch::channel::<Option<crate::interrupt::InterruptIntent>>(None);
        let observer = tokio_util::task::AbortOnDropHandle::new(
            crate::interrupt::spawn_request_interrupt_observer(
                self.node.clone(),
                request.doc_id.clone(),
                interrupt_tx,
                shutdown.clone(),
            ),
        );

        let requested_agent_id = request.agent_id.as_str();
        if requested_agent_id != self.agent_config.agent_id {
            let error = anyhow::anyhow!(
                "request targets agent {} but runtime is serving agent {}",
                requested_agent_id,
                self.agent_config.agent_id
            );
            record_current_request_outcome("rejected_agent_mismatch");
            record_current_failure_class(&error);
            tracing::warn!(
                agent_id = %self.agent_config.agent_id,
                request_id = %request.request_id,
                session_id = %request.session_id,
                requested_agent_id = %requested_agent_id,
                "rejecting request for unroutable agent"
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

        let sealed = request::prepare_unless_interrupted(
            &mut interrupt_rx,
            crate::workspace::writer_request_already_sealed(&self.node, &request),
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
                if let Err(error) = lifecycle.begin_owned_execution(&stream_writer).await {
                    finalize_request_failure(
                        &mut lifecycle,
                        &stream_writer,
                        &request_failure_reason(&error),
                        &request.request_id,
                    )
                    .await;
                    return Ok(());
                }
                record_current_request_outcome("completed");
                if let Err(error) = lifecycle.validate_owned_execution().await {
                    tracing::warn!(request_id = %request.request_id, %error, "stopping workspace completion after execution ownership loss");
                    return Ok(());
                }
                if let Err(error) = crate::workspace::seal_on_writer_success(
                    &self.node,
                    &request,
                    self.operator_tool_root.as_deref(),
                )
                .await
                {
                    record_current_failure_class(&error);
                    tracing::error!(
                        request_id = %request.request_id,
                        error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                        "failed to repair workspace seal after writer success"
                    );
                    finalize_request_failure(
                        &mut lifecycle,
                        &stream_writer,
                        &request_failure_reason(&error),
                        &request.request_id,
                    )
                    .await;
                    return Ok(());
                }
                if let Err(error) = terminalize_request(
                    &mut lifecycle,
                    &stream_writer,
                    RequestTerminalOutcome::Completed,
                    None,
                )
                .await
                {
                    record_current_request_outcome("terminalization_failed");
                    record_current_failure_class(&error);
                    tracing::error!(request_id = %request.request_id, error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                        "failed to atomically terminalize completed request and response");
                }
                return Ok(());
            }
            Ok(false) => {}
            Err(error) => {
                record_current_failure_class(&error);
                tracing::error!(
                    request_id = %request.request_id,
                    error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
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
            self.prepare_task_hooks(&request),
        )
        .await;
        let Some(prepared_hooks) = prepared_hooks else {
            self.finalize_interrupt_before_work(&mut lifecycle, &stream_writer, &request)
                .await?;
            return Ok(());
        };
        let (hooks, hook_cwd, hook_record, task_owned) = match prepared_hooks {
            Ok(prepared) => prepared,
            Err(error) => {
                record_current_failure_class(&error);
                tracing::error!(
                    agent_id = %self.agent_config.agent_id,
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
        let hook_cancellation = crate::task_hooks::TaskHookCancellation::default();
        let hook_cancellation_follower = (!hooks.is_empty()).then(|| {
            hook_cancellation.follow(
                interrupt_rx.clone(),
                shutdown.clone(),
                ownership_lost.clone(),
            )
        });
        let hook_exec = crate::task_hooks::ManagedTaskHookExec::new(hook_cwd, hook_cancellation)
            .with_record(hook_record.clone())
            .with_execution_lease(
                self.node.clone(),
                request.doc_id.clone(),
                crate::lifecycle::RequestExecutionLease::new(
                    lifecycle.execution_generation()?.to_owned(),
                ),
            );

        let mut owned_work = None;
        let run = crate::task_hooks::run_task_hooks(&hooks, &hook_exec, || async {
            if let Some(record) = &hook_record {
                if let Err(error) = record.work_started().await {
                    let outcome = OwnedWorkOutcome::observed(
                        crate::task_hooks::TaskAgentResult::Failure,
                        Some(format!("could not record task hook state: {error}")),
                        true,
                    );
                    let observation = outcome.observation();
                    owned_work = Some(outcome);
                    return observation;
                }
            }
            let outcome = self
                .observe_owned_work(
                    &mut lifecycle,
                    &stream_writer,
                    &request,
                    shutdown,
                    interrupt_rx,
                )
                .await;
            let observation = outcome.observation();
            owned_work = Some(outcome);
            observation
        })
        .await;
        observer.abort();
        if let Some(follower) = hook_cancellation_follower {
            follower.abort();
        }
        if let Some(record) = &hook_record {
            record.release().await;
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

        let final_outcome = run.final_outcome();
        let mut reason = match &final_outcome {
            crate::task_hooks::TaskHookOutcome::Failure(
                crate::task_hooks::HookPrimaryError::Hook(hook_id),
            ) => Some(run.hook_failure_reason(hook_id)),
            _ => work_reason,
        };
        let cleanup_errors = run.cleanup_errors();
        if !cleanup_errors.is_empty() {
            let note = format!("task cleanup hooks failed: {}", cleanup_errors.join(", "));
            tracing::error!(
                agent_id = %self.agent_config.agent_id,
                request_id = %request.request_id,
                cleanup_errors = %cleanup_errors.join(","),
                "task cleanup hooks failed"
            );
            reason = Some(reason.map_or(note.clone(), |reason| format!("{reason}\n{note}")));
        }

        let terminal = final_outcome.terminal_outcome();
        match final_outcome {
            crate::task_hooks::TaskHookOutcome::Success => {
                if skipped_before_work {
                    if let Err(error) = lifecycle.begin_owned_execution(&stream_writer).await {
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
                match terminalize_request(&mut lifecycle, &stream_writer, terminal, None).await {
                    Ok(true) if skipped_before_work => {
                        record_current_request_outcome("completed");
                        if let Err(error) =
                            crate::workspace::release_writer_binding(&self.node, &request).await
                        {
                            tracing::warn!(request_id = %request.request_id, error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                                "failed to release writer workspace binding after skipped task gate");
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        record_current_request_outcome("terminalization_failed");
                        record_current_failure_class(&error);
                        tracing::error!(request_id = %request.request_id, error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                            "failed to atomically terminalize completed request and response");
                    }
                }
            }
            crate::task_hooks::TaskHookOutcome::Failure(_) => {
                let reason = reason.unwrap_or_else(|| "request failed".to_string());
                if finalize_request_failure(
                    &mut lifecycle,
                    &stream_writer,
                    &reason,
                    &request.request_id,
                )
                .await
                    && release_writer_binding
                {
                    self.release_failed_writer_binding(&request).await;
                }
            }
            crate::task_hooks::TaskHookOutcome::Interrupted => {
                record_current_request_outcome("interrupted");
                let reason = reason.unwrap_or_else(|| "interrupted".to_string());
                match terminalize_request(&mut lifecycle, &stream_writer, terminal, Some(&reason))
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => return Ok(()),
                    Err(error) => {
                        record_current_request_outcome("terminalization_failed");
                        record_current_failure_class(&error);
                        tracing::error!(request_id = %request.request_id, error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                            "failed to atomically terminalize interrupted request and response");
                        return Ok(());
                    }
                }
                self.record_request_interruption(&request, interruption_source);
                if let Err(error) =
                    crate::workspace::release_writer_binding(&self.node, &request).await
                {
                    tracing::warn!(
                        request_id = %request.request_id,
                        error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                        "failed to release writer workspace binding after interrupt"
                    );
                }
            }
        }
        Ok(())
    }
}
