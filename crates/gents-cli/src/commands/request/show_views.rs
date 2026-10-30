use anyhow::{Context, Result};
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::request_input::RequestInput;
use serde::Serialize;
use serde_json::Value;

use crate::request_helpers::CliOutputObservation;

#[derive(Debug, Clone, Serialize)]
pub(super) struct RequestShowSnapshot {
    pub(super) request: RequestShowHeader,
    pub(super) output: CliOutputObservation,
    pub(super) cancel_cause: Option<RequestCancelCauseView>,
    pub(super) tool_calls: Vec<RequestToolCallView>,
    pub(super) backgrounded_tools: Vec<BackgroundedToolView>,
    pub(super) native_executors_available: bool,
    pub(super) native_executors: Vec<NativeExecutorView>,
    pub(super) child_requests: Vec<ChildRequestView>,
    pub(super) blocked: Option<gents::blocked_turn::BlockedTurn>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct RequestShowHeader {
    pub(super) request_id: String,
    pub(super) node_did: String,
    pub(super) agent_id: String,
    pub(super) session_id: String,
    pub(super) lifecycle_state: String,
    pub(super) backend_id: Option<String>,
    pub(super) execution_origin: Option<String>,
    pub(super) failure_reason: Option<String>,
    pub(super) terminal_cause: Option<String>,
    pub(super) transition_history: Vec<RequestTransitionView>,
    pub(super) created_at: Option<String>,
    pub(super) claimed_at: Option<String>,
    pub(super) deadline: Option<String>,
    pub(super) valid_until: Option<String>,
    pub(super) interrupt_requested_at: Option<String>,
    pub(super) retry_count: Option<i64>,
    pub(super) max_retries: Option<i64>,
    pub(super) input: Option<RequestInput>,
    pub(super) max_total_tokens: Option<i64>,
    pub(super) caused_by_parent_request_id: Option<String>,
    pub(super) caused_by_parent_tool_call_id: Option<String>,
    pub(super) caused_by_trigger_id: Option<String>,
    pub(super) caused_by_trigger_kind: Option<String>,
    pub(super) caused_by_correlation: Option<String>,
    pub(super) caused_by_trigger_context: Option<String>,
    pub(super) caused_by_source_doc_id: Option<String>,
    pub(super) workspace_id: Option<String>,
    pub(super) workspace_authority: Option<String>,
    pub(super) workspace_owner_node_did: Option<String>,
    pub(super) workspace_seal_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct RequestTransitionView {
    pub(super) action: String,
    pub(super) from: Option<String>,
    pub(super) to: String,
    pub(super) at: Option<String>,
    pub(super) source: String,
    pub(super) inferred: bool,
    pub(super) note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct RequestCancelCauseView {
    pub(super) cause: String,
    pub(super) interrupt_requested_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct RequestToolCallView {
    pub(super) tool_call_key: String,
    pub(super) request_id: String,
    pub(super) session_id: String,
    pub(super) message_sequence: Option<i64>,
    pub(super) tool_name: String,
    pub(super) tool_call_id: String,
    pub(super) status: String,
    pub(super) state: String,
    pub(super) await_mode: String,
    pub(super) cancel_cause: String,
    pub(super) started_at: Option<String>,
    pub(super) completed_at: Option<String>,
    pub(super) deadline_at: Option<String>,
    pub(super) active_tool_call: bool,
    pub(super) active_native_executor_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct BackgroundedToolView {
    pub(super) tool_call_id: String,
    pub(super) tool_name: String,
    pub(super) state: String,
    pub(super) await_mode: String,
    pub(super) started_at: Option<String>,
    pub(super) active_tool_call: bool,
    pub(super) active_native_executor_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct NativeExecutorView {
    pub(super) id: Option<u64>,
    pub(super) pid: Option<i64>,
    pub(super) argv0: Option<String>,
    pub(super) tool_name: Option<String>,
    pub(super) started_at: Option<String>,
    pub(super) kill_signaled_at: Option<String>,
    pub(super) reaped_at: Option<String>,
    pub(super) exit_code: Option<i64>,
    pub(super) age_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ChildRequestView {
    pub(super) request_id: String,
    pub(super) node_did: String,
    pub(super) session_id: String,
    pub(super) state: String,
    pub(super) agent_id: String,
    pub(super) created_at: Option<String>,
    pub(super) caused_by_parent_tool_call_id: Option<String>,
}

pub(super) fn request_header_view(
    row: &Value,
    terminal_cause: Option<String>,
    transition_history: Vec<RequestTransitionView>,
) -> Result<RequestShowHeader> {
    Ok(RequestShowHeader {
        request_id: string_field_or_unknown(row, "request_id"),
        node_did: string_field_or_unknown(row, "node_did"),
        agent_id: string_field_or_unknown(row, "agent_id"),
        session_id: string_field_or_unknown(row, "session_id"),
        lifecycle_state: string_field_or_unknown(row, "lifecycle_state"),
        backend_id: string_field(row, "backend_id"),
        execution_origin: string_field(row, "execution_origin"),
        failure_reason: string_field(row, "failure_reason"),
        terminal_cause,
        transition_history,
        created_at: string_field(row, "created_at"),
        claimed_at: string_field(row, "claimed_at"),
        deadline: string_field(row, "deadline"),
        valid_until: string_field(row, "valid_until"),
        interrupt_requested_at: string_field(row, "interrupt_requested_at"),
        retry_count: integer_field(row, "retry_count"),
        max_retries: integer_field(row, "max_retries"),
        input: row
            .get("input")
            .filter(|v| !v.is_null())
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .context("decoding request input")?,
        max_total_tokens: integer_field(row, "max_total_tokens"),
        caused_by_parent_request_id: string_field(row, "caused_by_parent_request_id"),
        caused_by_parent_tool_call_id: string_field(row, "caused_by_parent_tool_call_id"),
        caused_by_trigger_id: string_field(row, "caused_by_trigger_id"),
        caused_by_trigger_kind: string_field(row, "caused_by_trigger_kind"),
        caused_by_correlation: string_field(row, "caused_by_correlation"),
        caused_by_trigger_context: string_field(row, "caused_by_trigger_context"),
        caused_by_source_doc_id: string_field(row, "caused_by_source_doc_id"),
        workspace_id: string_field(row, "workspace_id"),
        workspace_authority: string_field(row, "workspace_authority"),
        workspace_owner_node_did: string_field(row, "workspace_owner_node_did"),
        workspace_seal_hash: string_field(row, "workspace_seal_hash"),
    })
}

pub(super) fn transition_history(
    request: &Value,
    terminal_cause: Option<&str>,
) -> Vec<RequestTransitionView> {
    let state = string_field(request, "lifecycle_state").unwrap_or_else(|| "unknown".to_string());
    let mut transitions = Vec::new();
    if let Some(claimed_at) = string_field(request, "claimed_at") {
        transitions.push(RequestTransitionView {
            action: "claim".to_string(),
            from: Some("pending".to_string()),
            to: "claimed".to_string(),
            at: Some(claimed_at),
            source: "AgentRequest.claimed_at".to_string(),
            inferred: false,
            note: None,
        });
    }
    if lifecycle_has_begun_inference(&state) {
        transitions.push(RequestTransitionView {
            action: "begin_inference".to_string(),
            from: Some("claimed".to_string()),
            to: "processing".to_string(),
            at: string_field(request, "claimed_at"),
            source: "AgentRequest.lifecycle_state".to_string(),
            inferred: true,
            note: Some("no dedicated begin timestamp is persisted".to_string()),
        });
    }
    if RequestLifecycleState::parse_opt(Some(&state)) != Some(RequestLifecycleState::Interrupted) {
        if let Some(interrupt_at) = string_field(request, "interrupt_requested_at") {
            let note = if terminal_action_for_state(&state).is_some() {
                "interrupt was requested before this terminal snapshot"
            } else {
                "interrupt requested; terminal transition not yet observed"
            };
            transitions.push(RequestTransitionView {
                action: "interrupt_requested".to_string(),
                from: None,
                to: state.clone(),
                at: Some(interrupt_at),
                source: "AgentRequest.interrupt_requested_at".to_string(),
                inferred: false,
                note: Some(note.to_string()),
            });
        }
    }
    if let Some(action) = terminal_action_for_state(&state) {
        transitions.push(RequestTransitionView {
            action: action.to_string(),
            from: None,
            to: state,
            at: terminal_timestamp(request, action),
            source: terminal_source(action).to_string(),
            inferred: true,
            note: terminal_cause.map(ToOwned::to_owned),
        });
    }
    transitions
}

fn lifecycle_has_begun_inference(state: &str) -> bool {
    matches!(
        RequestLifecycleState::parse_opt(Some(state)),
        Some(
            RequestLifecycleState::Processing
                | RequestLifecycleState::Completed
                | RequestLifecycleState::Superseded
        )
    )
}

fn terminal_action_for_state(state: &str) -> Option<&'static str> {
    match RequestLifecycleState::parse_opt(Some(state))? {
        RequestLifecycleState::Completed => Some("finish"),
        RequestLifecycleState::Failed => Some("fail"),
        RequestLifecycleState::Dead => Some("expire"),
        RequestLifecycleState::Interrupted => Some("interrupt"),
        RequestLifecycleState::Superseded => Some("supersede"),
        _ => None,
    }
}

pub(super) fn terminal_timestamp(request: &Value, action: &str) -> Option<String> {
    match action {
        "finish" | "fail" => string_field(request, "terminalized_at"),
        "expire" => string_field(request, "terminalized_at")
            .or_else(|| string_field(request, "valid_until")),
        "interrupt" => string_field(request, "interrupt_requested_at"),
        _ => None,
    }
}

pub(super) fn terminal_source(action: &str) -> &'static str {
    match action {
        "finish" | "fail" => "AgentRequest.terminalized_at",
        "expire" => "AgentRequest.terminalized_at",
        "interrupt" => "AgentRequest.interrupt_requested_at",
        _ => "AgentRequest.lifecycle_state",
    }
}

pub(super) fn terminal_cause(
    request: &Value,
    cancel_cause: Option<&RequestCancelCauseView>,
) -> Option<String> {
    let state = string_field(request, "lifecycle_state")?;
    match RequestLifecycleState::parse_opt(Some(&state))? {
        RequestLifecycleState::Completed => Some("finish".to_string()),
        RequestLifecycleState::Failed => Some(format!(
            "fail{}",
            cause_suffix(string_field(request, "failure_reason").as_deref())
        )),
        RequestLifecycleState::Dead => Some(format!(
            "expire{}",
            cause_suffix(string_field(request, "failure_reason").as_deref())
        )),
        RequestLifecycleState::Interrupted => {
            let cause = cancel_cause
                .map(|cause| cause.cause.as_str())
                .filter(|cause| !cause.trim().is_empty())
                .unwrap_or("unknown");
            Some(format!("interrupt: {cause}"))
        }
        RequestLifecycleState::Superseded => Some(format!(
            "supersede{}",
            cause_suffix(string_field(request, "superseded_by_request").as_deref())
        )),
        _ => None,
    }
}

fn cause_suffix(cause: Option<&str>) -> String {
    cause
        .map(str::trim)
        .filter(|cause| !cause.is_empty())
        .map(|cause| format!(": {cause}"))
        .unwrap_or_default()
}

pub(super) fn request_cancel_cause_view(request: &Value) -> Option<RequestCancelCauseView> {
    let request_cause = string_field(request, "cancel_cause");
    let interrupt_at = string_field(request, "interrupt_requested_at");
    let lifecycle_state = string_field(request, "lifecycle_state").unwrap_or_default();
    let is_interrupted = RequestLifecycleState::parse_opt(Some(lifecycle_state.as_str()))
        == Some(RequestLifecycleState::Interrupted);
    let was_cancelled = is_interrupted || request_cause.is_some() || interrupt_at.is_some();
    if !was_cancelled {
        return None;
    }
    Some(RequestCancelCauseView {
        cause: request_cause.unwrap_or_else(|| "unknown".to_string()),
        interrupt_requested_at: interrupt_at,
    })
}

pub(super) fn value_array(value: &Value, pointer: &str) -> Vec<Value> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub(super) fn string_field(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn string_field_or_unknown(row: &Value, field: &str) -> String {
    string_field(row, field).unwrap_or_else(|| "unknown".to_string())
}

pub(super) fn integer_field(row: &Value, field: &str) -> Option<i64> {
    row.get(field).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
    })
}

pub(super) fn unsigned_field(row: &Value, field: &str) -> Option<u64> {
    row.get(field).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
    })
}
