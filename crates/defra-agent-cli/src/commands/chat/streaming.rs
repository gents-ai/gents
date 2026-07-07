use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::time::Duration;

use anyhow::Result;
use defra_agent::graphql::escape_graphql_string;
use serde_json::Value;

use crate::{
    hydrate_materialized_response_content, is_terminal_lifecycle_state, post_graphql,
    request_diagnostic_hint,
};

use super::SubmittedRequest;

#[derive(Debug, Clone)]
pub(super) struct ChatTurnProgress {
    pub(super) error_message: Option<String>,
    pub(super) status: String,
}

#[derive(Debug, Clone)]
pub(super) struct ToolCallProgress {
    pub(super) tool_call_key: String,
    pub(super) tool_name: String,
    pub(super) status: String,
    pub(super) args: String,
    pub(super) result: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatProgressMarker {
    request_lifecycle_state: Option<String>,
    request_failure_len: Option<usize>,
    request_interrupt_requested_at: Option<String>,
    request_valid_until: Option<String>,
    response_status: Option<String>,
    response_content_len: Option<usize>,
    response_reasoning_fingerprint: Option<(usize, u64)>,
    response_error_len: Option<usize>,
    response_progress_seq: Option<String>,
    response_materialized_message_sequence: Option<String>,
    response_materialized_at: Option<String>,
    response_completed_at: Option<String>,
    tools: Vec<ChatToolProgressMarker>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChatToolProgressMarker {
    tool_call_key: Option<String>,
    status: Option<String>,
    args_len: Option<usize>,
    result_len: Option<usize>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

pub(super) fn chat_progress_query(request_id: &str, session_id: &str) -> String {
    format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                lifecycle_state
                failure_reason
                interrupt_requested_at
                valid_until
            }}
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                session_id
                status
                content
                reasoning
                error_message
                progress_seq
                materialized_message_sequence
                materialized_at
                completed_at
            }}
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    request_id: {{ _eq: "{request_id}" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                tool_call_key
                tool_name
                status
                args
                result
                started_at
                completed_at
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        session_id = escape_graphql_string(session_id),
    )
}

pub(super) async fn load_existing_tool_call_keys(
    graphql: &str,
    session_id: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }}
            ) {{
                tool_call_key
                status
            }}
        }}"#,
        session_id = escape_graphql_string(session_id),
    );
    let response = post_graphql(graphql, &query).await?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some((
                row.get("tool_call_key")?.as_str()?.to_string(),
                row.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect())
}

pub(super) async fn stream_turn_progress(
    graphql: &str,
    submitted: &SubmittedRequest,
    mut known_tool_calls: std::collections::BTreeMap<String, String>,
    timeout_secs: u64,
    poll_secs: u64,
) -> Result<Value> {
    let idle_timeout = Duration::from_secs(timeout_secs);
    let mut last_progress_at = tokio::time::Instant::now();
    let mut latest_progress_marker: Option<ChatProgressMarker> = None;
    let mut thinking_printed = false;

    loop {
        let query = chat_progress_query(&submitted.request_id, &submitted.session_id);
        let response = post_graphql(graphql, &query).await?;
        let request_row = response
            .pointer("/data/AgentRequest")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned();

        let tool_rows = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for tool in tool_rows.iter().filter_map(decode_tool_call_progress) {
            let previous_status = known_tool_calls.get(&tool.tool_call_key).cloned();
            if previous_status.as_deref() == Some(tool.status.as_str()) {
                continue;
            }
            known_tool_calls.insert(tool.tool_call_key.clone(), tool.status.clone());
            last_progress_at = tokio::time::Instant::now();
            if previous_status.is_none() && matches!(tool.status.as_str(), "completed" | "error") {
                println!(
                    "[tool] {} {}",
                    tool.tool_name,
                    format_tool_args_preview(&tool.args)
                );
            }
            println!("{}", format_tool_progress_line(&tool));
            io::stdout().flush()?;
        }

        let response_row = response
            .pointer("/data/AgentResponse")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned();
        let marker = chat_progress_marker(request_row.as_ref(), response_row.as_ref(), &tool_rows);
        if latest_progress_marker.as_ref() != Some(&marker) {
            latest_progress_marker = Some(marker);
            last_progress_at = tokio::time::Instant::now();
        }
        let progress = response_row.as_ref().and_then(decode_chat_turn_progress);
        if let Some(progress) = progress.as_ref() {
            if !thinking_printed
                && progress.status == "streaming"
                && response_row
                    .as_ref()
                    .is_some_and(response_has_reasoning_without_content)
            {
                println!("[thinking]");
                io::stdout().flush()?;
                thinking_printed = true;
            }
        }

        let lifecycle_state = request_row
            .as_ref()
            .and_then(|row| row.get("lifecycle_state"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let failure_reason = request_row
            .as_ref()
            .and_then(|row| row.get("failure_reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let response_status = progress
            .as_ref()
            .map(|progress| progress.status.as_str())
            .unwrap_or("");
        let terminal_by_request = is_terminal_lifecycle_state(lifecycle_state);
        let terminal_by_response = matches!(response_status, "complete" | "completed" | "error");

        if terminal_by_request || terminal_by_response {
            let had_response_row = response_row.is_some();
            let mut terminal_response = response_row.unwrap_or_else(|| {
                serde_json::json!({
                    "request_id": submitted.request_id,
                    "session_id": submitted.session_id,
                    "status": null,
                    "content": null,
                    "error_message": failure_reason,
                })
            });
            let should_wait_for_materialized_content =
                matches!(response_status, "complete" | "completed")
                    && terminal_response
                        .get("content")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .unwrap_or_default()
                        .is_empty()
                    && terminal_response
                        .get("materialized_message_sequence")
                        .is_some_and(|value| !value.is_null());
            let hydrated = if had_response_row {
                hydrate_materialized_response_content(graphql, &mut terminal_response).await?
            } else {
                true
            };
            if should_wait_for_materialized_content && !hydrated {
                if last_progress_at.elapsed() >= idle_timeout {
                    anyhow::bail!(
                        "timed out waiting for materialized AgentMessage {} after {}s of inactivity\n{}",
                        submitted.request_id,
                        timeout_secs,
                        request_diagnostic_hint(&submitted.request_id)
                    );
                }
                tokio::time::sleep(Duration::from_secs(poll_secs)).await;
                continue;
            }

            let terminal_content = terminal_response
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !terminal_content.trim().is_empty() {
                println!("{}", terminal_content);
                io::stdout().flush()?;
            }

            let error_message = progress
                .as_ref()
                .and_then(|progress| progress.error_message.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    matches!(lifecycle_state, "failed" | "dead")
                        .then_some(failure_reason.trim())
                        .filter(|value| !value.is_empty())
                });
            if let Some(error_message) = error_message {
                if !terminal_content.contains(error_message) {
                    println!("[agent error] {error_message}");
                    println!(
                        "[inspect] defra-agent response show {}",
                        submitted.request_id
                    );
                    io::stdout().flush()?;
                }
            } else if response_status == "error" || matches!(lifecycle_state, "failed" | "dead") {
                println!(
                    "[inspect] defra-agent response show {}",
                    submitted.request_id
                );
                io::stdout().flush()?;
            }

            if let Some(object) = terminal_response.as_object_mut() {
                object.insert(
                    "request".to_string(),
                    request_row.unwrap_or(serde_json::Value::Null),
                );
            }
            return Ok(terminal_response);
        }

        if last_progress_at.elapsed() >= idle_timeout {
            anyhow::bail!(
                "timed out waiting for AgentResponse {} after {}s of inactivity\n{}",
                submitted.request_id,
                timeout_secs,
                request_diagnostic_hint(&submitted.request_id)
            );
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}

pub(super) fn decode_chat_turn_progress(row: &Value) -> Option<ChatTurnProgress> {
    Some(ChatTurnProgress {
        error_message: row
            .get("error_message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        status: row.get("status")?.as_str()?.to_string(),
    })
}

fn chat_progress_marker(
    request_row: Option<&Value>,
    response_row: Option<&Value>,
    tool_rows: &[Value],
) -> ChatProgressMarker {
    ChatProgressMarker {
        request_lifecycle_state: scalar_marker(request_row, "lifecycle_state"),
        request_failure_len: string_len_marker(request_row, "failure_reason"),
        request_interrupt_requested_at: scalar_marker(request_row, "interrupt_requested_at"),
        request_valid_until: scalar_marker(request_row, "valid_until"),
        response_status: scalar_marker(response_row, "status"),
        response_content_len: string_len_marker(response_row, "content"),
        response_reasoning_fingerprint: string_fingerprint_marker(response_row, "reasoning"),
        response_error_len: string_len_marker(response_row, "error_message"),
        response_progress_seq: scalar_marker(response_row, "progress_seq"),
        response_materialized_message_sequence: scalar_marker(
            response_row,
            "materialized_message_sequence",
        ),
        response_materialized_at: scalar_marker(response_row, "materialized_at"),
        response_completed_at: scalar_marker(response_row, "completed_at"),
        tools: tool_rows.iter().map(chat_tool_progress_marker).collect(),
    }
}

fn chat_tool_progress_marker(row: &Value) -> ChatToolProgressMarker {
    ChatToolProgressMarker {
        tool_call_key: scalar_marker(Some(row), "tool_call_key"),
        status: scalar_marker(Some(row), "status"),
        args_len: string_len_marker(Some(row), "args"),
        result_len: string_len_marker(Some(row), "result"),
        started_at: scalar_marker(Some(row), "started_at"),
        completed_at: scalar_marker(Some(row), "completed_at"),
    }
}

fn response_has_reasoning_without_content(row: &Value) -> bool {
    row.get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .is_empty()
        && !row
            .get("reasoning")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .is_empty()
}

fn scalar_marker(row: Option<&Value>, field: &str) -> Option<String> {
    let value = row?.get(field)?;
    if value.is_null() {
        return None;
    }
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_i64().map(|value| value.to_string()))
        .or_else(|| value.as_u64().map(|value| value.to_string()))
        .or_else(|| value.as_bool().map(|value| value.to_string()))
}

fn string_len_marker(row: Option<&Value>, field: &str) -> Option<usize> {
    row?.get(field)?.as_str().map(str::len)
}

fn string_fingerprint_marker(row: Option<&Value>, field: &str) -> Option<(usize, u64)> {
    let value = row?.get(field)?.as_str()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    Some((value.len(), hasher.finish()))
}

pub(super) fn decode_tool_call_progress(row: &Value) -> Option<ToolCallProgress> {
    Some(ToolCallProgress {
        tool_call_key: row.get("tool_call_key")?.as_str()?.to_string(),
        tool_name: row.get("tool_name")?.as_str()?.to_string(),
        status: row.get("status")?.as_str()?.to_string(),
        args: row
            .get("args")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        result: row
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

pub(super) fn format_tool_progress_line(tool: &ToolCallProgress) -> String {
    match tool.status.as_str() {
        "completed" => match preview_compact_text(&tool.result) {
            Some(result) => format!(
                "[tool done] {} {} => {}",
                tool.tool_name,
                format_tool_args_preview(&tool.args),
                result
            ),
            None => format!(
                "[tool done] {} {}",
                tool.tool_name,
                format_tool_args_preview(&tool.args)
            ),
        },
        "error" => format!(
            "[tool error] {} {} => {}",
            tool.tool_name,
            format_tool_args_preview(&tool.args),
            preview_compact_text(&tool.result).unwrap_or_else(|| "-".to_string())
        ),
        _ => format!(
            "[tool] {} {}",
            tool.tool_name,
            format_tool_args_preview(&tool.args)
        ),
    }
}

pub(super) fn format_tool_args_preview(value: &str) -> String {
    preview_compact_text(value)
        .map(|preview| format!("({preview})"))
        .unwrap_or_default()
}

pub(super) fn preview_compact_text(value: &str) -> Option<String> {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = compact.trim();
    if trimmed.is_empty() {
        return None;
    }
    let preview = if trimmed.chars().count() > 120 {
        format!("{}...", trimmed.chars().take(120).collect::<String>())
    } else {
        trimmed.to_string()
    };
    Some(preview)
}
