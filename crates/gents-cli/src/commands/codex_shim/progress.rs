use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::ToolCallState;
use gents_codex_protocol as codex;
use gents_protocol::client_protocol::{ClientTurnState, RequestLifecycleState};
use serde_json::{json, Value};

use super::projection_state::ProjectionStatus;

#[derive(Debug, Clone, Default)]
pub(super) struct GentsToolCallProgress {
    pub(super) doc_id: Option<String>,
    pub(super) node_did: Option<String>,
    pub(super) requester_did: Option<String>,
    pub(super) request_doc_id: Option<String>,
    pub(super) tool_call_key: String,
    pub(super) tool_name: String,
    pub(super) lifecycle_state: Option<String>,
    pub(super) await_mode: Option<String>,
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
}

/// No `order` on the `_docID` read: DefraDB applies `limit` before the `_docID` filter.
pub(super) fn gents_turn_progress_query(request_doc_id: &str, session_id: &str) -> String {
    format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{request_doc_id}" }} }},
                limit: 2
            ) {{
                _docID
                node_did
                requester_did
                session_id
                request_id
                lifecycle_state
                execution_generation
                execution_lease_secs
                execution_lease_expires_at
                failure_reason
                created_at
                terminalized_at
                terminal_output
                interrupt_requested_at
                valid_until
            }}
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    request_doc_id: {{ _eq: "{request_doc_id}" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                _docID
                node_did
                requester_did
                session_id
                request_doc_id
                tool_call_key
                tool_name
                lifecycle_state
                await_mode
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
                    request_doc_id: {{ _eq: "{request_doc_id}" }},
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
        request_doc_id = escape_graphql_string(request_doc_id),
        session_id = escape_graphql_string(session_id),
    )
}

pub(super) fn gents_tool_progress_query(request_doc_id: &str, session_id: &str) -> String {
    format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    request_doc_id: {{ _eq: "{request_doc_id}" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                _docID
                node_did
                requester_did
                session_id
                request_doc_id
                tool_call_key
                tool_name
                lifecycle_state
                await_mode
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
        request_doc_id = escape_graphql_string(request_doc_id),
        session_id = escape_graphql_string(session_id),
    )
}

pub(super) fn decode_gents_tool_call_progress(row: &Value) -> Option<GentsToolCallProgress> {
    Some(GentsToolCallProgress {
        doc_id: optional_nonempty_string(row, "_docID"),
        node_did: optional_nonempty_string(row, "node_did"),
        requester_did: optional_nonempty_string(row, "requester_did"),
        request_doc_id: optional_nonempty_string(row, "request_doc_id"),
        tool_call_key: row.get("tool_call_key")?.as_str()?.to_string(),
        tool_name: row.get("tool_name")?.as_str()?.to_string(),
        lifecycle_state: row
            .get("lifecycle_state")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        await_mode: row
            .get("await_mode")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        args: String::new(),
        result: String::new(),
        selected_service_id: optional_nonempty_string(row, "selected_service_id"),
        selected_tool_name: optional_nonempty_string(row, "selected_tool_name"),
        tool_failure_class: optional_nonempty_string(row, "tool_failure_class"),
        denial_reason: optional_nonempty_string(row, "denial_reason"),
        cancel_cause: optional_nonempty_string(row, "cancel_cause"),
        latency_ms: row.get("latency_ms").and_then(json_i64),
        started_at: optional_nonempty_string(row, "started_at"),
        completed_at: optional_nonempty_string(row, "completed_at"),
    })
}

pub(super) async fn hydrate_gents_tool_call_progress(
    access: &ConfigAccess,
    row: &Value,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
) -> Result<Option<GentsToolCallProgress>> {
    let mut tool = decode_gents_tool_call_progress(row)
        .context("decoding exact AgentToolCall progress row")?;
    anyhow::ensure!(
        tool.node_did.as_deref() == Some(node_did)
            && tool.requester_did.as_deref() == requester_did
            && row.get("session_id").and_then(Value::as_str) == Some(session_id)
            && row.get("request_doc_id").and_then(Value::as_str) == Some(request_doc_id),
        "Codex tool projection crossed exact request/session scope"
    );
    let doc_id = tool
        .doc_id
        .as_deref()
        .context("AgentToolCall omitted _docID")?;
    let presentation = gents::tool_call_lifecycle::load_tool_call_presentation(
        access,
        doc_id,
        node_did,
        session_id,
        requester_did,
    )
    .await
    .with_context(|| format!("reconstructing canonical tool {doc_id}"))?;
    tool.args = presentation.arguments;
    let terminal = observed_tool_status(&tool) != ProjectionStatus::InProgress;
    match presentation.result {
        Some(result) => tool.result = result,
        None if terminal => return Ok(None),
        None => {}
    }
    Ok(Some(tool))
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

pub(super) fn observed_tool_status(tool: &GentsToolCallProgress) -> ProjectionStatus {
    let lifecycle_state = tool
        .lifecycle_state
        .as_deref()
        .map(str::trim)
        .and_then(ToolCallState::from_persisted);
    match lifecycle_state {
        Some(ToolCallState::Completed) => ProjectionStatus::Completed,
        Some(ToolCallState::Failed | ToolCallState::TimedOut | ToolCallState::Cancelled) => {
            ProjectionStatus::Failed
        }
        Some(ToolCallState::Pending | ToolCallState::Running) => ProjectionStatus::InProgress,
        None => ProjectionStatus::Failed,
    }
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

pub(super) fn codex_turn_status(state: ClientTurnState) -> codex::TurnStatus {
    match state {
        ClientTurnState::WaitingForClaim | ClientTurnState::Running => {
            codex::TurnStatus::InProgress
        }
        ClientTurnState::Completed => codex::TurnStatus::Completed,
        ClientTurnState::Failed => codex::TurnStatus::Failed,
        ClientTurnState::Superseded | ClientTurnState::Interrupted => {
            codex::TurnStatus::Interrupted
        }
    }
}

pub(super) fn terminal_error_message(
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
    if matches!(
        RequestLifecycleState::parse_opt(Some(lifecycle_state)),
        Some(RequestLifecycleState::Failed | RequestLifecycleState::Dead)
    ) {
        return Some(if failure_reason.trim().is_empty() {
            format!("GENTS request ended with lifecycle_state {lifecycle_state}")
        } else {
            failure_reason.trim().to_string()
        });
    }
    None
}

fn gents_tool_result_content(result: &str) -> Vec<Value> {
    preview_compact_text(result)
        .map(|text| vec![json!({ "type": "text", "text": text })])
        .unwrap_or_default()
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

    #[tokio::test]
    async fn turn_progress_finds_request_older_than_later_requests() {
        let temp = tempfile::tempdir().expect("progress directory");
        let state =
            super::super::turn_projection::tests::notification_test_state(temp.path()).await;
        let node_did = escape_graphql_string(state.node_did.as_ref());
        let agent_id = escape_graphql_string(state.agent_id.as_ref());
        for (id, purpose, created_at) in [
            ("turn", "normal", "2026-09-25T00:00:00Z"),
            ("title", "title-audit", "2026-09-25T00:00:01Z"),
            ("steer", "normal", "2026-09-25T00:00:02Z"),
        ] {
            let mutation = format!(
                r#"mutation {{ create_AgentRequest(input: {{request_id: "{id}", purpose: "{purpose}", session_id: "thread", node_did: "{node_did}", requester_did: "{node_did}", agent_id: "{agent_id}", content: "prompt", lifecycle_state: "processing", created_at: "{created_at}"}}) {{_docID}} }}"#,
                id = escape_graphql_string(id),
                purpose = escape_graphql_string(purpose),
                created_at = escape_graphql_string(created_at),
            );
            ConfigAccess::write_local(&state.node, "codex.progress.test_request", &mutation)
                .await
                .expect("create request");
        }
        let turn = super::super::store::query_node_json(
            &state.node,
            r#"{ AgentRequest(filter: { request_id: {_eq: "turn"} }) {_docID} }"#,
        )
        .await
        .expect("turn doc id");
        let turn_doc_id = turn["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .expect("turn doc id");

        let response = super::super::store::query_node_json(
            &state.node,
            &gents_turn_progress_query(turn_doc_id, "thread"),
        )
        .await
        .expect("progress query");
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .expect("request rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["request_id"], "turn");
    }
}
