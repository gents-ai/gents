use super::*;
use super::support::*;

#[tokio::test]
async fn resolve_behavior_uses_default_when_session_is_unbound() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let resolved =
        resolve_behavior_for_request(node.as_ref(), &request(None, "session-default"), "general")
            .await
            .unwrap();

    assert_eq!(resolved.behavior_id, "general");
    assert!(resolved.rejection_reason.is_none());
}

#[tokio::test]
async fn resolve_behavior_prefers_existing_session_binding() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    crate::session::create_session_with_behavior_id(
        node.as_ref(),
        "session-bound",
        "general",
        "code",
    )
    .await
    .unwrap();

    let resolved =
        resolve_behavior_for_request(node.as_ref(), &request(None, "session-bound"), "general")
            .await
            .unwrap();

    assert_eq!(resolved.behavior_id, "code");
    assert!(resolved.rejection_reason.is_none());
}

#[tokio::test]
async fn resolve_behavior_rejects_session_switches() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    crate::session::create_session_with_behavior_id(
        node.as_ref(),
        "session-pinned",
        "general",
        "general",
    )
    .await
    .unwrap();

    let resolved = resolve_behavior_for_request(
        node.as_ref(),
        &request(Some("code"), "session-pinned"),
        "general",
    )
    .await
    .unwrap();

    assert_eq!(resolved.behavior_id, "code");
    assert_eq!(
        resolved.rejection_reason.as_deref(),
        Some("session session-pinned is pinned to behavior general and cannot switch to code")
    );
}
