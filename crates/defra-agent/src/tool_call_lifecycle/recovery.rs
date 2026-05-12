//! Startup recovery for persisted running tool calls.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;
use crate::interrupt::interrupt_request;
use crate::session::execute_mutation_with_retry;

use super::{CancelPolicy, FailureClass, ToolCallState};

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
    started_at: Option<String>,
    #[serde(default)]
    deadline_at: Option<String>,
    #[serde(default)]
    cancel_policy: Option<String>,
    #[serde(default)]
    child_request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ParentRequestRow {
    status: String,
    #[serde(default)]
    lifecycle_state: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryOutcome {
    TimedOut,
    Cancelled,
    Failed,
}

impl super::ToolCallLifecycle {
    pub async fn recover_all(
        node: &EmbeddedNode,
        agent_did: &str,
    ) -> Result<ToolCallRecoveryReport> {
        Ok(ToolCallRecoveryReport {
            tool_calls_recovered: recover_stuck_running_tool_calls(node, agent_did).await?,
        })
    }
}

async fn recover_stuck_running_tool_calls(node: &EmbeddedNode, agent_did: &str) -> Result<usize> {
    let query = r#"{
        AgentToolCall(
            filter: { lifecycle_state: { _eq: "running" } }
        ) {
            _docID
            request_id
            session_id
            tool_call_id
            started_at
            deadline_at
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

        let outcome = if deadline_at.is_some_and(|deadline| Utc::now() >= deadline) {
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
                status
                lifecycle_state
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

fn is_detached_subagent_tool(row: &RunningToolCallRow) -> bool {
    child_request_id(row).is_some() && cancel_policy(row) == CancelPolicy::Detach
}

fn cascade_child_request_id(row: &RunningToolCallRow) -> Option<&str> {
    let child_request_id = child_request_id(row)?;
    (cancel_policy(row) == CancelPolicy::Cascade).then_some(child_request_id)
}

impl RecoveryOutcome {
    fn lifecycle_state(self) -> ToolCallState {
        match self {
            Self::TimedOut => ToolCallState::TimedOut,
            Self::Cancelled => ToolCallState::Cancelled,
            Self::Failed => ToolCallState::Failed,
        }
    }

    fn failure_class(self) -> Option<FailureClass> {
        match self {
            Self::Failed => Some(FailureClass::External),
            Self::TimedOut | Self::Cancelled => None,
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
            Self::Failed => {
                "tool call failed because parent request was already terminal".to_string()
            }
        }
    }
}
