use codex_app_server_protocol as codex;
use gents::graphql::escape_graphql_string;
use serde_json::{json, Value};

use super::subagent_projection::LinkedSubagentThread;

#[derive(Debug, Clone, Default)]
pub(super) struct GentsToolCallProgress {
    pub(super) tool_call_key: String,
    pub(super) tool_name: String,
    pub(super) status: String,
    pub(super) lifecycle_state: Option<String>,
    pub(super) await_mode: Option<String>,
    pub(super) child_request_id: Option<String>,
    pub(super) args: String,
    pub(super) result: String,
    pub(super) selected_service_id: Option<String>,
    pub(super) selected_tool_name: Option<String>,
    pub(super) tool_failure_class: Option<String>,
    pub(super) denial_reason: Option<String>,
    pub(super) cancel_cause: Option<String>,
    pub(super) latency_ms: Option<i64>,
    pub(super) started_at: Option<String>,
    pub(super) completed_at: Option<String>,
    pub(super) subagent_link: Option<LinkedSubagentThread>,
}

pub(super) fn gents_turn_progress_query(request_id: &str, session_id: &str) -> String {
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
                created_at
                terminalized_at
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
                created_at
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
                selected_service_id
                selected_tool_name
                tool_failure_class
                denial_reason
                cancel_cause
                latency_ms
            }}
            InferenceCall(
                filter: {{
                    request_id: {{ _eq: "{request_id}" }},
                    call_kind: {{ _in: ["inference", "compaction"] }}
                }},
                order: {{ call_seq: ASC }}
            ) {{
                call_id
                call_seq
                call_kind
                call_state
                queued_at
                started_at
                ended_at
                prompt_tokens
                completion_tokens
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        session_id = escape_graphql_string(session_id),
    )
}

pub(super) fn gents_tool_progress_query(request_id: &str, session_id: &str) -> String {
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
                selected_service_id
                selected_tool_name
                tool_failure_class
                denial_reason
                cancel_cause
                latency_ms
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        session_id = escape_graphql_string(session_id),
    )
}

pub(super) fn decode_gents_tool_call_progress(row: &Value) -> Option<GentsToolCallProgress> {
    Some(GentsToolCallProgress {
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
        selected_service_id: optional_nonempty_string(row, "selected_service_id"),
        selected_tool_name: optional_nonempty_string(row, "selected_tool_name"),
        tool_failure_class: optional_nonempty_string(row, "tool_failure_class"),
        denial_reason: optional_nonempty_string(row, "denial_reason"),
        cancel_cause: optional_nonempty_string(row, "cancel_cause"),
        latency_ms: row.get("latency_ms").and_then(json_i64),
        started_at: optional_nonempty_string(row, "started_at"),
        completed_at: optional_nonempty_string(row, "completed_at"),
        subagent_link: None,
    })
}

pub(super) fn gents_tool_item(
    tool: &GentsToolCallProgress,
    status: codex::McpToolCallStatus,
) -> codex::ThreadItem {
    let (result, error) = match status {
        codex::McpToolCallStatus::Completed => (
            Some(Box::new(codex::McpToolCallResult {
                content: gents_tool_result_content(&tool.result),
                structured_content: parse_json_value(&tool.result),
                meta: None,
            })),
            None,
        ),
        codex::McpToolCallStatus::Failed => (
            None,
            Some(codex::McpToolCallError {
                message: tool_failure_message(tool),
            }),
        ),
        codex::McpToolCallStatus::InProgress => (None, None),
    };

    codex::ThreadItem::McpToolCall {
        id: tool.tool_call_key.clone(),
        server: selected_tool_identity(tool.selected_service_id.as_deref(), "gents"),
        tool: selected_tool_identity(tool.selected_tool_name.as_deref(), &tool.tool_name),
        status,
        arguments: parse_json_value(&tool.args).unwrap_or_else(|| json!({})),
        mcp_app_resource_uri: None,
        plugin_id: None,
        result,
        error,
        duration_ms: tool_duration_ms(tool),
    }
}

pub(super) fn tool_duration_ms(tool: &GentsToolCallProgress) -> Option<i64> {
    tool.latency_ms.filter(|latency| *latency >= 0).or_else(|| {
        let started = tool.started_at.as_deref().and_then(timestamp_millis)?;
        let completed = tool.completed_at.as_deref().and_then(timestamp_millis)?;
        Some(completed.saturating_sub(started).max(0))
    })
}

pub(super) fn tool_started_at_ms(tool: &GentsToolCallProgress) -> Option<i64> {
    tool.started_at.as_deref().and_then(timestamp_millis)
}

pub(super) fn tool_completed_at_ms(tool: &GentsToolCallProgress) -> Option<i64> {
    tool.completed_at.as_deref().and_then(timestamp_millis)
}

fn selected_tool_identity(selected: Option<&str>, fallback: &str) -> String {
    selected
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn tool_failure_message(tool: &GentsToolCallProgress) -> String {
    [tool.denial_reason.as_deref(), tool.cancel_cause.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| preview_compact_text(&tool.result))
        .or_else(|| {
            tool.tool_failure_class
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "GENTS tool call failed".to_string())
}

fn optional_nonempty_string(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

pub(super) fn timestamp_millis(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

pub(super) fn gents_tool_call_status(tool: &GentsToolCallProgress) -> codex::McpToolCallStatus {
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
        || gents_exec_result_failed(&tool.result)
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
        return Some("GENTS response ended with status error".to_string());
    }
    if matches!(lifecycle_state, "failed" | "dead") {
        return Some(
            failure_reason
                .trim()
                .is_empty()
                .then(|| format!("GENTS request ended with lifecycle_state {lifecycle_state}"))
                .unwrap_or_else(|| failure_reason.trim().to_string()),
        );
    }
    None
}

fn gents_tool_result_content(result: &str) -> Vec<Value> {
    preview_compact_text(result)
        .map(|text| vec![json!({ "type": "text", "text": text })])
        .unwrap_or_default()
}

fn tool_result_looks_error(result: &str) -> bool {
    let trimmed = result.trim_start();
    trimmed.starts_with("Toolset error:") || trimmed.starts_with("JsonError:")
}

fn gents_exec_result_failed(result: &str) -> bool {
    let Some(metadata) = gents_exec_metadata(result) else {
        return false;
    };
    metadata.get("ok").and_then(Value::as_bool) == Some(false)
        || metadata
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "success")
}

pub(super) fn gents_exec_metadata(result: &str) -> Option<Value> {
    let first_line = result.lines().next()?.trim();
    let raw = first_line.strip_prefix("gents_exec:")?.trim();
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
    fn gents_tool_errors_render_as_failed_codex_tool_calls() {
        let tool = test_tool("glob", "completed", r#"{"pattern":"**/*.lean"}"#)
            .with_result("Toolset error: missing runner");

        assert_eq!(
            gents_tool_call_status(&tool),
            codex::McpToolCallStatus::Failed
        );
        let item = gents_tool_item(&tool, codex::McpToolCallStatus::Failed);
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
        assert_eq!(server, "gents");
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

    #[test]
    fn turn_progress_query_observes_compaction_lifecycle() {
        let query = gents_turn_progress_query("request-1", "session-1");
        assert!(query.contains("InferenceCall("));
        assert!(query.contains(r#"call_kind: { _in: ["inference", "compaction"] }"#));
        assert!(query.contains("call_state"));
        assert!(query.contains("ended_at"));
    }

    #[test]
    fn tool_projection_prefers_runtime_identity_failure_and_latency() {
        let mut tool = test_tool("configured_search", "failed", r#"{"query":"GENTS"}"#)
            .with_result("provider returned a generic failure");
        tool.selected_service_id = Some("search-service".to_string());
        tool.selected_tool_name = Some("search".to_string());
        tool.tool_failure_class = Some("provider_error".to_string());
        tool.cancel_cause = Some("operator cancelled".to_string());
        tool.denial_reason = Some("policy denied search".to_string());
        tool.latency_ms = Some(17);
        tool.started_at = Some("2026-07-15T10:00:00Z".to_string());
        tool.completed_at = Some("2026-07-15T10:00:01Z".to_string());

        let item = gents_tool_item(&tool, codex::McpToolCallStatus::Failed);
        let codex::ThreadItem::McpToolCall {
            server,
            tool,
            error,
            duration_ms,
            ..
        } = item
        else {
            panic!("expected MCP tool call item");
        };
        assert_eq!(server, "search-service");
        assert_eq!(tool, "search");
        assert_eq!(
            error.expect("failed tool should carry diagnostics").message,
            "policy denied search"
        );
        assert_eq!(duration_ms, Some(17));
    }

    #[test]
    fn tool_projection_keeps_result_diagnostic_ahead_of_failure_class() {
        let mut tool =
            test_tool("search", "failed", "{}").with_result("connection refused to search service");
        tool.tool_failure_class = Some("external".to_string());

        let item = gents_tool_item(&tool, codex::McpToolCallStatus::Failed);
        let codex::ThreadItem::McpToolCall { error, .. } = item else {
            panic!("expected MCP tool call item");
        };
        assert_eq!(
            error.expect("failed tool should carry diagnostics").message,
            "connection refused to search service"
        );

        tool.result.clear();
        let item = gents_tool_item(&tool, codex::McpToolCallStatus::Failed);
        let codex::ThreadItem::McpToolCall { error, .. } = item else {
            panic!("expected MCP tool call item");
        };
        assert_eq!(
            error
                .expect("failure class should remain a fallback")
                .message,
            "external"
        );
    }

    #[test]
    fn tool_duration_falls_back_to_persisted_timestamps() {
        let mut tool = test_tool("search", "completed", "{}");
        tool.started_at = Some("2026-07-15T10:00:00.100Z".to_string());
        tool.completed_at = Some("2026-07-15T10:00:00.125Z".to_string());
        assert_eq!(tool_duration_ms(&tool), Some(25));
        assert_eq!(
            tool_completed_at_ms(&tool).unwrap() - tool_started_at_ms(&tool).unwrap(),
            25
        );

        tool.completed_at = None;
        assert_eq!(tool_duration_ms(&tool), None);
    }

    #[test]
    fn tool_queries_hydrate_runtime_presentation_metadata() {
        for query in [
            gents_turn_progress_query("request-1", "session-1"),
            gents_tool_progress_query("request-1", "session-1"),
        ] {
            for field in [
                "selected_service_id",
                "selected_tool_name",
                "tool_failure_class",
                "denial_reason",
                "cancel_cause",
                "latency_ms",
                "started_at",
                "completed_at",
            ] {
                assert!(query.contains(field), "query must load {field}: {query}");
            }
        }
    }

    fn test_tool(tool_name: &str, status: &str, args: &str) -> GentsToolCallProgress {
        GentsToolCallProgress {
            tool_call_key: "session:call".to_string(),
            tool_name: tool_name.to_string(),
            status: status.to_string(),
            lifecycle_state: Some(status.to_string()),
            await_mode: None,
            child_request_id: None,
            args: args.to_string(),
            result: String::new(),
            subagent_link: None,
            ..Default::default()
        }
    }

    trait ToolTestExt {
        fn with_result(self, result: &str) -> Self;
    }

    impl ToolTestExt for GentsToolCallProgress {
        fn with_result(mut self, result: &str) -> Self {
            self.result = result.to_string();
            self
        }
    }
}
