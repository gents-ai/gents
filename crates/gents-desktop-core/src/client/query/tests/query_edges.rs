use super::super::*;
use crate::client::schema::ensure_runtime_schemas;
use defra_node::NodeBuilder;
use gents_protocol::schemas::AGENT_MESSAGE_NAME;
use std::sync::Arc;

#[tokio::test]
async fn fetch_doc_patch_returns_empty_store_for_no_matches() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let patch = fetch_doc_patch(node.as_ref(), AGENT_MESSAGE_NAME, &["never-existed"])
        .await
        .expect("fetch_doc_patch");
    assert_eq!(patch.messages.len(), 0);
}

#[tokio::test]
async fn fetch_doc_patch_empty_input_is_no_op() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let patch = fetch_doc_patch(node.as_ref(), AGENT_MESSAGE_NAME, &[])
        .await
        .expect("fetch_doc_patch");
    assert_eq!(patch.row_count(), 0);
}

#[tokio::test]
async fn fetch_doc_patch_unknown_collection_errors() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let result = fetch_doc_patch(node.as_ref(), "NotARealCollection", &["x"]).await;
    assert!(result.is_err());
}

#[test]
fn doc_patch_support_excludes_pairing_control_collections() {
    assert!(supports_doc_patch_collection(INFERENCE_BACKEND_NAME));
    assert!(supports_doc_patch_collection(TOOL_SERVICE_REGISTRY_NAME));
    assert!(!supports_doc_patch_collection("PeerPairingApplied"));
    assert!(!supports_doc_patch_collection("BearerPairingReady"));
    assert!(!supports_doc_patch_collection("SessionHydrationRequest"));
}

#[tokio::test]
async fn load_agent_runtimes_hydrates_executor_capacity_and_queue_depth() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let response = node
        .execute(
            r#"mutation {
                create_AgentRuntime(input: {
                    agent_did: "did:key:runtime-capacity",
                    behavior_executor_capacity: 7,
                    behavior_executor_queue_depth: 3
                }) { agent_did }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let runtimes = load_agent_runtimes(node.as_ref())
        .await
        .expect("load agent runtimes");
    let runtime = runtimes
        .iter()
        .find(|row| row.agent_did == "did:key:runtime-capacity")
        .expect("created runtime");
    assert_eq!(runtime.behavior_executor_capacity, Some(7));
    assert_eq!(runtime.behavior_executor_queue_depth, Some(3));
}

#[tokio::test]
async fn load_agent_tool_calls_hydrates_subagent_projection_fields() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let response = node
        .execute(
            r#"mutation {
                create_AgentToolCall(input: {
                    tool_call_key: "session-1:spawn-1",
                    request_id: "parent-1",
                    session_id: "session-1",
                    message_sequence: 1,
                    tool_name: "spawn_subagent",
                    tool_call_id: "spawn-1",
                    args: "{}",
                    result: "",
                    status: "called",
                    lifecycle_state: "running",
                    child_request_id: "child-1",
                    await_mode: "background",
                    started_at: "2026-07-29T00:00:00Z"
                }) { tool_call_key }
            }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let tool_calls = load_agent_tool_calls(node.as_ref())
        .await
        .expect("load agent tool calls");
    let tool_call = tool_calls
        .iter()
        .find(|row| row.tool_call_key == "session-1:spawn-1")
        .expect("created tool call");
    assert_eq!(tool_call.child_request_id.as_deref(), Some("child-1"));
    assert_eq!(tool_call.await_mode.as_deref(), Some("background"));
}

#[tokio::test]
async fn load_agent_scoped_snapshot_excludes_other_agents() {
    let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
    ensure_runtime_schemas(node.as_ref())
        .await
        .expect("schemas");

    let mutation = r#"mutation {
        alpha: create_AgentConversation(input: {
            session_id: "alpha-1",
            agent_did: "did:alpha",
            behavior_id: "default",
            title: "alpha",
            title_source: "user",
            preview_text: "",
            status: "active",
            created_at: "2026-05-07T00:00:00Z",
            updated_at: "2026-05-07T00:00:00Z",
            latest_request_id: ""
        }) { _docID }
        beta: create_AgentConversation(input: {
            session_id: "beta-1",
            agent_did: "did:beta",
            behavior_id: "default",
            title: "beta",
            title_source: "user",
            preview_text: "",
            status: "active",
            created_at: "2026-05-07T00:00:00Z",
            updated_at: "2026-05-07T00:00:00Z",
            latest_request_id: ""
        }) { _docID }
    }"#;
    let response = node.execute(mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let goal_mutation = r#"mutation {
        alpha: create_Goal(input: {
            goal_id: "alpha-goal",
            session_id: "alpha-goal-only",
            agent_did: "did:alpha",
            objective: "goal-only session",
            status: "active",
            created_at: "2026-05-07T00:00:00Z"
        }) { _docID }
        beta: create_Goal(input: {
            goal_id: "beta-goal",
            session_id: "beta-goal-only",
            agent_did: "did:beta",
            objective: "other agent",
            status: "active",
            created_at: "2026-05-07T00:00:00Z"
        }) { _docID }
    }"#;
    let response = node.execute(goal_mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let store = load_agent_scoped_snapshot(node.as_ref(), "did:alpha")
        .await
        .expect("load_agent_scoped_snapshot");

    let dids: Vec<&str> = store
        .conversations
        .iter()
        .filter_map(|c| c.agent_did.as_deref())
        .collect();
    assert!(
        dids.iter().all(|d| *d == "did:alpha"),
        "expected only did:alpha conversations; got {dids:?}"
    );
    assert_eq!(store.goals.len(), 1);
    assert_eq!(store.goals[0].session_id, "alpha-goal-only");
}
