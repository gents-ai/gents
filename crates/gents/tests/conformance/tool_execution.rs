//! Bucket 3 conformance: runtime-on-Rust integration tests for
//! ToolCallLifecycle. Exercises the real GraphQL mutations through a live
//! EmbeddedNode and asserts persisted state matches the Lean spec.
//!
//! Each test spins up its own isolated EmbeddedNode via `test_db()`.
//! `ToolCallState` is `pub(crate)` so the load-reconstruction test asserts
//! via snapshot fields rather than the internal accessor.

use crate::support::snapshots::fetch_tool_call_snapshots_for_session;
use crate::support::test_db;
use gents::tool_call_lifecycle::{CancelCause, FailureClass, ToolCallLifecycle};

use crate::lean_vocab_test::{lean_tool_preflight_case, lean_tool_retry_case};

fn test_deadline() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::minutes(5)
}

// ---------------------------------------------------------------------------
// Test 1: Pending → Running → Completed persists correctly
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_pending_to_running_to_completed_persists_correctly() {
    let db = test_db("tc-lc-1").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-1".into(),
        "test-session-1".into(),
        "did:defra-agent:test".to_string(),
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

// ---------------------------------------------------------------------------
// Test 2: Running → Failed persists failure_class
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_running_to_failed_persists_failure_class() {
    let db = test_db("tc-lc-2").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-2".into(),
        "test-session-2".into(),
        "did:defra-agent:test".to_string(),
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

// ---------------------------------------------------------------------------
// Test 3: Terminal irreversibility — fail() after complete() errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_terminal_irreversibility() {
    let db = test_db("tc-lc-3").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-3".into(),
        "test-session-3".into(),
        "did:defra-agent:test".to_string(),
        "tool-call-3".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    lc.complete("done").await.unwrap();

    // Attempting fail() after complete must return an error.
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

// ---------------------------------------------------------------------------
// Test 4: Idempotent start_running — second call is a no-op, single DB row
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_idempotent_start_running() {
    let db = test_db("tc-lc-4").await;

    let mut lc = ToolCallLifecycle::new(
        db.node.clone(),
        "request-4".into(),
        "test-session-4".into(),
        "did:defra-agent:test".to_string(),
        "tool-call-4".into(),
        0,
        "test_tool".into(),
        r#"{}"#.into(),
        test_deadline(),
    );

    lc.start_running().await.unwrap();
    // Second call should be a no-op (already Running).
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

// ---------------------------------------------------------------------------
// Test 5: load() reconstructs persisted state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lifecycle_load_returns_persisted_state() {
    let db = test_db("tc-lc-5").await;

    {
        let mut lc = ToolCallLifecycle::new(
            db.node.clone(),
            "request-5".into(),
            "test-session-5".into(),
            "did:defra-agent:test".to_string(),
            "tool-call-5".into(),
            0,
            "test_tool".into(),
            r#"{}"#.into(),
            test_deadline(),
        );
        lc.start_running().await.unwrap();
        lc.fail("oops", FailureClass::Transport).await.unwrap();
    }

    // Load from the live node — should reconstruct the Failed state.
    let loaded = ToolCallLifecycle::load(db.node.clone(), "test-session-5", "tool-call-5")
        .await
        .unwrap()
        .expect("row should exist after start_running + fail");

    // Verify the reconstructed lifecycle reflects the failed state via snapshot.
    // (ToolCallState is pub(crate) so we assert by querying the node directly.)
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
            "did:defra-agent:test".to_string(),
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
        "did:defra-agent:test".to_string(),
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
        "did:defra-agent:test".to_string(),
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
        "did:defra-agent:test".to_string(),
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

// --- Moved from tooling_slots_queue_command.rs (#446 mirror split): the
// preflight/retry contract is ToolExecution's surface (MCP list/call retry
// evidence + preflight gating).
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
