use super::super::super::types::{
    ActiveToolCallView, NativeExecutorStatusView, RuntimeLivenessView,
};
use super::*;

fn row(
    tool_call_id: &str,
    lifecycle_state: Option<&str>,
    started_at: Option<&str>,
    completed_at: Option<&str>,
) -> ToolCallRow {
    ToolCallRow {
        request_id: "req_a".into(),
        tool_call_id: tool_call_id.into(),
        tool_name: "grep".into(),
        lifecycle_state: lifecycle_state.map(str::to_string),
        status: None,
        started_at: started_at.map(str::to_string),
        completed_at: completed_at.map(str::to_string),
        deadline_at: None,
        await_mode: Some("background".into()),
        stuck_since: None,
    }
}

#[test]
fn project_filters_to_background_await_mode_only() {
    let mut toolcall_rows = vec![
        row("tc_bg", Some("running"), Some("2026-05-20T12:00:00Z"), None),
        row("tc_fg", Some("running"), Some("2026-05-20T12:00:00Z"), None),
    ];
    let mut no_mode = toolcall_rows[1].clone();
    no_mode.tool_call_id = "tc_no_mode".into();
    no_mode.await_mode = None;
    toolcall_rows.push(no_mode);
    let projected =
        project_backgrounded_tools(&toolcall_rows, &liveness_with(Vec::new(), Vec::new()));
    assert_eq!(projected.len(), 1);
    assert_eq!(projected[0].tool_call_id, "tc_bg");
}

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
fn project_attaches_native_executor_when_correlated() {
    let started = "2026-05-20T12:00:00Z";
    let rows = vec![row("tc", Some("running"), Some(started), None)];
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
fn project_propagates_liveness_age_and_deadline_per_tool_call() {
    let rows = vec![
        row(
            "tc_live",
            Some("running"),
            Some("2026-05-20T12:00:00Z"),
            None,
        ),
        row(
            "tc_dark",
            Some("running"),
            Some("2026-05-20T12:00:00Z"),
            None,
        ),
    ];
    let liveness = liveness_with(
        vec![ActiveToolCallView {
            request_id: "req_a".into(),
            tool_call_id: "tc_live".into(),
            tool_name: "grep".into(),
            started_at: Some("2026-05-20T12:00:00Z".into()),
            deadline_at: Some("2026-05-20T12:05:00Z".into()),
            await_mode: Some("background".into()),
            running_age_ms: 4_321,
            deadline_expired: true,
        }],
        Vec::new(),
    );

    let projected = project_backgrounded_tools(&rows, &liveness);
    assert_eq!(projected.len(), 2);
    let live = projected
        .iter()
        .find(|view| view.tool_call_id == "tc_live")
        .expect("live tool projected");
    assert_eq!(live.age_ms, Some(4_321), "age comes from the live snapshot");
    assert!(
        live.deadline_expired,
        "deadline expiry must propagate from the liveness owner"
    );
    let dark = projected
        .iter()
        .find(|view| view.tool_call_id == "tc_dark")
        .expect("unobserved tool still projected");
    assert_eq!(
        dark.age_ms, None,
        "a tool absent from the live snapshot has no live age"
    );
    assert!(
        !dark.deadline_expired,
        "an unobserved tool must not inherit another call's deadline expiry"
    );
}

#[test]
fn native_executor_correlation_respects_window_and_tool_name() {
    let started = "2026-05-20T12:00:00Z";
    let row = row("tc", Some("running"), Some(started), None);
    let exec = |tool_name: &str, started_at: &str| NativeExecutorStatusView {
        id: 1,
        pid: 100,
        argv0: "/usr/bin/grep".into(),
        tool_name: Some(tool_name.into()),
        started_at: started_at.into(),
        age_ms: 0,
    };
    let execs = vec![
        exec("index_repo", "2026-05-20T12:00:00Z"),
        exec("grep", "2026-05-20T12:00:01.500Z"),
        exec("grep", "2026-05-20T12:00:00.500Z"),
    ];
    let liveness = liveness_with(Vec::new(), execs);

    let projected = project_backgrounded_tools(std::slice::from_ref(&row), &liveness);
    let correlated = projected[0]
        .native_executor
        .as_ref()
        .expect("in-window matching-name executor must correlate");
    assert_eq!(correlated.started_at, "2026-05-20T12:00:00.500Z");

    let no_start = ToolCallRow {
        started_at: None,
        ..row.clone()
    };
    let projected = project_backgrounded_tools(std::slice::from_ref(&no_start), &liveness);
    assert!(
        projected[0].native_executor.is_none(),
        "missing started_at must not correlate a native executor"
    );
}

#[test]
fn project_carries_shared_wait_status_for_running_row() {
    let rows = vec![row(
        "tc_run",
        Some("running"),
        Some("2026-05-20T12:00:00Z"),
        None,
    )];
    let projected = project_backgrounded_tools(&rows, &liveness_with(Vec::new(), Vec::new()));
    let ws = projected[0]
        .wait_status
        .as_ref()
        .expect("shared projection attached");
    assert_eq!(ws.lifecycle, "running");
    assert_eq!(ws.lifecycle_state_raw.as_deref(), Some("running"));
    assert!(ws.running_elapsed_seconds.is_some());
    assert_eq!(ws.terminal_elapsed_seconds, None);
}

#[test]
fn project_preserves_pending_and_unknown_without_relabelling() {
    let rows = vec![
        row(
            "tc_pending",
            Some("pending"),
            Some("2026-05-20T12:00:00Z"),
            None,
        ),
        row("tc_garbage", Some("garbage"), None, None),
        row("tc_none", None, None, None),
    ];
    let projected = project_backgrounded_tools(&rows, &liveness_with(Vec::new(), Vec::new()));
    assert_eq!(projected.len(), 3, "malformed rows are kept, not dropped");
    let pending = projected
        .iter()
        .find(|v| v.tool_call_id == "tc_pending")
        .unwrap()
        .wait_status
        .as_ref()
        .unwrap();
    assert_eq!(pending.lifecycle, "pending");
    assert!(pending.running_elapsed_seconds.is_some());
    let garbage = projected
        .iter()
        .find(|v| v.tool_call_id == "tc_garbage")
        .unwrap()
        .wait_status
        .as_ref()
        .unwrap();
    assert_eq!(garbage.lifecycle, "unknown");
    assert_eq!(garbage.lifecycle_state_raw.as_deref(), Some("garbage"));
    assert_eq!(garbage.running_elapsed_seconds, None);
    let none = projected
        .iter()
        .find(|v| v.tool_call_id == "tc_none")
        .unwrap()
        .wait_status
        .as_ref()
        .unwrap();
    assert_eq!(none.lifecycle, "unknown");
}

#[test]
fn project_terminal_rows_are_not_active_workers() {
    let rows = vec![row(
        "tc_done",
        Some("completed"),
        Some("2026-05-20T12:00:00Z"),
        Some("2026-05-20T12:01:30Z"),
    )];
    let projected = project_backgrounded_tools(&rows, &liveness_with(Vec::new(), Vec::new()));
    assert!(projected.is_empty(), "terminal rows are not active workers");
}

#[test]
fn wait_status_terminal_without_completed_at_has_no_duration() {
    let projection = gents_protocol::wait_status::WaitStatusProjection::from_parts(
        Some("failed"),
        Some("2026-05-20T12:00:00Z"),
        None,
        chrono::Utc::now(),
    );
    assert_eq!(projection.terminal_elapsed_seconds, None);
}

#[test]
fn wait_status_future_started_at_yields_no_age() {
    let projection = gents_protocol::wait_status::WaitStatusProjection::from_parts(
        Some("running"),
        Some("2026-05-20T12:00:00Z"),
        None,
        chrono::DateTime::parse_from_rfc3339("2026-05-20T11:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    );
    assert_eq!(projection.running_elapsed_seconds, None);
}
