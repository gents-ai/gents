use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use gents_inference::llm::rig_compat::ProviderModel;
use tokio::task::JoinError;

/// Tracing target of the daemon's request interruption events; tests observe it by this name.
pub(crate) const TRACING_TARGET: &str = module_path!();

mod auxiliary_tasks;
mod construction;
#[cfg(test)]
mod enrollment_claim_boundary_tests;
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
use crate::turn::prompt::LayeredPromptBuilder;
use gents_inference::compaction::{ReductionEngine, ReductionOptions};
use gents_inference::resolved::ResolvedAgent;
use gents_observe::observe::runtime_trace::{
    record_current_claim_outcome, record_current_request_outcome,
};
use gents_protocol::row::AgentRequest;
use gents_toolcalls::call::hook::FailurePolicy;

/// Task hooks and workspace write/integration effects must finish before their
/// request terminalizes; only requests without such effects may finish inside
/// the steering gate.
fn permits_natural_finish(request: &AgentRequest, task_owned: bool) -> bool {
    !task_owned
        && gents_session::request::lifecycle::queue::heads_user_turn(request)
        && (request
            .workspace_id
            .as_deref()
            .is_none_or(|id| id.trim().is_empty())
            || request
                .workspace_authority
                .as_deref()
                .and_then(|authority| {
                    gents_tools::native::WorkspaceAuthority::parse(authority).ok()
                })
                == Some(gents_tools::native::WorkspaceAuthority::ReadOnly))
}

/// Authenticate the exact durable request immediately before claim so a stale
/// queued snapshot never reaches provider execution.
pub(crate) async fn verify_request_at_claim_boundary(
    verifier: &gents_session::request::admission::AgentRequestAdmissionVerifier,
    node: Arc<defra_node::Node>,
    agent_identity: &dyn gents_store::identity::NodeIdentity,
    request: AgentRequest,
) -> Option<(
    AgentRequest,
    Option<gents_session::request::admission::ClientClaimAdmission>,
)> {
    let agent_did = agent_identity.did();
    match verifier
        .verify_fresh_for_claim_with_identity(&request, agent_identity)
        .await
    {
        Ok(verified) => Some(verified),
        Err(error) if error.is_denied() => {
            let reason = format!("request admission denied: {error:#}");
            record_current_claim_outcome("admission_denied");
            record_current_request_outcome("admission_denied");
            if let Err(persist_error) =
                gents_session::request::admission::terminalize_pending_request_rejection(
                    &node,
                    &request.doc_id,
                    &request.node_did,
                    agent_did,
                    agent_identity,
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
                error = %gents_protocol::failure_text::BoundedErrorChain(&error),
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
    node: Arc<defra_node::Node>,
    backend_health: Option<gents_inference::backends::health::BackendHealthMap>,
    agent_config: Arc<ResolvedAgent>,
    tool_config: Arc<gents_tools::surface::AgentToolSurfaceConfig>,
    provider_family: Option<String>,
    replay_issuer: Option<gents_loop::claude_messages_body::ReplayIssuer>,
    account_hops: u32,
    compaction_provider_family: Option<String>,
    model: Arc<M>,
    preamble: String,
    loop_tools: Arc<Vec<Box<dyn gents_loop::tool::ToolDyn>>>,
    prompt_builder: LayeredPromptBuilder,
    compactor: Arc<dyn ReductionEngine>,
    compaction_options: ReductionOptions,
    hook_failure_policy: FailurePolicy,
    rendered_request_capture_factory:
        Option<gents_inference::rendered_request::RenderedRequestCaptureFactory>,
    background_tool_registry: gents_toolcalls::call::hook::BackgroundToolRegistry,
    background_execution_registry: gents_toolcalls::call::hook::BackgroundExecutionRegistry,
    inference_observations: gents_inference::inference_observation::InferenceObservationRegistry,
    remote_tools: Option<gents_documents::RemoteTools>,
    job_tools: rapidhash::RapidHashMap<String, gents_toolcalls::background::job::JobBinding>,
    job_driver: Option<Arc<gents_toolcalls::background::job::driver::JobDriver>>,
    output_obligations: Arc<Vec<(String, gents_documents::WriteToolOutputObligation)>>,
    startup_barrier: Arc<StartupBarrier>,
    runtime_status: crate::runtime::status::RuntimeStatusHandle,
    slot_generation: u64,
    operator_tool_root: Option<PathBuf>,
    approval_home: Option<PathBuf>,
    request_admission: gents_session::request::admission::AgentRequestAdmissionVerifier,
    auxiliary_tasks: auxiliary_tasks::AuxiliaryTasks,
    root_execution_guard: Option<gents_tools::surface::RootExecutionGuard>,
}

enum HandleRequestOutcome {
    Completed,
    FailedAfterResponse(anyhow::Error),
    /// The provider stayed unavailable through the transport budget; the
    /// request may wait for it instead of failing.
    BackendUnavailable(anyhow::Error),
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
            tracing::warn!(error = %gents_protocol::failure_text::BoundedErrorChain(error.as_ref()), "failed to resume owned auxiliary request");
            Ok(())
        }
        Err(error) => Err(anyhow::anyhow!("owned auxiliary task join failed: {error}")),
    }
}

pub(super) struct AgentDaemonInit<M>
where
    M: ProviderModel,
{
    pub(super) node: Arc<defra_node::Node>,
    pub(super) agent_config: Arc<ResolvedAgent>,
    pub(super) tool_config: Arc<gents_tools::surface::AgentToolSurfaceConfig>,
    pub(super) provider_family: Option<String>,
    pub(super) model: Arc<M>,
    pub(super) preamble: String,
    pub(super) loop_tools: Arc<Vec<Box<dyn gents_loop::tool::ToolDyn>>>,
    pub(super) prompt_builder: LayeredPromptBuilder,
    pub(super) hook_failure_policy: FailurePolicy,
    pub(super) rendered_request_capture_factory:
        Option<gents_inference::rendered_request::RenderedRequestCaptureFactory>,
    pub(super) background_tool_registry: gents_toolcalls::call::hook::BackgroundToolRegistry,
    pub(super) background_execution_registry:
        gents_toolcalls::call::hook::BackgroundExecutionRegistry,
    pub(super) startup_barrier: Arc<StartupBarrier>,
    pub(super) runtime_status: crate::runtime::status::RuntimeStatusHandle,
    pub(super) slot_generation: u64,
    pub(super) request_admission: gents_session::request::admission::AgentRequestAdmissionVerifier,
}
