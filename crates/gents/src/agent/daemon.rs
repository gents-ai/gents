use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::llm::rig_compat::ProviderModel;
use anyhow::Result;
use tokio::sync::{mpsc, Mutex};
use tokio::task::{JoinError, JoinSet};
use tracing::Instrument;

mod inference;
mod request;
#[cfg(test)]
mod task_hook_tests;
mod title;

#[derive(Debug, thiserror::Error)]
#[error("graceful shutdown could not confirm received provider output was retained: {reason}")]
pub(super) struct ShutdownDrainFailure {
    pub(super) reason: String,
}

use super::runtime::StartupBarrier;
use crate::agent::worker_capacity::{
    bind_current_claim, current_slot_capacity, scope_request_capacity, WorkerTicket,
};
use crate::compaction::{ProviderReductionEngine, ReductionEngine, ReductionOptions};
use crate::config::ResolvedBehavior;
use crate::hook::FailurePolicy;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult};
use crate::prompt::LayeredPromptBuilder;
use crate::runtime_trace::{
    record_current_claim_outcome, record_current_failure_class, record_current_request_outcome,
    RequestTraceAttrs,
};
use crate::streaming::DefraStreamWriter;
use crate::watcher::AgentRequest;

/// Only the winning terminal CAS authorizes follow-up effects. A matching
/// durable terminal row is an observation, not a second completion event.
async fn terminalize_request(
    lifecycle: &mut RequestLifecycle,
    stream_writer: &DefraStreamWriter,
    outcome: RequestTerminalOutcome,
    reason: Option<&str>,
) -> Result<bool> {
    let selection = stream_writer
        .terminal_output(&lifecycle.request().doc_id)
        .await;
    match lifecycle
        .terminalize_owned(outcome, selection, reason)
        .await?
    {
        TerminalizeResult::Won => Ok(true),
        TerminalizeResult::AlreadySame => Ok(false),
        TerminalizeResult::Lost => anyhow::bail!(
            "request {} lost its execution generation before terminalization",
            lifecycle.request().request_id,
        ),
    }
}

async fn finalize_request_failure(
    lifecycle: &mut RequestLifecycle,
    stream_writer: &DefraStreamWriter,
    reason: &str,
    request_id: &str,
) -> bool {
    match terminalize_request(
        lifecycle,
        stream_writer,
        RequestTerminalOutcome::Failed,
        Some(reason),
    )
    .await
    {
        Ok(won) => won,
        Err(error) => {
            record_current_request_outcome("terminalization_failed");
            record_current_failure_class(&error);
            tracing::error!(request_id, error = %error,
                "failed to atomically terminalize request and response; durable lease recovery remains pending");
            false
        }
    }
}

/// Authenticate the exact durable request immediately before claim. Admission
/// rejection is terminalized here so no caller can accidentally continue into
/// `claim_with_identity` or provider execution with the stale queued snapshot.
pub(crate) async fn verify_request_at_claim_boundary(
    verifier: &crate::request_admission::AgentRequestAdmissionVerifier,
    node: Arc<defra_node::EmbeddedNode>,
    behavior_id: &str,
    request: AgentRequest,
) -> Option<AgentRequest> {
    match verifier.verify_fresh(&request, behavior_id).await {
        Ok(verified) => Some(verified),
        Err(error) if error.is_denied() => {
            let reason = format!("request admission denied: {error:#}");
            record_current_claim_outcome("admission_denied");
            record_current_request_outcome("admission_denied");
            if let Err(persist_error) =
                crate::request_admission::terminalize_pending_request_rejection(
                    node.as_ref(),
                    &request.doc_id,
                    &request.agent_did,
                    &reason,
                    "terminalize_request_admission_rejection",
                )
                .await
            {
                tracing::error!(
                    request_id = %request.request_id,
                    error = %persist_error,
                    "failed to terminalize rejected AgentRequest after bounded retries"
                );
            }
            None
        }
        Err(error) => {
            record_current_claim_outcome("admission_unavailable");
            record_current_request_outcome("admission_retry");
            tracing::warn!(
                request_id = %request.request_id,
                error = %error,
                "request admission authority is temporarily unavailable; leaving request pending"
            );
            None
        }
    }
}

pub(super) struct BehaviorDaemon<M: ProviderModel> {
    node: Arc<defra_node::EmbeddedNode>,
    behavior: Arc<ResolvedBehavior>,
    provider_family: Option<String>,
    replay_issuer: Option<gents_loop::claude_messages_body::ReplayIssuer>,
    compaction_provider_family: Option<String>,
    model: Arc<M>,
    preamble: String,
    loop_tools: Arc<Vec<Box<dyn crate::llm::tool::ToolDyn>>>,
    prompt_builder: LayeredPromptBuilder,
    compactor: Arc<dyn ReductionEngine>,
    compaction_options: ReductionOptions,
    hook_failure_policy: FailurePolicy,
    rendered_request_capture_factory:
        Option<crate::rendered_request::RenderedRequestCaptureFactory>,
    background_tool_registry: crate::hook::BackgroundToolRegistry,
    background_execution_registry: crate::hook::BackgroundExecutionRegistry,
    remote_tools: Option<crate::document_config::RemoteTools>,
    output_obligations: Arc<Vec<(String, crate::document_config::WriteToolOutputObligation)>>,
    startup_barrier: Arc<StartupBarrier>,
    runtime_status: crate::runtime_status::RuntimeStatusHandle,
    slot_generation: u64,
    operator_tool_root: Option<PathBuf>,
    request_admission: crate::request_admission::AgentRequestAdmissionVerifier,
    root_execution_guard: Option<crate::tool_surface::RootExecutionGuard>,
}

enum HandleRequestOutcome {
    Completed,
    FailedAfterResponse(anyhow::Error),
    Interrupted,
}

fn title_task_join_result(joined: std::result::Result<Result<()>, JoinError>) -> Result<()> {
    match joined {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) if error.is::<ShutdownDrainFailure>() => Err(error),
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "failed to resume owned conversation title request");
            Ok(())
        }
        Err(error) => Err(anyhow::anyhow!(
            "owned conversation title task join failed: {error}"
        )),
    }
}

impl<M: ProviderModel> BehaviorDaemon<M> {
    pub(super) fn new(
        node: Arc<defra_node::EmbeddedNode>,
        behavior: Arc<ResolvedBehavior>,
        provider_family: Option<String>,
        model: Arc<M>,
        preamble: String,
        loop_tools: Arc<Vec<Box<dyn crate::llm::tool::ToolDyn>>>,
        prompt_builder: LayeredPromptBuilder,
        hook_failure_policy: FailurePolicy,
        rendered_request_capture_factory: Option<
            crate::rendered_request::RenderedRequestCaptureFactory,
        >,
        background_tool_registry: crate::hook::BackgroundToolRegistry,
        background_execution_registry: crate::hook::BackgroundExecutionRegistry,
        startup_barrier: Arc<StartupBarrier>,
        runtime_status: crate::runtime_status::RuntimeStatusHandle,
        slot_generation: u64,
        request_admission: crate::request_admission::AgentRequestAdmissionVerifier,
    ) -> Result<Self> {
        let mut compaction_config = crate::completion_factory::loop_config(
            behavior.as_ref(),
            preamble.clone(),
            0,
            crate::rendered_request::CaptureScopeKind::Compaction,
        );
        compaction_config.max_turns = 0;
        let compactor = Arc::new(ProviderReductionEngine::new(
            model.clone(),
            compaction_config,
        ));
        let compaction_options = crate::compaction::reduction_options_for_behavior(&behavior)?;

        Ok(Self {
            node,
            behavior,
            provider_family: provider_family.clone(),
            replay_issuer: None,
            compaction_provider_family: provider_family,
            model,
            preamble,
            loop_tools,
            prompt_builder,
            compactor,
            compaction_options,
            hook_failure_policy,
            rendered_request_capture_factory,
            background_tool_registry,
            background_execution_registry,
            remote_tools: None,
            output_obligations: Arc::new(Vec::new()),
            startup_barrier,
            runtime_status,
            slot_generation,
            operator_tool_root: None,
            request_admission,
            root_execution_guard: None,
        })
    }

    pub(super) fn with_compactor(
        mut self,
        compactor: Arc<dyn ReductionEngine>,
        provider_family: String,
    ) -> Self {
        self.compactor = compactor;
        self.compaction_provider_family = Some(provider_family);
        self
    }

    pub(super) fn with_replay_issuer(
        mut self,
        issuer: Option<gents_loop::claude_messages_body::ReplayIssuer>,
    ) -> Self {
        self.replay_issuer = issuer;
        self
    }

    pub(super) fn with_operator_tool_root(mut self, root: Option<PathBuf>) -> Self {
        crate::workspace::install_process_operator_tool_root(root.clone());
        self.operator_tool_root = root;
        self
    }

    pub(super) fn with_root_execution_guard(
        mut self,
        guard: Option<crate::tool_surface::RootExecutionGuard>,
    ) -> Self {
        self.root_execution_guard = guard;
        self
    }

    /// Attach the filesystem-policy observations assembled with a tool
    /// surface. Production RuntimeContext and focused owned-loop tests share
    /// this step so request-time root revalidation cannot be test-only wiring.
    pub(super) fn with_tool_surface_runtime_policy(
        self,
        guard: Option<crate::tool_surface::RootExecutionGuard>,
        operator_root: Option<PathBuf>,
    ) -> Self {
        self.with_root_execution_guard(guard)
            .with_operator_tool_root(operator_root)
    }

    /// Request-scoped compaction options: the daemon-lifetime knobs plus the
    /// claimed deadline of the request this compaction serves. The deadline is
    /// a required argument so no call site can omit it — the compactor's
    /// stored config is daemon-lifetime and carries no deadline, so this is
    /// the only path by which compaction recovery becomes deadline-aware
    /// (#1016). Both entry points force summarization: they fire only after
    /// the caller has established the assembled input is over budget.
    pub(super) fn compaction_options_for_request(
        &self,
        deadline: Option<chrono::DateTime<chrono::Utc>>,
        aggregate_token_budget: Option<crate::agent::loop_stream::AggregateTokenBudget>,
        sampling_seed: Option<i64>,
    ) -> ReductionOptions {
        ReductionOptions {
            deadline,
            aggregate_token_budget,
            sampling_seed,
            ..self.compaction_options.clone()
        }
    }

    pub(super) fn with_remote_tools(
        mut self,
        tools: Option<crate::document_config::RemoteTools>,
    ) -> Self {
        self.remote_tools = tools;
        self
    }

    pub(super) fn with_output_obligations(
        mut self,
        obligations: Vec<(String, crate::document_config::WriteToolOutputObligation)>,
    ) -> Self {
        self.output_obligations = Arc::new(obligations);
        self
    }

    pub(super) async fn run(
        &mut self,
        request_rx: Arc<Mutex<mpsc::Receiver<AgentRequest>>>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        tracing::info!(
            behavior_id = %self.behavior.behavior_id,
            did = %self.behavior.agent_did(),
            model = %self.behavior.model_name,
            context_window = self.behavior.context_window,
            "gents behavior started"
        );

        if self
            .runtime_status
            .readiness()
            .mark_slot_ready(&self.behavior.behavior_id, self.slot_generation)
            .await?
        {
            self.startup_barrier
                .mark_behavior_ready(&self.behavior.behavior_id, self.slot_generation)
                .await;
        }
        tracing::info!(
            behavior_id = %self.behavior.behavior_id,
            did = %self.behavior.agent_did(),
            "gents behavior executor online"
        );

        let mut title_tasks = JoinSet::new();
        let run_result: Result<()> = loop {
            if *shutdown.borrow() {
                break Ok(());
            }
            let request = tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    tracing::info!(behavior_id = %self.behavior.behavior_id, "shutdown signal received");
                    break Ok(());
                }

                Some(joined) = title_tasks.join_next(), if !title_tasks.is_empty() => {
                    if let Err(error) = title_task_join_result(joined) {
                        break Err(error);
                    }
                    continue;
                }

                req = async {
                    let mut receiver = request_rx.lock().await;
                    receiver.recv().await
                } => {
                    match req {
                        Some(req) => req,
                        None => break Ok(()),
                    }
                }
            };

            if request.purpose == gents_protocol::request_admission::RequestPurpose::TitleAudit {
                self.spawn_title_audit_request(&mut title_tasks, request, shutdown.clone());
                continue;
            }

            let trace_attrs = RequestTraceAttrs::from_request(&request);
            let behavior_id = self.behavior.behavior_id.clone();
            let backend_id = self.behavior.backend_id.clone().unwrap_or_default();

            // The slot's fixed workers may be idle or waiting on this shared
            // active semaphore. Dequeue happens first; no idle worker holds
            // active capacity. The unbound guard is retained across the whole
            // process future and bound to the generation only after claim.
            let active_guard = if let Some(capacity) = current_slot_capacity() {
                let cancellation = tokio_util::sync::CancellationToken::new();
                let guard = tokio::select! {
                    biased;
                    _ = shutdown.changed() => break Ok(()),
                    guard = capacity.acquire_unbound(&cancellation) => guard,
                };
                match guard {
                    Ok(guard) => Some(guard),
                    Err(error) => {
                        tracing::warn!(behavior_id, error = %error, "request worker capacity admission stopped");
                        break Ok(());
                    }
                }
            } else {
                None
            };

            let process =
                self.process_request(request, shutdown.clone())
                    .instrument(tracing::info_span!(
                        "agent.request",
                        request_doc_id = %trace_attrs.request_doc_id,
                        request_id = %trace_attrs.request_id,
                        session_id = %trace_attrs.session_id,
                        agent_did = %trace_attrs.agent_did,
                        behavior_id = %behavior_id,
                        requested_behavior_id = %trace_attrs.requested_behavior_id,
                        backend_id = %backend_id,
                        execution_origin = %trace_attrs.execution_origin,
                        persisted_execution_origin = %trace_attrs.execution_origin,
                        deadline_at = %trace_attrs.deadline_at,
                        has_deadline = trace_attrs.has_deadline,
                        request_hop = trace_attrs.request_hop,
                        parent_request_id = %trace_attrs.parent_request_id,
                        parent_tool_call_id = %trace_attrs.parent_tool_call_id,
                        selected_skill_count = trace_attrs.selected_skill_count,
                        workspace_cwd_set = trace_attrs.workspace_cwd_set,
                        claim_outcome = tracing::field::Empty,
                        request_outcome = tracing::field::Empty,
                        failure_class = tracing::field::Empty,
                    ));
            if let Some(guard) = active_guard {
                if let Err(error) = scope_request_capacity(guard, process).await {
                    break Err(error);
                }
            } else {
                if let Err(error) = process.await {
                    break Err(error);
                }
            }
        };

        let mut title_error = None;
        while let Some(joined) = title_tasks.join_next().await {
            if let Err(error) = title_task_join_result(joined) {
                tracing::error!(error = %error, "owned conversation title task failed during daemon exit");
                if title_error.is_none() {
                    title_error = Some(error);
                }
            }
        }
        match (run_result, title_error) {
            (Err(error), Some(title_error)) => {
                Err(error.context(format!("owned title task also failed: {title_error:#}")))
            }
            (Err(error), None) => Err(error),
            (Ok(()), Some(error)) => Err(error),
            (Ok(()), None) => Ok(()),
        }
    }

    pub(in crate::agent) async fn process_request(
        &mut self,
        request: AgentRequest,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        // Publication and terminal selection belong to this one execution.
        // Dropping the request future also drops its in-memory projection;
        // durable transcript and recovery state remain in DefraDB.
        let stream_writer = DefraStreamWriter::new(
            self.node.clone(),
            self.behavior.agent_did(),
            Duration::from_millis(self.behavior.stream_batch_ms),
        );
        let Some(request) = verify_request_at_claim_boundary(
            &self.request_admission,
            self.node.clone(),
            &self.behavior.behavior_id,
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
            &self.behavior.behavior_id,
            self.behavior.agent_did(),
            request.clone(),
            self.behavior.deadline_duration.as_secs(),
            execution_origin,
            self.behavior.backend_id.clone().unwrap_or_default(),
        );
        lifecycle.set_execution_lease_duration(self.behavior.stream_liveness_timeout);
        lifecycle.set_configured_max_total_tokens(self.behavior.max_total_tokens);

        let claim_result = lifecycle
            .claim_with_identity()
            .instrument(tracing::info_span!(
                "request.claim",
                request_id = %request.request_id,
                session_id = %request.session_id,
                agent_did = %request.agent_did,
                behavior_id = %self.behavior.behavior_id,
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
                            &error.to_string(),
                            &request,
                        )
                        .await;
                        return Ok(());
                    }
                };
                let ticket = WorkerTicket::new(lifecycle.request().doc_id.clone(), generation);
                if let Err(error) = bind_current_claim(ticket) {
                    let error = anyhow::Error::new(error);
                    record_current_request_outcome("worker_ticket_refused");
                    record_current_failure_class(&error);
                    self.finalize_failure_before_work(
                        &mut lifecycle,
                        &stream_writer,
                        &error.to_string(),
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
                    behavior_id = %self.behavior.behavior_id,
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
                    behavior_id = %self.behavior.behavior_id,
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
                    behavior_id = %self.behavior.behavior_id,
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
                        behavior_id = %self.behavior.behavior_id,
                        request_id = %request.request_id,
                        error = %error,
                        "rejecting request with an invalid canonical admission binding"
                    );
                    if let Err(rejection_error) =
                        lifecycle.reject_admission(&error.to_string()).await
                    {
                        tracing::error!(
                            behavior_id = %self.behavior.behavior_id,
                            request_id = %request.request_id,
                            error = %rejection_error,
                            "failed to persist request admission rejection"
                        );
                    }
                    return Ok(());
                }
                tracing::warn!(
                    behavior_id = %self.behavior.behavior_id,
                    request_id = %request.request_id,
                    error = %error,
                    "failed to claim request; leaving it pending for retry"
                );
                return Ok(());
            }
        }

        let requested_behavior_id = request.behavior_id.as_str();
        if requested_behavior_id != self.behavior.behavior_id {
            let error = anyhow::anyhow!(
                "request targets behavior {} but runtime is serving behavior {}",
                requested_behavior_id,
                self.behavior.behavior_id
            );
            record_current_request_outcome("rejected_behavior_mismatch");
            record_current_failure_class(&error);
            tracing::warn!(
                behavior_id = %self.behavior.behavior_id,
                request_id = %request.request_id,
                session_id = %request.session_id,
                requested_behavior_id = %requested_behavior_id,
                "rejecting request for unroutable behavior"
            );
            self.finalize_failure_before_work(
                &mut lifecycle,
                &stream_writer,
                &error.to_string(),
                &request,
            )
            .await;
            return Ok(());
        }

        match crate::workspace::writer_request_already_sealed(self.node.as_ref(), &request).await {
            // An earlier execution already ran and sealed this work. Its task
            // hooks are not rerun: host commands are never replayed, and that
            // execution's remaining cleanup belongs to task hook recovery.
            Ok(true) => {
                if let Err(error) = lifecycle.begin_owned_execution(&stream_writer).await {
                    finalize_request_failure(
                        &mut lifecycle,
                        &stream_writer,
                        &error.to_string(),
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
                    self.node.as_ref(),
                    &request,
                    self.operator_tool_root.as_deref(),
                )
                .await
                {
                    record_current_failure_class(&error);
                    tracing::error!(
                        request_id = %request.request_id,
                        error = %error,
                        "failed to repair workspace seal after writer success"
                    );
                    finalize_request_failure(
                        &mut lifecycle,
                        &stream_writer,
                        &error.to_string(),
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
                    tracing::error!(request_id = %request.request_id, error = %error,
                        "failed to atomically terminalize completed request and response");
                }
                return Ok(());
            }
            Ok(false) => {}
            Err(error) => {
                record_current_failure_class(&error);
                tracing::error!(
                    request_id = %request.request_id,
                    error = %error,
                    "failed to inspect writer workspace seal state"
                );
                self.finalize_failure_before_work(
                    &mut lifecycle,
                    &stream_writer,
                    &error.to_string(),
                    &request,
                )
                .await;
                return Ok(());
            }
        }

        let (hooks, hook_cwd, hook_record) = match self.prepare_task_hooks(&request).await {
            Ok(prepared) => prepared,
            Err(error) => {
                record_current_failure_class(&error);
                tracing::error!(
                    behavior_id = %self.behavior.behavior_id,
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

        // One observer spans the hooks and the owned work, so an interrupt
        // latched during a hook cancels that hook and reaches the work too.
        let (interrupt_tx, interrupt_rx) =
            tokio::sync::watch::channel::<Option<crate::interrupt::InterruptIntent>>(None);
        let observer = crate::interrupt::spawn_request_interrupt_observer(
            self.node.clone(),
            request.doc_id.clone(),
            interrupt_tx,
            shutdown.clone(),
        );
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

        // A revoked execution's terminal was written by the owner that
        // revoked it; this one only finished its cleanup.
        if ownership_lost.is_cancelled() {
            tracing::warn!(request_id = %request.request_id, "task hooks stopped after execution ownership loss");
            return Ok(());
        }

        let (work_reason, release_writer_binding) = match owned_work {
            // No work ran, so nothing else can hold the writer binding the
            // request was materialized with.
            None => (None, true),
            Some(OwnedWorkOutcome::OwnershipLost) => return Ok(()),
            Some(OwnedWorkOutcome::DrainFailed(error)) => return Err(error),
            Some(OwnedWorkOutcome::Observed {
                reason,
                release_writer_binding,
                ..
            }) => (reason, release_writer_binding),
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
                behavior_id = %self.behavior.behavior_id,
                request_id = %request.request_id,
                cleanup_errors = %cleanup_errors.join(","),
                "task cleanup hooks failed"
            );
            reason = Some(reason.map_or(note.clone(), |reason| format!("{reason}\n{note}")));
        }

        let terminal = final_outcome.terminal_outcome();
        match final_outcome {
            crate::task_hooks::TaskHookOutcome::Success => {
                if let Err(error) =
                    terminalize_request(&mut lifecycle, &stream_writer, terminal, None).await
                {
                    record_current_request_outcome("terminalization_failed");
                    record_current_failure_class(&error);
                    tracing::error!(request_id = %request.request_id, error = %error,
                        "failed to atomically terminalize completed request and response");
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
                        tracing::error!(request_id = %request.request_id, error = %error,
                            "failed to atomically terminalize interrupted request and response");
                        return Ok(());
                    }
                }
                if let Err(error) =
                    crate::workspace::release_writer_binding(self.node.as_ref(), &request).await
                {
                    tracing::warn!(
                        request_id = %request.request_id,
                        error = %error,
                        "failed to release writer workspace binding after interrupt"
                    );
                }
                tracing::info!(
                    behavior_id = %self.behavior.behavior_id,
                    request_id = %request.request_id,
                    session_id = %request.session_id,
                    cancellation_source = "mid_flight",
                    "request interrupted mid-flight"
                );
            }
        }
        Ok(())
    }

    /// A workspace-bound automated request receives its writer binding before
    /// this execution exists. Base freeze refuses an Active writer binding even
    /// once its request is terminal, so a failure that terminalizes before the
    /// owned work ran must release it, and only the winning terminal CAS may.
    async fn finalize_failure_before_work(
        &self,
        lifecycle: &mut RequestLifecycle,
        stream_writer: &DefraStreamWriter,
        reason: &str,
        request: &AgentRequest,
    ) {
        if finalize_request_failure(lifecycle, stream_writer, reason, &request.request_id).await {
            self.release_failed_writer_binding(request).await;
        }
    }

    async fn release_failed_writer_binding(&self, request: &AgentRequest) {
        if let Err(error) =
            crate::workspace::release_writer_binding(self.node.as_ref(), request).await
        {
            tracing::warn!(
                request_id = %request.request_id,
                error = %error,
                "failed to release writer workspace binding after failure"
            );
        }
    }

    /// The request's Task hooks, the admitted cwd they run from and the claim
    /// on their host record; no cwd or record when the Task has no hooks.
    async fn prepare_task_hooks(
        &self,
        request: &AgentRequest,
    ) -> Result<(
        Vec<crate::document_config::TaskHook>,
        PathBuf,
        Option<crate::task_hooks::TaskHookRecordHandle>,
    )> {
        let hooks =
            crate::task_hooks::resolve_request_task_hooks(self.node.as_ref(), request).await?;
        if hooks.is_empty() {
            return Ok((hooks, PathBuf::new(), None));
        }
        let cwd = self.task_hook_cwd().await?;
        let record = self
            .background_execution_registry
            .task_hook_records()
            .begin(crate::task_hooks::TaskHookRecord {
                request_doc_id: request.doc_id.clone(),
                request_id: request.request_id.clone(),
                agent_did: request.agent_did.clone(),
                cwd: cwd.clone(),
                root_guard: self.root_execution_guard.clone(),
                hooks: hooks.clone(),
                attempts: Vec::new(),
            })?;
        Ok((hooks, cwd, Some(record)))
    }

    /// Task hooks run from the behavior's host-tools root, revalidated through
    /// its admission owner, or the runtime cwd when the behavior has none.
    /// Workspace association for hooks remains an open design question, so a
    /// hook never runs inside the request's workspace overlay.
    async fn task_hook_cwd(&self) -> Result<PathBuf> {
        if let Some(guard) = &self.root_execution_guard {
            guard.validate(&self.node).await?;
            if let Some(root) = guard.selected_root.clone() {
                return Ok(root);
            }
        }
        std::env::current_dir()
            .map_err(|error| anyhow::anyhow!("resolving the runtime cwd for a task hook: {error}"))
    }

    /// Runs the owned work and its workspace completion, returning the result
    /// the task hook phases observe. Terminalization stays with the caller so
    /// the after-phases can gate it.
    async fn observe_owned_work(
        &mut self,
        lifecycle: &mut RequestLifecycle,
        stream_writer: &DefraStreamWriter,
        request: &AgentRequest,
        shutdown: tokio::sync::watch::Receiver<bool>,
        interrupt_rx: tokio::sync::watch::Receiver<Option<crate::interrupt::InterruptIntent>>,
    ) -> OwnedWorkOutcome {
        use crate::task_hooks::TaskAgentResult;

        let result = self
            .handle_request(lifecycle, stream_writer, shutdown, interrupt_rx)
            .await;

        match result {
            Ok(HandleRequestOutcome::Completed) => {
                record_current_request_outcome("completed");
                if let Err(error) = lifecycle.validate_owned_execution().await {
                    tracing::warn!(request_id = %request.request_id, %error, "stopping workspace completion after execution ownership loss");
                    return OwnedWorkOutcome::OwnershipLost;
                }
                if let Err(error) = crate::workspace::seal_on_writer_success(
                    self.node.as_ref(),
                    request,
                    self.operator_tool_root.as_deref(),
                )
                .await
                {
                    record_current_failure_class(&error);
                    tracing::error!(
                        request_id = %request.request_id,
                        error = %error,
                        "failed to seal workspace after writer success"
                    );
                    return OwnedWorkOutcome::observed(
                        TaskAgentResult::Failure,
                        Some(error.to_string()),
                        true,
                    );
                }
                if let Err(error) = lifecycle.validate_owned_execution().await {
                    tracing::warn!(request_id = %request.request_id, %error, "stopping workspace integration after execution ownership loss");
                    return OwnedWorkOutcome::OwnershipLost;
                }
                if let Err(error) = crate::workspace::integrate_on_integrator_success(
                    self.node.as_ref(),
                    request,
                    self.operator_tool_root.as_deref(),
                )
                .await
                {
                    record_current_failure_class(&error);
                    tracing::error!(
                        request_id = %request.request_id,
                        error = %error,
                        "failed to integrate workspace after integrator success"
                    );
                    // Keep the Active Integrate binding so a retry can observe
                    // a pending commit-tree and write the durable receipt.
                    return OwnedWorkOutcome::observed(
                        TaskAgentResult::Failure,
                        Some(error.to_string()),
                        false,
                    );
                }
                OwnedWorkOutcome::observed(TaskAgentResult::Success, None, false)
            }
            // The owned loop returns this only with an interrupt latch
            // present: a cancellation of active work, not an unknown outcome.
            Ok(HandleRequestOutcome::Interrupted) => {
                OwnedWorkOutcome::observed(TaskAgentResult::Cancelled, None, false)
            }
            Ok(HandleRequestOutcome::FailedAfterResponse(error))
                if lost_execution_ownership(lifecycle, &error).await =>
            {
                tracing::warn!(request_id = %request.request_id, %error, "request work stopped after execution ownership loss");
                OwnedWorkOutcome::OwnershipLost
            }
            Ok(HandleRequestOutcome::FailedAfterResponse(error)) => {
                record_current_request_outcome("failed_after_response");
                record_current_failure_class(&error);
                tracing::error!(
                    behavior_id = %self.behavior.behavior_id,
                    request_id = %request.request_id,
                    error = %error,
                    "request failed after response started"
                );
                OwnedWorkOutcome::observed(TaskAgentResult::Failure, Some(error.to_string()), true)
            }
            Err(error) if error.is::<ShutdownDrainFailure>() => {
                record_current_request_outcome("shutdown_drain_failed");
                record_current_failure_class(&error);
                tracing::error!(
                    behavior_id = %self.behavior.behavior_id,
                    request_id = %request.request_id,
                    error = %error,
                    "graceful shutdown could not confirm provider output retention; leaving durable execution for recovery"
                );
                OwnedWorkOutcome::DrainFailed(error)
            }
            Err(error) if lost_execution_ownership(lifecycle, &error).await => {
                tracing::warn!(request_id = %request.request_id, %error, "request work stopped after execution ownership loss");
                OwnedWorkOutcome::OwnershipLost
            }
            Err(error) => {
                record_current_request_outcome("failed");
                record_current_failure_class(&error);
                tracing::error!(
                    behavior_id = %self.behavior.behavior_id,
                    request_id = %request.request_id,
                    error = %error,
                    "request handling failed"
                );
                OwnedWorkOutcome::observed(TaskAgentResult::Failure, Some(error.to_string()), true)
            }
        }
    }
}

/// A work failure caused by, or observed after, losing the execution lease is
/// not this execution's failure to report: the lease poll and the begin CAS
/// both surface it as an ordinary error.
async fn lost_execution_ownership(lifecycle: &RequestLifecycle, error: &anyhow::Error) -> bool {
    if error.is::<crate::lifecycle::ExecutionOwnershipLost>() {
        return true;
    }
    matches!(lifecycle.owns_execution().await, Ok(false))
}

/// What the owned work decided, before the task hook phases react to it.
/// `release_writer_binding` keeps each arm's existing choice: an integrate
/// failure retains its Active binding so a retry can observe the pending
/// commit-tree. Lost ownership is observed as an interruption so cleanup still
/// runs, and the request's current owner keeps its terminal. A drain failure
/// leaves the durable execution to recovery and its error to the runtime.
enum OwnedWorkOutcome {
    Observed {
        observation: crate::task_hooks::TaskAgentResult,
        reason: Option<String>,
        release_writer_binding: bool,
    },
    OwnershipLost,
    DrainFailed(anyhow::Error),
}

impl OwnedWorkOutcome {
    fn observed(
        observation: crate::task_hooks::TaskAgentResult,
        reason: Option<String>,
        release_writer_binding: bool,
    ) -> Self {
        Self::Observed {
            observation,
            reason,
            release_writer_binding,
        }
    }

    fn observation(&self) -> crate::task_hooks::TaskAgentResult {
        match self {
            Self::Observed { observation, .. } => *observation,
            Self::OwnershipLost | Self::DrainFailed(_) => {
                crate::task_hooks::TaskAgentResult::Interrupted
            }
        }
    }
}
