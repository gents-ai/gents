use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::show_views::{
    integer_field, string_field, string_field_or_unknown, unsigned_field, value_array,
    BackgroundedToolView, ChildRequestView, NativeExecutorView, RequestToolCallView,
};

pub(super) fn tool_call_view(
    row: &Value,
    active_tool_calls: &BTreeSet<(String, String)>,
    active_executor_counts: &BTreeMap<String, usize>,
) -> RequestToolCallView {
    let request_id = string_field_or_unknown(row, "request_id");
    let tool_call_id = string_field_or_unknown(row, "tool_call_id");
    let active_tool_call = active_tool_calls.contains(&(request_id.clone(), tool_call_id.clone()));
    let active_native_executor_count = string_field(row, "tool_call_key")
        .as_ref()
        .and_then(|key| active_executor_counts.get(key).copied())
        .unwrap_or(0);
    RequestToolCallView {
        tool_call_key: string_field_or_unknown(row, "tool_call_key"),
        request_id,
        session_id: string_field_or_unknown(row, "session_id"),
        message_sequence: integer_field(row, "message_sequence"),
        tool_name: string_field_or_unknown(row, "tool_name"),
        tool_call_id,
        status: string_field_or_unknown(row, "status"),
        state: string_field(row, "lifecycle_state")
            .or_else(|| string_field(row, "status"))
            .unwrap_or_else(|| "unknown".to_string()),
        await_mode: string_field_or_unknown(row, "await_mode"),
        cancel_cause: string_field_or_unknown(row, "cancel_cause"),
        started_at: string_field(row, "started_at"),
        completed_at: string_field(row, "completed_at"),
        deadline_at: string_field(row, "deadline_at"),
        active_tool_call,
        active_native_executor_count,
    }
}

pub(super) fn backgrounded_tool_view(tool: &RequestToolCallView) -> BackgroundedToolView {
    BackgroundedToolView {
        tool_call_id: tool.tool_call_id.clone(),
        tool_name: tool.tool_name.clone(),
        state: tool.state.clone(),
        await_mode: tool.await_mode.clone(),
        started_at: tool.started_at.clone(),
        active_tool_call: tool.active_tool_call,
        active_native_executor_count: tool.active_native_executor_count,
    }
}

pub(super) fn child_request_view(row: &Value) -> ChildRequestView {
    ChildRequestView {
        request_id: string_field_or_unknown(row, "request_id"),
        node_did: string_field_or_unknown(row, "node_did"),
        session_id: string_field_or_unknown(row, "session_id"),
        state: string_field_or_unknown(row, "lifecycle_state"),
        agent_id: string_field_or_unknown(row, "agent_id"),
        created_at: string_field(row, "created_at"),
        caused_by_parent_tool_call_id: string_field(row, "caused_by_parent_tool_call_id"),
    }
}

pub(super) fn native_executor_view(row: &Value) -> NativeExecutorView {
    NativeExecutorView {
        id: unsigned_field(row, "id"),
        pid: integer_field(row, "pid"),
        argv0: string_field(row, "argv0"),
        tool_name: string_field(row, "tool_name"),
        started_at: string_field(row, "started_at"),
        kill_signaled_at: string_field(row, "kill_signaled_at"),
        reaped_at: string_field(row, "reaped_at"),
        exit_code: integer_field(row, "exit_code"),
        age_ms: integer_field(row, "age_ms"),
    }
}

pub(super) fn active_tool_call_keys(liveness: &Value) -> BTreeSet<(String, String)> {
    value_array(liveness, "/active_tool_calls")
        .iter()
        .filter_map(|row| {
            Some((
                string_field(row, "request_id")?,
                string_field(row, "tool_call_id")?,
            ))
        })
        .collect()
}

pub(super) fn native_executor_matches_request(
    executor: &Value,
    request_id: &str,
    tool_names: &BTreeSet<String>,
    tool_call_ids: &BTreeSet<String>,
    tool_call_keys: &BTreeSet<String>,
) -> bool {
    if string_field(executor, "request_id").as_deref() == Some(request_id) {
        return true;
    }
    if string_field(executor, "tool_call_id")
        .as_ref()
        .is_some_and(|id| tool_call_ids.contains(id))
    {
        return true;
    }
    if string_field(executor, "tool_call_key")
        .as_ref()
        .is_some_and(|key| tool_call_keys.contains(key))
    {
        return true;
    }
    string_field(executor, "tool_name")
        .as_ref()
        .is_some_and(|name| tool_names.contains(name))
}

pub(super) fn active_native_executor_counts_by_tool(
    tool_rows: &[Value],
    native_executors: &[Value],
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for tool in tool_rows {
        let Some(tool_call_key) = string_field(tool, "tool_call_key") else {
            continue;
        };
        let tool_call_id = string_field(tool, "tool_call_id");
        let tool_name = string_field(tool, "tool_name");
        let count = native_executors
            .iter()
            .filter(|executor| {
                (tool_call_id.is_some() && string_field(executor, "tool_call_id") == tool_call_id)
                    || string_field(executor, "tool_call_key").as_deref()
                        == Some(tool_call_key.as_str())
                    || (tool_name.is_some() && string_field(executor, "tool_name") == tool_name)
            })
            .count();
        counts.insert(tool_call_key, count);
    }
    counts
}
