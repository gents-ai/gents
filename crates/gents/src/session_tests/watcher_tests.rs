use gents_session::request::watcher::cooldown::ProcessedMark;
use gents_session::request::watcher::*;
use gents_session::test_support::ScopedWatcherFixture;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

#[tokio::test]
async fn pending_requests_are_scoped_by_node_did_without_host_identity() {
    let fixture = ScopedWatcherFixture::new().await;
    let node_did = fixture.node_did.as_str();
    let mut input = fixture.request_input("req-owned", "sess-owned");
    input["created_at"] = serde_json::json!("2026-08-21T00:00:00Z");
    input["workspace_id"] = serde_json::json!("ws-1");
    input["workspace_owner_node_did"] = serde_json::json!(node_did);
    input["workspace_authority"] = serde_json::json!("readWrite");
    fixture.create_request(input).await;

    let foreign = DefraWatcher::for_agents(
        fixture.node.clone(),
        "did:key:foreign",
        vec![fixture.agent.clone()],
    )
    .expect("request watcher");
    assert!(foreign.pending_requests().await.unwrap().is_empty());
    let owner = fixture.watcher();
    let pending = owner.pending_requests().await.unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|request| request.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-owned"]
    );
    assert_eq!(
        pending[0].workspace_owner_node_did.as_deref(),
        Some(node_did)
    );
    let continuation = gents_automation::goal::continuation::prepare_goal_continuation(
        &pending[0],
        pending[0].agent_did.clone(),
        "goal-workspace-owner",
        "continue",
        1,
        false,
        "2026-08-21T00:00:01Z",
        pending[0].request_hop,
    )
    .expect("workspace-bound parent produces a coherent goal continuation");
    assert_eq!(
        continuation.workspace_owner_node_did.as_deref(),
        Some(node_did)
    );
}
