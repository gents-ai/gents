//! Registry for recovery sweeps that must run on the live daemon tick.
//!
//! Lean owns the sweep vocabulary (`RecoverySweep.sweepId`, cadence, and
//! `rustFunction`). Rust owns the executable registry the daemon iterates.
//! Keeping the mapping explicit here makes new periodic sweeps discoverable by
//! conformance tests instead of burying them as ad hoc calls in the observer.

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::lifecycle::{RequestLifecycle, TerminalRepairReport};
use crate::llm::tool::BoxFuture;
use crate::tool_call_lifecycle::{
    SubagentLivenessReport, TerminalParentToolReport, ToolCallLifecycle,
};

const SUBAGENT_LIVENESS_SWEEP_IDS: &[&str] = &[
    "subagent_liveness_terminalize_expired_children",
    "subagent_liveness_interrupt_queued_descendants",
];
const REQUEST_TERMINAL_REPAIR_SWEEP_IDS: &[&str] = &["request_lifecycle_recover_all_requests"];
const TERMINAL_PARENT_TOOL_SWEEP_IDS: &[&str] =
    &["tool_call_lifecycle_reconcile_terminal_parent_owned_tools"];

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
}

impl PeriodicRecoverySweepOutcome {
    pub fn is_noop(&self) -> bool {
        match self {
            Self::RequestTerminalRepair(report) => report.is_noop(),
            Self::SubagentLiveness(report) => report.is_noop(),
            Self::TerminalParentTools(report) => report.is_noop(),
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

type PeriodicRecoverySweepFn =
    for<'a> fn(&'a EmbeddedNode, &'a str) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>>;

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
];

pub fn periodic_recovery_sweep_metadata() -> &'static [PeriodicRecoverySweepMetadata] {
    PERIODIC_RECOVERY_SWEEP_METADATA
}

pub async fn run_periodic_recovery_sweeps(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Vec<PeriodicRecoverySweepRun>> {
    let mut runs = Vec::with_capacity(PERIODIC_RECOVERY_SWEEP_EXECUTORS.len());
    for executor in PERIODIC_RECOVERY_SWEEP_EXECUTORS {
        let metadata = PERIODIC_RECOVERY_SWEEP_METADATA[executor.metadata_index];
        let outcome = (executor.run)(node, agent_did).await?;
        runs.push(PeriodicRecoverySweepRun { metadata, outcome });
    }
    Ok(runs)
}

fn reconcile_subagent_liveness<'a>(
    node: &'a EmbeddedNode,
    agent_did: &'a str,
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
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_terminal_parent_owned_tools(node, agent_did)
            .await
            .map(PeriodicRecoverySweepOutcome::TerminalParentTools)
    })
}

fn repair_terminal_requests<'a>(
    node: &'a EmbeddedNode,
    agent_did: &'a str,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        RequestLifecycle::repair_terminal_requests(node, agent_did)
            .await
            .map(PeriodicRecoverySweepOutcome::RequestTerminalRepair)
    })
}
