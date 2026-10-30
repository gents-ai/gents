use std::path::PathBuf;
use std::sync::Arc;

use crate::llm::rig_compat::ProviderModel;
use anyhow::Result;
use tokio::task::JoinError;

mod auxiliary_tasks;
mod construction;
#[cfg(test)]
mod failure_reason_tests;
mod inference;
mod owned_work;
mod process;
mod request;
mod run;
#[cfg(test)]
mod task_hook_tests;
mod terminal;
mod title;
mod wake_input;

#[derive(Debug, thiserror::Error)]
#[error("graceful shutdown could not confirm received provider output was retained: {reason}")]
pub(super) struct ShutdownDrainFailure {
    pub(super) reason: String,
}

impl ShutdownDrainFailure {
    fn occurs_in(error: &anyhow::Error) -> bool {
        error
            .chain()
            .any(|cause| cause.downcast_ref::<Self>().is_some())
    }
}

use super::runtime::StartupBarrier;
use crate::compaction::{ReductionEngine, ReductionOptions};
use crate::config::ResolvedAgent;
use crate::hook::FailurePolicy;
use crate::prompt::LayeredPromptBuilder;
use crate::runtime_trace::{record_current_claim_outcome, record_current_request_outcome};
use crate::watcher::AgentRequest;

/// Task hooks and workspace write/integration effects must finish before their
/// request terminalizes; only requests without such effects may finish inside
/// the steering gate.
fn permits_natural_finish(request: &AgentRequest, task_owned: bool) -> bool {
    !task_owned
        && crate::lifecycle::queue::heads_user_turn(request)
        && (request
            .workspace_id
            .as_deref()
            .is_none_or(|id| id.trim().is_empty())
            || request
                .workspace_authority
                .as_deref()
                .and_then(|authority| crate::toolset::WorkspaceAuthority::parse(authority).ok())
                == Some(crate::toolset::WorkspaceAuthority::ReadOnly))
}

/// Authenticate the exact durable request immediately before claim so a stale
/// queued snapshot never reaches provider execution.
pub(crate) async fn verify_request_at_claim_boundary(
    verifier: &crate::request_admission::AgentRequestAdmissionVerifier,
    node: Arc<defra_node::EmbeddedNode>,
    agent_id: &str,
    request: AgentRequest,
) -> Option<AgentRequest> {
    match verifier.verify_fresh(&request, agent_id).await {
        Ok(verified) => Some(verified),
        Err(error) if error.is_denied() => {
            let reason = format!("request admission denied: {error:#}");
            record_current_claim_outcome("admission_denied");
            record_current_request_outcome("admission_denied");
            if let Err(persist_error) =
                crate::request_admission::terminalize_pending_request_rejection(
                    &node,
                    &request.doc_id,
                    &request.node_did,
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
                error = %crate::failure_text::BoundedErrorChain(&error),
                "request admission authority is temporarily unavailable; leaving request pending"
            );
            None
        }
    }
}

pub(super) struct AgentDaemon<M>
where
    M: ProviderModel,
{
    node: Arc<defra_node::EmbeddedNode>,
    agent_config: Arc<ResolvedAgent>,
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
    inference_observations: crate::inference_observation::InferenceObservationRegistry,
    remote_tools: Option<crate::document_config::RemoteTools>,
    job_tools: rapidhash::RapidHashMap<String, crate::plugin_job::JobBinding>,
    job_driver: Option<Arc<crate::plugin_job::driver::JobDriver>>,
    output_obligations: Arc<Vec<(String, crate::document_config::WriteToolOutputObligation)>>,
    startup_barrier: Arc<StartupBarrier>,
    runtime_status: crate::runtime_status::RuntimeStatusHandle,
    slot_generation: u64,
    operator_tool_root: Option<PathBuf>,
    approval_home: Option<PathBuf>,
    request_admission: crate::request_admission::AgentRequestAdmissionVerifier,
    auxiliary_tasks: auxiliary_tasks::AuxiliaryTasks,
    root_execution_guard: Option<crate::tool_surface::RootExecutionGuard>,
}

enum HandleRequestOutcome {
    Completed,
    FailedAfterResponse(anyhow::Error),
    Interrupted(RequestInterruptionSource),
}

#[derive(Clone, Copy)]
enum RequestInterruptionSource {
    Claimed,
    Preparation,
    Inference,
    TaskHook,
}

impl RequestInterruptionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Preparation => "preparation",
            Self::Inference => "mid_flight",
            Self::TaskHook => "task_hook",
        }
    }
}

fn auxiliary_task_join_result(joined: std::result::Result<Result<()>, JoinError>) -> Result<()> {
    match joined {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) if ShutdownDrainFailure::occurs_in(&error) => Err(error),
        Ok(Err(error)) => {
            tracing::warn!(error = %crate::failure_text::BoundedErrorChain(error.as_ref()), "failed to resume owned auxiliary request");
            Ok(())
        }
        Err(error) => Err(anyhow::anyhow!("owned auxiliary task join failed: {error}")),
    }
}

pub(super) struct AgentDaemonInit<M>
where
    M: ProviderModel,
{
    pub(super) node: Arc<defra_node::EmbeddedNode>,
    pub(super) agent_config: Arc<ResolvedAgent>,
    pub(super) provider_family: Option<String>,
    pub(super) model: Arc<M>,
    pub(super) preamble: String,
    pub(super) loop_tools: Arc<Vec<Box<dyn crate::llm::tool::ToolDyn>>>,
    pub(super) prompt_builder: LayeredPromptBuilder,
    pub(super) hook_failure_policy: FailurePolicy,
    pub(super) rendered_request_capture_factory:
        Option<crate::rendered_request::RenderedRequestCaptureFactory>,
    pub(super) background_tool_registry: crate::hook::BackgroundToolRegistry,
    pub(super) background_execution_registry: crate::hook::BackgroundExecutionRegistry,
    pub(super) startup_barrier: Arc<StartupBarrier>,
    pub(super) runtime_status: crate::runtime_status::RuntimeStatusHandle,
    pub(super) slot_generation: u64,
    pub(super) request_admission: crate::request_admission::AgentRequestAdmissionVerifier,
}
