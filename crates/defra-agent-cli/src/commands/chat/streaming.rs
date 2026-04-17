use std::io::{self, Write};
use std::time::Duration;

use anyhow::Result;
use defra_agent::graphql::escape_graphql_string;
use serde_json::Value;

use crate::{post_graphql, request_diagnostic_hint};

use super::SubmittedRequest;

#[derive(Debug, Clone)]
pub(super) struct ChatTurnProgress {
    pub(super) content: String,
    pub(super) reasoning: String,
    pub(super) error_message: Option<String>,
    pub(super) progress_seq: u64,
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

pub(super) fn chat_progress_query(request_id: &str, session_id: &str) -> String {
    format!(
        r#"{{
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
                completed_at
            }}
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
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
    let mut latest_content = String::new();
    let mut latest_reasoning = String::new();
    let mut latest_progress_seq = 0;
    let mut latest_error_message: Option<String> = None;
    let mut thinking_printed = false;

    loop {
        let query = chat_progress_query(&submitted.request_id, &submitted.session_id);
        let response = post_graphql(graphql, &query).await?;

        let tool_rows = response
            .pointer("/data/AgentToolCall")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for tool in tool_rows
            .into_iter()
            .filter_map(|row| decode_tool_call_progress(&row))
        {
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
        if let Some(progress) = response_row
            .as_ref()
            .and_then(|row| decode_chat_turn_progress(row))
        {
            if progress.progress_seq > latest_progress_seq
                || progress.content != latest_content
                || progress.reasoning != latest_reasoning
                || progress.error_message != latest_error_message
            {
                last_progress_at = tokio::time::Instant::now();
            }
            if !thinking_printed
                && progress.status == "streaming"
                && progress.content.is_empty()
                && !progress.reasoning.trim().is_empty()
            {
                println!("[thinking]");
                io::stdout().flush()?;
                thinking_printed = true;
            }
            latest_progress_seq = progress.progress_seq;
            latest_error_message = progress.error_message.clone();
            latest_content = progress.content.clone();
            latest_reasoning = progress.reasoning.clone();

            if matches!(progress.status.as_str(), "complete" | "error") {
                if !progress.content.trim().is_empty() {
                    println!("{}", progress.content);
                    io::stdout().flush()?;
                }
                if progress.status == "error" {
                    if let Some(error_message) = progress
                        .error_message
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        if !progress.content.contains(error_message) {
                            println!("[agent error] {error_message}");
                            println!(
                                "[inspect] defra-agent show response {}",
                                submitted.request_id
                            );
                            io::stdout().flush()?;
                        }
                    } else {
                        println!(
                            "[inspect] defra-agent show response {}",
                            submitted.request_id
                        );
                        io::stdout().flush()?;
                    }
                }
                return Ok(response_row.unwrap_or(Value::Null));
            }
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
        content: row.get("content")?.as_str()?.to_string(),
        reasoning: row
            .get("reasoning")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        error_message: row
            .get("error_message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        progress_seq: row.get("progress_seq").and_then(Value::as_u64).unwrap_or(0),
        status: row.get("status")?.as_str()?.to_string(),
    })
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
