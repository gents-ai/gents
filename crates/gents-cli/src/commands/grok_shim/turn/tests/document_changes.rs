use super::*;

#[tokio::test]
async fn late_caused_session_provenance_wakes_the_live_projection_before_recovery() {
    let (_tempdir, node, node_did) = test_node().await;
    let parent = seed_runtime_wake(&node, &node_did, "start child work").await;
    let tool = seed_tool_call(&node, &parent, "call-1", "agent_new", "running", "").await;
    seed_child_request_row(&node, &parent, &tool, "late-child", "processing").await;
    let manager = TurnManager::new(node.clone(), test_config(String::new(), &node_did));
    let projections = test_engine(node.clone());
    let mut cursor = RequestCursor::new();
    let before = projections
        .project_request_updates(&parent, &mut cursor, Some("prompt-1"))
        .await
        .unwrap();
    assert!(!before
        .events
        .iter()
        .any(|event| event.payload["sessionUpdate"] == "subagent_spawned"));
    for event in before.events {
        cursor.record(event.advance);
    }
    let mut wakeup = manager.terminal_document_changes(Duration::from_secs(3600));
    crate::commands::grok_shim::test_fixtures::seed_started_session(
        &node,
        &node_did,
        "session-1-child",
        Some(&node_did),
        parent.agent_id.as_deref().unwrap(),
        parent.doc_id.as_deref().unwrap(),
    )
    .await;

    tokio::time::pause();
    let start = tokio::time::Instant::now();
    wakeup.wait().await;
    let elapsed = start.elapsed();
    tokio::time::resume();
    assert!(
        elapsed < Duration::from_secs(1),
        "session provenance must wake at the existing read cadence, before the recovery timer: {elapsed:?}"
    );

    let after = projections
        .project_request_updates(&parent, &mut cursor, Some("prompt-1"))
        .await
        .unwrap();
    let spawned = after
        .events
        .iter()
        .find(|event| event.payload["sessionUpdate"] == "subagent_spawned")
        .expect("late session provenance must expose the child");
    assert_eq!(spawned.payload["child_session_id"], "session-1-child");
}
