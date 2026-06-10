//! Recovery for persisted running tool calls: the startup sweep over rows
//! orphaned by a daemon restart, plus the periodic subagent-liveness sweep
//! (#465) that terminalizes expired children and orphaned queued descendants
//! on the live reconciler tick.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::background_completion::ensure_background_subagent_completion_side_effects;
use crate::background_tools::{
    child_request_completed, fail_running_subagent_tool_call, load_parent_subagent_authorization,
    project_child_terminal, subagent_spawn_denial, subagent_tool_not_allowed_payload,
};
use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::interrupt::interrupt_request;
use crate::session::execute_mutation_with_retry;

use super::{
    subagent_request::create_subagent_request_with_request_id, AwaitMode, CancelCause,
    CancelPolicy, ChildTerminal, FailureClass, ToolCallState,
};

#[derive(Debug, Default)]
pub struct ToolCallRecoveryReport {
    pub tool_calls_recovered: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SubagentLivenessReport {
    pub expired_children_terminalized: usize,
    pub bridges_projected: usize,
    pub queued_descendants_interrupted: usize,
}

impl SubagentLivenessReport {
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
    session_id: String,
    tool_call_id: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    args: String,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    deadline_at: Option<String>,
    #[serde(default)]
    await_mode: Option<String>,
    #[serde(default)]
    cancel_policy: Option<String>,
    #[serde(default)]
    cancel_cause: Option<String>,
    #[serde(default)]
    child_request_id: Option<String>,
    #[serde(default)]
    unclaimed_deadline_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ParentRequestRow {
    agent_did: String,
    status: String,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    subagent_depth: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ChildRequestLivenessRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    request_id: String,
    agent_did: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    deadline: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamingChildResponseRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpawnArgs {
    #[serde(default)]
    name: Option<String>,
    /// Resolved owning DID of the target behavior (#377). Absent on legacy
    /// fixtures, which fall back to the parent's DID.
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(alias = "target", alias = "target_behavior_id")]
    behavior_id: String,
    #[serde(alias = "message", alias = "content")]
    prompt: String,
    #[serde(default)]
    deadline: Option<String>,
}

impl SpawnArgs {
    fn target_name(&self) -> &str {
        self.name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(&self.behavior_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryOutcome {
    TimedOut,
    Cancelled,
    Failed,
    BackgroundInterrupted,
    UnclaimedCrossDeploymentSpawn,
}

impl super::ToolCallLifecycle {
    pub async fn recover_all(
        node: &EmbeddedNode,
        agent_did: &str,
    ) -> Result<ToolCallRecoveryReport> {
        let materialized_children = recover_orphan_subagent_children(node, agent_did).await?;
        if materialized_children > 0 {
            tracing::info!(
                materialized_children,
                "materialized orphan subagent child requests during tool-call recovery"
            );
        }

        Ok(ToolCallRecoveryReport {
            tool_calls_recovered: recover_stuck_running_tool_calls(node, agent_did).await?,
        })
    }

    /// Periodic subagent-liveness reconciliation (#465; Lean:
    /// `Recovery.expiredSubagentChildSweep` / `Recovery.queuedDescendantSweep`,
    /// cadence `periodic`). Startup recovery already terminalizes expired
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
    /// 3. Interrupt pending (queued) descendants whose parent request is
    ///    already terminal — they can never legally run.
    pub async fn reconcile_subagent_liveness(
        node: &EmbeddedNode,
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

        report.queued_descendants_interrupted =
            interrupt_queued_descendants_of_terminal_parents(node, agent_did).await?;

        if !report.is_noop() {
            tracing::info!(
                expired_children_terminalized = report.expired_children_terminalized,
                bridges_projected = report.bridges_projected,
                queued_descendants_interrupted = report.queued_descendants_interrupted,
                "reconciled subagent liveness"
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
}

async fn recover_orphan_subagent_children(node: &EmbeddedNode, agent_did: &str) -> Result<usize> {
    let rows = load_running_tool_call_rows(node).await?;
    let mut materialized = 0;

    for row in rows {
        let Some(child_request_id) = child_request_id(&row).map(str::to_string) else {
            continue;
        };
        if child_request_exists(node, &child_request_id).await? {
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

        let spawn_args = match serde_json::from_str::<SpawnArgs>(&row.args) {
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

        let child_agent_did = spawn_args
            .agent_did
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| parent.agent_did.clone());
        if let Err(error) = create_subagent_request_with_request_id(
            node,
            child_request_id.clone(),
            parent_request_id.clone(),
            row.tool_call_id.clone(),
            parent_depth,
            child_agent_did,
            spawn_args.behavior_id,
            spawn_args.prompt,
            deadline,
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
    node: &EmbeddedNode,
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
        row.started_at.as_deref(),
        row.deadline_at.as_deref(),
        &payload,
        FailureClass::ServiceUnavailable,
    )
    .await
}

async fn recover_stuck_running_tool_calls(node: &EmbeddedNode, agent_did: &str) -> Result<usize> {
    let rows = load_running_tool_call_rows(node).await?;

    let mut recovered = 0;
    for row in rows {
        let deadline_at = parse_datetime(row.deadline_at.as_deref());
        let unclaimed_deadline_at = parse_datetime(row.unclaimed_deadline_at.as_deref());
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

        let outcome = if deadline_at.is_some_and(|deadline| Utc::now() >= deadline) {
            Some(RecoveryOutcome::TimedOut)
        } else if unclaimed_deadline_at.is_some_and(|deadline| Utc::now() >= deadline) {
            Some(RecoveryOutcome::UnclaimedCrossDeploymentSpawn)
        } else if is_background_tool_row(&row)
            && parent
                .as_ref()
                .is_some_and(|parent| !request_is_terminal(parent))
        {
            Some(RecoveryOutcome::BackgroundInterrupted)
        } else if is_detached_subagent_tool(&row)
            && parent
                .as_ref()
                .is_some_and(|parent| request_is_interrupted(parent))
        {
            None
        } else if parent
            .as_ref()
            .is_some_and(|parent| request_is_interrupted(parent))
        {
            Some(RecoveryOutcome::Cancelled)
        } else if parent.as_ref().is_some_and(request_is_terminal) {
            Some(RecoveryOutcome::Failed)
        } else {
            None
        };

        let Some(outcome) = outcome else {
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

        let mut remote_cancel_intent_at = None;
        if outcome != RecoveryOutcome::UnclaimedCrossDeploymentSpawn {
            if let Some(child_request_id) = cascade_child_request_id(&row) {
                if child_request_is_locally_owned(node, agent_did, child_request_id).await? {
                    if let Err(error) = interrupt_request(node, child_request_id).await {
                        tracing::warn!(
                            doc_id = %row.doc_id,
                            request_id = row.request_id.as_deref().unwrap_or(""),
                            session_id = %row.session_id,
                            tool_call_id = %row.tool_call_id,
                            child_request_id,
                            error = %error,
                            "failed to cascade recovery interrupt to child request"
                        );
                    }
                } else {
                    remote_cancel_intent_at = Some(Utc::now());
                }
            }
        }

        if let Err(error) =
            recover_tool_call_row(node, &row, deadline_at, outcome, remote_cancel_intent_at).await
        {
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

        if outcome == RecoveryOutcome::BackgroundInterrupted {
            if let Some(parent_request_id) = row.request_id.as_deref().filter(|id| !id.is_empty()) {
                if let Err(error) = crate::background_completion::append_background_tool_completion(
                    node,
                    &row.session_id,
                    parent_request_id,
                    &row.tool_call_id,
                    &row.tool_name,
                    "cancelled",
                    "",
                    Some("interrupted_on_restart"),
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

async fn load_running_tool_call_rows(node: &EmbeddedNode) -> Result<Vec<RunningToolCallRow>> {
    load_running_tool_call_rows_with_filter(node, "").await
}

/// Running bridge rows only (`child_request_id` set) — the periodic liveness
/// sweep's scope, filtered server-side so the 5s tick never pays for
/// non-subagent tool rows.
async fn load_running_subagent_bridge_rows(
    node: &EmbeddedNode,
) -> Result<Vec<RunningToolCallRow>> {
    load_running_tool_call_rows_with_filter(node, r#", child_request_id: { _ne: "" }"#).await
}

async fn load_running_tool_call_rows_with_filter(
    node: &EmbeddedNode,
    extra_filter: &str,
) -> Result<Vec<RunningToolCallRow>> {
    let query = format!(
        r#"{{
        AgentToolCall(
            filter: {{ lifecycle_state: {{ _eq: "running" }}{extra_filter} }}
        ) {{
            _docID
            request_id
            session_id
            tool_call_id
            tool_name
            args
            started_at
            deadline_at
            await_mode
            cancel_policy
            cancel_cause
            child_request_id
            unclaimed_deadline_at
        }}
    }}"#
    );

    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("querying stuck running tool calls: {:?}", resp.errors);
    }

    let rows: Vec<RunningToolCallRow> = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows)
}

async fn lookup_parent_request(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
) -> Result<Option<ParentRequestRow>> {
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
                agent_did
                status
                lifecycle_state
                subagent_depth
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

    let rows: Vec<ParentRequestRow> = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows.into_iter().next())
}

#[derive(Debug, Deserialize)]
struct PendingDescendantRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    #[serde(default)]
    caused_by_parent_request_id: Option<String>,
    #[serde(default)]
    caused_by_parent_tool_call_id: Option<String>,
}

/// Interrupt pending (queued) subagent child requests whose parent request is
/// already terminal (#465; Lean: `Recovery.queuedDescendantSweep`). A queued
/// spawn child of a terminal parent can never legally run; leaving it pending
/// wedges the live queue forever. This is the queued-side analogue of the
/// running-child cascade interrupt, applied as a direct filtered terminal
/// write because a pending row has no executor to observe an interrupt.
///
/// Scope guard (Lean: `QueuedDescendantRow.bridgeLinked`): only requests
/// referenced by an `AgentToolCall` bridge (`child_request_id == request_id`)
/// qualify. Queue rows that merely CARRY spawn lineage —
/// background-completion wake notifications, steering messages — are never
/// referenced by a bridge and must survive a terminal caller, so lineage
/// fields alone are deliberately not trusted.
///
/// The parent is looked up by `request_id` alone (no agent_did filter) so a
/// CROSS-DEPLOYMENT terminal parent whose replicated row is visible here also
/// releases its queued children; a parent row that has not replicated yet
/// yields `None` and the child is conservatively left pending.
async fn interrupt_queued_descendants_of_terminal_parents(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<usize> {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    lifecycle_state: {{ _eq: "pending" }},
                    caused_by_parent_tool_call_id: {{ _ne: "" }}
                }}
            ) {{
                _docID
                request_id
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("querying pending descendant requests: {:?}", resp.errors);
    }
    let rows: Vec<PendingDescendantRow> = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();

    let candidates = rows
        .iter()
        .filter(|row| {
            row.caused_by_parent_request_id
                .as_deref()
                .is_some_and(|id| !id.is_empty())
                && row
                    .caused_by_parent_tool_call_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty())
        })
        .collect::<Vec<_>>();
    let bridged_children = load_bridged_child_ids(
        node,
        &candidates
            .iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
    )
    .await?;

    let mut parent_terminal_cache: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    let mut interrupted = 0usize;
    for row in candidates {
        let Some(parent_request_id) = row
            .caused_by_parent_request_id
            .as_deref()
            .filter(|id| !id.is_empty())
        else {
            continue;
        };

        let parent_terminal = match parent_terminal_cache.get(parent_request_id) {
            Some(&terminal) => terminal,
            None => {
                // By request_id alone: the parent of a cross-deployment spawn
                // carries a remote agent_did, and its replicated terminal row
                // must still release the queued child here.
                let terminal = load_request_liveness_row(node, parent_request_id)
                    .await?
                    .is_some_and(|parent| {
                        request_status_or_lifecycle_is_terminal(
                            parent.status.as_deref(),
                            parent.lifecycle_state.as_deref(),
                        )
                    });
                parent_terminal_cache.insert(parent_request_id.to_string(), terminal);
                terminal
            }
        };
        if !parent_terminal {
            continue;
        }
        if !bridged_children.contains(&row.request_id) {
            continue;
        }

        if interrupt_pending_descendant_row(node, &row.doc_id, parent_request_id).await? {
            interrupted += 1;
            tracing::info!(
                doc_id = %row.doc_id,
                request_id = %row.request_id,
                parent_request_id,
                "interrupted queued subagent descendant of terminal parent"
            );
        }
    }
    Ok(interrupted)
}

/// One `_in` query for the bridge-existence scope guard: which of these
/// pending request ids are referenced by an `AgentToolCall` bridge as its
/// child (`child_request_id == request_id`)?
async fn load_bridged_child_ids(
    node: &EmbeddedNode,
    child_request_ids: &[&str],
) -> Result<std::collections::HashSet<String>> {
    if child_request_ids.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let id_list = child_request_ids
        .iter()
        .map(|id| format!("\"{}\"", escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ child_request_id: {{ _in: [{id_list}] }} }}
            ) {{ child_request_id }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!("querying bridges for pending children: {:?}", resp.errors);
    }
    #[derive(Debug, Deserialize)]
    struct BridgeChildRow {
        #[serde(default)]
        child_request_id: Option<String>,
    }
    let rows: Vec<BridgeChildRow> = resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .filter_map(|row| row.child_request_id)
        .collect())
}

async fn interrupt_pending_descendant_row(
    node: &EmbeddedNode,
    doc_id: &str,
    parent_request_id: &str,
) -> Result<bool> {
    let reason = format!(
        "parent request {parent_request_id} reached a terminal state before this queued child was claimed"
    );
    let escaped_doc_id = escape_graphql_string(doc_id);
    let escaped_reason = escape_graphql_string(&reason);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    _docID: {{ _eq: "{escaped_doc_id}" }},
                    lifecycle_state: {{ _eq: "pending" }}
                }},
                input: {{
                    status: "interrupted",
                    lifecycle_state: "interrupted",
                    failure_reason: "{escaped_reason}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response =
        execute_mutation_with_retry(node, &mutation, "interrupt_queued_descendant").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_AgentRequest"))
        .is_some_and(response_has_documents))
}

async fn child_request_exists(node: &EmbeddedNode, request_id: &str) -> Result<bool> {
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
    node: &EmbeddedNode,
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

    if child_request_completed(&child) {
        let result = load_child_completion_result(node, child_request_id)
            .await?
            .unwrap_or_else(|| format!("child request {child_request_id} completed"));
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
    node: &EmbeddedNode,
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
    node: &EmbeddedNode,
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
    node: &EmbeddedNode,
    agent_did: &str,
    row: &RunningToolCallRow,
    child: &ChildRequestLivenessRow,
) -> Result<bool> {
    let Some(child_request_id) = child_request_id(row) else {
        return Ok(false);
    };
    if child.agent_did != agent_did {
        return Ok(false);
    }
    if request_status_or_lifecycle_is_terminal(
        child.status.as_deref(),
        child.lifecycle_state.as_deref(),
    ) {
        return Ok(false);
    }
    let Some(deadline_at) = parse_datetime(child.deadline.as_deref()) else {
        return Ok(false);
    };
    if Utc::now() < deadline_at {
        return Ok(false);
    }

    let reason = format!(
        "child request deadline exceeded at {} before terminal response",
        deadline_at.to_rfc3339()
    );
    if !mark_child_request_dead(node, child, &reason).await? {
        return Ok(false);
    }
    finalize_streaming_child_response(node, child_request_id, &reason).await?;
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
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<ChildRequestLivenessRow>> {
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
                status
                lifecycle_state
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
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| {
            serde_json::from_value::<Vec<ChildRequestLivenessRow>>(value.clone()).ok()
        })
        .and_then(|mut rows| rows.pop()))
}

/// Batched form of `load_child_liveness_row`: one `_in` query for every
/// bridge's child on the periodic tick, keyed by `request_id`.
async fn load_child_liveness_rows(
    node: &EmbeddedNode,
    child_request_ids: &[&str],
) -> Result<std::collections::HashMap<String, ChildRequestLivenessRow>> {
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
                status
                lifecycle_state
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
    let rows: Vec<ChildRequestLivenessRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .map(|row| (row.request_id.clone(), row))
        .collect())
}

async fn mark_child_request_dead(
    node: &EmbeddedNode,
    child: &ChildRequestLivenessRow,
    reason: &str,
) -> Result<bool> {
    let active_runtime_states = crate::lifecycle::active_runtime_lifecycle_state_graphql_list();
    let escaped_doc_id = escape_graphql_string(&child.doc_id);
    let escaped_agent_did = escape_graphql_string(&child.agent_did);
    let escaped_reason = escape_graphql_string(reason);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{
                    _docID: {{ _eq: "{escaped_doc_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    lifecycle_state: {{ _in: {active_runtime_states} }},
                    status: {{ _nin: ["completed", "interrupted", "dead", "superseded", "error"] }}
                }},
                input: {{
                    status: "dead",
                    lifecycle_state: "dead",
                    failure_reason: "{escaped_reason}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response =
        execute_mutation_with_retry(node, &mutation, "terminalize_expired_child_request").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("update_AgentRequest"))
        .is_some_and(response_has_documents))
}

async fn finalize_streaming_child_response(
    node: &EmbeddedNode,
    child_request_id: &str,
    reason: &str,
) -> Result<()> {
    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{
                    request_id: {{ _eq: "{escaped_child_request_id}" }},
                    status: {{ _eq: "streaming" }}
                }}
            ) {{
                _docID
                content
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query streaming child AgentResponse {child_request_id} failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<StreamingChildResponseRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    let now = Utc::now().to_rfc3339();
    let escaped_reason = escape_graphql_string(reason);
    for row in rows {
        let content = row.content.unwrap_or_default();
        let final_content = if content.trim().is_empty() {
            format!("Error: {reason}")
        } else {
            format!("{content}\n\n[Response interrupted - {reason}]")
        };
        let escaped_doc_id = escape_graphql_string(&row.doc_id);
        let escaped_content = escape_graphql_string(&final_content);
        let escaped_now = escape_graphql_string(&now);
        let mutation = format!(
            r#"mutation {{
                update_AgentResponse(
                    filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                    input: {{
                        content: "{escaped_content}",
                        status: "error",
                        error_message: "{escaped_reason}",
                        completed_at: "{escaped_now}"
                    }}
                ) {{ _docID }}
            }}"#
        );
        execute_mutation_with_retry(node, &mutation, "finalize_expired_child_response").await?;
    }
    Ok(())
}

async fn load_child_completion_result(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Result<Option<String>> {
    #[derive(Deserialize)]
    struct ResponseRow {
        content: Option<String>,
    }

    let escaped_child_request_id = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                content
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query child AgentResponse {child_request_id} for bridge recovery failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<ResponseRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentResponse"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    Ok(rows
        .into_iter()
        .next()
        .and_then(|row| row.content)
        .filter(|content| !content.trim().is_empty()))
}

async fn recover_bridge_completed_row(
    node: &EmbeddedNode,
    row: &RunningToolCallRow,
    child_result: &str,
) -> Result<()> {
    let now = Utc::now();
    let started_at = parse_datetime(row.started_at.as_deref()).unwrap_or(now);
    let deadline_at = parse_datetime(row.deadline_at.as_deref()).unwrap_or(now);
    let latency_ms = (now - started_at).num_milliseconds().max(0);
    let escaped_doc_id = escape_graphql_string(&row.doc_id);
    let escaped_result = escape_graphql_string(child_result);
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{
                    _docID: {{ _eq: "{escaped_doc_id}" }},
                    lifecycle_state: {{ _eq: "running" }}
                }},
                input: {{
                    result: "{escaped_result}",
                    status: "completed",
                    lifecycle_state: "completed",
                    started_at: "{started_at}",
                    deadline_at: "{deadline_at}",
                    completed_at: "{completed_at}",
                    latency_ms: {latency_ms},
                    unclaimed_deadline_at: null
                }}
            ) {{ _docID }}
        }}"#,
        started_at = started_at.to_rfc3339(),
        deadline_at = deadline_at.to_rfc3339(),
        completed_at = now.to_rfc3339(),
    );

    execute_mutation_with_retry(node, &mutation, "recover_bridge_completed_child")
        .await
        .context("recover bridge completed child mutation")?;
    Ok(())
}

async fn recover_bridge_failed_row(
    node: &EmbeddedNode,
    row: &RunningToolCallRow,
    terminal: &ChildTerminal,
) -> Result<()> {
    let now = Utc::now();
    let started_at = parse_datetime(row.started_at.as_deref()).unwrap_or(now);
    let deadline_at = parse_datetime(row.deadline_at.as_deref()).unwrap_or(now);
    let latency_ms = (now - started_at).num_milliseconds().max(0);
    let escaped_doc_id = escape_graphql_string(&row.doc_id);
    let projected = terminal.projected_state().as_str();
    let cancel_cause_field = if terminal.projected_state() == ToolCallState::Cancelled {
        let cause = row
            .cancel_cause
            .as_deref()
            .and_then(CancelCause::from_persisted)
            .unwrap_or(CancelCause::Interrupted)
            .as_str();
        format!(r#"cancel_cause: "{cause}","#)
    } else {
        String::new()
    };
    let optional_fields = match terminal {
        ChildTerminal::Failed {
            reason,
            failure_class,
        } => {
            let escaped_reason = escape_graphql_string(reason);
            let failure_class = failure_class.as_str();
            format!(
                r#"result: "{escaped_reason}",
                    tool_failure_class: "{failure_class}","#
            )
        }
        _ => String::new(),
    };
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{
                    _docID: {{ _eq: "{escaped_doc_id}" }},
                    lifecycle_state: {{ _eq: "running" }}
                }},
                input: {{
                    {optional_fields}
                    {cancel_cause_field}
                    status: "completed",
                    lifecycle_state: "{projected}",
                    started_at: "{started_at}",
                    deadline_at: "{deadline_at}",
                    completed_at: "{completed_at}",
                    latency_ms: {latency_ms},
                    unclaimed_deadline_at: null
                }}
            ) {{ _docID }}
        }}"#,
        started_at = started_at.to_rfc3339(),
        deadline_at = deadline_at.to_rfc3339(),
        completed_at = now.to_rfc3339(),
    );

    execute_mutation_with_retry(node, &mutation, "recover_bridge_terminal_child")
        .await
        .context("recover bridge terminal child mutation")?;
    Ok(())
}

async fn recover_tool_call_row(
    node: &EmbeddedNode,
    row: &RunningToolCallRow,
    deadline_at: Option<DateTime<Utc>>,
    outcome: RecoveryOutcome,
    remote_cancel_intent_at: Option<DateTime<Utc>>,
) -> Result<()> {
    let now = Utc::now();
    let started_at = parse_datetime(row.started_at.as_deref()).unwrap_or(now);
    let latency_ms = (now - started_at).num_milliseconds().max(0);
    let escaped_doc_id = escape_graphql_string(&row.doc_id);
    let escaped_result = escape_graphql_string(&outcome.result_text(deadline_at));
    let started_at_str = started_at.to_rfc3339();
    let completed_at_str = now.to_rfc3339();
    let deadline_field = deadline_at
        .map(|deadline| format!(r#", deadline_at: "{}""#, deadline.to_rfc3339()))
        .unwrap_or_default();
    let failure_class_field = outcome
        .failure_class()
        .map(|failure| format!(r#", tool_failure_class: "{}""#, failure.as_str()))
        .unwrap_or_default();
    let cancel_cause_field = outcome
        .cancel_cause(row.cancel_cause.as_deref())
        .map(|cause| format!(r#", cancel_cause: "{}""#, cause.as_str()))
        .unwrap_or_default();
    let remote_cancel_intent_fields = remote_cancel_intent_at
        .map(|at| {
            format!(
                r#", cancel_cascade_intent_at: "{}", cancel_pending_remote_ack: true"#,
                escape_graphql_string(&at.to_rfc3339())
            )
        })
        .unwrap_or_default();

    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                input: {{
                    result: "{escaped_result}",
                    status: "completed",
                    lifecycle_state: "{lifecycle_state}",
                    started_at: "{started_at_str}"{deadline_field},
                    completed_at: "{completed_at_str}",
                    latency_ms: {latency_ms},
                    unclaimed_deadline_at: null{failure_class_field}{cancel_cause_field}{remote_cancel_intent_fields}
                }}
            ) {{ _docID }}
        }}"#,
        lifecycle_state = outcome.lifecycle_state().as_str(),
    );

    execute_mutation_with_retry(node, &mutation, "recover_running_tool_call")
        .await
        .context("recover running tool call mutation")?;
    Ok(())
}

fn parse_datetime(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .filter(|value| !value.is_empty())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|datetime| datetime.with_timezone(&Utc))
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

fn request_is_interrupted(parent: &ParentRequestRow) -> bool {
    parent.status == "interrupted" || parent.lifecycle_state.as_deref() == Some("interrupted")
}

fn request_is_terminal(parent: &ParentRequestRow) -> bool {
    request_status_or_lifecycle_is_terminal(
        Some(parent.status.as_str()),
        parent.lifecycle_state.as_deref(),
    )
}

fn request_status_or_lifecycle_is_terminal(
    status: Option<&str>,
    lifecycle_state: Option<&str>,
) -> bool {
    matches!(
        status,
        Some("completed" | "complete" | "error" | "failed" | "superseded" | "dead" | "interrupted")
    ) || matches!(
        lifecycle_state,
        Some("completed" | "complete" | "failed" | "error" | "superseded" | "dead" | "interrupted")
    )
}

fn child_request_id(row: &RunningToolCallRow) -> Option<&str> {
    row.child_request_id.as_deref().filter(|id| !id.is_empty())
}

fn cancel_policy(row: &RunningToolCallRow) -> CancelPolicy {
    row.cancel_policy
        .as_deref()
        .and_then(CancelPolicy::from_persisted)
        .unwrap_or(CancelPolicy::Cascade)
}

fn await_mode(row: &RunningToolCallRow) -> AwaitMode {
    row.await_mode
        .as_deref()
        .and_then(AwaitMode::from_persisted)
        .unwrap_or(AwaitMode::Foreground)
}

fn subagent_tool_name(row: &RunningToolCallRow) -> &str {
    row.tool_name
        .as_str()
        .trim()
        .is_empty()
        .then_some("spawn_subagent")
        .unwrap_or(row.tool_name.as_str())
}

fn is_background_subagent_tool(row: &RunningToolCallRow) -> bool {
    child_request_id(row).is_some() && await_mode(row) == AwaitMode::Background
}

fn is_background_tool_row(row: &RunningToolCallRow) -> bool {
    child_request_id(row).is_none() && await_mode(row) == AwaitMode::Background
}

fn is_detached_subagent_tool(row: &RunningToolCallRow) -> bool {
    child_request_id(row).is_some() && cancel_policy(row) == CancelPolicy::Detach
}

fn cascade_child_request_id(row: &RunningToolCallRow) -> Option<&str> {
    let child_request_id = child_request_id(row)?;
    (cancel_policy(row) == CancelPolicy::Cascade).then_some(child_request_id)
}

async fn child_request_is_locally_owned(
    node: &EmbeddedNode,
    local_did: &str,
    child_request_id: &str,
) -> Result<bool> {
    let escaped = escape_graphql_string(child_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{ agent_did }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query AgentRequest for recovery cascade ownership failed: {:?}",
            response.errors
        );
    }
    let did = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("agent_did"))
        .and_then(|v| v.as_str());
    Ok(did == Some(local_did))
}

impl RecoveryOutcome {
    fn lifecycle_state(self) -> ToolCallState {
        match self {
            Self::TimedOut => ToolCallState::TimedOut,
            Self::Cancelled | Self::BackgroundInterrupted => ToolCallState::Cancelled,
            Self::Failed | Self::UnclaimedCrossDeploymentSpawn => ToolCallState::Failed,
        }
    }

    fn failure_class(self) -> Option<FailureClass> {
        match self {
            Self::TimedOut | Self::Failed => Some(FailureClass::External),
            Self::UnclaimedCrossDeploymentSpawn => Some(FailureClass::ServiceUnavailable),
            Self::Cancelled | Self::BackgroundInterrupted => None,
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
        }
    }

    fn cancel_cause(self, persisted: Option<&str>) -> Option<CancelCause> {
        persisted
            .and_then(CancelCause::from_persisted)
            .or(match self {
                Self::TimedOut => Some(CancelCause::Deadline),
                Self::Cancelled | Self::BackgroundInterrupted => Some(CancelCause::Interrupted),
                Self::Failed | Self::UnclaimedCrossDeploymentSpawn => None,
            })
    }
}
