use defra_node::EmbeddedNode;

use crate::inference::admission::{InferenceCall, InferenceCallRecoveryReport};
use crate::request::lifecycle::{RecoveryReport, RequestLifecycle};
use crate::tool_call_lifecycle::{ToolCallLifecycle, ToolCallRecoveryReport};

#[derive(Debug)]
pub struct StartupRecoveryOutcome {
    pub tool_calls: anyhow::Result<ToolCallRecoveryReport>,
    pub requests: anyhow::Result<RecoveryReport>,
    pub inference_calls: anyhow::Result<InferenceCallRecoveryReport>,
    pub task_hooks: anyhow::Result<usize>,
}

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

pub async fn run_startup_recovery_with_executions(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    executions: &crate::hook::BackgroundExecutionRegistry,
) -> StartupRecoveryOutcome {
    let tool_calls =
        ToolCallLifecycle::recover_all_with_executions(node, node_did, executions).await;
    let requests = RequestLifecycle::recover_all(node, node_did).await;
    let inference_calls = InferenceCall::recover_all(node, node_did).await;
    let task_hooks = crate::request::task_hooks::recover_task_hook_records(
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
