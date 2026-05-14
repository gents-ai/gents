//! Startup recovery for persisted running tool calls.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::background_tools::{
    fail_running_subagent_tool_call, load_parent_subagent_authorization, subagent_spawn_denial,
    subagent_tool_not_allowed_payload,
};
use crate::graphql::escape_graphql_string;
use crate::interrupt::interrupt_request;
use crate::session::execute_mutation_with_retry;

use super::{
    subagent_request::create_subagent_request_with_request_id, AwaitMode, CancelPolicy,
    FailureClass, ToolCallState,
};

#[derive(Debug, Default)]
pub struct ToolCallRecoveryReport {
    pub tool_calls_recovered: usize,
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
    child_request_id: Option<String>,
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
struct SpawnArgs {
    #[serde(alias = "target", alias = "target_behavior_id")]
    behavior_id: String,
    #[serde(alias = "message", alias = "content")]
    prompt: String,
    #[serde(default)]
    deadline: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryOutcome {
    TimedOut,
    Cancelled,
    Failed,
    BackgroundInterrupted,
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
                    "/behavior_id",
                    &spawn_args.behavior_id,
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
                    target_behavior_id = %spawn_args.behavior_id,
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
            &spawn_args.behavior_id,
            row_await_mode,
            tool_name,
        ) {
            let failed = fail_unauthorized_orphan_subagent_tool_call(
                node,
                &row,
                denial.path,
                &denial.requested,
                denial.message,
                &authorization.allowed_targets,
            )
            .await?;
            tracing::warn!(
                doc_id = %row.doc_id,
                request_id = %parent_request_id,
                session_id = %row.session_id,
                tool_call_id = %row.tool_call_id,
                child_request_id = %child_request_id,
                parent_behavior_id = %authorization.behavior_id,
                target_behavior_id = %spawn_args.behavior_id,
                await_mode = %row_await_mode.as_str(),
                failed_tool_call = failed,
                "cannot materialize orphan subagent child because spawn is not authorized"
            );
            continue;
        }

        if let Err(error) = create_subagent_request_with_request_id(
            node,
            child_request_id.clone(),
            parent_request_id.clone(),
            row.tool_call_id.clone(),
            parent_depth,
            parent.agent_did,
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
        let parent = match row
            .request_id
            .as_deref()
            .filter(|request_id| !request_id.is_empty())
        {
            Some(request_id) => lookup_parent_request(node, agent_did, request_id).await?,
            None => None,
        };

        if is_background_subagent_tool(&row)
            && !parent
                .as_ref()
                .is_some_and(|parent| request_is_interrupted(parent))
        {
            tracing::info!(
                doc_id = %row.doc_id,
                request_id = row.request_id.as_deref().unwrap_or(""),
                session_id = %row.session_id,
                tool_call_id = %row.tool_call_id,
                child_request_id = row.child_request_id.as_deref().unwrap_or(""),
                "leaving background subagent tool call running during recovery"
            );
            continue;
        }

        let outcome = if is_background_tool_row(&row)
            && parent
                .as_ref()
                .is_some_and(|parent| !request_is_terminal(parent))
        {
            Some(RecoveryOutcome::BackgroundInterrupted)
        } else if deadline_at.is_some_and(|deadline| Utc::now() >= deadline) {
            Some(RecoveryOutcome::TimedOut)
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
            continue;
        };

        if is_detached_subagent_tool(&row) {
            tracing::info!(
                doc_id = %row.doc_id,
                request_id = row.request_id.as_deref().unwrap_or(""),
                session_id = %row.session_id,
                tool_call_id = %row.tool_call_id,
                child_request_id = row.child_request_id.as_deref().unwrap_or(""),
                "leaving detached subagent tool call running during recovery"
            );
            continue;
        }

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
            } else if let Err(error) =
                write_bridge_cancel_cascade_intent(node, &row.doc_id, Utc::now()).await
            {
                tracing::warn!(
                    doc_id = %row.doc_id,
                    request_id = row.request_id.as_deref().unwrap_or(""),
                    session_id = %row.session_id,
                    tool_call_id = %row.tool_call_id,
                    child_request_id,
                    error = %error,
                    "failed to write recovery cross-deployment cancel intent"
                );
            }
        }

        if let Err(error) = recover_tool_call_row(node, &row, deadline_at, outcome).await {
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
    let query = r#"{
        AgentToolCall(
            filter: { lifecycle_state: { _eq: "running" } }
        ) {
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
            child_request_id
        }
    }"#;

    let resp = node.execute(query).await;
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

async fn recover_tool_call_row(
    node: &EmbeddedNode,
    row: &RunningToolCallRow,
    deadline_at: Option<DateTime<Utc>>,
    outcome: RecoveryOutcome,
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
                    latency_ms: {latency_ms}{failure_class_field}
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
    matches!(
        parent.status.as_str(),
        "completed" | "error" | "superseded" | "dead" | "interrupted"
    ) || matches!(
        parent.lifecycle_state.as_deref(),
        Some("completed" | "failed" | "superseded" | "dead" | "interrupted")
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

async fn write_bridge_cancel_cascade_intent(
    node: &EmbeddedNode,
    doc_id: &str,
    at: DateTime<Utc>,
) -> Result<()> {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let at = escape_graphql_string(&at.to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                input: {{
                    cancel_cascade_intent_at: "{at}",
                    cancel_pending_remote_ack: true
                }}
            ) {{ _docID }}
        }}"#
    );
    execute_mutation_with_retry(
        node,
        &mutation,
        "recovery_write_bridge_cancel_cascade_intent",
    )
    .await
    .context("write recovery bridge cancel cascade intent mutation")?;
    Ok(())
}

impl RecoveryOutcome {
    fn lifecycle_state(self) -> ToolCallState {
        match self {
            Self::TimedOut => ToolCallState::TimedOut,
            Self::Cancelled | Self::BackgroundInterrupted => ToolCallState::Cancelled,
            Self::Failed => ToolCallState::Failed,
        }
    }

    fn failure_class(self) -> Option<FailureClass> {
        match self {
            Self::TimedOut | Self::Failed => Some(FailureClass::External),
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
        }
    }
}
