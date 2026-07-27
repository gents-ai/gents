//! Pure projection functions over runtime liveness + AgentToolCall rows.
//!
//! The Tauri command body lives in `bridge::tauri_commands::operations` and
//! is responsible for I/O (DefraDB query + in-process executor snapshot);
//! this module is pure for testability.

use super::super::types::{
    ActiveToolCallView, BackgroundedToolView, NativeExecutorStatusView, RuntimeLivenessView,
    StuckWorkDiagnosticView,
};

/// Internal shape representing one row pulled from the `AgentToolCall`
/// collection in DefraDB. The Tauri command parses GraphQL JSON into this
/// type before passing to the projection functions.
#[derive(Debug, Clone)]
pub struct ToolCallRow {
    pub request_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub lifecycle_state: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<String>,
    pub deadline_at: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub child_request_id: Option<String>,
    pub stuck_since: Option<String>,
    pub cancel_pending_remote_ack: bool,
}

const TERMINAL_LIFECYCLE_STATES: &[&str] =
    &["completed", "failed", "cancelled", "timedOut", "superseded"];
const CORRELATION_WINDOW_MS: i64 = 1_000;

pub fn project_backgrounded_tools(
    rows: &[ToolCallRow],
    liveness: &RuntimeLivenessView,
) -> Vec<BackgroundedToolView> {
    rows.iter()
        .filter(|r| r.await_mode.as_deref() == Some("background"))
        .filter(|r| {
            !r.lifecycle_state
                .as_deref()
                .is_some_and(|s| TERMINAL_LIFECYCLE_STATES.contains(&s))
        })
        .map(|r| {
            let age_ms =
                age_from_live_snapshot(r.tool_call_id.as_str(), &liveness.active_tool_calls);
            let deadline_expired = liveness
                .active_tool_calls
                .iter()
                .find(|tc| tc.tool_call_id == r.tool_call_id)
                .map(|tc| tc.deadline_expired)
                .unwrap_or(false);
            let native_executor = correlate_native_executor(r, &liveness.active_native_executors);
            BackgroundedToolView {
                request_id: r.request_id.clone(),
                tool_call_id: r.tool_call_id.clone(),
                tool_name: r.tool_name.clone(),
                lifecycle_state: r.lifecycle_state.clone(),
                status: r.status.clone(),
                started_at: r.started_at.clone(),
                age_ms,
                deadline_at: r.deadline_at.clone(),
                deadline_expired,
                await_mode: r.await_mode.clone(),
                cancel_policy: r.cancel_policy.clone(),
                child_request_id: r.child_request_id.clone(),
                stuck_since: r.stuck_since.clone(),
                cancel_pending_remote_ack: r.cancel_pending_remote_ack,
                native_executor,
            }
        })
        .collect()
}

pub fn stuck_diagnostics_from_tool_calls(
    rows: &[ToolCallRow],
) -> Vec<StuckWorkDiagnosticView> {
    rows.iter()
        .filter(|r| r.await_mode.as_deref() == Some("background"))
        .filter(|r| {
            !r.lifecycle_state
                .as_deref()
                .is_some_and(|s| TERMINAL_LIFECYCLE_STATES.contains(&s))
        })
        .filter_map(|r| {
            let reason = if r.cancel_pending_remote_ack {
                "pendingRemoteCancelAck"
            } else if r.stuck_since.is_some() {
                "stuckTool"
            } else {
                return None;
            };
            Some(StuckWorkDiagnosticView {
                request_id: r.request_id.clone(),
                session_id: None,
                severity: "warning".to_string(),
                reason: reason.to_string(),
                deadline_age_ms: None,
                last_progress_age_ms: None,
                tool_call_id: Some(r.tool_call_id.clone()),
                tool_name: Some(r.tool_name.clone()),
                stuck_since: r.stuck_since.clone(),
            })
        })
        .collect()
}

fn age_from_live_snapshot(tool_call_id: &str, live: &[ActiveToolCallView]) -> Option<i64> {
    live.iter()
        .find(|tc| tc.tool_call_id == tool_call_id)
        .map(|tc| tc.running_age_ms)
}

fn correlate_native_executor(
    row: &ToolCallRow,
    execs: &[NativeExecutorStatusView],
) -> Option<NativeExecutorStatusView> {
    let started_at = row.started_at.as_deref()?;
    let started_ms = parse_iso_ms(started_at)?;
    execs
        .iter()
        .find(|ne| {
            ne.tool_name.as_deref() == Some(row.tool_name.as_str())
                && parse_iso_ms(&ne.started_at)
                    .map(|m| (m - started_ms).abs() <= CORRELATION_WINDOW_MS)
                    .unwrap_or(false)
        })
        .cloned()
}

fn parse_iso_ms(iso: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

#[cfg(test)]
#[path = "operations_snapshot/tests.rs"]
mod tests;
