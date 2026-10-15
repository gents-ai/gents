//! Recovery for persisted running tool calls: the startup sweep over rows
//! orphaned by a daemon restart, the session-message sweep that settles a
//! `agent_new`/`agent_message` row from its caused request's terminal (or
//! fails it closed when it cannot name that request), and the live
//! terminal-parent owned-tool cleanup that
//! cancels running foreground tools whose parent is already terminal without
//! waiting for deadline or daemon restart.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::graphql::escape_graphql_string;

use super::{AwaitMode, CancelCause, FailureClass, ToolCallLifecycle, ToolCallState};

#[derive(Debug, Default)]
pub struct ToolCallRecoveryReport {
    pub tool_calls_recovered: usize,
    pub notifications_repaired: usize,
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
    requester_did: Option<String>,
    /// Immutable owner principal stamped at create. Recovery scopes by this
    /// field — `request_id` alone is not unique across agents.
    #[serde(default)]
    node_did: Option<String>,
    session_id: String,
    tool_call_id: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    deadline_at: Option<String>,
    #[serde(default)]
    await_mode: Option<String>,
    #[serde(default)]
    cancel_cause: Option<String>,
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
    node_did: Option<String>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryOutcome {
    TimedOut,
    Cancelled,
    Failed,
    BackgroundInterrupted,
    ProcessLost,
    TaskDeleted,
}

impl super::ToolCallLifecycle {
    /// Startup recovery without host process records: a surviving native
    /// background process cannot be proven owned, so its row settles as lost.
    pub async fn recover_all(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
    ) -> Result<ToolCallRecoveryReport> {
        Self::recover_all_with_executions(
            node,
            node_did,
            &crate::hook::BackgroundExecutionRegistry::default(),
        )
        .await
    }

    /// Startup recovery. Native background rows go through the host process
    /// owner in `executions`, which stops a proven-owned surviving process
    /// before its row is settled.
    pub async fn recover_all_with_executions(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
        executions: &crate::hook::BackgroundExecutionRegistry,
    ) -> Result<ToolCallRecoveryReport> {
        let tool_calls_recovered = recover_stuck_running_tool_calls(node, node_did).await?
            + Self::reconcile_orphaned_background_tools(node, node_did, executions)
                .await?
                .tool_calls_terminalized;
        let notifications_repaired =
            Self::reconcile_background_completion_side_effects(node, node_did)
                .await?
                .side_effects_converged;

        Ok(ToolCallRecoveryReport {
            tool_calls_recovered,
            notifications_repaired,
        })
    }

    /// Live reconcile: terminalize running tool calls whose parent request is
    /// already terminal (#837). Unlike full startup `recover_all`, this does
    /// **not** interrupt live-parent background tools (restart-only path).
    ///
    /// Scope and ordering:
    /// 1. Load only tool rows stamped with this agent's immutable `node_did`
    ///    (not global `request_id` matches — that field is not unique).
    /// 2. Resolve the parent under the same DID; skip missing/foreign parents.
    /// 3. Require a terminal parent before any write.
    /// 4. Leave every background row to its own sweep regardless of parent
    ///    state: native processes to the registry-aware orphan sweep and
    ///    `agent_new`/`agent_message` rows to the session-message sweep.
    ///    No parent terminal is a cancel signal for another session.
    ///
    /// Covers running native tool calls stranded under a terminal parent with
    /// no executor active.
    pub async fn reconcile_terminal_parent_owned_tools(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
    ) -> Result<TerminalParentToolReport> {
        let rows = load_running_tool_call_rows_for_agent(node, node_did).await?;
        let mut report = TerminalParentToolReport::default();
        let mut parent_cache: std::collections::HashMap<String, Option<AgentRequestRow>> =
            std::collections::HashMap::new();

        for row in rows {
            // Defense in depth: never mutate a row whose stamped owner differs.
            if row.node_did.as_deref() != Some(node_did) {
                continue;
            }
            // Background rows belong exclusively to their own sweeps, which
            // apply deadline and ownership precedence before parent state.
            if await_mode(&row) == AwaitMode::Background {
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
                        let loaded = lookup_parent_request(node, node_did, request_id).await?;
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
        node_did: &str,
        executions: &crate::hook::BackgroundExecutionRegistry,
    ) -> Result<OrphanedBackgroundToolReport> {
        let rows = load_running_tool_call_rows_for_agent(node, node_did).await?;
        let mut report = OrphanedBackgroundToolReport::default();
        let mut running_background = std::collections::HashSet::new();

        for row in rows {
            if row.node_did.as_deref() != Some(node_did) || !is_background_tool_row(&row) {
                continue;
            }
            running_background.insert(row.tool_call_id.clone());
            let registered = executions.contains(&row.tool_call_id).await;
            let parent = match row.request_id.as_deref().filter(|id| !id.is_empty()) {
                Some(request_id) => lookup_parent_request(node, node_did, request_id).await?,
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
            let task_deleted = owner_task_deleted(node, node_did, &parent).await?;
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
                classify_orphaned_background_tool(&row, process, task_deleted, Utc::now())
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
                let won = lifecycle
                    .fail_owned_with_completion_reason(
                        &result,
                        class,
                        outcome.notification_reason(),
                    )
                    .await?;
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
                crate::lifecycle::RequestHopCause::Continuation,
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
        node_did: &str,
    ) -> Result<BackgroundCompletionSideEffectReport> {
        let rows = load_pending_background_completion_rows(node, node_did).await?;
        let mut report = BackgroundCompletionSideEffectReport::default();

        for row in rows {
            if row.node_did.as_deref() != Some(node_did) {
                continue;
            }
            let Some(request_id) = non_empty(row.request_id.as_deref()) else {
                continue;
            };
            if lookup_parent_request(node, node_did, request_id)
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
                node_did,
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
            // A session-message completion returns its result to this session
            // (Lean `CausalHop.return_keeps_caller_hop`).
            let wake = if crate::toolset::is_session_message_tool(&row.tool_name) {
                crate::lifecycle::RequestHopCause::Return
            } else {
                crate::lifecycle::RequestHopCause::Continuation
            };
            let appended = crate::background_completion::append_background_tool_completion(
                node,
                session_id,
                request_id,
                &row.doc_id,
                &row.tool_name,
                status,
                &output,
                reason,
                wake,
            )
            .await;
            match appended {
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
    use super::*;

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
            node_did: Some("did:test:agent".to_string()),
            requester_did: None,
            session_id: Some("session-1".to_string()),
            tool_call_id: Some("tool-1".to_string()),
            tool_name: "test_tool".to_string(),
            status: "completionPending:tool_failed".to_string(),
            lifecycle_state: Some("failed".to_string()),
            cancel_cause: None,
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
            node_did: Some("did:test:agent".to_string()),
            requester_did: None,
            session_id: Some("session-custom".to_string()),
            tool_call_id: Some("tool-custom".to_string()),
            tool_name: "test_tool".to_string(),
            status: "completionPending:operator requested drain".to_string(),
            lifecycle_state: Some("cancelled".to_string()),
            cancel_cause: Some("userCancelled".to_string()),
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
        let row = |deadline: bool| -> RunningToolCallRow {
            serde_json::from_value(serde_json::json!({
                "_docID": "tool-doc",
                "session_id": "session",
                "tool_call_id": "spawned:parent",
                "await_mode": "background",
                "deadline_at": if deadline { "2020-01-01T00:00:00Z" } else { "2999-01-01T00:00:00Z" },
            }))
            .unwrap()
        };
        let lean_cause = |outcome: RecoveryOutcome| match outcome {
            RecoveryOutcome::TimedOut => "deadlineExceeded",
            RecoveryOutcome::Cancelled => "parentInterrupted",
            RecoveryOutcome::Failed => "parentTerminal",
            RecoveryOutcome::BackgroundInterrupted => "TerminalizeBackgroundedAsInterrupted",
            RecoveryOutcome::ProcessLost => "processLost",
            RecoveryOutcome::TaskDeleted => "taskDeleted",
        };
        let mut checked = 0;
        for case in crate::lean_vocab_test::lean_restart_disposition_cases() {
            // A missing parent defers classification; every resolvable
            // parent observation reaches the orphan classifier alike.
            if !matches!(
                case.parent_observation.as_str(),
                "live" | "interrupted" | "cleanlyCompleted" | "otherTerminal"
            ) {
                continue;
            }
            if case.await_mode != "background" || case.session_message {
                continue;
            }
            let outcome = classify_orphaned_background_tool(
                &row(case.deadline_expired),
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
            if case.parent_live != Some(true)
                && case.parent_interrupted != Some(true)
                && case.parent_terminal != Some(true)
            {
                continue;
            }
            let outcome = classify_orphaned_background_tool(
                &row(case.deadline_expired == Some(true)),
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
    fn no_terminal_parent_terminalizes_background_rows() {
        let row = |await_mode: &str, tool_name: &str| -> RunningToolCallRow {
            serde_json::from_value(serde_json::json!({
                "_docID": "tool-doc",
                "session_id": "session",
                "tool_call_id": "tool",
                "tool_name": tool_name,
                "await_mode": await_mode,
            }))
            .unwrap()
        };
        let parent = |state| AgentRequestRow {
            request_id: "parent".to_string(),
            lifecycle_state: Some(state),
            ..Default::default()
        };
        for tool_name in [crate::toolset::AGENT_NEW_TOOL_NAME, "bash"] {
            let background = row("background", tool_name);
            for state in [
                RequestLifecycleState::Interrupted,
                RequestLifecycleState::Completed,
                RequestLifecycleState::Failed,
                RequestLifecycleState::Dead,
                RequestLifecycleState::Superseded,
            ] {
                assert_eq!(
                    classify_terminal_parent_tool_recovery(&background, &parent(state)),
                    None
                );
            }
        }
        assert_eq!(
            classify_terminal_parent_tool_recovery(
                &row("foreground", "bash"),
                &parent(RequestLifecycleState::Interrupted)
            ),
            Some(RecoveryOutcome::Cancelled)
        );
    }
}

async fn recover_stuck_running_tool_calls(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
) -> Result<usize> {
    let rows = load_running_tool_call_rows_for_agent(node, node_did).await?;

    let mut recovered = 0;
    for row in rows {
        // Background rows have their own owners: native processes the
        // orphan sweep, session-message rows the session-message sweep.
        if row.node_did.as_deref() != Some(node_did) || await_mode(&row) == AwaitMode::Background {
            continue;
        }

        let deadline_at = parse_datetime(row.deadline_at.as_deref());
        let parent = match row
            .request_id
            .as_deref()
            .filter(|request_id| !request_id.is_empty())
        {
            Some(request_id) => lookup_parent_request(node, node_did, request_id).await?,
            None => None,
        };

        let outcome = classify_running_tool_recovery(&row, parent.as_ref(), Utc::now());

        let Some(outcome) = outcome else {
            continue;
        };

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
        RecoveryOutcome::TimedOut | RecoveryOutcome::Failed | RecoveryOutcome::ProcessLost => {
            "failed"
        }
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
        crate::lifecycle::RequestHopCause::Continuation,
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

/// Running tool rows owned by `node_did` (immutable scope key on create).
async fn load_running_tool_call_rows_for_agent(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
) -> Result<Vec<RunningToolCallRow>> {
    let escaped = escape_graphql_string(node_did);
    load_running_tool_call_rows_with_filter(node, &format!(r#", node_did: {{ _eq: "{escaped}" }}"#))
        .await
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
            node_did
            session_id
            tool_call_id
            tool_name
            started_at
            deadline_at
            await_mode
            cancel_cause
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
    node_did: &str,
) -> Result<Vec<TerminalBackgroundToolRow>> {
    let node_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            AgentToolCall(filter: {{
                node_did: {{ _eq: "{node_did}" }},
                await_mode: {{ _eq: "background" }},
                lifecycle_state: {{ _in: ["completed", "failed", "timedOut", "cancelled"] }},
                status: {{ _like: "completionPending%" }}
            }}) {{
                _docID
                request_id
                request_doc_id
                node_did
                requester_did
                session_id
                tool_call_id
                tool_name
                status
                lifecycle_state
                cancel_cause
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
    node_did: &str,
    request_id: &str,
) -> Result<Option<AgentRequestRow>> {
    let escaped_node_did = escape_graphql_string(node_did);
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    node_did: {{ _eq: "{escaped_node_did}" }},
                    request_id: {{ _eq: "{escaped_request_id}" }}
                }},
                limit: 1
            ) {{
                request_id
                node_did
                lifecycle_state
                caused_by_trigger_id
                request_hop
                workspace_id
                workspace_authority
                workspace_owner_node_did
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

async fn load_recovery_lifecycle(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &RunningToolCallRow,
) -> Result<ToolCallLifecycle> {
    let node_did = row
        .node_did
        .as_deref()
        .context("recovery row omitted node_did")?;
    ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &row.doc_id,
        node_did,
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
        RecoveryOutcome::Failed | RecoveryOutcome::ProcessLost => {
            let failure_class = outcome.failure_class().unwrap_or(FailureClass::External);
            if lifecycle.is_spawned_background() {
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
    node_did: &str,
    parent: &AgentRequestRow,
) -> Result<bool> {
    let Some(trigger_id) = non_empty(parent.caused_by_trigger_id.as_deref()) else {
        return Ok(false);
    };
    let node_did = escape_graphql_string(node_did);
    let trigger_id = escape_graphql_string(trigger_id);
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ Trigger(filter: {{ node_did: {{ _eq: "{node_did}" }}, trigger_id: {{ _eq: "{trigger_id}" }} }}, showDeleted: true) {{ _deleted task_id }} }}"#
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
            r#"{{ Task(filter: {{ node_did: {{ _eq: "{node_did}" }}, task_id: {{ _eq: "{task_id}" }} }}, showDeleted: true) {{ _deleted }} }}"#
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
    if await_mode(row) == AwaitMode::Background {
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

fn request_is_interrupted(parent: &AgentRequestRow) -> bool {
    parent.lifecycle_state == Some(RequestLifecycleState::Interrupted)
}

fn request_is_terminal(parent: &AgentRequestRow) -> bool {
    parent
        .lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
}

fn await_mode(row: &RunningToolCallRow) -> AwaitMode {
    row.await_mode
        .as_deref()
        .and_then(AwaitMode::from_persisted)
        .unwrap_or(AwaitMode::Foreground)
}

/// A native background row: its process is the host's, not this request's.
fn is_background_tool_row(row: &RunningToolCallRow) -> bool {
    await_mode(row) == AwaitMode::Background
        && !crate::toolset::is_session_message_tool(&row.tool_name)
}

impl RecoveryOutcome {
    fn notification_reason(self) -> &'static str {
        match self {
            Self::TimedOut => "deadline_exceeded",
            Self::Cancelled => "parent_interrupted",
            Self::Failed => "parent_terminal",
            Self::BackgroundInterrupted => "interrupted_on_restart",
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
            Self::Failed | Self::ProcessLost => ToolCallState::Failed,
        }
    }

    fn failure_class(self) -> Option<FailureClass> {
        match self {
            Self::TimedOut | Self::Failed | Self::ProcessLost => Some(FailureClass::External),
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
                Self::Failed | Self::ProcessLost => None,
            })
    }
}
