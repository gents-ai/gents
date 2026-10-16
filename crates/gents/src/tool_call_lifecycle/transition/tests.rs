use std::sync::Arc;

use super::super::{ToolCallLifecycle, ToolCallState};
use super::IllegalToolCallTransition;

async fn test_node() -> Arc<defra_node::EmbeddedNode> {
    Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap())
}

fn test_deadline() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::minutes(5)
}

async fn running(tool_name: &str, background: bool) -> ToolCallLifecycle {
    let node = test_node().await;
    let mut lc = if background {
        ToolCallLifecycle::new_background_tool(
            node,
            "req".to_string(),
            "sess".to_string(),
            "did:test:test".to_string(),
            "tc".to_string(),
            1,
            tool_name.to_string(),
            "{}".to_string(),
            test_deadline(),
        )
    } else {
        ToolCallLifecycle::new(
            node,
            "req".to_string(),
            "sess".to_string(),
            "did:test:test".to_string(),
            "tc".to_string(),
            1,
            tool_name.to_string(),
            "{}".to_string(),
            test_deadline(),
        )
    };
    lc.set_state(ToolCallState::Running);
    lc.set_doc_id(Some("fake-doc-id".to_string()));
    lc.set_started_at(Some(chrono::Utc::now()));
    lc
}

#[tokio::test]
async fn background_rejects_already_background() {
    let mut lc = running("bash", true).await;
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
        "did:test:test".to_string(),
        "tc-bg-3".to_string(),
        1,
        "bash".to_string(),
        "{}".to_string(),
        test_deadline(),
    );
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
    let mut lc = running("bash", false).await;
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
        "did:test:test".to_string(),
        "tc-fg-2".to_string(),
        1,
        "bash".to_string(),
        "{}".to_string(),
        test_deadline(),
    );
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
async fn foreground_rejects_session_message_rows() {
    for tool_name in [
        crate::toolset::AGENT_NEW_TOOL_NAME,
        crate::toolset::AGENT_MESSAGE_TOOL_NAME,
    ] {
        let mut lc = running(tool_name, true).await;
        let err = lc.foreground().await.unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<IllegalToolCallTransition>(),
                Some(IllegalToolCallTransition::SessionMessageIsBackgroundOnly)
            ),
            "expected SessionMessageIsBackgroundOnly for {tool_name}, got: {err:?}"
        );
    }
}
