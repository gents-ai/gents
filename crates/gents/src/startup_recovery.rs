//! Ordered startup recovery sweeps.
//!
//! Lean requires inference-call recovery at startup and periodically
//! (`Proofs/Recovery/Sweeps/Inference.lean`); its parent gate makes the
//! sweep's convergence depend on ordering: request repair must run first
//! (`Proofs/Recovery/StartupOrder.lean`).

use defra_node::EmbeddedNode;

use crate::admission::{InferenceCall, InferenceCallRecoveryReport};
use crate::lifecycle::{RecoveryReport, RequestLifecycle};
use crate::tool_call_lifecycle::{ToolCallLifecycle, ToolCallRecoveryReport};

/// Per-sweep results of one ordered startup recovery pass.
#[derive(Debug)]
pub struct StartupRecoveryOutcome {
    pub tool_calls: anyhow::Result<ToolCallRecoveryReport>,
    pub requests: anyhow::Result<RecoveryReport>,
    pub inference_calls: anyhow::Result<InferenceCallRecoveryReport>,
    pub task_hooks: anyhow::Result<usize>,
}

/// Run the startup recovery sweeps in dependency order: tool-call repair
/// observes parent liveness before request repair terminalizes those parents;
/// inference-call repair is parent-gated and runs last
/// (`Recovery.request_before_inference_converges`); task-hook cleanup reports
/// what terminal request recovery decided and runs after it.
pub async fn run_startup_recovery(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
) -> StartupRecoveryOutcome {
    run_startup_recovery_with_executions(
        node,
        node_did,
        &crate::hook::BackgroundExecutionRegistry::default(),
    )
    .await
}

/// [`run_startup_recovery`] with the runtime's background execution owner.
pub async fn run_startup_recovery_with_executions(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    executions: &crate::hook::BackgroundExecutionRegistry,
) -> StartupRecoveryOutcome {
    let tool_calls =
        ToolCallLifecycle::recover_all_with_executions(node, node_did, executions).await;
    let requests = RequestLifecycle::recover_all(node, node_did).await;
    let inference_calls = InferenceCall::recover_all(node, node_did).await;
    let task_hooks = crate::task_hooks::recover_task_hook_records(
        node,
        node_did,
        executions.task_hook_records(),
    )
    .await;
    StartupRecoveryOutcome {
        tool_calls,
        requests,
        inference_calls,
        task_hooks,
    }
}
