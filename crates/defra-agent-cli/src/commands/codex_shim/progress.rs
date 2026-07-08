use codex_app_server_protocol as codex;
use defra_agent::graphql::escape_graphql_string;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub(super) struct DefraToolCallProgress {
    pub(super) tool_call_key: String,
    pub(super) tool_name: String,
    pub(super) status: String,
    pub(super) lifecycle_state: Option<String>,
    pub(super) await_mode: Option<String>,
    pub(super) child_request_id: Option<String>,
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
                _docID
                request_id
                session_id
                status
                content
                reasoning
                error_message
                token_count
                progress_seq
                reasoning_progress_seq
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
                lifecycle_state
                await_mode
                child_request_id
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

pub(super) fn defra_tool_progress_query(request_id: &str, session_id: &str) -> String {
    format!(
        r#"{{
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
                lifecycle_state
                await_mode
                child_request_id
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

pub(super) fn decode_defra_tool_call_progress(row: &Value) -> Option<DefraToolCallProgress> {
    Some(DefraToolCallProgress {
        tool_call_key: row.get("tool_call_key")?.as_str()?.to_string(),
        tool_name: row.get("tool_name")?.as_str()?.to_string(),
        status: row.get("status")?.as_str()?.to_string(),
        lifecycle_state: row
            .get("lifecycle_state")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        await_mode: row
            .get("await_mode")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        child_request_id: row
            .get("child_request_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned),
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
    let status = tool
        .lifecycle_state
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&tool.status)
        .trim()
        .to_ascii_lowercase();
    if matches!(
        status.as_str(),
        "cancelled" | "dead" | "error" | "failed" | "failure" | "timedout"
    ) || tool_result_looks_error(&tool.result)
        || defra_exec_result_failed(&tool.result)
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
        ("failed" | "dead", _) => codex::TurnStatus::Failed,
        ("completed", _) => codex::TurnStatus::Completed,
        (_, "error") => codex::TurnStatus::Failed,
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

fn defra_exec_result_failed(result: &str) -> bool {
    let Some(metadata) = defra_exec_metadata(result) else {
        return false;
    };
    metadata.get("ok").and_then(Value::as_bool) == Some(false)
        || metadata
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "success")
}

pub(super) fn defra_exec_metadata(result: &str) -> Option<Value> {
    let first_line = result.lines().next()?.trim();
    let raw = first_line.strip_prefix("defra_exec:")?.trim();
    serde_json::from_str(raw).ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defra_tool_errors_render_as_failed_codex_tool_calls() {
        let tool = test_tool("glob", "completed", r#"{"pattern":"**/*.lean"}"#)
            .with_result("Toolset error: missing runner");

        assert_eq!(
            defra_tool_call_status(&tool),
            codex::McpToolCallStatus::Failed
        );
        let item = defra_tool_item(&tool, codex::McpToolCallStatus::Failed);
        let codex::ThreadItem::McpToolCall {
            server,
            tool: tool_name,
            arguments,
            status,
            error,
            ..
        } = item
        else {
            panic!("expected MCP tool call item");
        };
        assert_eq!(server, "defra");
        assert_eq!(tool_name, "glob");
        assert_eq!(arguments["pattern"], "**/*.lean");
        assert_eq!(status, codex::McpToolCallStatus::Failed);
        assert_eq!(
            error.expect("failed tool should carry error").message,
            "Toolset error: missing runner"
        );
    }

    #[test]
    fn content_delta_ignores_terminal_leading_whitespace_normalization() {
        assert_eq!(
            content_delta("\n\nAnswer with context", "Answer with context"),
            ""
        );
        assert_eq!(
            content_delta("\n\nAnswer", "Answer with context"),
            " with context"
        );
    }

    #[test]
    fn terminal_turn_status_matches_codex_projection_request_precedence() {
        assert_eq!(
            terminal_turn_status("completed", "error"),
            codex::TurnStatus::Completed
        );
        assert_eq!(
            terminal_turn_status("processing", "error"),
            codex::TurnStatus::Failed
        );
        assert_eq!(
            terminal_turn_status("failed", "complete"),
            codex::TurnStatus::Failed
        );
        assert_eq!(
            terminal_turn_status("superseded", "error"),
            codex::TurnStatus::Interrupted
        );
    }

    fn test_tool(tool_name: &str, status: &str, args: &str) -> DefraToolCallProgress {
        DefraToolCallProgress {
            tool_call_key: "session:call".to_string(),
            tool_name: tool_name.to_string(),
            status: status.to_string(),
            lifecycle_state: Some(status.to_string()),
            await_mode: None,
            child_request_id: None,
            args: args.to_string(),
            result: String::new(),
        }
    }

    trait ToolTestExt {
        fn with_result(self, result: &str) -> Self;
    }

    impl ToolTestExt for DefraToolCallProgress {
        fn with_result(mut self, result: &str) -> Self {
            self.result = result.to_string();
            self
        }
    }
}
