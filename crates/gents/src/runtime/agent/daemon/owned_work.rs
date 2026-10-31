use std::path::PathBuf;

use anyhow::Result;

use super::terminal::request_failure_reason;
use super::{AgentDaemon, HandleRequestOutcome, RequestInterruptionSource, ShutdownDrainFailure};
use crate::inference::llm::rig_compat::ProviderModel;
use crate::observe::runtime_trace::{record_current_failure_class, record_current_request_outcome};
use crate::request::lifecycle::RequestLifecycle;
use crate::request::streaming::DefraStreamWriter;
use crate::request::watcher::AgentRequest;

impl<M> AgentDaemon<M>
where
    M: ProviderModel,
{
    pub(super) async fn prepare_task_hooks(
        &self,
        request: &AgentRequest,
    ) -> Result<(
        Vec<crate::documents::TaskHook>,
        PathBuf,
        Option<crate::request::task_hooks::TaskHookRecordHandle>,
        bool,
    )> {
        let (hooks, task_owned) =
            crate::request::task_hooks::resolve_request_task_hooks(self.node.as_ref(), request)
                .await?;
        if hooks.is_empty() {
            return Ok((hooks, PathBuf::new(), None, task_owned));
        }
        let cwd = self.task_hook_cwd().await?;
        let record = self
            .background_execution_registry
            .task_hook_records()
            .begin(crate::request::task_hooks::TaskHookRecord {
                request_doc_id: request.doc_id.clone(),
                request_id: request.request_id.clone(),
                node_did: request.node_did.clone(),
                cwd: cwd.clone(),
                root_guard: self.root_execution_guard.clone(),
                hooks: hooks.clone(),
                attempts: Vec::new(),
            })?;
        Ok((hooks, cwd, Some(record), task_owned))
    }

    /// Task hooks run from the agent_config's host-tools root or the runtime
    /// cwd when it has none; a hook never runs inside the request's workspace
    /// overlay while workspace integration remains an open design question.
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

    /// Runs the owned work and its workspace completion; terminalization stays
    /// with the caller so the task-hook after-phases can gate it.
    pub(super) async fn observe_owned_work(
        &mut self,
        lifecycle: &mut RequestLifecycle,
        stream_writer: &DefraStreamWriter,
        request: &AgentRequest,
        shutdown: tokio::sync::watch::Receiver<bool>,
        interrupt_rx: tokio::sync::watch::Receiver<
            Option<crate::request::interrupt::InterruptIntent>,
        >,
    ) -> OwnedWorkOutcome {
        use crate::request::task_hooks::TaskAgentResult;

        let result = self
            .handle_request(lifecycle, stream_writer, shutdown, interrupt_rx)
            .await;

        match result {
            Ok(HandleRequestOutcome::Completed) => {
                record_current_request_outcome("completed");
                if lifecycle.natural_finish_completed() {
                    return OwnedWorkOutcome::observed(TaskAgentResult::Success, None, false);
                }
                if let Err(error) = lifecycle.validate_owned_execution().await {
                    tracing::warn!(request_id = %request.request_id, %error, "stopping workspace completion after execution ownership loss");
                    return OwnedWorkOutcome::OwnershipLost;
                }
                if let Err(error) = crate::tools::workspace::seal_on_writer_success(
                    &self.node,
                    request,
                    self.operator_tool_root.as_deref(),
                )
                .await
                {
                    record_current_failure_class(&error);
                    tracing::error!(
                        request_id = %request.request_id,
                        error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                        "failed to seal workspace after writer success"
                    );
                    return OwnedWorkOutcome::observed(
                        TaskAgentResult::Failure,
                        Some(request_failure_reason(&error)),
                        true,
                    );
                }
                if let Err(error) = lifecycle.validate_owned_execution().await {
                    tracing::warn!(request_id = %request.request_id, %error, "stopping workspace integration after execution ownership loss");
                    return OwnedWorkOutcome::OwnershipLost;
                }
                if let Err(error) = crate::tools::workspace::integrate_on_integrator_success(
                    &self.node,
                    request,
                    self.operator_tool_root.as_deref(),
                )
                .await
                {
                    record_current_failure_class(&error);
                    tracing::error!(
                        request_id = %request.request_id,
                        error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                        "failed to integrate workspace after integrator success"
                    );
                    // Keep the Active Integrate binding so a retry can observe
                    // the pending commit-tree and write the durable receipt.
                    return OwnedWorkOutcome::observed(
                        TaskAgentResult::Failure,
                        Some(request_failure_reason(&error)),
                        false,
                    );
                }
                OwnedWorkOutcome::observed(TaskAgentResult::Success, None, false)
            }
            // The owned loop returns this only with an interrupt latch present.
            Ok(HandleRequestOutcome::Interrupted(source)) => OwnedWorkOutcome::Interrupted(source),
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
                    agent_id = %self.agent_config.agent_id,
                    request_id = %request.request_id,
                    error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                    "request failed after response started"
                );
                OwnedWorkOutcome::observed(
                    TaskAgentResult::Failure,
                    Some(request_failure_reason(&error)),
                    true,
                )
            }
            Err(error) if ShutdownDrainFailure::occurs_in(&error) => {
                record_current_request_outcome("shutdown_drain_failed");
                record_current_failure_class(&error);
                tracing::error!(
                    agent_id = %self.agent_config.agent_id,
                    request_id = %request.request_id,
                    error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
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
                    agent_id = %self.agent_config.agent_id,
                    request_id = %request.request_id,
                    error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                    "request handling failed"
                );
                OwnedWorkOutcome::observed(
                    TaskAgentResult::Failure,
                    Some(request_failure_reason(&error)),
                    true,
                )
            }
        }
    }
}

/// A work failure caused by losing the execution lease is not this execution's
/// failure to report.
async fn lost_execution_ownership(lifecycle: &RequestLifecycle, error: &anyhow::Error) -> bool {
    if error.is::<crate::request::lifecycle::ExecutionOwnershipLost>() {
        return true;
    }
    matches!(lifecycle.owns_execution().await, Ok(false))
}

pub(super) enum OwnedWorkOutcome {
    Interrupted(RequestInterruptionSource),
    Observed {
        observation: crate::request::task_hooks::TaskAgentResult,
        reason: Option<String>,
        release_writer_binding: bool,
    },
    OwnershipLost,
    DrainFailed(anyhow::Error),
}

impl OwnedWorkOutcome {
    pub(super) fn observed(
        observation: crate::request::task_hooks::TaskAgentResult,
        reason: Option<String>,
        release_writer_binding: bool,
    ) -> Self {
        Self::Observed {
            observation,
            reason,
            release_writer_binding,
        }
    }

    pub(super) fn observation(&self) -> crate::request::task_hooks::TaskAgentResult {
        match self {
            Self::Interrupted(_) => crate::request::task_hooks::TaskAgentResult::Cancelled,
            Self::Observed { observation, .. } => *observation,
            Self::OwnershipLost | Self::DrainFailed(_) => {
                crate::request::task_hooks::TaskAgentResult::Interrupted
            }
        }
    }
}
