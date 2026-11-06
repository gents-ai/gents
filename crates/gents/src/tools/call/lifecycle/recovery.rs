use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use serde::Deserialize;

use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::store::graphql::escape_graphql_string;

use super::{AwaitMode, CancelCause, FailureClass, ToolCallLifecycle, ToolCallState};

pub type RecoveryIdentities = [std::sync::Arc<dyn crate::store::identity::NodeIdentity>];
type RecoveryAccesses = std::collections::BTreeMap<String, crate::ConfigAccess>;

fn recovery_accesses(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    identities: &RecoveryIdentities,
) -> Result<RecoveryAccesses> {
    anyhow::ensure!(
        node.node_identity_did() == Some(node_did),
        "tool recovery node identity mismatch"
    );
    let mut accesses = RecoveryAccesses::new();
    for identity in identities {
        let did = identity.did();
        anyhow::ensure!(
            did != node_did,
            "runtime identity cannot recover agent tools"
        );
        let actor = identity::Did::new(did)?;
        let access = match crate::ConfigAccess::local_managed_agent(
            node.clone(),
            actor,
            identity.as_ref(),
        ) {
            Ok(access) => access,
            Err(error) => {
                tracing::warn!(agent_did = did, %error, "tool recovery signer unavailable");
                continue;
            }
        };
        anyhow::ensure!(
            accesses.insert(did.to_owned(), access).is_none(),
            "duplicate tool recovery agent identity"
        );
    }
    Ok(accesses)
}

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
        identities: &RecoveryIdentities,
    ) -> Result<ToolCallRecoveryReport> {
        Self::recover_all_with_executions(
            node,
            node_did,
            identities,
            &crate::tools::call::hook::BackgroundExecutionRegistry::default(),
        )
        .await
    }

    pub async fn recover_all_with_executions(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
        identities: &RecoveryIdentities,
        executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
    ) -> Result<ToolCallRecoveryReport> {
        let accesses = recovery_accesses(node, node_did, identities)?;
        let tool_calls_recovered =
            recover_stuck_running_tool_calls(node, node_did, &accesses, executions).await?
                + Self::reconcile_orphaned_background_tools(node, node_did, identities, executions)
                    .await?
                    .tool_calls_terminalized;
        let notifications_repaired =
            Self::reconcile_background_completion_side_effects(node, node_did, identities)
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
        identities: &RecoveryIdentities,
    ) -> Result<TerminalParentToolReport> {
        Self::reconcile_terminal_parent_owned_tools_with_executions(
            node,
            node_did,
            identities,
            &crate::tools::call::hook::BackgroundExecutionRegistry::default(),
        )
        .await
    }

    pub(crate) async fn reconcile_terminal_parent_owned_tools_with_executions(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
        identities: &RecoveryIdentities,
        executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
    ) -> Result<TerminalParentToolReport> {
        let accesses = recovery_accesses(node, node_did, identities)?;
        let rows = load_running_tool_call_rows_for_agent(node_did, &accesses).await?;
        let mut report = TerminalParentToolReport::default();

        for row in rows {
            if row.node_did.as_deref() != Some(node_did) {
                continue;
            }
            if await_mode(&row) == AwaitMode::Background
                || executions.contains(&row.tool_call_id, &row.doc_id).await
            {
                continue;
            }

            let Some((access, parent)) = selected_running_parent(node_did, &row, &accesses).await?
            else {
                continue;
            };
            if !request_is_terminal(&parent) {
                continue;
            }

            let Some(outcome) = classify_terminal_parent_tool_recovery(&row, &parent) else {
                continue;
            };

            let deadline_at = parse_datetime(row.deadline_at.as_deref());
            let updated =
                match recover_tool_call_row(node, access, &row, deadline_at, outcome, true).await {
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
        identities: &RecoveryIdentities,
        executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
    ) -> Result<OrphanedBackgroundToolReport> {
        let accesses = recovery_accesses(node, node_did, identities)?;
        executions.recover_retained_foreground().await;
        let rows = load_running_tool_call_rows_for_agent(node_did, &accesses).await?;
        let mut report = OrphanedBackgroundToolReport::default();

        for row in rows {
            if row.node_did.as_deref() != Some(node_did) || !is_background_tool_row(&row) {
                continue;
            }
            let registered = executions.contains(&row.tool_call_id, &row.doc_id).await;
            let Some((access, parent)) = selected_running_parent(node_did, &row, &accesses).await?
            else {
                if unresolved_parent_warning_due(&row.doc_id) {
                    tracing::warn!(doc_id = %row.doc_id, tool_call_id = %row.tool_call_id, "background tool parent or validated agent is unavailable");
                }
                continue;
            };
            let task_deleted = owner_task_deleted(node, node_did, &parent).await?;
            let session_stopped = crate::session::session_is_stopped(
                node,
                &identity::Did::new(
                    parent
                        .agent_did
                        .as_deref()
                        .context("parent request has no agent DID")?,
                )?,
                node_did,
                &row.session_id,
                row.requester_did.as_deref(),
            )
            .await?;
            if registered && !task_deleted && !session_stopped {
                continue;
            }
            let deadline_at = parse_datetime(row.deadline_at.as_deref());

            if registered && session_stopped {
                let process = if crate::tools::native::is_bash_tool_name(&row.tool_name) {
                    executions
                        .stop_native_process(&row.tool_call_id, &row.doc_id)
                        .await
                } else {
                    executions
                        .stop_execution(&row.tool_call_id, &row.doc_id)
                        .await
                };
                let Some(cause) = classify_registered_session_stop(process) else {
                    tracing::info!(
                        doc_id = %row.doc_id,
                        tool_call_id = %row.tool_call_id,
                        "background process is still running after session stop; leaving tool call running"
                    );
                    continue;
                };
                if crate::tools::native::is_bash_tool_name(&row.tool_name) {
                    let mut retained = load_recovery_lifecycle(node, access, &row).await?;
                    if executions.settle_retained_stop(&mut retained).await? {
                        report.tool_calls_terminalized += usize::from(retained.is_terminal());
                        continue;
                    }
                }
                let updated =
                    match recover_tool_call_row(node, access, &row, deadline_at, cause, true).await
                    {
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
                    append_recovered_background_tool_completion(node, access, &row, cause).await;
                    report.tool_calls_terminalized += 1;
                }
                continue;
            }

            if registered {
                let cause = RecoveryOutcome::TaskDeleted;
                let process = if crate::tools::native::is_bash_tool_name(&row.tool_name) {
                    executions
                        .stop_native_process(&row.tool_call_id, &row.doc_id)
                        .await
                } else {
                    executions
                        .stop_execution(&row.tool_call_id, &row.doc_id)
                        .await
                };
                if !matches!(
                    process,
                    crate::tools::exec::ProcessStopOutcome::Stopped
                        | crate::tools::exec::ProcessStopOutcome::AlreadyExited
                ) {
                    tracing::warn!(doc_id = %row.doc_id, tool_call_id = %row.tool_call_id, process = process.as_str(), "background process was not observed to stop");
                    continue;
                }
                if crate::tools::native::is_bash_tool_name(&row.tool_name) {
                    let mut retained = load_recovery_lifecycle(node, access, &row).await?;
                    if executions.settle_retained_stop(&mut retained).await? {
                        report.tool_calls_terminalized += usize::from(retained.is_terminal());
                        continue;
                    }
                }
                let updated = match recover_tool_call_row(
                    node,
                    access,
                    &row,
                    deadline_at,
                    cause,
                    true,
                )
                .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        tracing::warn!(doc_id = %row.doc_id, tool_call_id = %row.tool_call_id, %error, "failed to cancel background tool after owner stop");
                        continue;
                    }
                };
                if updated {
                    append_recovered_background_tool_completion(node, access, &row, cause).await;
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

            let updated =
                match recover_tool_call_row(node, access, &row, deadline_at, outcome, true).await {
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
            executions.forget_process_record(&row.tool_call_id, &row.doc_id);
            if !updated {
                continue;
            }

            append_recovered_background_tool_completion(node, access, &row, outcome).await;
            report.tool_calls_terminalized += 1;
            tracing::info!(
                doc_id = %row.doc_id,
                tool_call_id = %row.tool_call_id,
                process = process.as_str(),
                lifecycle_state = %outcome.lifecycle_state().as_str(),
                "reconciled orphaned background tool"
            );
        }

        // Filtered absence does not prove settlement. Retain an unresolved
        // process record until its selected Agent can observe the terminal row.
        let records = {
            let executions = executions.clone();
            tokio::task::spawn_blocking(move || executions.process_record_list())
                .await
                .context("enumerating retained background process records")?
        };
        for record in records {
            if executions
                .contains(&record.tool_call_id, &record.tool_call_doc_id)
                .await
            {
                continue;
            }
            match selected_agent_observes_terminal_tool(
                &accesses,
                node_did,
                &record.tool_call_doc_id,
                &record.tool_call_id,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(doc_id = %record.tool_call_doc_id, "retaining unresolved background process record");
                    continue;
                }
                Err(error) => {
                    tracing::warn!(doc_id = %record.tool_call_doc_id, %error, "retaining background process record after failed terminal observation");
                    continue;
                }
            }
            let (before, after) = record.identity.stop().await;
            if crate::tools::exec::ProcessStopOutcome::from_observations(before, after)
                == crate::tools::exec::ProcessStopOutcome::StillRunning
            {
                tracing::warn!(
                    tool_call_id = %record.tool_call_id,
                    pid = record.identity.pid,
                    "settled background execution still has a running process"
                );
                continue;
            }
            executions.forget_process_record(&record.tool_call_id, &record.tool_call_doc_id);
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
        executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
        cause: CancelCause,
        completion_reason: &str,
    ) -> Result<(crate::tools::exec::ProcessStopOutcome, bool, bool)> {
        use crate::tools::exec::ProcessStopOutcome;
        let doc_id = lifecycle
            .doc_id()
            .context("background cancellation requires physical tool identity")?
            .to_owned();
        let tool_call_id = lifecycle.tool_call_id().to_owned();
        let process = executions.stop_execution(&tool_call_id, &doc_id).await;
        if matches!(
            process,
            ProcessStopOutcome::Stopped | ProcessStopOutcome::AlreadyExited
        ) && executions.settle_retained_stop(lifecycle).await?
        {
            return Ok((process, lifecycle.is_terminal(), true));
        }
        let (won, status, reason) = match process {
            ProcessStopOutcome::StillRunning => return Ok((process, false, false)),
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
        executions.forget_process_record(&tool_call_id, &doc_id);
        if won {
            let notification = async {
                let request_id = lifecycle.calling_request_id().await?;
                crate::tools::background::completion::append_background_tool_completion(
                    node,
                    &lifecycle.access()?,
                    lifecycle.session_id(),
                    &request_id,
                    &doc_id,
                    lifecycle.tool_name(),
                    status,
                    "",
                    Some(reason),
                    crate::request::lifecycle::RequestHopCause::Continuation,
                )
                .await
            }
            .await;
            if let Err(error) = notification {
                tracing::warn!(
                    tool_call_id,
                    error = %error,
                    "failed to append cancelled background tool notification"
                );
            }
        }
        Ok((process, won, false))
    }

    /// Persisted `status=completionPending:<reason>` advances to
    /// `completed` only after both side effects converge, so transient
    /// failures remain discoverable.
    pub async fn reconcile_background_completion_side_effects(
        node: &std::sync::Arc<EmbeddedNode>,
        node_did: &str,
        identities: &RecoveryIdentities,
    ) -> Result<BackgroundCompletionSideEffectReport> {
        let accesses = recovery_accesses(node, node_did, identities)?;
        let rows = load_pending_background_completion_rows(node_did, &accesses).await?;
        let mut report = BackgroundCompletionSideEffectReport::default();

        for row in rows {
            if row.node_did.as_deref() != Some(node_did) {
                continue;
            }
            let Some(request_id) = non_empty(row.request_id.as_deref()) else {
                continue;
            };
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
            let scope = crate::session::session_scope_filter(
                node_did,
                session_id,
                row.requester_did.as_deref(),
            );
            let Some((access, _parent)) =
                selected_parent_request(&accesses, node_did, request_id, request_doc_id, &scope)
                    .await?
            else {
                continue;
            };
            let output =
                match crate::tools::background::status::observe_canonical_tool_output_with_access(
                    access,
                    &row.doc_id,
                    request_doc_id,
                    session_id,
                    node_did,
                    row.requester_did.as_deref(),
                )
                .await
                {
                    Ok(output) => output.into_text(),
                    Err(error) => {
                        tracing::warn!(doc_id = %row.doc_id, error = %error, "canonical background output is unresolved");
                        continue;
                    }
                };
            // A session-message completion returns its result to this session
            // (Lean `CausalHop.return_keeps_caller_hop`).
            let wake =
                if super::ToolRowKind::of(&row.tool_name) == super::ToolRowKind::SessionMessage {
                    crate::request::lifecycle::RequestHopCause::Return
                } else {
                    crate::request::lifecycle::RequestHopCause::Continuation
                };
            let appended =
                crate::tools::background::completion::append_background_tool_completion_for_parent(
                    node,
                    access,
                    request_doc_id,
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
                    error = %format_args!("{error:#}"),
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
        use crate::tools::exec::ProcessStopOutcome;
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
            tool_name: crate::tools::native::AGENT_NEW_TOOL_NAME.into(),
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
        for tool_name in [crate::tools::native::AGENT_NEW_TOOL_NAME, "bash"] {
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
    accesses: &RecoveryAccesses,
    executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
) -> Result<usize> {
    let rows = load_running_tool_call_rows_for_agent(node_did, accesses).await?;

    let mut recovered = 0;
    for row in rows {
        // Background rows have their own owners: native processes the
        // orphan sweep, session-message rows the session-message sweep.
        if row.node_did.as_deref() != Some(node_did)
            || await_mode(&row) == AwaitMode::Background
            || executions.contains(&row.tool_call_id, &row.doc_id).await
        {
            continue;
        }

        let deadline_at = parse_datetime(row.deadline_at.as_deref());
        let Some((access, parent)) = selected_running_parent(node_did, &row, accesses).await?
        else {
            continue;
        };
        let outcome = classify_running_tool_recovery(&row, Some(&parent), Utc::now());

        let Some(outcome) = outcome else {
            continue;
        };

        let updated =
            match recover_tool_call_row(node, access, &row, deadline_at, outcome, true).await {
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
    access: &crate::ConfigAccess,
    row: &RunningToolCallRow,
    outcome: RecoveryOutcome,
) {
    let Some(parent_request_id) = row.request_id.as_deref().filter(|id| !id.is_empty()) else {
        return;
    };
    let Some(parent_request_doc_id) = non_empty(row.request_doc_id.as_deref()) else {
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
    if let Err(error) =
        crate::tools::background::completion::append_background_tool_completion_for_parent(
            node,
            access,
            parent_request_doc_id,
            &row.session_id,
            parent_request_id,
            &row.doc_id,
            &row.tool_name,
            status,
            "",
            Some(reason),
            crate::request::lifecycle::RequestHopCause::Continuation,
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
    node_did: &str,
    accesses: &RecoveryAccesses,
) -> Result<Vec<RunningToolCallRow>> {
    let escaped = escape_graphql_string(node_did);
    load_running_tool_call_rows_with_filter(
        accesses,
        &format!(r#", node_did: {{ _eq: "{escaped}" }}"#),
    )
    .await
}

async fn load_running_tool_call_rows_with_filter(
    accesses: &RecoveryAccesses,
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

    let mut rows = Vec::new();
    let mut seen = rapidhash::RapidHashSet::default();
    for access in accesses.values() {
        let response = access.execute(&query).await?;
        let values = response
            .pointer("/data/AgentToolCall")
            .and_then(serde_json::Value::as_array)
            .context("running tool-call recovery query omitted rows")?;
        for value in values {
            match serde_json::from_value::<RunningToolCallRow>(value.clone()) {
                Ok(row) if seen.insert(row.doc_id.clone()) => rows.push(row),
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    doc_id = value.get("_docID").and_then(serde_json::Value::as_str).unwrap_or(""),
                    error = %error,
                    "skipping malformed running tool-call row during recovery"
                ),
            }
        }
    }
    Ok(rows)
}

async fn load_pending_background_completion_rows(
    node_did: &str,
    accesses: &RecoveryAccesses,
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
    let mut rows = Vec::new();
    let mut seen = rapidhash::RapidHashSet::default();
    for access in accesses.values() {
        let response = access.execute(&query).await?;
        let values = response
            .pointer("/data/AgentToolCall")
            .and_then(serde_json::Value::as_array)
            .context("background completion recovery query omitted rows")?;
        for value in values {
            match serde_json::from_value::<TerminalBackgroundToolRow>(value.clone()) {
                Ok(row) if seen.insert(row.doc_id.clone()) => rows.push(row),
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    doc_id = value.get("_docID").and_then(serde_json::Value::as_str).unwrap_or(""),
                    error = %error,
                    "skipping malformed terminal background row during side-effect recovery"
                ),
            }
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

/// `exact` narrows the lookup to one physical request (`_docID`) within an
/// exact session scope filter.
async fn lookup_parent_request(
    access: &crate::ConfigAccess,
    node_did: &str,
    request_id: &str,
    exact: Option<(&str, &str)>,
) -> Result<Option<AgentRequestRow>> {
    let escaped_request_id = escape_graphql_string(request_id);
    let filter = match exact {
        Some((doc_id, scope)) => format!(
            r#"_docID: {{ _eq: "{}" }}, request_id: {{ _eq: "{escaped_request_id}" }}, {scope}"#,
            escape_graphql_string(doc_id)
        ),
        None => format!(
            r#"node_did: {{ _eq: "{}" }}, request_id: {{ _eq: "{escaped_request_id}" }}"#,
            escape_graphql_string(node_did)
        ),
    };
    let query = format!(
        r#"{{ AgentRequest(filter: {{ {filter} }}, limit: 2) {{
            _docID request_id node_did requester_did session_id agent_did lifecycle_state
            caused_by_trigger_id request_hop workspace_id workspace_authority
            workspace_owner_node_did workspace_seal_hash
        }} }}"#
    );

    let resp = access.execute(&query).await?;

    let value = resp
        .pointer("/data/AgentRequest")
        .context("AgentRequest field missing from parent recovery query")?;
    let rows: Vec<AgentRequestRow> = serde_json::from_value(value.clone())
        .context("decode parent AgentRequest rows for tool-call recovery")?;
    anyhow::ensure!(
        exact.is_none() || rows.len() <= 1,
        "background parent scope is ambiguous"
    );
    Ok(rows.into_iter().next())
}

async fn selected_running_parent<'a>(
    node_did: &str,
    row: &RunningToolCallRow,
    accesses: &'a RecoveryAccesses,
) -> Result<Option<(&'a crate::ConfigAccess, AgentRequestRow)>> {
    let (Some(request_doc_id), Some(request_id)) = (
        non_empty(row.request_doc_id.as_deref()),
        non_empty(row.request_id.as_deref()),
    ) else {
        return Ok(None);
    };
    if row.node_did.as_deref() != Some(node_did) {
        return Ok(None);
    }
    let scope = crate::session::session_scope_filter(
        node_did,
        &row.session_id,
        row.requester_did.as_deref(),
    );
    selected_parent_request(accesses, node_did, request_id, request_doc_id, &scope).await
}

async fn selected_parent_request<'a>(
    accesses: &'a RecoveryAccesses,
    node_did: &str,
    request_id: &str,
    request_doc_id: &str,
    scope: &str,
) -> Result<Option<(&'a crate::ConfigAccess, AgentRequestRow)>> {
    for (agent_did, access) in accesses {
        let Some(parent) =
            lookup_parent_request(access, node_did, request_id, Some((request_doc_id, scope)))
                .await?
        else {
            continue;
        };
        if parent.agent_did.as_deref() == Some(agent_did.as_str()) {
            return Ok(Some((access, parent)));
        }
    }
    Ok(None)
}

async fn selected_agent_observes_terminal_tool(
    accesses: &RecoveryAccesses,
    node_did: &str,
    tool_doc_id: &str,
    tool_call_id: &str,
) -> Result<bool> {
    #[derive(Deserialize)]
    struct ObservedTool {
        #[serde(flatten)]
        row: RunningToolCallRow,
        lifecycle_state: Option<String>,
    }
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{
            _docID request_id request_doc_id requester_did node_did session_id
            tool_call_id tool_name deadline_at await_mode cancel_cause job_pin lifecycle_state
        }} }}"#,
        escape_graphql_string(tool_doc_id)
    );
    for (agent_did, access) in accesses {
        let response = access.execute(&query).await?;
        let values = response
            .pointer("/data/AgentToolCall")
            .and_then(serde_json::Value::as_array)
            .context("terminal tool observation omitted rows")?;
        anyhow::ensure!(values.len() <= 1, "terminal tool observation is ambiguous");
        let Some(value) = values.first() else {
            continue;
        };
        let observed: ObservedTool = serde_json::from_value(value.clone())?;
        let row = observed.row;
        if row.doc_id != tool_doc_id
            || row.tool_call_id != tool_call_id
            || row.node_did.as_deref() != Some(node_did)
            || !observed
                .lifecycle_state
                .as_deref()
                .and_then(ToolCallState::from_persisted)
                .is_some_and(ToolCallState::is_terminal)
        {
            continue;
        }
        let (Some(request_id), Some(request_doc_id)) = (
            non_empty(row.request_id.as_deref()),
            non_empty(row.request_doc_id.as_deref()),
        ) else {
            continue;
        };
        let scope = crate::session::session_scope_filter(
            node_did,
            &row.session_id,
            row.requester_did.as_deref(),
        );
        let Some(parent) =
            lookup_parent_request(access, node_did, request_id, Some((request_doc_id, &scope)))
                .await?
        else {
            continue;
        };
        if parent.agent_did.as_deref() == Some(agent_did.as_str()) {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn load_recovery_lifecycle(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    row: &RunningToolCallRow,
) -> Result<ToolCallLifecycle> {
    let node_did = row
        .node_did
        .as_deref()
        .context("recovery row omitted node_did")?;
    access
        .transact_readonly("tool.recovery_load", |txn| {
            let node = node.clone();
            Box::pin(async move {
                let mut lifecycle = ToolCallLifecycle::load_by_doc_id_in_txn(
                    txn,
                    node,
                    &row.doc_id,
                    node_did,
                    &row.session_id,
                    row.requester_did.as_deref(),
                )
                .await?
                .context("recovery physical tool row disappeared")?;
                anyhow::ensure!(
                    txn.actor_did() == Some(lifecycle.actor().as_str()),
                    "tool recovery actor mismatch"
                );
                lifecycle.bind_agent_signer(txn.managed_agent_signer())?;
                Ok::<_, anyhow::Error>(lifecycle)
            })
        })
        .await
}

async fn recover_tool_call_row(
    node: &std::sync::Arc<EmbeddedNode>,
    access: &crate::ConfigAccess,
    row: &RunningToolCallRow,
    deadline_at: Option<DateTime<Utc>>,
    outcome: RecoveryOutcome,
    completion_side_effects_owed: bool,
) -> Result<bool> {
    let _ = completion_side_effects_owed;
    let mut lifecycle = load_recovery_lifecycle(node, access, row).await?;
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
        std::sync::Mutex<rapidhash::RapidHashMap<String, std::time::Instant>>,
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
    process: crate::tools::exec::ProcessStopOutcome,
    task_deleted: bool,
    session_stopped: bool,
    now: DateTime<Utc>,
) -> Option<RecoveryOutcome> {
    use crate::tools::exec::ProcessStopOutcome;
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
    process: crate::tools::exec::ProcessStopOutcome,
) -> Option<RecoveryOutcome> {
    use crate::tools::exec::ProcessStopOutcome;
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
    let response = crate::store::graphql::graphql_with_transaction_retry(
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
    let response = crate::store::graphql::graphql_with_transaction_retry(
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

#[cfg(test)]
mod actor_read_tests {
    use super::*;
    use crate::tools::call::lifecycle::admission_fixture::{
        published_admission_with_owner, PublishedAdmissionOptions,
    };

    #[tokio::test]
    async fn recovery_discovers_user_owned_tool_and_parent_as_selected_agent() {
        let (mut admission, parent) = published_admission_with_owner(PublishedAdmissionOptions {
            name: "recovery-selected-agent-observation".into(),
            tool_name: Some("bash".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        let node = admission.node.clone();
        let doc_id = admission.tool.doc_id().unwrap().to_owned();
        let query = format!(
            r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(&doc_id),
            escape_graphql_string(&parent.request().doc_id),
        );
        let node_view = crate::ConfigAccess::local(node.clone())
            .execute(&query)
            .await;
        if let Ok(response) = node_view {
            assert_eq!(response["data"]["AgentToolCall"], serde_json::json!([]));
            assert_eq!(response["data"]["AgentRequest"], serde_json::json!([]));
        }

        let identities: Vec<std::sync::Arc<dyn crate::store::identity::NodeIdentity>> =
            vec![std::sync::Arc::new(
                admission.tool.managed_agent_signer().unwrap(),
            )];
        let accesses = recovery_accesses(&node, &admission.node_did, &identities).unwrap();
        let rows = load_running_tool_call_rows_for_agent(&admission.node_did, &accesses)
            .await
            .unwrap();
        let row = rows.iter().find(|row| row.doc_id == doc_id).unwrap();
        let (access, selected) = selected_running_parent(&admission.node_did, row, &accesses)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            selected.doc_id.as_deref(),
            Some(parent.request().doc_id.as_str())
        );
        assert_eq!(access.actor_did().as_deref(), selected.agent_did.as_deref());
        let tool_call_id = admission.tool.tool_call_id().to_owned();
        assert!(!selected_agent_observes_terminal_tool(
            &accesses,
            &admission.node_did,
            &doc_id,
            &tool_call_id
        )
        .await
        .unwrap());
        assert!(!selected_agent_observes_terminal_tool(
            &RecoveryAccesses::new(),
            &admission.node_did,
            &doc_id,
            &tool_call_id
        )
        .await
        .unwrap());
        assert!(admission
            .tool
            .complete_owned("finished", None)
            .await
            .unwrap());
        assert!(selected_agent_observes_terminal_tool(
            &accesses,
            &admission.node_did,
            &doc_id,
            &tool_call_id
        )
        .await
        .unwrap());
        assert!(!selected_agent_observes_terminal_tool(
            &accesses,
            &admission.node_did,
            &doc_id,
            "wrong-tool"
        )
        .await
        .unwrap());
        node.shutdown().await.unwrap();
    }
}
