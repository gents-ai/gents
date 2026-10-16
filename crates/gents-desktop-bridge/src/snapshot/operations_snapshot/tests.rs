use super::super::super::types::{
    ActiveToolCallView, NativeExecutorStatusView, RuntimeLivenessView,
};
use super::*;

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
    let rows = vec![ToolCallRow {
        request_id: "req_a".into(),
        tool_call_id: "tc".into(),
        tool_name: "grep".into(),
        lifecycle_state: Some("running".into()),
        status: None,
        started_at: Some(started.into()),
        deadline_at: None,
        await_mode: Some("background".into()),
        stuck_since: None,
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
fn project_propagates_liveness_age_and_deadline_per_tool_call() {
    let rows = vec![
        ToolCallRow {
            request_id: "req_a".into(),
            tool_call_id: "tc_live".into(),
            tool_name: "grep".into(),
            lifecycle_state: Some("running".into()),
            status: None,
            started_at: Some("2026-05-20T12:00:00Z".into()),
            deadline_at: Some("2026-05-20T12:05:00Z".into()),
            await_mode: Some("background".into()),
            stuck_since: None,
        },
        ToolCallRow {
            request_id: "req_a".into(),
            tool_call_id: "tc_dark".into(),
            tool_name: "grep".into(),
            lifecycle_state: Some("running".into()),
            status: None,
            started_at: Some("2026-05-20T12:00:00Z".into()),
            deadline_at: None,
            await_mode: Some("background".into()),
            stuck_since: None,
        },
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
    let row = ToolCallRow {
        request_id: "req_a".into(),
        tool_call_id: "tc".into(),
        tool_name: "grep".into(),
        lifecycle_state: Some("running".into()),
        status: None,
        started_at: Some(started.into()),
        deadline_at: None,
        await_mode: Some("background".into()),
        stuck_since: None,
    };
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
