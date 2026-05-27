use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use codex_app_server_protocol as codex;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::UpdateSubscriptionSource;
use serde_json::Value;

use super::command_projection::{
    command_execution_item, command_output_payload, tool_projection_status, ToolProjectionStatus,
};
use super::progress::{
    decode_defra_tool_call_progress, defra_turn_progress_query, DefraToolCallProgress,
};
use super::protocol::{now_millis, send_notification};
use super::store::query_node_json;
use super::{ConnectionState, ShimState};

pub(super) fn spawn_background_tool_watcher(
    connection: ConnectionState,
    state: ShimState,
    request_id: String,
    session_id: String,
    thread_id: String,
    turn_id: String,
    cwd: PathBuf,
    mut running: BTreeMap<String, codex::CommandExecutionStatus>,
) {
    if running.is_empty() {
        return;
    }

    tokio::spawn(async move {
        let mut updates = state.node.subscribe_updates();
        while !running.is_empty() {
            let response = match query_node_json(
                state.node.as_ref(),
                &defra_turn_progress_query(&request_id, &session_id),
            )
            .await
            {
                Ok(response) => response,
                Err(error) => {
                    tracing::warn!(%error, "Codex shim background tool watcher query failed");
                    break;
                }
            };

            let tool_rows = response
                .pointer("/data/AgentToolCall")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let current_tools = tool_rows
                .iter()
                .filter_map(decode_defra_tool_call_progress)
                .map(|tool| (tool.tool_call_key.clone(), tool))
                .collect::<BTreeMap<_, _>>();

            let tracked = running.keys().cloned().collect::<Vec<_>>();
            for tool_key in tracked {
                let Some(tool) = current_tools.get(&tool_key) else {
                    running.remove(&tool_key);
                    continue;
                };
                match tool_projection_status(tool) {
                    ToolProjectionStatus::Command(codex::CommandExecutionStatus::InProgress) => {}
                    ToolProjectionStatus::Command(status) => {
                        if let Err(error) = send_background_tool_completion(
                            &connection.outbound,
                            &state,
                            &thread_id,
                            &turn_id,
                            tool,
                            status,
                            &cwd,
                        )
                        .await
                        {
                            tracing::warn!(%error, "Codex shim background tool completion send failed");
                            return;
                        }
                        running.remove(&tool_key);
                    }
                    ToolProjectionStatus::Mcp(_) => {
                        let mut foreground_tool = tool.clone();
                        foreground_tool.result.clear();
                        if let Err(error) = send_background_tool_completion(
                            &connection.outbound,
                            &state,
                            &thread_id,
                            &turn_id,
                            &foreground_tool,
                            codex::CommandExecutionStatus::Completed,
                            &cwd,
                        )
                        .await
                        {
                            tracing::warn!(%error, "Codex shim background foreground send failed");
                            return;
                        }
                        running.remove(&tool_key);
                    }
                }
            }

            if running.is_empty() {
                break;
            }

            tokio::select! {
                _ = tokio::time::sleep(state.poll_interval) => {}
                msg = updates.recv() => {
                    if msg.is_none() {
                        tracing::warn!("Codex shim background update subscription closed");
                    }
                    let dropped = updates.check_and_reset_dropped();
                    if dropped > 0 {
                        tracing::warn!(dropped, "Codex shim background update subscription dropped messages");
                    }
                }
            }
        }
    });
}

async fn send_background_tool_completion(
    outbound: &super::Outbound,
    state: &ShimState,
    thread_id: &str,
    turn_id: &str,
    tool: &DefraToolCallProgress,
    status: codex::CommandExecutionStatus,
    cwd: &Path,
) -> Result<()> {
    if let Some(delta) = command_output_payload(tool) {
        send_notification(
            outbound,
            state,
            codex::ServerNotification::CommandExecutionOutputDelta(
                codex::CommandExecutionOutputDeltaNotification {
                    thread_id: thread_id.to_string(),
                    turn_id: turn_id.to_string(),
                    item_id: tool.tool_call_key.clone(),
                    delta,
                },
            ),
        )
        .await?;
    }

    send_notification(
        outbound,
        state,
        codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
            item: command_execution_item(cwd, tool, status),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            completed_at_ms: now_millis(),
        }),
    )
    .await
}

#[derive(Debug, Clone)]
struct BackgroundTerminalRow {
    doc_id: String,
    tool_call_key: String,
    child_request_id: Option<String>,
    started_at: Option<String>,
    deadline_at: Option<String>,
}

pub(super) async fn clean_background_terminals(state: &ShimState, thread_id: &str) -> Result<()> {
    let rows = load_running_background_terminal_rows(state.node.as_ref(), thread_id).await?;
    for row in rows {
        if let Some(child_request_id) = row.child_request_id.as_deref() {
            if let Err(error) =
                defra_agent::interrupt_request(state.node.as_ref(), child_request_id).await
            {
                tracing::warn!(
                    %error,
                    child_request_id,
                    tool_call_key = %row.tool_call_key,
                    "Codex shim failed to interrupt background child request"
                );
            }
        }
        mark_background_terminal_cancelled(state.node.as_ref(), &row).await?;
    }
    Ok(())
}

async fn load_running_background_terminal_rows(
    node: &EmbeddedNode,
    thread_id: &str,
) -> Result<Vec<BackgroundTerminalRow>> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_thread_id}" }},
                    await_mode: {{ _eq: "background" }},
                    lifecycle_state: {{ _eq: "running" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                _docID
                tool_call_key
                child_request_id
                started_at
                deadline_at
            }}
        }}"#
    );
    let response = query_node_json(node, &query).await?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(rows
        .iter()
        .filter_map(decode_background_terminal_row)
        .collect())
}

fn decode_background_terminal_row(row: &Value) -> Option<BackgroundTerminalRow> {
    Some(BackgroundTerminalRow {
        doc_id: row.get("_docID")?.as_str()?.to_string(),
        tool_call_key: row.get("tool_call_key")?.as_str()?.to_string(),
        child_request_id: row
            .get("child_request_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned),
        started_at: row
            .get("started_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        deadline_at: row
            .get("deadline_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

async fn mark_background_terminal_cancelled(
    node: &EmbeddedNode,
    row: &BackgroundTerminalRow,
) -> Result<()> {
    let now = Utc::now();
    let started_at = parse_defra_datetime(row.started_at.as_deref()).unwrap_or(now);
    let deadline_at = parse_defra_datetime(row.deadline_at.as_deref()).unwrap_or(now);
    let latency_ms = (now - started_at).num_milliseconds().max(0);
    let escaped_doc_id = escape_graphql_string(&row.doc_id);
    let result = escape_graphql_string("cancelled by Codex background terminal cleanup");
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{
                    _docID: {{ _eq: "{escaped_doc_id}" }},
                    lifecycle_state: {{ _eq: "running" }},
                    await_mode: {{ _eq: "background" }}
                }},
                input: {{
                    result: "{result}",
                    status: "completed",
                    lifecycle_state: "cancelled",
                    cancel_cause: "userCancelled",
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
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!(
            "cancelling background tool {} failed: {}",
            row.tool_call_key,
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(())
}

fn parse_defra_datetime(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}
