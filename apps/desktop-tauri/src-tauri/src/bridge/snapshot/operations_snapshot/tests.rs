use super::*;
use super::super::super::types::{
    ActiveToolCallView, NativeExecutorStatusView, RuntimeLivenessView,
};

fn liveness_with(
    tools: Vec<ActiveToolCallView>,
    execs: Vec<NativeExecutorStatusView>,
) -> RuntimeLivenessView {
    RuntimeLivenessView {
        expired_processing_count: 0,
        requests: Vec::new(),
        active_tool_calls: tools,
        active_native_executors_available: true,
        active_native_executors: execs,
    }
}

#[test]
fn project_filters_to_background_await_mode_only() {
    let toolcall_rows = vec![
        ToolCallRow {
            request_id: "req_a".into(),
            tool_call_id: "tc_bg".into(),
            tool_name: "grep".into(),
            lifecycle_state: Some("running".into()),
            status: None,
            started_at: Some("2026-05-20T12:00:00Z".into()),
            deadline_at: None,
            await_mode: Some("background".into()),
            cancel_policy: Some("cascade".into()),
            child_request_id: None,
            stuck_since: None,
            cancel_pending_remote_ack: false,
        },
        ToolCallRow {
            request_id: "req_a".into(),
            tool_call_id: "tc_fg".into(),
            tool_name: "grep_fg".into(),
            lifecycle_state: Some("running".into()),
            status: None,
            started_at: Some("2026-05-20T12:00:00Z".into()),
            deadline_at: None,
            await_mode: Some("foreground".into()),
            cancel_policy: None,
            child_request_id: None,
            stuck_since: None,
            cancel_pending_remote_ack: false,
        },
    ];

    let projected = project_backgrounded_tools(&toolcall_rows, &liveness_with(Vec::new(), Vec::new()));
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].tool_call_id, "tc_bg");
}

#[test]
fn project_skips_terminal_lifecycle_state() {
    let rows = vec![ToolCallRow {
        request_id: "req_a".into(),
        tool_call_id: "tc".into(),
        tool_name: "grep".into(),
        lifecycle_state: Some("completed".into()),
        status: None,
        started_at: None,
        deadline_at: None,
        await_mode: Some("background".into()),
        cancel_policy: None,
        child_request_id: None,
        stuck_since: None,
        cancel_pending_remote_ack: false,
    }];
    let projected = project_backgrounded_tools(&rows, &liveness_with(Vec::new(), Vec::new()));
    assert!(projected.is_empty());
}

#[test]
fn project_attaches_native_executor_when_correlated() {
    let started = "2026-05-20T12:00:00Z";
    let rows = vec![ToolCallRow {
        request_id: "req_a".into(),
        tool_call_id: "tc".into(),
        tool_name: "grep".into(),
        lifecycle_state: Some("running".into()),
        status: None,
        started_at: Some(started.into()),
        deadline_at: None,
        await_mode: Some("background".into()),
        cancel_policy: None,
        child_request_id: None,
        stuck_since: None,
        cancel_pending_remote_ack: false,
    }];
    let execs = vec![NativeExecutorStatusView {
        id: 902,
        pid: 41812,
        argv0: "/usr/bin/grep".into(),
        tool_name: Some("grep".into()),
        started_at: started.into(),
        age_ms: 5_000,
    }];
    let liveness = liveness_with(Vec::new(), execs);

    let projected = project_backgrounded_tools(&rows, &liveness);
    assert!(projected[0].native_executor.is_some());
    assert_eq!(projected[0].native_executor.as_ref().unwrap().pid, 41812);
}

#[test]
fn stuck_diagnostic_emitted_for_cancel_pending_or_stuck_since() {
    let rows = vec![ToolCallRow {
        request_id: "req_a".into(),
        tool_call_id: "tc".into(),
        tool_name: "index_repo".into(),
        lifecycle_state: Some("running".into()),
        status: None,
        started_at: Some("2026-05-20T12:00:00Z".into()),
        deadline_at: None,
        await_mode: Some("background".into()),
        cancel_policy: None,
        child_request_id: None,
        stuck_since: Some("2026-05-20T12:00:00Z".into()),
        cancel_pending_remote_ack: true,
    }];
    let diagnostics = stuck_diagnostics_from_tool_calls(&rows);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].reason, "pendingRemoteCancelAck");
}

#[test]
fn stuck_diagnostic_uses_stuck_tool_when_no_cancel_pending() {
    let rows = vec![ToolCallRow {
        request_id: "req_a".into(),
        tool_call_id: "tc".into(),
        tool_name: "index_repo".into(),
        lifecycle_state: Some("running".into()),
        status: None,
        started_at: Some("2026-05-20T12:00:00Z".into()),
        deadline_at: None,
        await_mode: Some("background".into()),
        cancel_policy: None,
        child_request_id: None,
        stuck_since: Some("2026-05-20T12:00:00Z".into()),
        cancel_pending_remote_ack: false,
    }];
    let diagnostics = stuck_diagnostics_from_tool_calls(&rows);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].reason, "stuckTool");
}
