use defra_node::Node as EmbeddedNode;

use gents_inference::admission::{InferenceCall, InferenceCallRecoveryReport};
use gents_session::request::lifecycle::{RecoveryReport, RequestLifecycle};
use gents_toolcalls::call::lifecycle::{ToolCallLifecycle, ToolCallRecoveryReport};

#[derive(Debug)]
pub struct StartupRecoveryOutcome {
    pub tool_calls: anyhow::Result<ToolCallRecoveryReport>,
    pub requests: anyhow::Result<RecoveryReport>,
    pub inference_calls: anyhow::Result<InferenceCallRecoveryReport>,
    pub task_hooks: anyhow::Result<usize>,
}

pub async fn run_startup_recovery_with_identities(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    identities: &[std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
) -> StartupRecoveryOutcome {
    run_startup_recovery_with_executions_and_identities(
        node,
        node_did,
        identities,
        &gents_toolcalls::call::hook::BackgroundExecutionRegistry::default(),
    )
    .await
}

pub async fn run_startup_recovery_with_executions_and_identities(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    identities: &[std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    executions: &gents_toolcalls::call::hook::BackgroundExecutionRegistry,
) -> StartupRecoveryOutcome {
    let tool_calls =
        ToolCallLifecycle::recover_all_with_executions(node, node_did, identities, executions)
            .await;
    let requests = RequestLifecycle::recover_all_with_identities(node, node_did, identities).await;
    let inference_calls =
        InferenceCall::recover_all_with_identities(node, node_did, identities).await;
    let task_hooks = gents_toolcalls::call::task_hooks::recover_task_hook_records(
        node,
        node_did,
        identities,
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

#[cfg(test)]
mod task_hook_executor_tests;
#[cfg(test)]
mod task_hook_recovery_tests;
