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
    /// Immutable owner scope; request IDs can overlap across agents.
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
    #[serde(default)]
    job_pin: Option<gents_protocol::job::JobPin>,
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
    SessionStopped,
}

impl super::ToolCallLifecycle {
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

    pub async fn reconcile_terminal_parent_owned_tools(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
    ) -> Result<TerminalParentToolReport> {
        let rows = load_running_tool_call_rows_for_agent(node, node_did).await?;
        let mut report = TerminalParentToolReport::default();
        let mut parent_cache: std::collections::HashMap<String, Option<AgentRequestRow>> =
            std::collections::HashMap::new();

        for row in rows {
            if row.node_did.as_deref() != Some(node_did) {
                continue;
            }
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
            let Some(parent) = parent else {
                continue;
            };
            if !request_is_terminal(&parent) {
                continue;
            }

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
            let parent = lookup_background_parent(node, &row).await?;
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
            let session_stopped = crate::ConfigAccess::transact_local_readonly(
                node,
                None,
                "background_recovery.session_stop",
                |txn| {
                    let row = &row;
                    Box::pin(async move {
                        crate::session::session_stopped_in_txn(
                            txn,
                            node_did,
                            &row.session_id,
                            row.requester_did.as_deref(),
                        )
                        .await
                    })
                },
            )
            .await?;
            if registered && !task_deleted && !session_stopped {
                continue;
            }
            let deadline_at = parse_datetime(row.deadline_at.as_deref());

            if registered && session_stopped {
                let process = executions
                    .stop_execution(&row.tool_call_id, &row.doc_id)
                    .await;
                let Some(cause) = classify_registered_session_stop(process) else {
                    tracing::info!(
                        doc_id = %row.doc_id,
                        tool_call_id = %row.tool_call_id,
                        "background process is still running after session stop; leaving tool call running"
                    );
                    continue;
                };
                let updated =
                    match recover_tool_call_row(node, &row, deadline_at, cause, true).await {
                        Ok(updated) => updated,
                        Err(error) => {
                            tracing::warn!(
                                doc_id = %row.doc_id,
                                tool_call_id = %row.tool_call_id,
                                error = %error,
                                "failed to recover stopped-session background tool"
                            );
                            continue;
                        }
                    };
                // The worker may have won the terminal CAS while stop was in
                // flight. Its own completion owns the durable outcome.
                if updated {
                    append_recovered_background_tool_completion(node, &row, cause).await;
                    report.tool_calls_terminalized += 1;
                }
                continue;
            }

            if registered {
                let cause = RecoveryOutcome::TaskDeleted;
                let updated =
                    match recover_tool_call_row(node, &row, deadline_at, cause, true).await {
                        Ok(updated) => updated,
                        Err(error) => {
                            tracing::warn!(
                                doc_id = %row.doc_id,
                                tool_call_id = %row.tool_call_id,
                                error = %error,
                                "failed to cancel background tool after owner stop"
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
                        "background process was not observed to stop"
                    );
                }
                if updated {
                    append_recovered_background_tool_completion(node, &row, cause).await;
                    report.tool_calls_terminalized += 1;
                }
                continue;
            }

            let process = executions
                .stop_execution(&row.tool_call_id, &row.doc_id)
                .await;
            let Some(outcome) = classify_orphaned_background_tool(
                &row,
                process,
                task_deleted,
                session_stopped,
                Utc::now(),
            ) else {
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

    /// The host process owner stops the process only if a durable record
    /// proves ownership.
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

    /// Persisted `status=completionPending:<reason>` advances to
    /// `completed` only after both side effects converge, so transient
    /// failures remain discoverable.
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
            let wake =
                if super::ToolRowKind::of(&row.tool_name) == super::ToolRowKind::SessionMessage {
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

    /// The one deadline-expiry predicate (#1334), shared by tool-call
    /// recovery here and by `gents-cli`'s fleet-slot and liveness snapshots.
    /// Past deadlines (including exactly `now`) are expired; future, missing,
    /// and malformed deadlines are documented as not expired.
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
            RecoveryOutcome::SessionStopped => "sessionStopped",
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
            if case.await_mode != "background"
                || case.kind != crate::lean_vocab_test::LeanRestartRowKind::Native
            {
                continue;
            }
            let outcome = classify_orphaned_background_tool(
                &row(case.deadline_expired),
                verdict(&case.process_outcome),
                false,
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
                case.owner_session_stopped == Some(true),
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
        let mut registered_session_stop_cases = 0;
        for case in crate::lean_vocab_test::lean_recovery_sweep_cases() {
            if case.execution_registered != Some(true) || case.owner_session_stopped != Some(true) {
                continue;
            }
            let Some(process) = case.process_outcome.as_deref() else {
                continue;
            };
            let outcome = classify_registered_session_stop(verdict(process));
            assert_eq!(
                outcome.map(lean_cause),
                case.recovery_cause.as_deref(),
                "{}",
                case.name
            );
            registered_session_stop_cases += 1;
        }
        assert_eq!(registered_session_stop_cases, 2);
        assert!(checked >= 12, "only {checked} generated native witnesses");
    }

    /// The persisted `completionPending:<reason>` suffix is the replay
    /// authority for the completed arm: a session-message settlement's
    /// assignment handoff survives a crash-before-notice redrive
    /// byte-identically, while a bare native `completionPending` stays
    /// reasonless. The source snapshot cannot change on replay because the
    /// projection reads only the persisted suffix, never live state.
    #[test]
    fn projection_completed_arm_replays_the_persisted_reason() {
        // The real private row shape; no test-only re-encoding is exposed.
        let row = |status: &str| TerminalBackgroundToolRow {
            doc_id: "tool-doc".into(),
            request_id: None,
            request_doc_id: None,
            node_did: None,
            requester_did: None,
            session_id: None,
            tool_call_id: None,
            tool_name: crate::toolset::AGENT_NEW_TOOL_NAME.into(),
            status: status.into(),
            lifecycle_state: Some("completed".into()),
            cancel_cause: None,
        };
        assert_eq!(
            background_completion_projection(&row("completionPending")),
            Some(("completed", None)),
            "a native completion keeps the reasonless success form"
        );
        assert_eq!(
            background_completion_projection(&row("completionPending:assignment_active")),
            Some(("completed", Some("assignment_active"))),
            "a session-message handoff re-renders its persisted reason"
        );
        assert_eq!(
            background_completion_projection(&row("completionPending:assignment_absent")),
            Some(("completed", Some("assignment_absent"))),
            "a no-goal handoff is distinguished from assignment success on replay"
        );
        assert_eq!(
            background_completion_projection(&row("completionPending:assignment_unknown")),
            Some(("completed", Some("assignment_unknown"))),
            "an unreadable assignment never reads as success on replay"
        );
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
        | RecoveryOutcome::TaskDeleted
        | RecoveryOutcome::SessionStopped => "cancelled",
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
            job_pin
        }}
    }}"#
    );

    let resp = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "load_running_tool_call_rows_with_filter",
    )
    .await?;

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
                status: {{
                    _like: "completionPending%",
                    _ge: "completionPending",
                    _lt: "completionPendinh"
                }}
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
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "load_pending_background_completion_rows",
    )
    .await?;
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
    // The persisted `completionPending:<reason>` suffix is the authoritative
    // snapshot of the notification reason: a redrive re-renders it instead of
    // re-observing live state, so the first notice and a crash-before-notice
    // replay render byte-identically. Session-message settlement persists the
    // assignment handoff there (Lean
    // `Recovery.sessionMessageHandoffReason`); native completions persist the
    // bare status and keep the reasonless success form.
    let persisted_reason = row.status.strip_prefix("completionPending:");
    match row.lifecycle_state.as_deref()? {
        "completed" => Some(("completed", persisted_reason)),
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

async fn lookup_background_parent(
    node: &std::sync::Arc<EmbeddedNode>,
    row: &RunningToolCallRow,
) -> Result<Option<AgentRequestRow>> {
    let (Some(doc_id), Some(node_did), Some(request_id)) = (
        non_empty(row.request_doc_id.as_deref()),
        non_empty(row.node_did.as_deref()),
        non_empty(row.request_id.as_deref()),
    ) else {
        return Ok(None);
    };
    let scope = crate::session::session_scope_filter(
        node_did,
        &row.session_id,
        row.requester_did.as_deref(),
    );
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }}, request_id: {{ _eq: "{}" }}, {scope} }}, limit: 2) {{
            _docID request_id node_did requester_did session_id lifecycle_state
            caused_by_trigger_id request_hop workspace_id workspace_authority
            workspace_owner_node_did workspace_seal_hash
        }} }}"#,
        escape_graphql_string(doc_id),
        escape_graphql_string(request_id),
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "background_recovery.physical_parent",
    )
    .await?;
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .context("background parent query omitted rows")?;
    let mut parents: Vec<AgentRequestRow> = serde_json::from_value(value.clone())?;
    anyhow::ensure!(parents.len() <= 1, "background parent scope is ambiguous");
    Ok(parents.pop())
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

    let resp =
        crate::graphql::graphql_with_transaction_retry(node, &query, "lookup_parent_request")
            .await?;

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
        | RecoveryOutcome::TaskDeleted
        | RecoveryOutcome::SessionStopped => {
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

pub fn deadline_at_is_expired(now: DateTime<Utc>, deadline_at: Option<DateTime<Utc>>) -> bool {
    deadline_at.is_some_and(|deadline| now >= deadline)
}

pub fn deadline_is_expired(now: DateTime<Utc>, deadline_at: Option<&str>) -> bool {
    deadline_at_is_expired(now, parse_datetime(deadline_at))
}

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

fn classify_orphaned_background_tool(
    row: &RunningToolCallRow,
    process: crate::managed_exec::ProcessStopOutcome,
    task_deleted: bool,
    session_stopped: bool,
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
            } else if session_stopped {
                Some(RecoveryOutcome::SessionStopped)
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

fn classify_registered_session_stop(
    process: crate::managed_exec::ProcessStopOutcome,
) -> Option<RecoveryOutcome> {
    use crate::managed_exec::ProcessStopOutcome;
    match process {
        ProcessStopOutcome::StillRunning => None,
        ProcessStopOutcome::NotOwned | ProcessStopOutcome::AlreadyExited => {
            Some(RecoveryOutcome::ProcessLost)
        }
        ProcessStopOutcome::Stopped => Some(RecoveryOutcome::SessionStopped),
    }
}

/// A missing document may not have replicated here, so it does not imply deletion.
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

fn is_background_tool_row(row: &RunningToolCallRow) -> bool {
    super::ToolRowKind::of_row(&row.tool_name, row.job_pin.is_some())
        .in_orphan_sweep_scope(await_mode(row))
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
            Self::SessionStopped => "session_stopped",
        }
    }

    fn lifecycle_state(self) -> ToolCallState {
        match self {
            Self::TimedOut => ToolCallState::TimedOut,
            Self::Cancelled
            | Self::BackgroundInterrupted
            | Self::TaskDeleted
            | Self::SessionStopped => ToolCallState::Cancelled,
            Self::Failed | Self::ProcessLost => ToolCallState::Failed,
        }
    }

    fn failure_class(self) -> Option<FailureClass> {
        match self {
            Self::TimedOut | Self::Failed | Self::ProcessLost => Some(FailureClass::External),
            Self::Cancelled
            | Self::BackgroundInterrupted
            | Self::TaskDeleted
            | Self::SessionStopped => None,
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
            Self::SessionStopped => {
                "background process stopped because its session was stopped".to_string()
            }
        }
    }

    fn cancel_cause(self, persisted: Option<&str>) -> Option<CancelCause> {
        persisted
            .and_then(CancelCause::from_persisted)
            .or(match self {
                Self::TimedOut => Some(CancelCause::Deadline),
                Self::SessionStopped => Some(CancelCause::UserCancelled),
                Self::Cancelled | Self::BackgroundInterrupted | Self::TaskDeleted => {
                    Some(CancelCause::Interrupted)
                }
                Self::Failed | Self::ProcessLost => None,
            })
    }
}
