use defra_node::Node as EmbeddedNode;

use crate::inference::admission::{InferenceCall, InferenceCallRecoveryReport};
use crate::request::lifecycle::{RecoveryReport, RequestLifecycle};
use crate::tools::call::lifecycle::{ToolCallLifecycle, ToolCallRecoveryReport};

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
    identities: &[std::sync::Arc<dyn crate::store::identity::NodeIdentity>],
) -> StartupRecoveryOutcome {
    run_startup_recovery_with_executions_and_identities(
        node,
        node_did,
        identities,
        &crate::tools::call::hook::BackgroundExecutionRegistry::default(),
    )
    .await
}

pub async fn run_startup_recovery_with_executions_and_identities(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    identities: &[std::sync::Arc<dyn crate::store::identity::NodeIdentity>],
    executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
) -> StartupRecoveryOutcome {
    let tool_calls =
        ToolCallLifecycle::recover_all_with_executions(node, node_did, identities, executions)
            .await;
    let requests = RequestLifecycle::recover_all_with_identities(node, node_did, identities).await;
    let inference_calls =
        InferenceCall::recover_all_with_identities(node, node_did, identities).await;
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
