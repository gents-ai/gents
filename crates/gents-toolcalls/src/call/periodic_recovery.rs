use anyhow::Result;
use defra_node::Node as EmbeddedNode;

use crate::call::lifecycle::{
    BackgroundCompletionSideEffectReport, OrphanedBackgroundToolReport, TerminalParentToolReport,
    ToolCallLifecycle,
};
use gents_inference::admission::{InferenceCall, InferenceCallRecoveryReport};
use gents_loop::tool::BoxFuture;
use gents_session::request::lifecycle::{RequestLifecycle, TerminalRepairReport};

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
const OWED_DELIVERY_SWEEP_IDS: &[&str] = &["lifecycle_hook_redrive_owed_deliveries"];

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
    OwedDeliveries(gents_session::request::hooks::owed::OwedSweepReport),
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
            Self::OwedDeliveries(report) => report.is_noop(),
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
    &'a [std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    &'a crate::call::hook::BackgroundExecutionRegistry,
    &'a dyn gents_session::session::message::SessionMessageGoals,
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
    PeriodicRecoverySweepMetadata {
        sweep_ids: OWED_DELIVERY_SWEEP_IDS,
        rust_function: "hooks::owed::redrive_pending",
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
    PeriodicRecoverySweepExecutor {
        metadata_index: 8,
        run: redrive_owed_deliveries,
    },
];

pub fn periodic_recovery_sweep_metadata() -> &'static [PeriodicRecoverySweepMetadata] {
    PERIODIC_RECOVERY_SWEEP_METADATA
}

pub async fn run_periodic_recovery_sweeps_with_identities(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    identities: &[std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    background_executions: &crate::call::hook::BackgroundExecutionRegistry,
    goals: &dyn gents_session::session::message::SessionMessageGoals,
) -> Result<Vec<PeriodicRecoverySweepRun>> {
    let mut runs = Vec::with_capacity(PERIODIC_RECOVERY_SWEEP_EXECUTORS.len());
    for executor in PERIODIC_RECOVERY_SWEEP_EXECUTORS {
        let metadata = PERIODIC_RECOVERY_SWEEP_METADATA[executor.metadata_index];
        match (executor.run)(node, node_did, identities, background_executions, goals).await {
            Ok(outcome) => runs.push(PeriodicRecoverySweepRun { metadata, outcome }),
            Err(error) => {
                tracing::warn!(
                    sweep_ids = ?metadata.sweep_ids,
                    rust_function = metadata.rust_function,
                    error = %gents_protocol::error_chain::ErrorChain(error.as_ref()),
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
    identities: &'a [std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    background_executions: &'a crate::call::hook::BackgroundExecutionRegistry,
    _goals: &'a dyn gents_session::session::message::SessionMessageGoals,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_terminal_parent_owned_tools_with_executions(
            node,
            node_did,
            identities,
            background_executions,
        )
        .await
        .map(PeriodicRecoverySweepOutcome::TerminalParentTools)
    })
}

fn reconcile_orphaned_background_tools<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    identities: &'a [std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    background_executions: &'a crate::call::hook::BackgroundExecutionRegistry,
    _goals: &'a dyn gents_session::session::message::SessionMessageGoals,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_orphaned_background_tools(
            node,
            node_did,
            identities,
            background_executions,
        )
        .await
        .map(PeriodicRecoverySweepOutcome::OrphanedBackgroundTools)
    })
}

fn reconcile_background_completion_side_effects<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    identities: &'a [std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    _background_executions: &'a crate::call::hook::BackgroundExecutionRegistry,
    _goals: &'a dyn gents_session::session::message::SessionMessageGoals,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        ToolCallLifecycle::reconcile_background_completion_side_effects(node, node_did, identities)
            .await
            .map(PeriodicRecoverySweepOutcome::BackgroundCompletionSideEffects)
    })
}

fn repair_terminal_requests<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    identities: &'a [std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    _background_executions: &'a crate::call::hook::BackgroundExecutionRegistry,
    _goals: &'a dyn gents_session::session::message::SessionMessageGoals,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        RequestLifecycle::repair_terminal_requests_with_identities(node, node_did, identities)
            .await
            .map(PeriodicRecoverySweepOutcome::RequestTerminalRepair)
    })
}

fn recover_inference_calls<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    identities: &'a [std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    _background_executions: &'a crate::call::hook::BackgroundExecutionRegistry,
    _goals: &'a dyn gents_session::session::message::SessionMessageGoals,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        InferenceCall::recover_all_with_identities(node, node_did, identities)
            .await
            .map(PeriodicRecoverySweepOutcome::InferenceCalls)
    })
}

fn settle_session_message_rows<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    identities: &'a [std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    _background_executions: &'a crate::call::hook::BackgroundExecutionRegistry,
    goals: &'a dyn gents_session::session::message::SessionMessageGoals,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        let mut settled = 0;
        for identity in identities {
            if identity.did() == node_did {
                anyhow::bail!("runtime identity cannot settle agent work");
            }
            let actor = identity::Did::new(identity.did())?;
            let access = match gents_store::config_client::ConfigAccess::local_managed_agent(
                node.clone(),
                actor,
                identity.as_ref(),
            ) {
                Ok(access) => access,
                Err(error) => {
                    tracing::warn!(agent_did = identity.did(), %error, "session-message recovery signing capability unavailable");
                    continue;
                }
            };
            match crate::background::completion::settle_running_session_message_rows_with_access(
                node, &access, goals, node_did,
            )
            .await
            {
                Ok(count) => settled += count,
                Err(error) => {
                    tracing::warn!(agent_did = identity.did(), %error, "session-message recovery failed; will retry")
                }
            }
        }
        Ok(PeriodicRecoverySweepOutcome::SessionMessageRows(settled))
    })
}

fn recover_task_hook_records<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    identities: &'a [std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    background_executions: &'a crate::call::hook::BackgroundExecutionRegistry,
    _goals: &'a dyn gents_session::session::message::SessionMessageGoals,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        crate::call::task_hooks::recover_task_hook_records(
            node,
            node_did,
            identities,
            background_executions.task_hook_records(),
        )
        .await
        .map(PeriodicRecoverySweepOutcome::TaskHookRecords)
    })
}

fn redrive_owed_deliveries<'a>(
    node: &'a std::sync::Arc<EmbeddedNode>,
    node_did: &'a str,
    identities: &'a [std::sync::Arc<dyn gents_store::identity::NodeIdentity>],
    _background_executions: &'a crate::call::hook::BackgroundExecutionRegistry,
    _goals: &'a dyn gents_session::session::message::SessionMessageGoals,
) -> BoxFuture<'a, Result<PeriodicRecoverySweepOutcome>> {
    Box::pin(async move {
        gents_session::request::hooks::owed::redrive_pending(node, node_did, identities)
            .await
            .map(PeriodicRecoverySweepOutcome::OwedDeliveries)
            .map_err(anyhow::Error::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_lean_vocab::families::periodic_recovery_registry::{SweepInput, SweepOutput};

    #[test]
    fn periodic_registry_matches_the_lean_tick_order() {
        let lean_sweep_ids = std::cell::RefCell::new(Vec::new());
        gents_lean_vocab::families::periodic_recovery_registry::replay(|input: &SweepInput| {
            lean_sweep_ids.borrow_mut().push(input.sweep_id.clone());
            let metadata = periodic_recovery_sweep_metadata();
            let position_of = |sweep_id: &str| {
                metadata
                    .iter()
                    .position(|entry| entry.sweep_ids.contains(&sweep_id))
            };
            let position = position_of(&input.sweep_id);
            SweepOutput {
                position: position.map(|index| index as u64),
                rust_function: position.map(|index| metadata[index].rust_function.to_owned()),
                runs_after_session_message: matches!(
                    (position, position_of("tool_call_lifecycle_recover_session_message_rows")),
                    (Some(index), Some(session)) if index > session
                ),
            }
        });
        let lean_sweep_ids = lean_sweep_ids.into_inner();
        for metadata in periodic_recovery_sweep_metadata() {
            for sweep_id in metadata.sweep_ids {
                assert!(
                    lean_sweep_ids.iter().any(|lean| lean == sweep_id),
                    "periodic recovery sweep {sweep_id} is not a registered Lean sweep"
                );
            }
        }
    }
}
