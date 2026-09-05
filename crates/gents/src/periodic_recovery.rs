//! Registry for recovery sweeps that must run on the live daemon tick.
//!
//! Lean owns the sweep vocabulary (`RecoverySweep.sweepId`, cadence, and
//! `rustFunction`). Rust owns the executable registry the daemon iterates.
//! Keeping the mapping explicit here makes new periodic sweeps discoverable by
//! conformance tests instead of burying them as ad hoc calls in the observer.

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::admission::{InferenceCall, InferenceCallRecoveryReport};
use crate::lifecycle::{RequestLifecycle, TerminalRepairReport};
use crate::llm::tool::BoxFuture;
use crate::tool_call_lifecycle::{
    BackgroundCompletionSideEffectReport, OrphanedBackgroundToolReport, SubagentLivenessReport,
    TerminalParentToolReport, ToolCallLifecycle,
};

const SUBAGENT_LIVENESS_SWEEP_IDS: &[&str] = &[
    "subagent_liveness_terminalize_expired_children",
    "subagent_liveness_interrupt_queued_descendants",
];
const REQUEST_TERMINAL_REPAIR_SWEEP_IDS: &[&str] = &["request_lifecycle_recover_all_requests"];
const TERMINAL_PARENT_TOOL_SWEEP_IDS: &[&str] =
    &["tool_call_lifecycle_reconcile_terminal_parent_owned_tools"];
const ORPHANED_BACKGROUND_TOOL_SWEEP_IDS: &[&str] =
    &["tool_call_lifecycle_reconcile_orphaned_background_tools"];
const BACKGROUND_COMPLETION_SIDE_EFFECT_SWEEP_IDS: &[&str] =
    &["tool_call_lifecycle_reconcile_background_completion_side_effects"];
const INFERENCE_CALL_SWEEP_IDS: &[&str] = &["inference_call_recover_all_stale_calls"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodicRecoverySweepMetadata {
    pub sweep_ids: &'static [&'static str],
    /// Fully-qualified Rust function name emitted in the Lean contract.
    pub rust_function: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PeriodicRecoverySweepOutcome {
    RequestTerminalRepair(TerminalRepairReport),
    SubagentLiveness(SubagentLivenessReport),
    TerminalParentTools(TerminalParentToolReport),
    OrphanedBackgroundTools(OrphanedBackgroundToolReport),
    BackgroundCompletionSideEffects(BackgroundCompletionSideEffectReport),
    InferenceCalls(InferenceCallRecoveryReport),
}

impl PeriodicRecoverySweepOutcome {
    pub fn is_noop(&self) -> bool {
        match self {
            Self::RequestTerminalRepair(report) => report.is_noop(),
            Self::SubagentLiveness(report) => report.is_noop(),
            Self::TerminalParentTools(report) => report.is_noop(),
            Self::OrphanedBackgroundTools(report) => report.is_noop(),
            Self::BackgroundCompletionSideEffects(report) => report.is_noop(),
            Self::InferenceCalls(report) => report.calls_recovered == 0,
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
    &'a EmbeddedNode,
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
        sweep_ids: SUBAGENT_LIVENESS_SWEEP_IDS,
        rust_function: "ToolCallLifecycle::reconcile_subagent_liveness",
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
];

const PERIODIC_RECOVERY_SWEEP_EXECUTORS: &[PeriodicRecoverySweepExecutor] = &[
    PeriodicRecoverySweepExecutor {
        metadata_index: 0,
        run: repair_terminal_requests,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 1,
        run: reconcile_subagent_liveness,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 2,
        run: reconcile_terminal_parent_owned_tools,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 3,
        run: reconcile_orphaned_background_tools,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 4,
        run: reconcile_background_completion_side_effects,
    },
    PeriodicRecoverySweepExecutor {
        metadata_index: 5,
        run: recover_inference_calls,
    },
];

pub fn periodic_recovery_sweep_metadata() -> &'static [PeriodicRecoverySweepMetadata] {
    PERIODIC_RECOVERY_SWEEP_METADATA
}

pub async fn run_periodic_recovery_sweeps(
    node: &EmbeddedNode,
    agent_did: &str,
    background_executions: &crate::hook::BackgroundExecutionRegistry,
) -> Result<Vec<PeriodicRecoverySweepRun>> {
    let mut runs = Vec::with_capacity(PERIODIC_RECOVERY_SWEEP_EXECUTORS.len());
    for executor in PERIODIC_RECOVERY_SWEEP_EXECUTORS {
        let metadata = PERIODIC_RECOVERY_SWEEP_METADATA[executor.metadata_index];
        match (executor.run)(node, agent_did, background_executions).await {
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

fn reconcile_subagent_liveness<'a>(
    node: &'a EmbeddedNode,
    agent_did: &'a str,
    _background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_subagent_liveness(node, agent_did)
            .await
            .map(PeriodicRecoverySweepOutcome::SubagentLiveness)
    })
}

fn reconcile_terminal_parent_owned_tools<'a>(
    node: &'a EmbeddedNode,
    agent_did: &'a str,
    _background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_terminal_parent_owned_tools(node, agent_did)
            .await
            .map(PeriodicRecoverySweepOutcome::TerminalParentTools)
    })
}

fn reconcile_orphaned_background_tools<'a>(
    node: &'a EmbeddedNode,
    agent_did: &'a str,
    background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_orphaned_background_tools(
            node,
            agent_did,
            background_executions,
        )
        .await
        .map(PeriodicRecoverySweepOutcome::OrphanedBackgroundTools)
    })
}

fn reconcile_background_completion_side_effects<'a>(
    node: &'a EmbeddedNode,
    agent_did: &'a str,
    _background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_background_completion_side_effects(node, agent_did)
            .await
            .map(PeriodicRecoverySweepOutcome::BackgroundCompletionSideEffects)
    })
}

fn repair_terminal_requests<'a>(
    node: &'a EmbeddedNode,
    agent_did: &'a str,
    _background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        RequestLifecycle::repair_terminal_requests(node, agent_did)
            .await
            .map(PeriodicRecoverySweepOutcome::RequestTerminalRepair)
    })
}

fn recover_inference_calls<'a>(
    node: &'a EmbeddedNode,
    agent_did: &'a str,
    _background_executions: &'a crate::hook::BackgroundExecutionRegistry,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        InferenceCall::recover_all(node, agent_did)
            .await
            .map(PeriodicRecoverySweepOutcome::InferenceCalls)
    })
}
