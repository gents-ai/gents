use codex_app_server_protocol as codex;
use defra_agent::graphql::escape_graphql_string;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub(super) struct DefraTurnProgress {
    pub(super) content: String,
    pub(super) reasoning: String,
    pub(super) error_message: Option<String>,
    pub(super) status: String,
}

#[derive(Debug, Clone)]
pub(super) struct DefraToolCallProgress {
    pub(super) tool_call_key: String,
    pub(super) tool_name: String,
    pub(super) status: String,
    pub(super) args: String,
    pub(super) result: String,
}

pub(super) fn defra_turn_progress_query(request_id: &str, session_id: &str) -> String {
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
                interrupted_at
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

pub(super) fn decode_defra_turn_progress(row: &Value) -> Option<DefraTurnProgress> {
    Some(DefraTurnProgress {
        content: row
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        reasoning: row
            .get("reasoning")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        error_message: row
            .get("error_message")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned),
        status: row.get("status")?.as_str()?.to_string(),
    })
}

pub(super) fn decode_defra_tool_call_progress(row: &Value) -> Option<DefraToolCallProgress> {
    Some(DefraToolCallProgress {
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

pub(super) fn defra_tool_item(
    tool: &DefraToolCallProgress,
    status: codex::McpToolCallStatus,
) -> codex::ThreadItem {
    let (result, error) = match status {
        codex::McpToolCallStatus::Completed => (
            Some(Box::new(codex::McpToolCallResult {
                content: defra_tool_result_content(&tool.result),
                structured_content: parse_json_value(&tool.result),
                meta: None,
            })),
            None,
        ),
        codex::McpToolCallStatus::Failed => (
            None,
            Some(codex::McpToolCallError {
                message: preview_compact_text(&tool.result)
                    .unwrap_or_else(|| "DEFRA tool call failed".to_string()),
            }),
        ),
        codex::McpToolCallStatus::InProgress => (None, None),
    };

    codex::ThreadItem::McpToolCall {
        id: tool.tool_call_key.clone(),
        server: "defra".to_string(),
        tool: tool.tool_name.clone(),
        status,
        arguments: parse_json_value(&tool.args).unwrap_or_else(|| json!({})),
        mcp_app_resource_uri: None,
        plugin_id: None,
        result,
        error,
        duration_ms: None,
    }
}

pub(super) fn defra_tool_call_status(tool: &DefraToolCallProgress) -> codex::McpToolCallStatus {
    let status = tool.status.trim().to_ascii_lowercase();
    if matches!(status.as_str(), "error" | "failed" | "failure" | "dead")
        || tool_result_looks_error(&tool.result)
    {
        return codex::McpToolCallStatus::Failed;
    }
    if matches!(
        status.as_str(),
        "completed" | "complete" | "success" | "succeeded"
    ) {
        return codex::McpToolCallStatus::Completed;
    }
    codex::McpToolCallStatus::InProgress
}

pub(super) fn content_delta(previous: &str, current: &str) -> String {
    if current.is_empty() || previous == current {
        return String::new();
    }
    if let Some(delta) = current.strip_prefix(previous) {
        return delta.to_string();
    }
    let previous_trimmed_start = previous.trim_start();
    let current_trimmed_start = current.trim_start();
    if previous_trimmed_start == current_trimmed_start {
        return String::new();
    }
    if let Some(delta) = current_trimmed_start.strip_prefix(previous_trimmed_start) {
        return delta.to_string();
    }
    if previous.trim() == current.trim() {
        return String::new();
    }
    if previous.is_empty() {
        current.to_string()
    } else {
        format!("\n{current}")
    }
}

pub(super) fn response_field_is_blank(response: &Value, field: &str) -> bool {
    response
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .is_empty()
}

pub(super) fn terminal_turn_status(
    lifecycle_state: &str,
    response_status: &str,
) -> codex::TurnStatus {
    match (lifecycle_state, response_status) {
        ("interrupted" | "superseded", _) => codex::TurnStatus::Interrupted,
        ("failed" | "dead", _) | (_, "error") => codex::TurnStatus::Failed,
        _ => codex::TurnStatus::Completed,
    }
}

pub(super) fn terminal_error_message(
    response_status: &str,
    response_error: Option<&str>,
    lifecycle_state: &str,
    failure_reason: &str,
) -> Option<String> {
    if let Some(error) = response_error
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(error.to_string());
    }
    if response_status == "error" {
        return Some("DEFRA response ended with status error".to_string());
    }
    if matches!(lifecycle_state, "failed" | "dead") {
        return Some(
            failure_reason
                .trim()
                .is_empty()
                .then(|| format!("DEFRA request ended with lifecycle_state {lifecycle_state}"))
                .unwrap_or_else(|| failure_reason.trim().to_string()),
        );
    }
    None
}

fn defra_tool_result_content(result: &str) -> Vec<Value> {
    preview_compact_text(result)
        .map(|text| vec![json!({ "type": "text", "text": text })])
        .unwrap_or_default()
}

fn tool_result_looks_error(result: &str) -> bool {
    let trimmed = result.trim_start();
    trimmed.starts_with("Toolset error:") || trimmed.starts_with("JsonError:")
}

fn parse_json_value(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed)
        .ok()
        .or_else(|| Some(Value::String(trimmed.to_string())))
}

fn preview_compact_text(value: &str) -> Option<String> {
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
