use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::admission::{InferenceCall, InferenceCallRecoveryReport};
use crate::lifecycle::{RequestLifecycle, TerminalRepairReport};
use crate::llm::tool::BoxFuture;
use crate::tool_call_lifecycle::{
    BackgroundCompletionSideEffectReport, OrphanedBackgroundToolReport, TerminalParentToolReport,
    ToolCallLifecycle,
};

const REQUEST_TERMINAL_REPAIR_SWEEP_IDS: &[&str] = &["request_lifecycle_recover_all_requests"];
const TERMINAL_PARENT_TOOL_SWEEP_IDS: &[&str] =
    &["tool_call_lifecycle_reconcile_terminal_parent_owned_tools"];
const ORPHANED_BACKGROUND_TOOL_SWEEP_IDS: &[&str] =
    &["tool_call_lifecycle_reconcile_orphaned_background_tools"];
const BACKGROUND_COMPLETION_SIDE_EFFECT_SWEEP_IDS: &[&str] =
    &["tool_call_lifecycle_reconcile_background_completion_side_effects"];
const INFERENCE_CALL_SWEEP_IDS: &[&str] = &["inference_call_recover_all_stale_calls"];
const SESSION_MESSAGE_SWEEP_IDS: &[&str] = &["tool_call_lifecycle_recover_session_message_rows"];
const PLUGIN_JOB_SWEEP_IDS: &[&str] = &["tool_call_lifecycle_recover_plugin_job_rows"];
const TASK_HOOK_SWEEP_IDS: &[&str] = &["task_hook_recover_interrupted_cleanup"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodicRecoverySweepMetadata {
    pub sweep_ids: &'static [&'static str],
    pub rust_function: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PeriodicRecoverySweepOutcome {
    RequestTerminalRepair(TerminalRepairReport),
    TerminalParentTools(TerminalParentToolReport),
    OrphanedBackgroundTools(OrphanedBackgroundToolReport),
    BackgroundCompletionSideEffects(BackgroundCompletionSideEffectReport),
    InferenceCalls(InferenceCallRecoveryReport),
    SessionMessageRows(usize),
    TaskHookRecords(usize),
}

impl PeriodicRecoverySweepOutcome {
    pub fn is_noop(&self) -> bool {
        match self {
            Self::RequestTerminalRepair(report) => report.is_noop(),
            Self::TerminalParentTools(report) => report.is_noop(),
            Self::OrphanedBackgroundTools(report) => report.is_noop(),
            Self::BackgroundCompletionSideEffects(report) => report.is_noop(),
            Self::InferenceCalls(report) => report.calls_recovered == 0,
            Self::SessionMessageRows(settled) => *settled == 0,
            Self::TaskHookRecords(started) => *started == 0,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PeriodicRecoverySweepRun {
    pub metadata: PeriodicRecoverySweepMetadata,
    pub outcome: PeriodicRecoverySweepOutcome,
}

impl PeriodicRecoverySweepRun {
    pub fn is_noop(&self) -> bool {
        self.outcome.is_noop()
    }
}

type PeriodicRecoverySweepFn = for<'a> fn(
    &'a std::sync::Arc<EmbeddedNode>,
    &'a str,
    &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>>;

struct PeriodicRecoverySweepExecutor {
    metadata_index: usize,
    run: PeriodicRecoverySweepFn,
}

const PERIODIC_RECOVERY_SWEEP_METADATA: &[PeriodicRecoverySweepMetadata] = &[
    PeriodicRecoverySweepMetadata {
        sweep_ids: REQUEST_TERMINAL_REPAIR_SWEEP_IDS,
        rust_function: "RequestLifecycle::repair_terminal_requests",
    },
    PeriodicRecoverySweepMetadata {
        sweep_ids: TERMINAL_PARENT_TOOL_SWEEP_IDS,
        rust_function: "ToolCallLifecycle::reconcile_terminal_parent_owned_tools",
    },
    PeriodicRecoverySweepMetadata {
        sweep_ids: ORPHANED_BACKGROUND_TOOL_SWEEP_IDS,
        rust_function: "ToolCallLifecycle::reconcile_orphaned_background_tools",
    },
    PeriodicRecoverySweepMetadata {
        sweep_ids: BACKGROUND_COMPLETION_SIDE_EFFECT_SWEEP_IDS,
        rust_function: "ToolCallLifecycle::reconcile_background_completion_side_effects",
    },
    // Parent-gated recovery must observe terminal parents from earlier sweeps,
    // including requests whose execution leases were still live at startup.
    PeriodicRecoverySweepMetadata {
        sweep_ids: INFERENCE_CALL_SWEEP_IDS,
        rust_function: "InferenceCall::recover_all",
    },
    PeriodicRecoverySweepMetadata {
        sweep_ids: SESSION_MESSAGE_SWEEP_IDS,
        rust_function: "background_completion::settle_running_session_message_rows",
    },
    PeriodicRecoverySweepMetadata {
        sweep_ids: TASK_HOOK_SWEEP_IDS,
        rust_function: "task_hooks::recover_task_hook_records",
    },
    // Run by the completion observer, which holds the runtime's job driver;
    // it has no executor in this registry.
    PeriodicRecoverySweepMetadata {
        sweep_ids: PLUGIN_JOB_SWEEP_IDS,
        rust_function: "plugin_job::recovery::redrive_running_jobs",
    },
];

const PERIODIC_RECOVERY_SWEEP_EXECUTORS: &[PeriodicRecoverySweepExecutor] = &[
    PeriodicRecoverySweepExecutor {
        metadata_index: 0,
        run: repair_terminal_requests,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 1,
        run: reconcile_terminal_parent_owned_tools,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 2,
        run: reconcile_orphaned_background_tools,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 3,
        run: reconcile_background_completion_side_effects,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 4,
        run: recover_inference_calls,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 5,
        run: settle_session_message_rows,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 6,
        run: recover_task_hook_records,
    },
];

pub fn periodic_recovery_sweep_metadata() -> &'static [PeriodicRecoverySweepMetadata] {
    PERIODIC_RECOVERY_SWEEP_METADATA
}

pub async fn run_periodic_recovery_sweeps(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    background_executions: &crate::hook::BackgroundExecutionRegistry,
) -> Result<Vec<PeriodicRecoverySweepRun>> {
    let mut runs = Vec::with_capacity(PERIODIC_RECOVERY_SWEEP_EXECUTORS.len());
    for executor in PERIODIC_RECOVERY_SWEEP_EXECUTORS {
        let metadata = PERIODIC_RECOVERY_SWEEP_METADATA[executor.metadata_index];
        match (executor.run)(node, node_did, background_executions).await {
            Ok(outcome) => runs.push(PeriodicRecoverySweepRun { metadata, outcome }),
            Err(error) => {
                tracing::warn!(
                    sweep_ids = ?metadata.sweep_ids,
                    rust_function = metadata.rust_function,
                    error = %error,
                    "periodic recovery sweep failed; continuing remaining sweeps and retrying next tick"
                );
            }
        }
    }
    Ok(runs)
}

fn reconcile_terminal_parent_owned_tools<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    _background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_terminal_parent_owned_tools(node, node_did)
            .await
            .map(PeriodicRecoverySweepOutcome::TerminalParentTools)
    })
}

fn reconcile_orphaned_background_tools<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_orphaned_background_tools(
            node,
            node_did,
            background_executions,
        )
        .await
        .map(PeriodicRecoverySweepOutcome::OrphanedBackgroundTools)
    })
}

fn reconcile_background_completion_side_effects<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    _background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_background_completion_side_effects(node, node_did)
            .await
            .map(PeriodicRecoverySweepOutcome::BackgroundCompletionSideEffects)
    })
}

fn repair_terminal_requests<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    _background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        RequestLifecycle::repair_terminal_requests(node, node_did)
            .await
            .map_err(anyhow::Error::from)
            .map(PeriodicRecoverySweepOutcome::RequestTerminalRepair)
    })
}

fn recover_inference_calls<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    _background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        InferenceCall::recover_all(node, node_did)
            .await
            .map(PeriodicRecoverySweepOutcome::InferenceCalls)
            .map_err(Into::into)
    })
}

fn settle_session_message_rows<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    _background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        crate::background_completion::settle_running_session_message_rows(node, node_did)
            .await
            .map(PeriodicRecoverySweepOutcome::SessionMessageRows)
    })
}

fn recover_task_hook_records<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        Ok(PeriodicRecoverySweepOutcome::TaskHookRecords(
            crate::task_hooks::recover_task_hook_records(
                node,
                node_did,
                background_executions.task_hook_records(),
            )
            .await?,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(sweep_id: &str) -> usize {
        PERIODIC_RECOVERY_SWEEP_METADATA
            .iter()
            .position(|metadata| metadata.sweep_ids.contains(&sweep_id))
            .unwrap_or_else(|| panic!("{sweep_id} is not registered"))
    }

    #[test]
    fn plugin_job_sweep_is_registered_after_the_session_message_sweep() {
        let plugin_job = position("tool_call_lifecycle_recover_plugin_job_rows");
        assert!(plugin_job > position("tool_call_lifecycle_recover_session_message_rows"));
        assert_eq!(
            PERIODIC_RECOVERY_SWEEP_METADATA[plugin_job].rust_function,
            "plugin_job::recovery::redrive_running_jobs"
        );
    }
}
