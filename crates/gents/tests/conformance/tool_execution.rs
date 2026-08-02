use crate::support::snapshots::fetch_tool_call_snapshots_for_session;
use crate::support::test_db;
use gents::tool_call_lifecycle::{CancelCause, FailureClass, ToolCallLifecycle};

use crate::lean_vocab_test::{lean_tool_preflight_case, lean_tool_retry_case};

fn test_deadline() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::minutes(5)
}

#[tokio::test]
async fn lifecycle_pending_to_running_to_completed_persists_correctly() {
    let db = test_db("tc-lc-1").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-1".into(),
        "test-session-1".into(),
        "did:test:test".to_string(),
        "tool-call-1".into(),
        0,
        "test_tool".into(),
        r#"{"x":1}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-1").await;
    assert_eq!(snapshots.len(), 1, "one row after start_running");
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("running"),
        "lifecycle_state should be running after start_running"
    );
    assert_eq!(snapshots[0].request_id.as_deref(), Some("request-1"));
    assert!(
        snapshots[0]
            .deadline_at
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        "deadline_at should persist on running tool calls"
    );

    lc.complete("ok").await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-1").await;
    assert_eq!(snapshots.len(), 1, "still one row after complete");
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("completed"),
        "lifecycle_state should be completed after complete()"
    );
}

#[tokio::test]
async fn lifecycle_running_to_failed_persists_failure_class() {
    let db = test_db("tc-lc-2").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-2".into(),
        "test-session-2".into(),
        "did:test:test".to_string(),
        "tool-call-2".into(),
        0,
        "test_tool".into(),
        r#"{"x":1}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    lc.fail("error message", FailureClass::ToolReturnedError)
        .await
        .unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-2").await;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("failed"),
        "lifecycle_state should be failed after fail()"
    );
    assert_eq!(
        snapshots[0].tool_failure_class.as_deref(),
        Some("toolReturnedError"),
        "tool_failure_class should be toolReturnedError"
    );
}

#[tokio::test]
async fn lifecycle_terminal_irreversibility() {
    let db = test_db("tc-lc-3").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-3".into(),
        "test-session-3".into(),
        "did:test:test".to_string(),
        "tool-call-3".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    lc.complete("done").await.unwrap();

    let err = lc
        .fail("late error", FailureClass::External)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("illegal tool call transition"),
        "expected guard error, got: {msg}"
    );
}

#[tokio::test]
async fn lifecycle_idempotent_start_running() {
    let db = test_db("tc-lc-4").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-4".into(),
        "test-session-4".into(),
        "did:test:test".to_string(),
        "tool-call-4".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    lc.start_running().await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-4").await;
    assert_eq!(
        snapshots.len(),
        1,
        "exactly one row should exist after duplicate start_running"
    );
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("running"),
        "state should still be running"
    );
}

#[tokio::test]
async fn lifecycle_load_returns_persisted_state() {
    let db = test_db("tc-lc-5").await;

    {
        let mut lc = ToolCallLifecycle::new(
            db.node.clone(),
            "request-5".into(),
            "test-session-5".into(),
            "did:test:test".to_string(),
            "tool-call-5".into(),
            0,
            "test_tool".into(),
            r#"{}"#.into(),
            test_deadline(),
        );
        lc.start_running().await.unwrap();
        lc.fail("oops", FailureClass::Transport).await.unwrap();
    }

    let loaded = ToolCallLifecycle::load(db.node.clone(), "test-session-5", "tool-call-5")
        .await
        .unwrap()
        .expect("row should exist after start_running + fail");

    drop(loaded);

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-5").await;
    assert_eq!(snapshots.len(), 1, "exactly one row");
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("failed"),
        "persisted lifecycle_state should be failed"
    );
    assert_eq!(
        snapshots[0].tool_failure_class.as_deref(),
        Some("transport"),
        "persisted tool_failure_class should be transport"
    );
}

#[tokio::test]
async fn lifecycle_load_preserves_deadline_for_terminal_update() {
    let db = test_db("tc-lc-6").await;
    let deadline = chrono::DateTime::parse_from_rfc3339("2026-05-08T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    {
        let mut lc = ToolCallLifecycle::new(
            db.node.clone(),
            "request-6".into(),
            "test-session-6".into(),
            "did:test:test".to_string(),
            "tool-call-6".into(),
            0,
            "test_tool".into(),
            r#"{}"#.into(),
            deadline,
        );
        lc.start_running().await.unwrap();
    }

    let mut loaded = ToolCallLifecycle::load(db.node.clone(), "test-session-6", "tool-call-6")
        .await
        .unwrap()
        .expect("row should exist after start_running");
    loaded.timeout().await.unwrap();

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, "test-session-6").await;
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("timedOut"),
        "loaded lifecycle should be able to terminalize as timedOut"
    );
    let observed_deadline =
        chrono::DateTime::parse_from_rfc3339(snapshots[0].deadline_at.as_deref().unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc);
    assert_eq!(
        observed_deadline, deadline,
        "deadline_at should survive load and terminal update"
    );
    assert_eq!(
        snapshots[0].cancel_cause.as_deref(),
        Some("deadline"),
        "timeout() should persist cancel_cause=deadline"
    );
}

#[tokio::test]
async fn lifecycle_cancel_during_run_persists_cancel_cause() {
    let db = test_db("tc-lc-cancel-cause-run").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-cancel-run".into(),
        "test-session-cancel-run".into(),
        "did:test:test".to_string(),
        "tool-call-cancel-run".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    lc.cancel_during_run(CancelCause::UserCancelled)
        .await
        .unwrap();

    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "test-session-cancel-run").await;
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(snapshots[0].cancel_cause.as_deref(), Some("userCancelled"));
}

#[tokio::test]
async fn lifecycle_load_with_null_cancel_cause_can_persist_cancel_cause() {
    let db = test_db("tc-lc-cancel-cause-load").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-cancel-load".into(),
        "test-session-cancel-load".into(),
        "did:test:test".to_string(),
        "tool-call-cancel-load".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "test-session-cancel-load").await;
    assert_eq!(snapshots[0].cancel_cause, None);

    let mut loaded = ToolCallLifecycle::load(
        db.node.clone(),
        "test-session-cancel-load",
        "tool-call-cancel-load",
    )
    .await
    .unwrap()
    .expect("tool call lifecycle should load");
    loaded
        .cancel_during_run(CancelCause::Interrupted)
        .await
        .unwrap();

    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "test-session-cancel-load").await;
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(snapshots[0].cancel_cause.as_deref(), Some("interrupted"));
}

#[tokio::test]
async fn lifecycle_cancel_before_dispatch_persists_cancel_cause() {
    let db = test_db("tc-lc-cancel-cause-pending").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-cancel-pending".into(),
        "test-session-cancel-pending".into(),
        "did:test:test".to_string(),
        "tool-call-cancel-pending".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.cancel_before_dispatch(CancelCause::Interrupted)
        .await
        .unwrap();

    let snapshots =
        fetch_tool_call_snapshots_for_session(&db.node, "test-session-cancel-pending").await;
    assert_eq!(snapshots[0].lifecycle_state.as_deref(), Some("cancelled"));
    assert_eq!(snapshots[0].cancel_cause.as_deref(), Some("interrupted"));
}

pub(super) fn generated_tool_execution_cases_cover_preflight_and_retry_contracts() {
    let unreachable =
        lean_tool_preflight_case("preflight_unreachable_valid_blocks_serviceUnavailable");
    assert_eq!(unreachable.decision, "block");
    assert_eq!(
        unreachable.failure_class.as_deref(),
        Some("serviceUnavailable")
    );

    let invalid = lean_tool_preflight_case("preflight_healthy_invalid_blocks_argumentInvalid");
    assert_eq!(invalid.decision, "block");
    assert_eq!(invalid.failure_class.as_deref(), Some("argumentInvalid"));

    for name in [
        "preflight_healthy_valid_dispatch",
        "preflight_stale_valid_dispatch",
    ] {
        let case = lean_tool_preflight_case(name);
        assert_eq!(case.decision, "dispatch", "{name}");
        assert_eq!(case.failure_class, None, "{name}");
    }

    let safe_read = lean_tool_retry_case("retry_mcpListTools_unknown_transport_retrySafeRead");
    assert_eq!(safe_read.disposition, "retrySafeRead");

    let idempotent =
        lean_tool_retry_case("retry_mcpCall_idempotent_transport_retryIdempotentToolCall");
    assert_eq!(idempotent.disposition, "retryIdempotentToolCall");

    for name in [
        "retry_mcpCall_unknown_transport_doNotRetry",
        "retry_mcpCall_nonIdempotent_transport_doNotRetry",
        "retry_nativeCommand_idempotent_transport_doNotRetry",
    ] {
        let case = lean_tool_retry_case(name);
        assert_eq!(case.disposition, "doNotRetry", "{name}");
    }
}

/// Issue #1002 defect 1: `timeout()` must CAS on `running` like every sibling
/// terminal transition (`complete`, `fail`, `cancel_during_run`,
/// `bridge_complete`/`bridge_failure`, `recover_tool_call_row`).
///
/// The documented race, driven through the real writers: a native tool is
/// running past its deadline while its parent request is interrupted. The
/// periodic `reconcile_terminal_parent_owned_tools` sweep terminalizes the row
/// first (`cancelled` / cause `interrupted`), and only then does the deadline
/// wrapper's `timeout()` fire on the stale in-memory handle. Lean's
/// `ToolExecution.Transition.timeout` requires `pre.state = .running`, and
/// terminal irreversibility requires state AND recorded cause to survive — so
/// the straggler must lose the compare and adopt the durable terminal instead
/// of overwriting it with `timedOut`.
#[tokio::test]
async fn timeout_adopts_terminal_written_by_terminal_parent_sweep() {
    let db = test_db("tc-timeout-cas").await;

    let request_id = "timeout-cas-request";
    let session_id = "timeout-cas-session";
    let created_at = chrono::Utc::now().to_rfc3339();
    // Parent request already interrupted while the tool ran past its deadline.
    crate::support::create_request(&db.node, request_id, session_id, "interrupted", &created_at)
        .await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        request_id.into(),
        session_id.into(),
        crate::support::AGENT_DID.to_string(),
        "timeout-cas-tool-call".into(),
        0,
        "test_tool".into(),
        "{}".into(),
        // Already expired: the timeout writer is enabled the moment the
        // sweep loses interest in the row.
        chrono::Utc::now() - chrono::Duration::seconds(5),
    );
    lc.start_running().await.unwrap();

    // Actor A: the sweep terminalizes the running tool under its terminal
    // parent (interrupted parent => cancelled / cause interrupted).
    let report = ToolCallLifecycle::reconcile_terminal_parent_owned_tools(
        &db.node,
        crate::support::AGENT_DID,
    )
    .await
    .unwrap();
    assert_eq!(
        report.tool_calls_terminalized, 1,
        "sweep should terminalize the running tool under its interrupted parent"
    );

    // Actor B: the straggler timeout path fires on the stale handle. It must
    // lose the running-state compare and adopt the durable terminal.
    let won = lc.timeout().await.unwrap();
    assert!(
        !won,
        "timeout() must report a lost compare when the row was already terminal"
    );

    let snapshots = fetch_tool_call_snapshots_for_session(&db.node, session_id).await;
    assert_eq!(snapshots.len(), 1, "exactly one persisted tool-call row");
    assert_eq!(
        snapshots[0].lifecycle_state.as_deref(),
        Some("cancelled"),
        "timeout() must not overwrite a terminal another actor already recorded"
    );
    assert_eq!(
        snapshots[0].cancel_cause.as_deref(),
        Some("interrupted"),
        "the recorded cancellation cause must be preserved"
    );
    assert_eq!(
        snapshots[0].tool_failure_class, None,
        "the sweep's cancelled terminal carries no failure class; timeout() must not stamp one"
    );
}
