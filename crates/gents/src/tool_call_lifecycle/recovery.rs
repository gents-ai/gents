//! Recovery for persisted running tool calls: the startup sweep over rows
//! orphaned by a daemon restart, the periodic subagent-liveness sweep
//! (#465) that terminalizes expired children and orphaned queued descendants
//! on the live reconciler tick, and the live terminal-parent owned-tool
//! cleanup (#837) that cancels running composites/tools whose parent is
//! already terminal without waiting for deadline or daemon restart.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::background_completion::ensure_background_subagent_completion_side_effects;
use crate::background_tools::{
    child_request_completed, fail_running_subagent_tool_call, load_parent_subagent_authorization,
    project_child_terminal, subagent_spawn_denial, subagent_tool_not_allowed_payload,
};
use crate::graphql::{escape_graphql_string, response_has_documents};

use super::{
    subagent_request::create_subagent_request_with_request_id_and_workspace,
    subagent_workspace::{resolve_child_workspace, ParentWorkspaceStamp},
    AwaitMode, CancelCause, ChildTerminal, FailureClass, ToolCallLifecycle, ToolCallState,
};

async fn execute_mutation_with_retry(
    node: &std::sync::Arc<EmbeddedNode>,
    mutation: &str,
    operation: &'static str,
) -> Result<defra_node::QueryResponse> {
    crate::config_client::ConfigAccess::write_local_response(node, operation, mutation).await
}

#[derive(Debug, Default)]
pub struct ToolCallRecoveryReport {
    pub tool_calls_recovered: usize,
    pub notifications_repaired: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SubagentLivenessReport {
    pub expired_children_terminalized: usize,
    pub bridges_projected: usize,
}

impl SubagentLivenessReport {
    pub fn is_noop(&self) -> bool {
        *self == Self::default()
    }
}

/// Live reconcile of running tool rows owned by a terminal parent (#837).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TerminalParentToolReport {
    pub tool_calls_terminalized: usize,
    pub notifications_repaired: usize,
}

impl TerminalParentToolReport {
    pub fn is_noop(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct OrphanedBackgroundToolReport {
    pub tool_calls_terminalized: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct BackgroundCompletionSideEffectReport {
    pub side_effects_converged: usize,
}

impl BackgroundCompletionSideEffectReport {
    pub fn is_noop(&self) -> bool {
        *self == Self::default()
    }
}

impl OrphanedBackgroundToolReport {
    pub fn is_noop(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Deserialize)]
struct RunningToolCallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    request_doc_id: Option<String>,
    #[serde(default)]
    requester_did: Option<String>,
    /// Immutable owner principal stamped at create. Recovery scopes by this
    /// field — `request_id` alone is not unique across agents.
    #[serde(default)]
    agent_did: Option<String>,
    session_id: String,
    tool_call_id: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    delegated_input: Option<gents_protocol::output::DelegatedToolInput>,
    #[serde(default)]
    delegated_workspace: Option<gents_protocol::output::DelegatedWorkspace>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    deadline_at: Option<String>,
    #[serde(default)]
    await_mode: Option<String>,
    #[serde(default)]
    cancel_cause: Option<String>,
    #[serde(default)]
    child_request_id: Option<String>,
    #[serde(default)]
    spawn_target_did: Option<String>,
    #[serde(default)]
    spawn_behavior_id: Option<String>,
    #[serde(default)]
    unclaimed_deadline_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TerminalBackgroundToolRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    request_doc_id: Option<String>,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    requester_did: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    cancel_cause: Option<String>,
    #[serde(default)]
    child_request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpawnArgs {
    name: String,
    prompt: String,
    #[serde(default)]
    deadline: Option<String>,
    #[serde(default)]
    workspace: Option<crate::background_tools::SpawnWorkspaceArg>,
}

impl SpawnArgs {
    fn target_name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryOutcome {
    TimedOut,
    Cancelled,
    Failed,
    BackgroundInterrupted,
    UnclaimedCrossDeploymentSpawn,
    ProcessLost,
    TaskDeleted,
}

impl super::ToolCallLifecycle {
    /// Startup recovery without host process records: a surviving native
    /// background process cannot be proven owned, so its row settles as lost.
    pub async fn recover_all(
        node: &std::sync::Arc<EmbeddedNode>,
        agent_did: &str,
    ) -> Result<ToolCallRecoveryReport> {
        Self::recover_all_with_executions(
            node,
            agent_did,
            &crate::hook::BackgroundExecutionRegistry::default(),
        )
        .await
    }

    /// Startup recovery. Native background rows go through the host process
    /// owner in `executions`, which stops a proven-owned surviving process
    /// before its row is settled.
    pub async fn recover_all_with_executions(
        node: &std::sync::Arc<EmbeddedNode>,
        agent_did: &str,
        executions: &crate::hook::BackgroundExecutionRegistry,
    ) -> Result<ToolCallRecoveryReport> {
        let materialized_children = recover_orphan_subagent_children(node, agent_did).await?;
        if materialized_children > 0 {
            tracing::info!(
                materialized_children,
                "materialized orphan subagent child requests during tool-call recovery"
            );
        }

        let tool_calls_recovered = recover_stuck_running_tool_calls(node, agent_did).await?
            + Self::reconcile_orphaned_background_tools(node, agent_did, executions)
                .await?
                .tool_calls_terminalized;
        let notifications_repaired =
            Self::reconcile_background_completion_side_effects(node, agent_did)
                .await?
                .side_effects_converged;

        Ok(ToolCallRecoveryReport {
            tool_calls_recovered,
            notifications_repaired,
        })
    }

    /// Periodic subagent-liveness reconciliation (#465; Lean:
    /// `Recovery.expiredSubagentChildSweep`, cadence `periodic`). Startup recovery already terminalizes expired
    /// children and bridges terminal children — but only on restart. Without a
    /// restart, a background child whose executor died past its deadline stays
    /// `processing` forever: the bridge never projects a terminal result and
    /// the parent's response wait wedges. This applies the same transitions on
    /// the live reconciler tick:
    ///
    /// 1. Terminalize locally-owned claimed/processing children of running
    ///    bridges whose deadline has passed (a live executor enforces its own
    ///    request deadline, so an expired non-terminal row means the executor
    ///    is gone). Safe against races: the underlying mutation only flips
    ///    non-terminal rows.
    /// 2. For BACKGROUND bridges, immediately project the now-terminal child
    ///    onto the bridge (failed/deadline) and queue the parent wake
    ///    notification. Foreground bridges are left to their live waiter,
    ///    which polls the child edge and owns the bridge lifecycle in-memory.
    ///
    /// A parent's terminal state never releases its queued subagents.
    pub async fn reconcile_subagent_liveness(
        node: &std::sync::Arc<EmbeddedNode>,
        agent_did: &str,
    ) -> Result<SubagentLivenessReport> {
        let mut report = SubagentLivenessReport::default();

        let bridge_rows = load_running_subagent_bridge_rows(node).await?;
        // One batched liveness read for every bridge's child, instead of a
        // per-bridge query on the 5s tick.
        let child_ids = bridge_rows
            .iter()
            .filter_map(child_request_id)
            .collect::<Vec<_>>();
        let children = load_child_liveness_rows(node, &child_ids).await?;

        for row in &bridge_rows {
            let Some(child) = child_request_id(row).and_then(|id| children.get(id)) else {
                continue;
            };
            if !terminalize_expired_child_with_row(node, agent_did, row, child).await? {
                continue;
            }
            report.expired_children_terminalized += 1;
            if is_background_subagent_tool(row)
                && recover_bridge_terminal_child(node, agent_did, row).await?
            {
                report.bridges_projected += 1;
            }
        }

        if !report.is_noop() {
            tracing::info!(
                expired_children_terminalized = report.expired_children_terminalized,
                bridges_projected = report.bridges_projected,
                "reconciled subagent liveness"
            );
        }
        Ok(report)
    }

    /// Live reconcile: terminalize running tool calls whose parent request is
    /// already terminal (#837). Unlike full startup `recover_all`, this does
    /// **not** interrupt live-parent background tools (restart-only path).
    ///
    /// Scope and ordering:
    /// 1. Load only tool rows stamped with this agent's immutable `agent_did`
    ///    (not global `request_id` matches — that field is not unique).
    /// 2. Resolve the parent under the same DID; skip missing/foreign parents.
    /// 3. Require a terminal parent before any write.
    /// 4. Leave every native background row to the registry-aware orphan
    ///    sweep, regardless of parent state.
    /// 5. Project already-terminal children onto bridges first (matches
    ///    startup child-precedence so restart and live ticks converge).
    /// 6. Only then: a child-linked bridge under any terminal parent is kept
    ///    running as background work — no parent terminal is a cancel signal
    ///    for a subagent. Recovery never cascades into a child request.
    ///
    /// Covers running native tool calls stranded under a terminal parent with
    /// no executor active.
    pub async fn reconcile_terminal_parent_owned_tools(
        node: &std::sync::Arc<EmbeddedNode>,
        agent_did: &str,
    ) -> Result<TerminalParentToolReport> {
        let rows = load_running_tool_call_rows_for_agent(node, agent_did).await?;
        let mut report = TerminalParentToolReport::default();
        let mut parent_cache: std::collections::HashMap<String, Option<AgentRequestRow>> =
            std::collections::HashMap::new();

        for row in rows {
            // Defense in depth: never mutate a row whose stamped owner differs.
            if row.agent_did.as_deref() != Some(agent_did) {
                continue;
            }
            // Native background rows belong exclusively to the orphan sweep,
            // which checks volatile ownership and applies deadline/unclaimed
            // precedence before parent state. Keeping this sweep disjoint
            // prevents its earlier periodic slot from bypassing that classifier.
            if is_background_tool_row(&row) {
                continue;
            }

            let parent = match row
                .request_id
                .as_deref()
                .filter(|request_id| !request_id.is_empty())
            {
                Some(request_id) => {
                    if let Some(cached) = parent_cache.get(request_id) {
                        cached.clone()
                    } else {
                        let loaded = lookup_parent_request(node, agent_did, request_id).await?;
                        parent_cache.insert(request_id.to_string(), loaded.clone());
                        loaded
                    }
                }
                None => None,
            };
            // Ownership gate: parent must resolve under this agent's DID.
            let Some(parent) = parent else {
                continue;
            };
            // Live parents are out of scope for this sweep.
            if !request_is_terminal(&parent) {
                continue;
            }

            // Child-terminal precedence only after owner + terminal-parent
            // gates, matching the startup sweep's order.
            if recover_bridge_terminal_child(node, agent_did, &row).await? {
                report.tool_calls_terminalized += 1;
                tracing::info!(
                    doc_id = %row.doc_id,
                    request_id = row.request_id.as_deref().unwrap_or(""),
                    tool_call_id = %row.tool_call_id,
                    "reconciled running bridge from already-terminal child"
                );
                continue;
            }

            // A terminal parent never stops its subagents. Its terminal
            // accounting already retained an awaited bridge; repair one it
            // could not.
            if child_request_id(&row).is_some() {
                if await_mode(&row) == AwaitMode::Foreground {
                    if let Err(error) = retain_bridge_in_background(node, &row).await {
                        tracing::warn!(
                            doc_id = %row.doc_id,
                            tool_call_id = %row.tool_call_id,
                            error = %error,
                            "failed to retain subagent bridge of terminal parent in background"
                        );
                    }
                }
                continue;
            }

            // Parent-driven cause, shared with the startup/orphan classifier
            // (`classify_running_tool_recovery`) minus its deadline and
            // live-background-parent screens.
            let Some(outcome) = classify_terminal_parent_tool_recovery(&row, &parent) else {
                continue;
            };

            let deadline_at = parse_datetime(row.deadline_at.as_deref());
            let updated = match recover_tool_call_row(node, &row, deadline_at, outcome, true).await
            {
                Ok(updated) => updated,
                Err(error) => {
                    tracing::warn!(
                        doc_id = %row.doc_id,
                        request_id = row.request_id.as_deref().unwrap_or(""),
                        tool_call_id = %row.tool_call_id,
                        error = %error,
                        "failed to terminalize running tool owned by terminal parent"
                    );
                    continue;
                }
            };
            if !updated {
                // Lost CAS: concurrent complete/fail/cancel already terminalized.
                continue;
            }

            if is_background_tool_row(&row) {
                append_recovered_background_tool_completion(node, &row, outcome).await;
            }

            report.tool_calls_terminalized += 1;
            tracing::info!(
                doc_id = %row.doc_id,
                request_id = row.request_id.as_deref().unwrap_or(""),
                tool_call_id = %row.tool_call_id,
                lifecycle_state = %outcome.lifecycle_state().as_str(),
                "reconciled running tool owned by terminal parent"
            );
        }

        if !report.is_noop() {
            tracing::info!(
                tool_calls_terminalized = report.tool_calls_terminalized,
                notifications_repaired = report.notifications_repaired,
                "reconciled terminal-parent owned tools"
            );
        }
        Ok(report)
    }

    /// Native background rows (Lean `orphanedBackgroundToolSweep`), on the
    /// periodic tick and at startup. A row whose live worker is registered in
    /// `executions` belongs to that worker unless its task was deleted, in
    /// which case the cancellation is persisted first and the worker stopped.
    /// Without a live worker, the host process owner stops a proven-owned
    /// surviving process before the row is settled; a stop it cannot observe
    /// settles the row as lost, and a group it still observes running keeps
    /// the row running for a later tick.
    pub async fn reconcile_orphaned_background_tools(
        node: &std::sync::Arc<EmbeddedNode>,
        agent_did: &str,
        executions: &crate::hook::BackgroundExecutionRegistry,
    ) -> Result<OrphanedBackgroundToolReport> {
        let rows = load_running_tool_call_rows_for_agent(node, agent_did).await?;
        let mut report = OrphanedBackgroundToolReport::default();
        let mut running_background = std::collections::HashSet::new();

        for row in rows {
            if row.agent_did.as_deref() != Some(agent_did) || !is_background_tool_row(&row) {
                continue;
            }
            running_background.insert(row.tool_call_id.clone());
            let registered = executions.contains(&row.tool_call_id).await;
            let parent = match row.request_id.as_deref().filter(|id| !id.is_empty()) {
                Some(request_id) => lookup_parent_request(node, agent_did, request_id).await?,
                None => None,
            };
            // An unresolvable parent is an incomplete owner observation; it
            // licenses neither a signal nor a write.
            let Some(parent) = parent else {
                if unresolved_parent_warning_due(&row.doc_id) {
                    tracing::warn!(
                        doc_id = %row.doc_id,
                        tool_call_id = %row.tool_call_id,
                        request_id = row.request_id.as_deref().unwrap_or(""),
                        "background tool's parent request is unresolved; leaving it running"
                    );
                }
                continue;
            };
            let task_deleted = owner_task_deleted(node, agent_did, &parent).await?;
            if registered && !task_deleted {
                continue;
            }
            let deadline_at = parse_datetime(row.deadline_at.as_deref());

            if registered {
                // Persist the cause before signalling, as an explicit cancel
                // does, so the worker's own terminal write loses the compare.
                let updated = match recover_tool_call_row(
                    node,
                    &row,
                    deadline_at,
                    RecoveryOutcome::TaskDeleted,
                    true,
                )
                .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        tracing::warn!(
                            doc_id = %row.doc_id,
                            tool_call_id = %row.tool_call_id,
                            error = %error,
                            "failed to cancel background tool of a deleted task"
                        );
                        continue;
                    }
                };
                let process = executions
                    .stop_execution(&row.tool_call_id, &row.doc_id)
                    .await;
                if process != crate::managed_exec::ProcessStopOutcome::Stopped {
                    tracing::warn!(
                        doc_id = %row.doc_id,
                        tool_call_id = %row.tool_call_id,
                        process = process.as_str(),
                        "background process of a deleted task was not observed to stop"
                    );
                }
                if updated {
                    append_recovered_background_tool_completion(
                        node,
                        &row,
                        RecoveryOutcome::TaskDeleted,
                    )
                    .await;
                    report.tool_calls_terminalized += 1;
                }
                continue;
            }

            let process = executions
                .stop_execution(&row.tool_call_id, &row.doc_id)
                .await;
            let Some(outcome) =
                classify_orphaned_background_tool(&row, &parent, process, task_deleted, Utc::now())
            else {
                tracing::warn!(
                    doc_id = %row.doc_id,
                    tool_call_id = %row.tool_call_id,
                    process = process.as_str(),
                    "orphaned background process is still running; leaving its row running"
                );
                continue;
            };

            let updated = match recover_tool_call_row(node, &row, deadline_at, outcome, true).await
            {
                Ok(updated) => updated,
                Err(error) => {
                    tracing::warn!(
                        doc_id = %row.doc_id,
                        request_id = row.request_id.as_deref().unwrap_or(""),
                        session_id = %row.session_id,
                        tool_call_id = %row.tool_call_id,
                        error = %error,
                        "failed to reconcile orphaned background tool"
                    );
                    continue;
                }
            };
            executions.forget_process_record(&row.tool_call_id);
            if !updated {
                continue;
            }

            append_recovered_background_tool_completion(node, &row, outcome).await;
            report.tool_calls_terminalized += 1;
            tracing::info!(
                doc_id = %row.doc_id,
                tool_call_id = %row.tool_call_id,
                process = process.as_str(),
                lifecycle_state = %outcome.lifecycle_state().as_str(),
                "reconciled orphaned background tool"
            );
        }

        // A record whose execution is neither live here nor running is left
        // from a crash after settlement, or from a group that outlived its
        // settled row. Stop what is still proven owned, then forget it.
        for record in executions.process_record_list() {
            if running_background.contains(&record.tool_call_id)
                || executions.contains(&record.tool_call_id).await
            {
                continue;
            }
            let (before, after) = record.identity.stop().await;
            if crate::managed_exec::ProcessStopOutcome::from_observations(before, after)
                == crate::managed_exec::ProcessStopOutcome::StillRunning
            {
                tracing::warn!(
                    tool_call_id = %record.tool_call_id,
                    pid = record.identity.pid,
                    "settled background execution still has a running process"
                );
                continue;
            }
            executions.forget_process_record(&record.tool_call_id);
        }

        if !report.is_noop() {
            tracing::info!(
                tool_calls_terminalized = report.tool_calls_terminalized,
                "reconciled orphaned background tools"
            );
        }
        Ok(report)
    }

    /// Explicit cancellation of a running native background row that has no
    /// live worker in this runtime. The host process owner stops the process
    /// only if a durable record proves ownership. An observed stop settles
    /// the row as cancelled with `cause`; an unobserved one settles it as
    /// lost; a group still observed running leaves the row running. Returns
    /// the verdict and whether this call won the row's terminal compare.
    pub(crate) async fn cancel_unowned_background_tool(
        node: &std::sync::Arc<EmbeddedNode>,
        lifecycle: &mut ToolCallLifecycle,
        executions: &crate::hook::BackgroundExecutionRegistry,
        cause: CancelCause,
        completion_reason: &str,
    ) -> Result<(crate::managed_exec::ProcessStopOutcome, bool)> {
        use crate::managed_exec::ProcessStopOutcome;
        let doc_id = lifecycle
            .doc_id()
            .context("background cancellation requires physical tool identity")?
            .to_owned();
        let tool_call_id = lifecycle.tool_call_id().to_owned();
        let process = executions.stop_execution(&tool_call_id, &doc_id).await;
        let (won, status, reason) = match process {
            ProcessStopOutcome::StillRunning => return Ok((process, false)),
            ProcessStopOutcome::Stopped => (
                lifecycle
                    .cancel_during_run_owned(cause, completion_reason)
                    .await?,
                "cancelled",
                completion_reason,
            ),
            ProcessStopOutcome::NotOwned | ProcessStopOutcome::AlreadyExited => {
                let outcome = RecoveryOutcome::ProcessLost;
                let result = outcome.result_text(None);
                let class = outcome.failure_class().unwrap_or(FailureClass::External);
                let won = if lifecycle.is_bridge() {
                    lifecycle
                        .bridge_failure_with_completion_reason(
                            ChildTerminal::Failed {
                                reason: result,
                                failure_class: class,
                            },
                            outcome.notification_reason(),
                        )
                        .await?
                } else {
                    lifecycle
                        .fail_owned_with_completion_reason(
                            &result,
                            class,
                            outcome.notification_reason(),
                        )
                        .await?
                };
                (won, "failed", outcome.notification_reason())
            }
        };
        executions.forget_process_record(&tool_call_id);
        if won {
            if let Err(error) = crate::background_completion::append_background_tool_completion(
                node,
                lifecycle.session_id(),
                lifecycle.request_id(),
                &doc_id,
                lifecycle.tool_name(),
                status,
                "",
                Some(reason),
            )
            .await
            {
                tracing::warn!(
                    tool_call_id,
                    error = %error,
                    "failed to append cancelled background tool notification"
                );
            }
        }
        Ok((process, won))
    }

    /// Redrive the idempotent notification + session wake after the lifecycle
    /// row is already terminal. Persisted `status=completionPending:<reason>`
    /// advances to `completed` only after
    /// both side effects converge, so transient failures remain discoverable.
    pub async fn reconcile_background_completion_side_effects(
        node: &std::sync::Arc<EmbeddedNode>,
        agent_did: &str,
    ) -> Result<BackgroundCompletionSideEffectReport> {
        let rows = load_pending_background_completion_rows(node, agent_did).await?;
        let mut report = BackgroundCompletionSideEffectReport::default();

        for row in rows {
            if row.agent_did.as_deref() != Some(agent_did)
                || non_empty(row.child_request_id.as_deref()).is_some()
            {
                continue;
            }
            let Some(request_id) = non_empty(row.request_id.as_deref()) else {
                continue;
            };
            if lookup_parent_request(node, agent_did, request_id)
                .await?
                .is_none()
            {
                continue;
            }
            let Some(session_id) = non_empty(row.session_id.as_deref()) else {
                tracing::warn!(doc_id = %row.doc_id, "skipping completion redrive without session_id");
                continue;
            };
            let Some((status, reason)) = background_completion_projection(&row) else {
                continue;
            };

            let Some(request_doc_id) = non_empty(row.request_doc_id.as_deref()) else {
                tracing::warn!(doc_id = %row.doc_id, "skipping completion redrive without request_doc_id");
                continue;
            };
            let output = match crate::background_tools::canonical_tool_output(
                node,
                &row.doc_id,
                request_doc_id,
                session_id,
                agent_did,
                row.requester_did.as_deref(),
            )
            .await
            {
                Ok(output) => output,
                Err(error) => {
                    tracing::warn!(doc_id = %row.doc_id, error = %error, "canonical background output is unresolved");
                    continue;
                }
            };
            match crate::background_completion::append_background_tool_completion(
                node,
                session_id,
                request_id,
                &row.doc_id,
                &row.tool_name,
                status,
                &output,
                reason,
            )
            .await
            {
                Ok(()) => report.side_effects_converged += 1,
                Err(error) => tracing::warn!(
                    doc_id = %row.doc_id,
                    tool_call_id = row.tool_call_id.as_deref().unwrap_or(""),
                    error = %error,
                    "failed to redrive background completion side effects"
                ),
            }
        }

        if !report.is_noop() {
            tracing::info!(
                side_effects_converged = report.side_effects_converged,
                "reconciled background completion side effects"
            );
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::super::CancelPolicy;
    use super::*;
    use crate::lifecycle::{ClaimOutcome, RequestLifecycle, RequestTerminalOutcome};
    use crate::llm::message::{AssistantContent, Message, Text, ToolCall, ToolFunction};
    use crate::streaming::DefraStreamWriter;

    /// Mirrors `claimed_request` in `tool_call_lifecycle/delivery.rs`, with the
    /// child's immutable parent provenance stamped at create so the descendant
    /// edge corroborates the physical joins. The child's answer is a real
    /// `DefraStreamWriter` accepted native publication, terminalized through
    /// the owned completion loop with an exact terminal `Message` selection.
    async fn materialized_completed_child(
        node: &std::sync::Arc<EmbeddedNode>,
        child_request_id: &str,
        child_agent_did: &str,
    ) {
        let row = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
                escape_graphql_string(child_request_id),
                crate::watcher::AGENT_REQUEST_FIELDS,
            ))
            .await;
        let row: gents_protocol::row::AgentRequestRow =
            crate::graphql::first_row(&row, "AgentRequest")
                .unwrap()
                .unwrap();
        let mut lifecycle = RequestLifecycle::new_with_agent_did(
            node.clone(),
            child_request_id,
            child_agent_did,
            row.try_into().unwrap(),
            60,
        );
        assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
        let writer =
            DefraStreamWriter::new(node.clone(), child_agent_did, std::time::Duration::ZERO);
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        writer
            .start_provider_attempt(
                &lifecycle.request().doc_id,
                0,
                0,
                "inference.1".parse().unwrap(),
            )
            .await;
        let message = Message::Assistant {
            id: Some("child-answer".into()),
            content: vec![AssistantContent::Text(Text {
                text: "Durable child answer".into(),
            })],
        };
        let published = writer
            .publish_native_turn(&lifecycle, 0, 0, &message)
            .await
            .unwrap();
        lifecycle
            .terminalize_owned(
                RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: published.message_doc_id,
                },
                None,
            )
            .await
            .unwrap();
    }

    async fn published_parent_bridge(node: &std::sync::Arc<EmbeddedNode>) -> (String, String) {
        let now = crate::graphql::escape_graphql_string(&chrono::Utc::now().to_rfc3339());
        let created = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "parent", agent_did: "did:test:parent", behavior_id: "parent", session_id: "parent-session", retry_parent_request: "", retry_root_request: "parent", superseded_by_request: "", content: "Parent request", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{now}", retry_count: 0, max_retries: 3, subagent_depth: 0 }}) {{ _docID }} }}"#)).await;
        assert!(!created.has_errors(), "{:#?}", created.errors);
        let row = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "parent" }} }}) {{ {} }} }}"#,
                crate::watcher::AGENT_REQUEST_FIELDS,
            ))
            .await;
        let row: gents_protocol::row::AgentRequestRow =
            crate::graphql::first_row(&row, "AgentRequest")
                .unwrap()
                .unwrap();
        let mut lifecycle = RequestLifecycle::new_with_agent_did(
            node.clone(),
            "parent",
            "did:test:parent",
            row.try_into().unwrap(),
            60,
        );
        assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
        let writer =
            DefraStreamWriter::new(node.clone(), "did:test:parent", std::time::Duration::ZERO);
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        writer
            .start_provider_attempt(
                &lifecycle.request().doc_id,
                0,
                0,
                "inference.1".parse().unwrap(),
            )
            .await;
        let message = Message::Assistant {
            id: Some("parent-spawn".into()),
            content: vec![AssistantContent::ToolCall(ToolCall {
                id: "bridge".into(),
                call_id: None,
                function: ToolFunction::new(
                    crate::toolset::SPAWN_SUBAGENT_TOOL_NAME.into(),
                    serde_json::json!({"behavior_id":"child","prompt":"Child request","await_mode":"foreground"}),
                ),
                signature: None,
                additional_params: None,
            })],
        };
        let published = writer
            .publish_native_turn_with_spawn_admissions(
                &lifecycle,
                0,
                0,
                &message,
                &[crate::streaming::SpawnAdmissionPlan {
                    tool_call_id: "bridge".into(),
                    child_request_id: "child".into(),
                    spawn_target_did: "did:test:child".into(),
                    spawn_behavior_id: "child".into(),
                    delegated_workspace: None,
                    await_mode: AwaitMode::Foreground,
                }],
            )
            .await
            .unwrap();
        let accepted = published.accepted_tools.into_iter().next().unwrap();
        let bridge_doc = accepted.tool_call_doc_id.clone();
        let mut bridge = ToolCallLifecycle::from_accepted(
            node.clone(),
            "did:test:parent".into(),
            None,
            accepted,
            lifecycle.claimed_deadline_at().unwrap(),
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
        )
        .unwrap();
        bridge.start_running().await.unwrap();
        (lifecycle.request().doc_id.clone(), bridge_doc)
    }

    #[tokio::test]
    async fn completed_child_recovery_hydrates_materialized_answer_with_exact_scope() {
        let fixture_name = "recovery-completed-child";
        let child_request_id = format!("child-{fixture_name}");
        let crate::tool_call_lifecycle::admission_fixture::PublishedAdmission {
            node,
            path,
            tool: bridge,
            ..
        } = crate::tool_call_lifecycle::admission_fixture::published_admission(
            crate::tool_call_lifecycle::admission_fixture::PublishedAdmissionOptions {
                name: fixture_name.into(),
                real_identity: true,
                await_mode: AwaitMode::Foreground,
                cancel_policy: CancelPolicy::Cascade,
                start_running: true,
                request_created_at: None,
                spawn_plan: Some(crate::streaming::SpawnAdmissionPlan {
                    tool_call_id: "bridge-native-tool".into(),
                    child_request_id: child_request_id.clone(),
                    spawn_target_did: "did:test:test".into(),
                    spawn_behavior_id: "general".into(),
                    delegated_workspace: None,
                    await_mode: AwaitMode::Foreground,
                }),
                tool_name: None,
            },
        )
        .await
        .expect("publish canonical foreground subagent bridge");
        async fn create(
            node: &std::sync::Arc<EmbeddedNode>,
            collection: &str,
            input: serde_json::Value,
        ) -> String {
            let result = node
                .execute_request_with_retry(
                    defra_node::QueryRequest::new(format!(
                        "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
                    )).with_variables(serde_json::json!({"input": input})),
                    defra_node::ExecuteRetryPolicy::default(),
                )
                .await;
            assert!(!result.has_errors(), "{collection}: {:?}", result.errors);
            crate::graphql::single_mutation_document(&result, &format!("create_{collection}"))
                .expect("normalize create response")
                .expect("created document")
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .expect("created document ID")
                .to_string()
        }
        let parent_request_id = "request-recovery-completed-child";
        let parent_doc = bridge.request_doc_id().unwrap().to_owned();
        let bridge_doc = bridge.doc_id().unwrap().to_owned();
        crate::tool_call_lifecycle::create_subagent_request_with_request_id(
            &node,
            child_request_id.clone(),
            parent_request_id.into(),
            parent_doc.clone(),
            bridge.tool_call_id().to_owned(),
            bridge_doc.clone(),
            0,
            bridge.agent_did().to_owned(),
            "general".into(),
            "Child request".into(),
            None,
        )
        .await
        .expect("materialize signed child through the accepted bridge");
        // The child is a real claimed request whose answer was accepted through
        // `DefraStreamWriter` and terminalized with an exact `Message` selection.
        materialized_completed_child(&node, &child_request_id, bridge.agent_did()).await;
        // Newer canonical rows sharing logical labels must not displace the
        // exact child's answer: their physical identities differ from the
        // child's own. The decoys keep the same negative-control geometry as
        // the retired `AgentResponse` decoy — a foreign agent DID and a
        // foreign physical child document, so no exact-scope join can adopt
        // them. The foreign `AgentMessage` header alone is inert: the terminal
        // selection pins the child's own header, and reconstruction reads
        // payload bytes only through the child's exact `AgentOutputSegment`s.
        create(
            &node,
            "AgentMessage",
            serde_json::json!({
                "message_key": "foreign", "session_id": "child-session",
                "agent_did": "did:test:foreign",
                "publication": {"kind": "request_execution", "execution_generation": "foreign-gen"},
                "outcome": "complete", "sequence": 8, "role": "assistant", "blocks": null,
                "created_at": "2026-09-02T00:00:00Z"
            }),
        )
        .await;
        create(&node, "AgentOutputSegment", serde_json::json!({
            "agent_did": "did:test:foreign", "session_id": "child-session",
            "request_doc_id": "foreign-child-doc",
            "source": {"kind": "provider_turn", "scope": "inference.1", "turn_index": 0, "attempt": 0},
            "writer": {"kind": "request_execution", "execution_generation": "foreign-gen"},
            "ordinal": 0, "runs": [{"stream": 0, "bytes": 14,
                "declaration": {"block_index": 0, "part_index": 0, "payload": {"kind": "text"}}}],
            "payload": "Foreign answer",
            "close": {"kind": "closed", "outcome": "complete", "segments": 1, "stream_bytes": [14]},
            "created_at": "2026-09-02T00:00:00Z"
        })).await;
        let mut row = load_running_tool_call_rows_for_agent(&node, bridge.agent_did())
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.doc_id == bridge_doc)
            .expect("accepted bridge remains an exact running recovery row");
        row.request_doc_id = Some("foreign-parent-doc".into());
        assert!(
            recover_bridge_terminal_child(&node, bridge.agent_did(), &row)
                .await
                .is_err()
        );
        row.request_doc_id = Some(parent_doc);
        assert!(
            recover_bridge_terminal_child(&node, "did:test:foreign", &row)
                .await
                .is_err()
        );
        assert!(
            recover_bridge_terminal_child(&node, bridge.agent_did(), &row)
                .await
                .unwrap()
        );
        let stored = node.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ lifecycle_state }} }}"#, escape_graphql_string(&row.doc_id))).await;
        assert!(!stored.has_errors(), "{:?}", stored.errors);
        let tool = &stored.data.as_ref().unwrap()["AgentToolCall"][0];
        assert_eq!(tool["lifecycle_state"], "completed");
        node.shutdown().await;
        std::fs::remove_dir_all(path).expect("remove recovery fixture database");
    }

    #[test]
    fn timeout_recovery_persists_external_failure_class() {
        assert_eq!(
            RecoveryOutcome::TimedOut.failure_class(),
            Some(FailureClass::External)
        );
        assert_eq!(RecoveryOutcome::Cancelled.failure_class(), None);
    }

    /// The one deadline-expiry predicate (#1334), shared by tool-call
    /// recovery here and by `gents-cli`'s fleet-slot and liveness
    /// snapshots. Past deadlines (including exactly `now`) are expired;
    /// future, missing, and malformed deadlines are documented as not
    /// expired.
    #[test]
    fn deadline_is_expired_covers_past_future_missing_and_malformed() {
        let now = DateTime::parse_from_rfc3339("2026-09-03T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let past = "2026-09-03T11:59:59Z";
        let exactly_now = "2026-09-03T12:00:00Z";
        let future = "2026-09-03T12:00:01Z";

        assert!(deadline_is_expired(now, Some(past)), "past deadline");
        assert!(
            deadline_is_expired(now, Some(exactly_now)),
            "a deadline reached exactly now has expired"
        );
        assert!(!deadline_is_expired(now, Some(future)), "future deadline");
        assert!(!deadline_is_expired(now, None), "missing deadline");
        assert!(
            !deadline_is_expired(now, Some("not-a-timestamp")),
            "malformed deadline"
        );
        assert!(
            !deadline_is_expired(now, Some("   ")),
            "blank deadline is documented as missing, not malformed"
        );

        // The typed variant agrees with the string-form convenience.
        assert_eq!(
            deadline_at_is_expired(now, parse_datetime(Some(past))),
            deadline_is_expired(now, Some(past)),
        );
        assert!(!deadline_at_is_expired(now, None));
    }

    #[test]
    fn background_completion_reason_comes_from_cursor_not_tool_text() {
        let row = TerminalBackgroundToolRow {
            doc_id: "doc-1".to_string(),
            request_id: Some("request-1".to_string()),
            request_doc_id: Some("request-doc-1".to_string()),
            agent_did: Some("did:test:agent".to_string()),
            requester_did: None,
            session_id: Some("session-1".to_string()),
            tool_call_id: Some("tool-1".to_string()),
            tool_name: "test_tool".to_string(),
            status: "completionPending:tool_failed".to_string(),
            lifecycle_state: Some("failed".to_string()),
            cancel_cause: None,
            child_request_id: None,
        };
        assert_eq!(
            background_completion_projection(&row),
            Some(("failed", Some("tool_failed")))
        );
    }

    #[test]
    fn background_completion_redrive_preserves_custom_cancel_reason() {
        let row = TerminalBackgroundToolRow {
            doc_id: "doc-custom".to_string(),
            request_id: Some("request-custom".to_string()),
            request_doc_id: Some("request-doc-custom".to_string()),
            agent_did: Some("did:test:agent".to_string()),
            requester_did: None,
            session_id: Some("session-custom".to_string()),
            tool_call_id: Some("tool-custom".to_string()),
            tool_name: "test_tool".to_string(),
            status: "completionPending:operator requested drain".to_string(),
            lifecycle_state: Some("cancelled".to_string()),
            cancel_cause: Some("userCancelled".to_string()),
            child_request_id: None,
        };
        assert_eq!(
            background_completion_projection(&row),
            Some(("cancelled", Some("operator requested drain")))
        );
    }

    /// Every native restart and orphan witness, including the still-running
    /// verdict no test host can construct, through the native classifier.
    #[test]
    fn generated_native_process_verdicts_match_orphan_classifier() {
        use crate::managed_exec::ProcessStopOutcome;
        let verdict = |name: &str| match name {
            "stopped" => ProcessStopOutcome::Stopped,
            "alreadyExited" => ProcessStopOutcome::AlreadyExited,
            "stillRunning" => ProcessStopOutcome::StillRunning,
            "notOwned" => ProcessStopOutcome::NotOwned,
            other => panic!("unknown Lean stop outcome {other}"),
        };
        let row = |deadline: bool, unclaimed: bool| -> RunningToolCallRow {
            serde_json::from_value(serde_json::json!({
                "_docID": "tool-doc",
                "session_id": "session",
                "tool_call_id": "spawned:parent",
                "await_mode": "background",
                "deadline_at": if deadline { "2020-01-01T00:00:00Z" } else { "2999-01-01T00:00:00Z" },
                "unclaimed_deadline_at": if unclaimed { Some("2020-01-01T00:00:00Z") } else { None },
            }))
            .unwrap()
        };
        let parent = |state: RequestLifecycleState| AgentRequestRow {
            request_id: "parent".to_string(),
            lifecycle_state: Some(state),
            ..Default::default()
        };
        let lean_cause = |outcome: RecoveryOutcome| match outcome {
            RecoveryOutcome::TimedOut => "deadlineExceeded",
            RecoveryOutcome::Cancelled => "parentInterrupted",
            RecoveryOutcome::Failed => "parentTerminal",
            RecoveryOutcome::BackgroundInterrupted => "TerminalizeBackgroundedAsInterrupted",
            RecoveryOutcome::UnclaimedCrossDeploymentSpawn => "unclaimedCrossPrincipalSpawn",
            RecoveryOutcome::ProcessLost => "processLost",
            RecoveryOutcome::TaskDeleted => "taskDeleted",
        };
        let mut checked = 0;
        for case in crate::lean_vocab_test::lean_restart_disposition_cases() {
            let parent_state = match case.parent_observation.as_str() {
                "live" => RequestLifecycleState::Processing,
                "interrupted" => RequestLifecycleState::Interrupted,
                "cleanlyCompleted" => RequestLifecycleState::Completed,
                "otherTerminal" => RequestLifecycleState::Failed,
                _ => continue,
            };
            if case.await_mode != "background" || case.child_linked {
                continue;
            }
            let outcome = classify_orphaned_background_tool(
                &row(case.deadline_expired, case.unclaimed_expired),
                &parent(parent_state),
                verdict(&case.process_outcome),
                false,
                Utc::now(),
            );
            assert_eq!(
                outcome.map(lean_cause),
                case.cause.as_deref(),
                "{}",
                case.name
            );
            if let Some(outcome) = outcome {
                assert_eq!(
                    Some(outcome.notification_reason()),
                    case.notification_reason.as_deref(),
                    "{}",
                    case.name
                );
            }
            checked += 1;
        }
        for case in crate::lean_vocab_test::lean_recovery_sweep_cases() {
            let (Some(process), Some(false), Some(task_deleted)) = (
                case.process_outcome.as_deref(),
                case.execution_registered,
                case.owner_task_deleted,
            ) else {
                continue;
            };
            let parent_state = if case.parent_live == Some(true) {
                RequestLifecycleState::Processing
            } else if case.parent_interrupted == Some(true) {
                RequestLifecycleState::Interrupted
            } else if case.parent_terminal == Some(true) {
                RequestLifecycleState::Completed
            } else {
                continue;
            };
            let outcome = classify_orphaned_background_tool(
                &row(
                    case.deadline_expired == Some(true),
                    case.unclaimed_expired == Some(true),
                ),
                &parent(parent_state),
                verdict(process),
                task_deleted,
                Utc::now(),
            );
            assert_eq!(
                outcome.map(lean_cause),
                case.recovery_cause.as_deref(),
                "{}",
                case.name
            );
            checked += 1;
        }
        assert!(checked >= 12, "only {checked} generated native witnesses");
    }

    #[test]
    fn no_terminal_parent_terminalizes_linked_bridges() {
        let row = |await_mode: &str, child: Option<&str>| -> RunningToolCallRow {
            serde_json::from_value(serde_json::json!({
                "_docID": "tool-doc",
                "session_id": "session",
                "tool_call_id": "tool",
                "await_mode": await_mode,
                "child_request_id": child,
            }))
            .unwrap()
        };
        let parent = |state| AgentRequestRow {
            request_id: "parent".to_string(),
            lifecycle_state: Some(state),
            ..Default::default()
        };
        for await_mode in ["foreground", "background"] {
            let bridge = row(await_mode, Some("child"));
            for state in [
                RequestLifecycleState::Interrupted,
                RequestLifecycleState::Completed,
                RequestLifecycleState::Failed,
                RequestLifecycleState::Dead,
                RequestLifecycleState::Superseded,
            ] {
                assert_eq!(
                    classify_terminal_parent_tool_recovery(&bridge, &parent(state)),
                    None
                );
            }
        }
        assert_eq!(
            classify_terminal_parent_tool_recovery(
                &row("foreground", None),
                &parent(RequestLifecycleState::Interrupted)
            ),
            Some(RecoveryOutcome::Cancelled)
        );
    }
}

async fn load_accepted_spawn_arguments(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &RunningToolCallRow,
) -> Result<String> {
    if let Some(delegated) = &row.delegated_input {
        return Ok(delegated.arguments.clone());
    }
    let agent_did = non_empty(row.agent_did.as_deref()).context("tool row omitted agent_did")?;
    let request_doc_id =
        non_empty(row.request_doc_id.as_deref()).context("tool row omitted request_doc_id")?;
    let accepted = super::ToolCallLifecycle::load_accepted_for_dispatch(
        node,
        &row.doc_id,
        agent_did,
        &row.session_id,
        row.requester_did.as_deref(),
    )
    .await?;
    anyhow::ensure!(
        accepted.request_doc_id == request_doc_id,
        "accepted tool binding crossed physical request identity"
    );
    let stream = crate::session::load_canonical_payload_from_node(
        node,
        request_doc_id,
        agent_did,
        row.requester_did.as_deref(),
        &accepted.arguments,
    )
    .await?;
    anyhow::ensure!(
        matches!(
            stream.declaration.payload,
            gents_protocol::output::StreamPayload::ToolArguments { .. }
        ),
        "accepted tool arguments are not native JSON"
    );
    Ok(stream.text)
}

async fn recover_orphan_subagent_children(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
) -> Result<usize> {
    let rows = load_running_tool_call_rows_for_agent(node, agent_did).await?;
    let mut materialized = 0;

    for row in rows {
        if row.agent_did.as_deref() != Some(agent_did) {
            continue;
        }
        let Some(child_request_id) = child_request_id(&row).map(str::to_string) else {
            continue;
        };
        if child_request_exists(node, &child_request_id).await? {
            continue;
        }
        // An expired bridge is settled (and fenced) by this same recovery
        // pass; materializing its child now would start work nobody awaits
        // (Lean `SpawnClaimFence`: settled bridges refuse materialization).
        if deadline_is_expired(Utc::now(), row.deadline_at.as_deref()) {
            continue;
        }
        if row
            .unclaimed_deadline_at
            .as_deref()
            .is_some_and(|deadline| !deadline.is_empty())
        {
            continue;
        }

        let parent_request_id = match row
            .request_id
            .as_deref()
            .filter(|request_id| !request_id.is_empty())
        {
            Some(request_id) => request_id.to_string(),
            None => {
                tracing::warn!(
                    doc_id = %row.doc_id,
                    session_id = %row.session_id,
                    tool_call_id = %row.tool_call_id,
                    child_request_id = %child_request_id,
                    "cannot materialize orphan subagent child without parent request_id"
                );
                continue;
            }
        };

        let Some(parent) = lookup_parent_request(node, agent_did, &parent_request_id).await? else {
            tracing::warn!(
                doc_id = %row.doc_id,
                request_id = %parent_request_id,
                session_id = %row.session_id,
                tool_call_id = %row.tool_call_id,
                child_request_id = %child_request_id,
                "cannot materialize orphan subagent child because parent AgentRequest is missing"
            );
            continue;
        };

        let accepted_arguments = match load_accepted_spawn_arguments(node, &row).await {
            Ok(arguments) => arguments,
            Err(error) => {
                tracing::warn!(
                    doc_id = %row.doc_id,
                    request_id = %parent_request_id,
                    error = %error,
                    "cannot materialize orphan subagent child without exact accepted arguments"
                );
                continue;
            }
        };
        let spawn_args = match serde_json::from_str::<SpawnArgs>(&accepted_arguments) {
            Ok(spawn_args) => spawn_args,
            Err(error) => {
                tracing::warn!(
                    doc_id = %row.doc_id,
                    request_id = %parent_request_id,
                    session_id = %row.session_id,
                    tool_call_id = %row.tool_call_id,
                    child_request_id = %child_request_id,
                    error = %error,
                    "cannot materialize orphan subagent child because tool args are invalid"
                );
                continue;
            }
        };
        let Some(row_spawn_target_did) = non_empty(row.spawn_target_did.as_deref()) else {
            fail_unauthorized_orphan_subagent_tool_call(
                node,
                &row,
                "/spawn_target_did",
                "",
                "subagent tool call is missing immutable spawn_target_did",
                &[],
            )
            .await?;
            continue;
        };
        let Some(row_spawn_behavior_id) = non_empty(row.spawn_behavior_id.as_deref()) else {
            fail_unauthorized_orphan_subagent_tool_call(
                node,
                &row,
                "/spawn_behavior_id",
                "",
                "subagent tool call is missing immutable spawn_behavior_id",
                &[],
            )
            .await?;
            continue;
        };

        let parent_depth = parent
            .subagent_depth
            .and_then(|depth| u32::try_from(depth).ok())
            .unwrap_or(0);
        let deadline =
            effective_deadline(row.deadline_at.as_deref(), spawn_args.deadline.as_deref());

        let authorization = match load_parent_subagent_authorization(node, &parent_request_id).await
        {
            Ok(authorization) => authorization,
            Err(error) => {
                let failed = fail_unauthorized_orphan_subagent_tool_call(
                    node,
                    &row,
                    "/name",
                    spawn_args.target_name(),
                    "subagent authorization could not be verified for this behavior",
                    &[],
                )
                .await?;
                tracing::warn!(
                    doc_id = %row.doc_id,
                    request_id = %parent_request_id,
                    session_id = %row.session_id,
                    tool_call_id = %row.tool_call_id,
                    child_request_id = %child_request_id,
                    target_name = %spawn_args.target_name(),
                    failed_tool_call = failed,
                    error = %error,
                    "cannot materialize orphan subagent child because parent authorization could not be verified"
                );
                continue;
            }
        };
        let row_await_mode = await_mode(&row);
        let tool_name = subagent_tool_name(&row);
        if let Some(denial) = subagent_spawn_denial(
            &authorization,
            spawn_args.target_name(),
            row_await_mode,
            tool_name,
            agent_did,
        ) {
            let failed = fail_unauthorized_orphan_subagent_tool_call(
                node,
                &row,
                denial.path,
                &denial.requested,
                denial.message,
                &authorization.allowed_target_names(),
            )
            .await?;
            tracing::warn!(
                doc_id = %row.doc_id,
                request_id = %parent_request_id,
                session_id = %row.session_id,
                tool_call_id = %row.tool_call_id,
                child_request_id = %child_request_id,
                parent_behavior_id = %authorization.behavior_id,
                target_name = %spawn_args.target_name(),
                await_mode = %row_await_mode.as_str(),
                failed_tool_call = failed,
                "cannot materialize orphan subagent child because spawn is not authorized"
            );
            continue;
        }

        let Some(target) = authorization
            .resolve_target(spawn_args.target_name())
            .cloned()
        else {
            fail_unauthorized_orphan_subagent_tool_call(
                node,
                &row,
                "/name",
                spawn_args.target_name(),
                "subagent target disappeared after authorization",
                &authorization.allowed_target_names(),
            )
            .await?;
            continue;
        };
        if target.target_agent_did.as_str() != row_spawn_target_did
            || target.behavior_id.as_str() != row_spawn_behavior_id
        {
            let failed = fail_unauthorized_orphan_subagent_tool_call(
                node,
                &row,
                "/name",
                spawn_args.target_name(),
                "resolved target does not match immutable spawn route",
                &authorization.allowed_target_names(),
            )
            .await?;
            tracing::warn!(
                doc_id = %row.doc_id,
                request_id = %parent_request_id,
                session_id = %row.session_id,
                tool_call_id = %row.tool_call_id,
                child_request_id = %child_request_id,
                spawn_target_did = %row_spawn_target_did,
                spawn_behavior_id = %row_spawn_behavior_id,
                resolved_target_did = %target.target_agent_did,
                resolved_behavior_id = %target.behavior_id,
                failed_tool_call = failed,
                "cannot materialize orphan subagent child because immutable route drifted"
            );
            continue;
        }

        let child_agent_did = row_spawn_target_did.to_string();
        let Some(parent_request_doc_id) = row
            .request_doc_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            tracing::warn!(
                doc_id = %row.doc_id,
                request_id = %parent_request_id,
                tool_call_id = %row.tool_call_id,
                "cannot materialize orphan subagent child without exact parent request document"
            );
            continue;
        };
        let delegated_workspace = row
            .delegated_workspace
            .as_ref()
            .map(|workspace| crate::lifecycle::WorkspaceLineage {
                workspace_id: Some(workspace.workspace_id.clone()),
                workspace_owner_agent_did: Some(workspace.workspace_owner_agent_did.clone()),
                workspace_authority: Some(workspace.workspace_authority.clone()),
                workspace_seal_hash: workspace.workspace_seal_hash.clone(),
            })
            .unwrap_or_default();
        if let Err(error) = delegated_workspace.require_authority_if_workspace_id() {
            tracing::warn!(
                doc_id = %row.doc_id,
                request_id = %parent_request_id,
                tool_call_id = %row.tool_call_id,
                error = %error,
                "cannot materialize orphan subagent child from malformed immutable workspace provenance"
            );
            continue;
        }
        let parent_workspace = ParentWorkspaceStamp::from_fields(
            parent
                .agent_did
                .as_deref()
                .context("workspace parent request lacks agent_did")?,
            delegated_workspace.workspace_id.as_deref(),
            delegated_workspace.workspace_owner_agent_did.as_deref(),
            delegated_workspace.workspace_authority.as_deref(),
            delegated_workspace.workspace_seal_hash.as_deref(),
        );
        let operator_tool_root = crate::workspace::process_operator_tool_root();
        let workspace = match resolve_child_workspace(
            node,
            &parent_workspace,
            spawn_args.workspace.as_ref(),
            None,
            &child_agent_did,
            &row.tool_call_id,
            &parent_request_id,
            operator_tool_root.as_deref(),
        )
        .await
        {
            Ok(lineage) => lineage,
            Err(error) => {
                tracing::warn!(
                    doc_id = %row.doc_id,
                    request_id = %parent_request_id,
                    tool_call_id = %row.tool_call_id,
                    child_request_id = %child_request_id,
                    error = %error,
                    "cannot materialize orphan subagent child because workspace could not be resolved"
                );
                continue;
            }
        };
        if let Err(error) = create_subagent_request_with_request_id_and_workspace(
            node,
            child_request_id.clone(),
            parent_request_id.clone(),
            parent_request_doc_id.to_string(),
            row.tool_call_id.clone(),
            row.doc_id.clone(),
            parent_depth,
            child_agent_did,
            row_spawn_behavior_id.to_string(),
            spawn_args.prompt,
            deadline,
            workspace,
        )
        .await
        {
            tracing::warn!(
                doc_id = %row.doc_id,
                request_id = %parent_request_id,
                session_id = %row.session_id,
                tool_call_id = %row.tool_call_id,
                child_request_id = %child_request_id,
                error = %error,
                "failed to materialize orphan subagent child request during recovery"
            );
            continue;
        }

        materialized += 1;
        tracing::info!(
            doc_id = %row.doc_id,
            request_id = %parent_request_id,
            session_id = %row.session_id,
            tool_call_id = %row.tool_call_id,
            child_request_id = %child_request_id,
            "materialized orphan subagent child request during recovery"
        );
    }

    Ok(materialized)
}

async fn fail_unauthorized_orphan_subagent_tool_call(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &RunningToolCallRow,
    path: &str,
    requested: &str,
    message: impl Into<String>,
    allowed_targets: &[String],
) -> Result<bool> {
    let tool_name = subagent_tool_name(row);
    let payload =
        subagent_tool_not_allowed_payload(tool_name, path, requested, message, allowed_targets);
    fail_running_subagent_tool_call(
        node,
        &row.doc_id,
        &payload,
        FailureClass::ServiceUnavailable,
    )
    .await
}

async fn recover_stuck_running_tool_calls(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
) -> Result<usize> {
    let rows = load_running_tool_call_rows_for_agent(node, agent_did).await?;

    let mut recovered = 0;
    for row in rows {
        // Native background rows need the host process owner; the orphan
        // sweep settles them after this pass.
        if row.agent_did.as_deref() != Some(agent_did) || is_background_tool_row(&row) {
            continue;
        }

        let deadline_at = parse_datetime(row.deadline_at.as_deref());
        let parent = match row
            .request_id
            .as_deref()
            .filter(|request_id| !request_id.is_empty())
        {
            Some(request_id) => lookup_parent_request(node, agent_did, request_id).await?,
            None => None,
        };

        if child_request_id(&row).is_some() {
            let _ = terminalize_expired_local_child_request(node, agent_did, &row).await?;
        }

        if recover_bridge_terminal_child(node, agent_did, &row).await? {
            recovered += 1;
            continue;
        }

        let outcome = classify_running_tool_recovery(&row, parent.as_ref(), Utc::now());

        let Some(outcome) = outcome else {
            if child_request_id(&row).is_some() && parent.as_ref().is_some_and(request_is_terminal)
            {
                match retain_bridge_in_background(node, &row).await {
                    Ok(true) => recovered += 1,
                    Ok(false) => {}
                    Err(error) => tracing::warn!(
                        doc_id = %row.doc_id,
                        tool_call_id = %row.tool_call_id,
                        error = %error,
                        "failed to retain subagent bridge of terminal parent in background"
                    ),
                }
                continue;
            }
            if is_background_subagent_tool(&row) {
                tracing::info!(
                    doc_id = %row.doc_id,
                    request_id = row.request_id.as_deref().unwrap_or(""),
                    session_id = %row.session_id,
                    tool_call_id = %row.tool_call_id,
                    child_request_id = row.child_request_id.as_deref().unwrap_or(""),
                    "leaving background subagent tool call running during recovery"
                );
            }
            continue;
        };

        if outcome == RecoveryOutcome::UnclaimedCrossDeploymentSpawn {
            // Lean `missing_parent_never_terminalizes`: an unresolved exact
            // parent defers settlement even when the deadline has passed.
            if parent.is_none() {
                continue;
            }
            if child_request_id(&row).is_some() {
                // The expiry fences its child through the same owner as the
                // periodic reconciler (Lean `SpawnClaimFence.expire`).
                match crate::background_completion::settle_unclaimed_spawn(node, &row.doc_id).await
                {
                    Ok(crate::background_completion::UnclaimedSpawnSettlement::Abandoned) => {
                        recovered += 1;
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(
                        doc_id = %row.doc_id,
                        tool_call_id = %row.tool_call_id,
                        error = %error,
                        "failed to settle unclaimed subagent spawn during recovery"
                    ),
                }
                continue;
            }
        }

        let updated =
            match recover_tool_call_row(node, &row, deadline_at, outcome, parent.is_some()).await {
                Ok(updated) => updated,
                Err(error) => {
                    tracing::warn!(
                        doc_id = %row.doc_id,
                        request_id = row.request_id.as_deref().unwrap_or(""),
                        session_id = %row.session_id,
                        tool_call_id = %row.tool_call_id,
                        error = %error,
                        "failed to recover running tool call"
                    );
                    continue;
                }
            };
        if !updated {
            // Lost CAS against a concurrent terminal writer — leave the durable
            // terminal untouched (first-writer-wins).
            continue;
        }

        if is_background_tool_row(&row) {
            append_recovered_background_tool_completion(node, &row, outcome).await;
        }

        recovered += 1;
        tracing::info!(
            doc_id = %row.doc_id,
            request_id = row.request_id.as_deref().unwrap_or(""),
            session_id = %row.session_id,
            tool_call_id = %row.tool_call_id,
            lifecycle_state = %outcome.lifecycle_state().as_str(),
            "recovered stuck running tool call"
        );
    }

    Ok(recovered)
}

async fn append_recovered_background_tool_completion(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &RunningToolCallRow,
    outcome: RecoveryOutcome,
) {
    let Some(parent_request_id) = row.request_id.as_deref().filter(|id| !id.is_empty()) else {
        return;
    };
    let status = match outcome {
        RecoveryOutcome::Cancelled
        | RecoveryOutcome::BackgroundInterrupted
        | RecoveryOutcome::TaskDeleted => "cancelled",
        RecoveryOutcome::TimedOut
        | RecoveryOutcome::Failed
        | RecoveryOutcome::UnclaimedCrossDeploymentSpawn
        | RecoveryOutcome::ProcessLost => "failed",
    };
    let reason = outcome.notification_reason();
    if let Err(error) = crate::background_completion::append_background_tool_completion(
        node,
        &row.session_id,
        parent_request_id,
        &row.doc_id,
        &row.tool_name,
        status,
        "",
        Some(reason),
    )
    .await
    {
        tracing::warn!(
            doc_id = %row.doc_id,
            request_id = parent_request_id,
            session_id = %row.session_id,
            tool_call_id = %row.tool_call_id,
            error = %error,
            "failed to append recovered background tool notification"
        );
    }
}

/// Running tool rows owned by `agent_did` (immutable scope key on create).
async fn load_running_tool_call_rows_for_agent(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
) -> Result<Vec<RunningToolCallRow>> {
    let escaped = escape_graphql_string(agent_did);
    load_running_tool_call_rows_with_filter(
        node,
        &format!(r#", agent_did: {{ _eq: "{escaped}" }}"#),
    )
    .await
}

/// Running bridge rows only (`child_request_id` set) — the periodic liveness
/// sweep's scope, filtered server-side so the 5s tick never pays for
/// non-subagent tool rows.
async fn load_running_subagent_bridge_rows(
    node: &std::sync::Arc<EmbeddedNode>,
) -> Result<Vec<RunningToolCallRow>> {
    load_running_tool_call_rows_with_filter(node, r#", child_request_id: { _ne: "" }"#).await
}

async fn load_running_tool_call_rows_with_filter(
    node: &std::sync::Arc<EmbeddedNode>,
    extra_filter: &str,
) -> Result<Vec<RunningToolCallRow>> {
    let query = format!(
        r#"{{
        AgentToolCall(
            filter: {{ lifecycle_state: {{ _eq: "running" }}{extra_filter} }}
        ) {{
            _docID
            request_id
            request_doc_id
            requester_did
            agent_did
            session_id
            tool_call_id
            tool_name
            delegated_input
            delegated_workspace
            started_at
            deadline_at
            await_mode
            cancel_cause
            child_request_id
            spawn_target_did
            spawn_behavior_id
            unclaimed_deadline_at
        }}
    }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("querying stuck running tool calls: {:?}", resp.errors);
    }

    let values = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let mut rows = Vec::with_capacity(values.len());
    for value in values {
        match serde_json::from_value::<RunningToolCallRow>(value.clone()) {
            Ok(row) => rows.push(row),
            Err(error) => {
                tracing::warn!(
                    doc_id = value
                        .get("_docID")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(""),
                    error = %error,
                    "skipping malformed running tool-call row during recovery"
                );
            }
        }
    }
    Ok(rows)
}

async fn load_pending_background_completion_rows(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
) -> Result<Vec<TerminalBackgroundToolRow>> {
    let agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{
                agent_did: {{ _eq: "{agent_did}" }},
                await_mode: {{ _eq: "background" }},
                lifecycle_state: {{ _in: ["completed", "failed", "timedOut", "cancelled"] }},
                status: {{ _like: "completionPending%" }}
            }}) {{
                _docID
                request_id
                request_doc_id
                agent_did
                requester_did
                session_id
                tool_call_id
                tool_name
                status
                lifecycle_state
                cancel_cause
                child_request_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "querying pending background completion side effects: {:?}",
            response.errors
        );
    }
    let values = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut rows = Vec::with_capacity(values.len());
    for value in values {
        match serde_json::from_value::<TerminalBackgroundToolRow>(value.clone()) {
            Ok(row) => rows.push(row),
            Err(error) => tracing::warn!(
                doc_id = value
                    .get("_docID")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(""),
                error = %error,
                "skipping malformed terminal background row during side-effect recovery"
            ),
        }
    }
    Ok(rows)
}

fn background_completion_projection(
    row: &TerminalBackgroundToolRow,
) -> Option<(&str, Option<&str>)> {
    let persisted_reason = row.status.strip_prefix("completionPending:");
    match row.lifecycle_state.as_deref()? {
        "completed" => Some(("completed", None)),
        "timedOut" => Some((
            "failed",
            Some(persisted_reason.unwrap_or("deadline_exceeded")),
        )),
        "cancelled" => Some((
            "cancelled",
            Some(persisted_reason.unwrap_or_else(|| {
                if row.cancel_cause.as_deref() == Some("userCancelled") {
                    "explicit_cancel"
                } else {
                    "parent_interrupted"
                }
            })),
        )),
        "failed" => Some(("failed", Some(persisted_reason.unwrap_or("tool_failed")))),
        _ => None,
    }
}

async fn lookup_parent_request(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    request_id: &str,
) -> Result<Option<AgentRequestRow>> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    request_id: {{ _eq: "{escaped_request_id}" }}
                }},
                limit: 1
            ) {{
                request_id
                agent_did
                lifecycle_state
                caused_by_trigger_id
                subagent_depth
                workspace_id
                workspace_authority
                workspace_owner_agent_did
                workspace_seal_hash
            }}
        }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "querying parent request for tool-call recovery request_id={request_id}: {:?}",
            resp.errors
        );
    }

    let value = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .context("AgentRequest field missing from parent recovery query")?;
    let rows: Vec<AgentRequestRow> = serde_json::from_value(value.clone())
        .context("decode parent AgentRequest rows for tool-call recovery")?;
    Ok(rows.into_iter().next())
}

async fn child_request_exists(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: &str,
) -> Result<bool> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "querying child request for tool-call recovery: {:?}",
            resp.errors
        );
    }
    Ok(resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|rows| !rows.is_empty()))
}

async fn recover_bridge_terminal_child(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    row: &RunningToolCallRow,
) -> Result<bool> {
    let Some(child_request_id) = child_request_id(row) else {
        return Ok(false);
    };
    let Some(child) =
        crate::background_tools::load_child_terminal_row(node, child_request_id).await?
    else {
        return Ok(false);
    };

    let parent_request_id = row
        .request_id
        .as_deref()
        .context("bridge missing parent request")?;
    anyhow::ensure!(
        row.agent_did.as_deref() == Some(agent_did),
        "bridge recovery owner mismatch"
    );
    let parent_doc_id = crate::request_binding::resolve_request_doc_id(node, parent_request_id)
        .await?
        .context("bridge parent request missing")?;
    anyhow::ensure!(
        row.request_doc_id.as_deref() == Some(parent_doc_id.as_str()),
        "bridge recovery physical parent mismatch"
    );
    let Some(edge) = crate::descendant_graph::resolve_descendant_edge(
        crate::descendant_graph::DescendantGraphAccess::Local(node),
        parent_request_id,
        child_request_id,
    )
    .await?
    else {
        return Ok(false);
    };
    anyhow::ensure!(
        edge.is_direct()
            && edge.readable()
            && edge.immediate_parent_request_id == parent_request_id
            && edge.immediate_parent_session_id == row.session_id
            && edge.immediate_parent_tool_call_id == row.tool_call_id,
        "bridge recovery requires its physically corroborated direct child"
    );
    let edge = crate::background_tools::ChildEdge::from_descendant(&edge)
        .context("bridge child identity is not materialized")?;
    anyhow::ensure!(
        row.spawn_target_did
            .as_deref()
            .is_none_or(|target| target == edge.child_agent_did),
        "bridge recovery child owner mismatch"
    );

    if child_request_completed(&child) {
        let Some(result) = crate::background_tools::load_child_final_response(node, &edge).await?
        else {
            // The completed lifecycle may replicate before its terminal
            // selection or the selected message's immutable dependencies.
            // Leave the bridge running so recovery retries the exact result.
            return Ok(false);
        };
        recover_bridge_completed_row(node, row, &result).await?;
        ensure_background_subagent_projection_side_effects(node, agent_did, row, child_request_id)
            .await?;
        return Ok(true);
    }

    let Some(terminal) = project_child_terminal(&child) else {
        return Ok(false);
    };
    recover_bridge_failed_row(node, row, &terminal).await?;
    ensure_background_subagent_projection_side_effects(node, agent_did, row, child_request_id)
        .await?;
    Ok(true)
}

async fn ensure_background_subagent_projection_side_effects(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    row: &RunningToolCallRow,
    child_request_id: &str,
) -> Result<()> {
    if !is_background_subagent_tool(row) {
        return Ok(());
    }
    let outcome =
        ensure_background_subagent_completion_side_effects(node, child_request_id, agent_did)
            .await?;
    tracing::debug!(
        doc_id = %row.doc_id,
        request_id = row.request_id.as_deref().unwrap_or(""),
        session_id = %row.session_id,
        tool_call_id = %row.tool_call_id,
        child_request_id,
        outcome = ?outcome,
        "ensured recovered background subagent projection side effects"
    );
    Ok(())
}

async fn terminalize_expired_local_child_request(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    row: &RunningToolCallRow,
) -> Result<bool> {
    let Some(child_request_id) = child_request_id(row) else {
        return Ok(false);
    };
    let Some(child) = load_request_liveness_row(node, child_request_id).await? else {
        return Ok(false);
    };
    terminalize_expired_child_with_row(node, agent_did, row, &child).await
}

/// `terminalize_expired_local_child_request` over a preloaded child liveness
/// row, so the periodic sweep can batch the reads.
async fn terminalize_expired_child_with_row(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    row: &RunningToolCallRow,
    child: &AgentRequestRow,
) -> Result<bool> {
    let Some(child_request_id) = child_request_id(row) else {
        return Ok(false);
    };
    if child.agent_did.as_deref() != Some(agent_did) {
        return Ok(false);
    }
    if child
        .lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
    {
        return Ok(false);
    }
    let Some(deadline_at) = parse_datetime(child.deadline.as_deref()) else {
        return Ok(false);
    };
    if !deadline_at_is_expired(Utc::now(), Some(deadline_at)) {
        return Ok(false);
    }

    let reason = format!(
        "child request deadline exceeded at {} before terminal response",
        deadline_at.to_rfc3339()
    );
    if !mark_child_request_dead(node, child, &reason).await? {
        return Ok(false);
    }
    tracing::info!(
        doc_id = %row.doc_id,
        request_id = row.request_id.as_deref().unwrap_or(""),
        session_id = %row.session_id,
        tool_call_id = %row.tool_call_id,
        child_request_id,
        child_deadline_at = %deadline_at,
        "terminalized expired subagent child request during tool-call recovery"
    );
    Ok(true)
}

async fn load_request_liveness_row(
    node: &std::sync::Arc<EmbeddedNode>,
    request_id: &str,
) -> Result<Option<AgentRequestRow>> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{
                _docID
                request_id
                agent_did
                lifecycle_state
                session_id
                behavior_id
                requester_did
                execution_generation
                execution_lease_expires_at
                deadline
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentRequest liveness for {request_id} failed: {:?}",
            response.errors
        );
    }
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .context("AgentRequest field missing from request liveness query")?;
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(value.clone()).context("decode AgentRequest liveness rows")?;
    Ok(rows.into_iter().next())
}

/// Batched form of `load_child_liveness_row`: one `_in` query for every
/// bridge's child on the periodic tick, keyed by `request_id`.
async fn load_child_liveness_rows(
    node: &std::sync::Arc<EmbeddedNode>,
    child_request_ids: &[&str],
) -> Result<std::collections::HashMap<String, AgentRequestRow>> {
    if child_request_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let id_list = child_request_ids
        .iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _in: [{id_list}] }} }}
            ) {{
                _docID
                request_id
                agent_did
                lifecycle_state
                session_id
                behavior_id
                requester_did
                execution_generation
                execution_lease_expires_at
                deadline
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "batched child AgentRequest liveness query failed: {:?}",
            response.errors
        );
    }
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .context("AgentRequest field missing from batched liveness query")?;
    let rows: Vec<AgentRequestRow> = serde_json::from_value(value.clone())
        .context("decode batched child AgentRequest liveness rows")?;
    Ok(rows
        .into_iter()
        .map(|row| (row.request_id.clone(), row))
        .collect())
}

async fn mark_child_request_dead(
    node: &std::sync::Arc<EmbeddedNode>,
    child: &AgentRequestRow,
    reason: &str,
) -> Result<bool> {
    match child.lifecycle_state {
        Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing) => {
            return Ok(crate::lifecycle::revoke_execution_preserving_output(
                node,
                child,
                crate::lifecycle::RequestTerminalOutcome::Dead,
                reason,
            )
            .await?
                == crate::lifecycle::TerminalizeResult::Won);
        }
        Some(RequestLifecycleState::Pending) => {}
        _ => return Ok(false),
    }
    let escaped_doc_id = escape_graphql_string(
        child
            .doc_id
            .as_deref()
            .context("child AgentRequest liveness row is missing _docID")?,
    );
    let escaped_agent_did = escape_graphql_string(
        child
            .agent_did
            .as_deref()
            .context("child AgentRequest liveness row is missing agent_did")?,
    );
    let escaped_reason = escape_graphql_string(reason);
    let terminalized_at = escape_graphql_string(&Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    _docID: {{ _eq: "{escaped_doc_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    lifecycle_state: {{ _eq: "pending" }}
                }},
                input: {{
                    lifecycle_state: "dead",
                    failure_reason: "{escaped_reason}",
                    terminalized_at: "{terminalized_at}",
                    terminal_redrive_attempts: 0
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = crate::config_client::ConfigAccess::write_local_idempotent_update_response(
        node,
        "terminalize_expired_child_request",
        &mutation,
    )
    .await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_AgentRequest"))
        .is_some_and(response_has_documents))
}

async fn recover_bridge_completed_row(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &RunningToolCallRow,
    child_result: &str,
) -> Result<()> {
    let mut lifecycle = load_recovery_lifecycle(node, row).await?;
    if lifecycle.state == ToolCallState::Completed {
        return Ok(());
    }
    anyhow::ensure!(
        lifecycle.bridge_complete(child_result.to_owned()).await?,
        "bridge completion recovery lost its running compare"
    );
    Ok(())
}

async fn recover_bridge_failed_row(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &RunningToolCallRow,
    terminal: &ChildTerminal,
) -> Result<()> {
    let mut lifecycle = load_recovery_lifecycle(node, row).await?;
    if lifecycle.state == terminal.projected_state() {
        return Ok(());
    }
    anyhow::ensure!(
        lifecycle.bridge_failure(terminal.clone()).await?,
        "bridge failure recovery lost its running compare"
    );
    Ok(())
}

/// Keep a child-linked bridge of a terminal parent running as background work
/// (Lean `Recovery.restartDisposition`'s `retainInBackground`).
async fn retain_bridge_in_background(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &RunningToolCallRow,
) -> Result<bool> {
    let mut lifecycle = load_recovery_lifecycle(node, row).await?;
    let was_foreground = lifecycle.await_mode() == AwaitMode::Foreground;
    let receipt = crate::hook::persistence::background_receipt_payload(
        child_request_id(row).unwrap_or_default(),
        None,
        lifecycle.spawn_behavior_id.as_deref().unwrap_or_default(),
    );
    Ok(lifecycle.retain_in_background(&receipt).await? && was_foreground)
}

async fn load_recovery_lifecycle(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &RunningToolCallRow,
) -> Result<ToolCallLifecycle> {
    let agent_did = row
        .agent_did
        .as_deref()
        .context("recovery row omitted agent_did")?;
    ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &row.doc_id,
        agent_did,
        &row.session_id,
        row.requester_did.as_deref(),
    )
    .await?
    .context("recovery physical tool row disappeared")
}

/// Terminalize a running tool-call row. Returns `Ok(true)` when the
/// compare-and-set updated the row, `Ok(false)` when a concurrent writer
/// already left `running` (first terminal wins — do not overwrite).
async fn recover_tool_call_row(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &RunningToolCallRow,
    deadline_at: Option<DateTime<Utc>>,
    outcome: RecoveryOutcome,
    completion_side_effects_owed: bool,
) -> Result<bool> {
    let _ = completion_side_effects_owed;
    let mut lifecycle = load_recovery_lifecycle(node, row).await?;
    if lifecycle.state != ToolCallState::Running {
        return Ok(false);
    }
    let result = outcome.result_text(deadline_at);
    match outcome {
        RecoveryOutcome::TimedOut => lifecycle.timeout().await,
        RecoveryOutcome::Cancelled
        | RecoveryOutcome::BackgroundInterrupted
        | RecoveryOutcome::TaskDeleted => {
            lifecycle
                .cancel_during_run_owned(
                    outcome
                        .cancel_cause(row.cancel_cause.as_deref())
                        .unwrap_or(CancelCause::Interrupted),
                    outcome.notification_reason(),
                )
                .await
        }
        RecoveryOutcome::Failed
        | RecoveryOutcome::UnclaimedCrossDeploymentSpawn
        | RecoveryOutcome::ProcessLost => {
            let failure_class = outcome.failure_class().unwrap_or(FailureClass::External);
            if lifecycle.is_bridge() {
                // Bridge-owned rows must use the bridge transition; `fail_owned`
                // rejects them so a native executor cannot bypass that owner.
                lifecycle
                    .bridge_failure_with_completion_reason(
                        ChildTerminal::Failed {
                            reason: result,
                            failure_class,
                        },
                        outcome.notification_reason(),
                    )
                    .await
            } else if lifecycle.is_spawned_background() {
                lifecycle
                    .fail_owned_with_completion_reason(
                        &result,
                        failure_class,
                        outcome.notification_reason(),
                    )
                    .await
            } else {
                lifecycle.fail_owned(&result, failure_class, None).await
            }
        }
    }
}

fn parse_datetime(value: Option<&str>) -> Option<DateTime<Utc>> {
    non_empty(value)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|datetime| datetime.with_timezone(&Utc))
}

/// Whether an already-parsed deadline has been reached or passed as of
/// `now`. A missing deadline never expires (#1334 — the one deadline-expiry
/// predicate; also used by `gents-cli`'s fleet-slot and liveness snapshots).
pub fn deadline_at_is_expired(now: DateTime<Utc>, deadline_at: Option<DateTime<Utc>>) -> bool {
    deadline_at.is_some_and(|deadline| now >= deadline)
}

/// String-form convenience over [`deadline_at_is_expired`]: parses an
/// RFC3339 `deadline_at` and reports whether it has expired as of `now`. A
/// missing or malformed deadline is documented as *not* expired — there is
/// no evidence to expire the row on.
pub fn deadline_is_expired(now: DateTime<Utc>, deadline_at: Option<&str>) -> bool {
    deadline_at_is_expired(now, parse_datetime(deadline_at))
}

/// Startup classifier for every row except native background rows, which
/// `classify_orphaned_background_tool` settles with the host stop verdict.
/// Branch order is Lean-fenced by `restartDisposition`.
fn classify_running_tool_recovery(
    row: &RunningToolCallRow,
    parent: Option<&AgentRequestRow>,
    now: DateTime<Utc>,
) -> Option<RecoveryOutcome> {
    if deadline_is_expired(now, row.deadline_at.as_deref()) {
        Some(RecoveryOutcome::TimedOut)
    } else if deadline_is_expired(now, row.unclaimed_deadline_at.as_deref()) {
        Some(RecoveryOutcome::UnclaimedCrossDeploymentSpawn)
    } else {
        parent.and_then(|parent| classify_terminal_parent_tool_recovery(row, parent))
    }
}

/// The orphan sweep runs every few seconds; warn about one row's unresolved
/// parent at most once per interval.
fn unresolved_parent_warning_due(doc_id: &str) -> bool {
    const INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);
    static WARNED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    > = std::sync::OnceLock::new();
    let mut warned = WARNED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = std::time::Instant::now();
    warned.retain(|_, at| now.duration_since(*at) < INTERVAL);
    if warned.contains_key(doc_id) {
        return false;
    }
    warned.insert(doc_id.to_owned(), now);
    true
}

/// Lean `orphanedBackgroundToolCause` for a row without a live worker whose
/// parent resolved: the host stop verdict precedes every other cause.
fn classify_orphaned_background_tool(
    row: &RunningToolCallRow,
    parent: &AgentRequestRow,
    process: crate::managed_exec::ProcessStopOutcome,
    task_deleted: bool,
    now: DateTime<Utc>,
) -> Option<RecoveryOutcome> {
    use crate::managed_exec::ProcessStopOutcome;
    match process {
        ProcessStopOutcome::StillRunning => None,
        ProcessStopOutcome::NotOwned | ProcessStopOutcome::AlreadyExited => {
            Some(RecoveryOutcome::ProcessLost)
        }
        ProcessStopOutcome::Stopped => {
            if deadline_is_expired(now, row.deadline_at.as_deref()) {
                Some(RecoveryOutcome::TimedOut)
            } else if deadline_is_expired(now, row.unclaimed_deadline_at.as_deref()) {
                Some(RecoveryOutcome::UnclaimedCrossDeploymentSpawn)
            } else if task_deleted {
                Some(RecoveryOutcome::TaskDeleted)
            } else {
                // A parent's interrupt or terminal state never stops
                // background work: the lost process is the restart's.
                Some(RecoveryOutcome::BackgroundInterrupted)
            }
        }
    }
}

/// Whether the trigger that started `parent`, or that trigger's task, is
/// observed deleted. A missing document is not a deletion: it may not have
/// replicated here.
async fn owner_task_deleted(
    node: &std::sync::Arc<EmbeddedNode>,
    agent_did: &str,
    parent: &AgentRequestRow,
) -> Result<bool> {
    let Some(trigger_id) = non_empty(parent.caused_by_trigger_id.as_deref()) else {
        return Ok(false);
    };
    let agent_did = escape_graphql_string(agent_did);
    let trigger_id = escape_graphql_string(trigger_id);
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ Trigger(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, trigger_id: {{ _eq: "{trigger_id}" }} }}, showDeleted: true) {{ _deleted task_id }} }}"#
        ),
        "tool_call.recovery.owner_trigger",
    )
    .await?;
    let triggers = deletion_rows(&response, "Trigger")?;
    if triggers.is_empty() {
        return Ok(false);
    }
    let Some(task_id) = triggers
        .iter()
        .find(|row| !row.deleted)
        .map(|row| row.task_id.clone())
    else {
        return Ok(true);
    };
    let Some(task_id) = non_empty(task_id.as_deref()) else {
        return Ok(false);
    };
    let task_id = escape_graphql_string(task_id);
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ Task(filter: {{ agent_did: {{ _eq: "{agent_did}" }}, task_id: {{ _eq: "{task_id}" }} }}, showDeleted: true) {{ _deleted }} }}"#
        ),
        "tool_call.recovery.owner_task",
    )
    .await?;
    let tasks = deletion_rows(&response, "Task")?;
    Ok(!tasks.is_empty() && tasks.iter().all(|row| row.deleted))
}

#[derive(Debug, Deserialize)]
struct DeletionRow {
    #[serde(rename = "_deleted", default)]
    deleted: bool,
    #[serde(default)]
    task_id: Option<String>,
}

fn deletion_rows(
    response: &defra_node::QueryResponse,
    collection: &str,
) -> Result<Vec<DeletionRow>> {
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .cloned()
        .unwrap_or(serde_json::Value::Array(Vec::new()));
    serde_json::from_value(value).with_context(|| format!("decode {collection} deletion rows"))
}

/// Parent-driven recovery cause for a running tool call whose parent has
/// already resolved (Lean `terminalParentToolRecover`: cause is
/// `.parentInterrupted` when the parent is interrupted, else `.parentTerminal`
/// — deadline expiry is not a predicate of `TerminalParentToolRow` at all).
/// Shared by [`classify_running_tool_recovery`] (after its deadline and
/// live-background-parent screens) and
/// `ToolCallLifecycle::reconcile_terminal_parent_owned_tools`, whose Lean
/// counterpart (`terminalParentOwnedToolSweep`) intentionally excludes
/// deadline expiry from this sweep — a stranded row's own deadline is not
/// evidence about its *parent's* terminal state, and deadline-expired rows
/// are covered by the orphan/background sweep and the live in-flight
/// deadline sweep instead.
fn classify_terminal_parent_tool_recovery(
    row: &RunningToolCallRow,
    parent: &AgentRequestRow,
) -> Option<RecoveryOutcome> {
    if child_request_id(row).is_some() {
        None
    } else if request_is_interrupted(parent) {
        Some(RecoveryOutcome::Cancelled)
    } else if request_is_terminal(parent) {
        Some(RecoveryOutcome::Failed)
    } else {
        None
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn effective_deadline(
    tool_deadline: Option<&str>,
    args_deadline: Option<&str>,
) -> Option<DateTime<Utc>> {
    match (parse_datetime(tool_deadline), parse_datetime(args_deadline)) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn request_is_interrupted(parent: &AgentRequestRow) -> bool {
    parent.lifecycle_state == Some(RequestLifecycleState::Interrupted)
}

fn request_is_terminal(parent: &AgentRequestRow) -> bool {
    parent
        .lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
}

fn child_request_id(row: &RunningToolCallRow) -> Option<&str> {
    row.child_request_id.as_deref().filter(|id| !id.is_empty())
}

fn await_mode(row: &RunningToolCallRow) -> AwaitMode {
    row.await_mode
        .as_deref()
        .and_then(AwaitMode::from_persisted)
        .unwrap_or(AwaitMode::Foreground)
}

fn subagent_tool_name(row: &RunningToolCallRow) -> &str {
    &row.tool_name
}

fn is_background_subagent_tool(row: &RunningToolCallRow) -> bool {
    child_request_id(row).is_some() && await_mode(row) == AwaitMode::Background
}

fn is_background_tool_row(row: &RunningToolCallRow) -> bool {
    child_request_id(row).is_none() && await_mode(row) == AwaitMode::Background
}

impl RecoveryOutcome {
    fn notification_reason(self) -> &'static str {
        match self {
            Self::TimedOut => "deadline_exceeded",
            Self::Cancelled => "parent_interrupted",
            Self::Failed => "parent_terminal",
            Self::BackgroundInterrupted => "interrupted_on_restart",
            Self::UnclaimedCrossDeploymentSpawn => "unclaimed_spawn_timeout",
            Self::ProcessLost => "process_lost",
            Self::TaskDeleted => "task_deleted",
        }
    }

    fn lifecycle_state(self) -> ToolCallState {
        match self {
            Self::TimedOut => ToolCallState::TimedOut,
            Self::Cancelled | Self::BackgroundInterrupted | Self::TaskDeleted => {
                ToolCallState::Cancelled
            }
            Self::Failed | Self::UnclaimedCrossDeploymentSpawn | Self::ProcessLost => {
                ToolCallState::Failed
            }
        }
    }

    fn failure_class(self) -> Option<FailureClass> {
        match self {
            Self::TimedOut | Self::Failed | Self::ProcessLost => Some(FailureClass::External),
            Self::UnclaimedCrossDeploymentSpawn => Some(FailureClass::SpawnUnclaimed),
            Self::Cancelled | Self::BackgroundInterrupted | Self::TaskDeleted => None,
        }
    }

    fn result_text(self, deadline_at: Option<DateTime<Utc>>) -> String {
        match self {
            Self::TimedOut => match deadline_at {
                Some(deadline_at) => {
                    format!(
                        "tool call deadline exceeded at {}",
                        deadline_at.to_rfc3339()
                    )
                }
                None => "tool call deadline exceeded".to_string(),
            },
            Self::Cancelled => {
                "tool call cancelled because parent request was interrupted".to_string()
            }
            Self::BackgroundInterrupted => {
                "backgrounded tool call interrupted on restart".to_string()
            }
            Self::Failed => {
                "tool call failed because parent request was already terminal".to_string()
            }
            Self::UnclaimedCrossDeploymentSpawn => {
                "no peer claimed subagent spawn before the unclaimed spawn deadline".to_string()
            }
            Self::ProcessLost => "background process lost: its runtime restarted and could \
                 not prove it owned the process, or the process ended while no runtime \
                 observed its result"
                .to_string(),
            Self::TaskDeleted => {
                "background process stopped because its task was deleted".to_string()
            }
        }
    }

    fn cancel_cause(self, persisted: Option<&str>) -> Option<CancelCause> {
        persisted
            .and_then(CancelCause::from_persisted)
            .or(match self {
                Self::TimedOut => Some(CancelCause::Deadline),
                Self::Cancelled | Self::BackgroundInterrupted | Self::TaskDeleted => {
                    Some(CancelCause::Interrupted)
                }
                Self::Failed | Self::UnclaimedCrossDeploymentSpawn | Self::ProcessLost => None,
            })
    }
}
