use std::sync::Arc;

use super::super::{AwaitMode, CancelPolicy, ToolCallLifecycle, ToolCallState};
use super::IllegalToolCallTransition;

/// Build a minimal in-memory node. Schema setup is not required for these
/// tests because the h_native guards fire before any DB mutation.
async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
}

fn test_deadline() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::minutes(5)
}

/// Return a subagent-typed lifecycle already in Running state.
/// Uses the pub(crate) setters to skip `start_running` (which would
/// require schema setup). The guard under test fires before the DB call,
/// so no mutation ever reaches the node.
async fn subagent_lc_in_running() -> ToolCallLifecycle {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node,
        "request-1".to_string(),
        "session-1".to_string(),
        "tcid-1".to_string(),
        0,
        "spawn_agent".to_string(),
        "{}".to_string(),
        test_deadline(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-1".to_string(),
    );
    lc.set_state(ToolCallState::Running);
    lc.set_doc_id(Some("fake-doc-id".to_string()));
    lc.set_started_at(Some(chrono::Utc::now()));
    lc
}

#[tokio::test]
async fn complete_rejects_subagent_typed_tool() {
    let mut lc = subagent_lc_in_running().await;
    let err = lc.complete("result").await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::NativeCompleteOnSubagentTool)
        ),
        "expected NativeCompleteOnSubagentTool, got: {err:?}"
    );
}

#[tokio::test]
async fn fail_rejects_subagent_typed_tool() {
    use super::super::FailureClass;
    let mut lc = subagent_lc_in_running().await;
    let err = lc
        .fail("error output", FailureClass::External)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::NativeFailOnSubagentTool)
        ),
        "expected NativeFailOnSubagentTool, got: {err:?}"
    );
}

#[tokio::test]
async fn background_rejects_already_background() {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node,
        "req-bg-2".to_string(),
        "sess-bg-2".to_string(),
        "tc-bg-2".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        test_deadline(),
        AwaitMode::Background, // start already in Background
        CancelPolicy::Cascade,
        "child-req-bg-2".to_string(),
    );
    lc.set_state(ToolCallState::Running);
    lc.set_doc_id(Some("fake-doc-id-bg-2".to_string()));
    lc.set_started_at(Some(chrono::Utc::now()));

    let err = lc.background().await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::ModeAlreadyBackground)
        ),
        "expected ModeAlreadyBackground, got: {err:?}"
    );
}

#[tokio::test]
async fn background_rejects_pending_state() {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new(
        node,
        "req-bg-3".to_string(),
        "sess-bg-3".to_string(),
        "tc-bg-3".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        test_deadline(),
    );
    // state is Pending (default); do not advance it

    let err = lc.background().await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::BadState { .. })
        ),
        "expected BadState, got: {err:?}"
    );
}

#[tokio::test]
async fn foreground_rejects_already_foreground() {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node,
        "req-fg-1".to_string(),
        "sess-fg-1".to_string(),
        "tc-fg-1".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        test_deadline(),
        AwaitMode::Foreground, // start already in Foreground
        CancelPolicy::Cascade,
        "child-req-fg-1".to_string(),
    );
    lc.set_state(ToolCallState::Running);
    lc.set_doc_id(Some("fake-doc-id-fg-1".to_string()));
    lc.set_started_at(Some(chrono::Utc::now()));

    let err = lc.foreground().await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::ModeAlreadyForeground)
        ),
        "expected ModeAlreadyForeground, got: {err:?}"
    );
}

#[tokio::test]
async fn foreground_rejects_pending_state() {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new(
        node,
        "req-fg-2".to_string(),
        "sess-fg-2".to_string(),
        "tc-fg-2".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        test_deadline(),
    );
    // state is Pending (default); do not advance it

    let err = lc.foreground().await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::BadState { .. })
        ),
        "expected BadState, got: {err:?}"
    );
}

#[tokio::test]
async fn detach_rejects_already_detach() {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node,
        "req-detach-1".to_string(),
        "sess-detach-1".to_string(),
        "tc-detach-1".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        test_deadline(),
        AwaitMode::Foreground,
        CancelPolicy::Detach, // already in Detach policy
        "child-req-detach-1".to_string(),
    );
    lc.set_state(ToolCallState::Running);
    lc.set_doc_id(Some("fake-doc-id-detach-1".to_string()));
    lc.set_started_at(Some(chrono::Utc::now()));

    let err = lc.detach().await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::PolicyAlreadyDetach)
        ),
        "expected PolicyAlreadyDetach, got: {err:?}"
    );
}

#[tokio::test]
async fn detach_rejects_terminal_state() {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node,
        "req-detach-2".to_string(),
        "sess-detach-2".to_string(),
        "tc-detach-2".to_string(),
        1,
        "spawn_subagent".to_string(),
        "{}".to_string(),
        test_deadline(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-req-detach-2".to_string(),
    );
    lc.set_state(ToolCallState::Cancelled); // terminal state
    lc.set_doc_id(Some("fake-doc-id-detach-2".to_string()));
    lc.set_started_at(Some(chrono::Utc::now()));

    let err = lc.detach().await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::BadState { .. })
        ),
        "expected BadState, got: {err:?}"
    );
}

#[tokio::test]
async fn bridge_failure_rejects_native_tool() {
    // Native tool: constructed with new() — no child_request_id.
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new(
        node,
        "req-bf-1".to_string(),
        "sess-bf-1".to_string(),
        "tc-bf-1".to_string(),
        0,
        "native_tool".to_string(),
        "{}".to_string(),
        test_deadline(),
    );
    lc.set_state(ToolCallState::Running);
    lc.set_doc_id(Some("fake-doc-id-bf-1".to_string()));
    lc.set_started_at(Some(chrono::Utc::now()));

    let err = lc
        .bridge_failure(super::super::ChildTerminal::Dead)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::BridgeFailureRequiresChildLink)
        ),
        "expected BridgeFailureRequiresChildLink, got: {err:?}"
    );
}

#[tokio::test]
async fn bridge_failure_rejects_pending_state() {
    // Subagent tool, but never advanced to Running.
    let node = test_node().await;
    let lc_base = ToolCallLifecycle::new_subagent(
        node,
        "req-bf-2".to_string(),
        "sess-bf-2".to_string(),
        "tc-bf-2".to_string(),
        0,
        "spawn_agent".to_string(),
        "{}".to_string(),
        test_deadline(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-1".to_string(),
    );
    // Leave state as Pending (default); do not call start_running.
    let mut lc = lc_base;

    let err = lc
        .bridge_failure(super::super::ChildTerminal::Dead)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::BadState { .. })
        ),
        "expected BadState, got: {err:?}"
    );
}

#[tokio::test]
async fn bridge_failure_projected_state_interrupted_is_cancelled() {
    use super::super::ChildTerminal;
    assert_eq!(
        ChildTerminal::Interrupted.projected_state(),
        ToolCallState::Cancelled
    );
}

#[tokio::test]
async fn bridge_failure_projected_state_failed_is_failed() {
    use super::super::{ChildTerminal, FailureClass};
    assert_eq!(
        ChildTerminal::Failed {
            reason: "error".to_string(),
            failure_class: FailureClass::External,
        }
        .projected_state(),
        ToolCallState::Failed
    );
    assert_eq!(ChildTerminal::Dead.projected_state(), ToolCallState::Failed);
    assert_eq!(
        ChildTerminal::Superseded.projected_state(),
        ToolCallState::Failed
    );
}

#[tokio::test]
async fn bridge_complete_rejects_native_tool() {
    // Native tool: constructed with new() — no child_request_id.
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new(
        node,
        "req-bc-1".to_string(),
        "sess-bc-1".to_string(),
        "tc-bc-1".to_string(),
        0,
        "native_tool".to_string(),
        "{}".to_string(),
        test_deadline(),
    );
    lc.set_state(ToolCallState::Running);
    lc.set_doc_id(Some("fake-doc-id-bc-1".to_string()));
    lc.set_started_at(Some(chrono::Utc::now()));

    let err = lc.bridge_complete("x".to_string()).await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::BridgeCompleteRequiresChildLink)
        ),
        "expected BridgeCompleteRequiresChildLink, got: {err:?}"
    );
}

#[tokio::test]
async fn bridge_complete_rejects_pending_state() {
    // Subagent tool, but never advanced to Running.
    let node = test_node().await;
    let lc_base = ToolCallLifecycle::new_subagent(
        node,
        "req-bc-2".to_string(),
        "sess-bc-2".to_string(),
        "tc-bc-2".to_string(),
        0,
        "spawn_agent".to_string(),
        "{}".to_string(),
        test_deadline(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-1".to_string(),
    );
    // Leave state as Pending (default); do not call start_running.
    let mut lc = lc_base;

    let err = lc.bridge_complete("x".to_string()).await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::BadState { .. })
        ),
        "expected BadState, got: {err:?}"
    );
}

#[tokio::test]
async fn bridge_cancel_cascade_returns_intent_for_cascade_subagent() {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node,
        "req-cas-1".to_string(),
        "sess-cas-1".to_string(),
        "tc-cas-1".to_string(),
        0,
        "spawn_agent".to_string(),
        "{}".to_string(),
        test_deadline(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-cas-1".to_string(),
    );
    lc.set_state(ToolCallState::Cancelled);

    let intent = lc.bridge_cancel_cascade().await.unwrap();
    let intent = intent.expect("should return Some(CascadeIntent)");
    assert_eq!(intent.child_request_id, "child-cas-1");
}

#[tokio::test]
async fn bridge_cancel_cascade_returns_none_for_detached() {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node,
        "req-cas-2".to_string(),
        "sess-cas-2".to_string(),
        "tc-cas-2".to_string(),
        0,
        "spawn_agent".to_string(),
        "{}".to_string(),
        test_deadline(),
        AwaitMode::Foreground,
        CancelPolicy::Detach,
        "child-cas-2".to_string(),
    );
    lc.set_state(ToolCallState::Cancelled);

    let intent = lc.bridge_cancel_cascade().await.unwrap();
    assert!(intent.is_none(), "Detach policy returns None");
}

#[tokio::test]
async fn bridge_cancel_cascade_returns_none_for_native() {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new(
        node,
        "req-cas-3".to_string(),
        "sess-cas-3".to_string(),
        "tc-cas-3".to_string(),
        0,
        "native_tool".to_string(),
        "{}".to_string(),
        test_deadline(),
    );
    lc.set_state(ToolCallState::Cancelled);

    let intent = lc.bridge_cancel_cascade().await.unwrap();
    assert!(
        intent.is_none(),
        "Native tool (no child_request_id) returns None"
    );
}

#[tokio::test]
async fn bridge_cancel_cascade_rejects_non_cancelled_state() {
    let node = test_node().await;
    let mut lc = ToolCallLifecycle::new_subagent(
        node,
        "req-cas-4".to_string(),
        "sess-cas-4".to_string(),
        "tc-cas-4".to_string(),
        0,
        "spawn_agent".to_string(),
        "{}".to_string(),
        test_deadline(),
        AwaitMode::Foreground,
        CancelPolicy::Cascade,
        "child-cas-4".to_string(),
    );
    lc.set_state(ToolCallState::Running);

    let err = lc.bridge_cancel_cascade().await.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<IllegalToolCallTransition>(),
            Some(IllegalToolCallTransition::CascadeRequiresCancelled)
        ),
        "expected CascadeRequiresCancelled, got: {err:?}"
    );
}
