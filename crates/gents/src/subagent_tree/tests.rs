use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use tokio::net::TcpListener;

use super::*;

// The end-to-end walk (cross-deployment bridge metadata, max-depth
// truncation, terminal-subtree pruning) is golden-tested against this same
// shared code path by `gents-cli`'s `http::subagent_tree` fixture tests,
// which call `build_subagent_tree` through `load_subagent_tree_snapshot`.
// This module covers what only the owner itself can exercise: the max-depth
// clamp, multi-access aggregation with partial-failure reporting (the
// bridge's cross-deployment case, which the CLI's single-access route never
// exercises), and the local embedded-node access path.

#[derive(Clone)]
struct MockGraphqlState {
    responses: Arc<Mutex<Vec<Value>>>,
}

async fn mock_graphql(
    State(state): State<MockGraphqlState>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    let mut responses = state.responses.lock().unwrap();
    let response = if responses.len() == 1 {
        responses[0].clone()
    } else {
        responses.remove(0)
    };
    Json(response)
}

async fn spawn_mock_graphql(responses: Vec<Value>) -> anyhow::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let router = Router::new()
        .route("/api/v0/graphql", post(mock_graphql))
        .with_state(MockGraphqlState {
            responses: Arc::new(Mutex::new(responses)),
        });
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(format!("http://{addr}/api/v0/graphql"))
}

/// A GraphQL endpoint address nothing is listening on. `execute_graphql_async`
/// retries a bounded number of times, so a request against it fails fast with
/// a connection error rather than hanging.
async fn dead_graphql_endpoint() -> anyhow::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    drop(listener);
    Ok(format!("http://{addr}/api/v0/graphql"))
}

fn root_response() -> Value {
    json!({
        "data": {
            "AgentRequest": [
                {
                    "request_id": "req-root",
                    "session_id": "sess-root",
                    "agent_did": "deployment-a",
                    "behavior_id": "amy-general",
                    "lifecycle_state": "processing",
                    "subagent_depth": 0,
                    "caused_by_parent_request_id": null,
                    "caused_by_parent_tool_call_id": null,
                    "backend_id": null
                }
            ]
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn canonical_root_response(
    request_id: &str,
    doc_id: &str,
    session_id: &str,
    agent_did: &str,
    behavior_id: &str,
    lifecycle_state: &str,
) -> Value {
    json!({
        "data": { "AgentRequest": [{
            "_docID": doc_id,
            "request_id": request_id,
            "agent_did": agent_did,
            "requester_did": null,
            "behavior_id": behavior_id,
            "session_id": session_id,
            "lifecycle_state": lifecycle_state,
            "caused_by_parent_request_id": null,
            "caused_by_parent_request_doc_id": null,
            "caused_by_parent_tool_call_id": null,
            "caused_by_parent_tool_call_doc_id": null
        }]}
    })
}

#[allow(clippy::too_many_arguments)]
fn canonical_bridge_row(
    doc_id: &str,
    parent_request_id: &str,
    parent_doc_id: &str,
    parent_session_id: &str,
    parent_agent_did: &str,
    tool_call_id: &str,
    child_request_id: &str,
    await_mode: &str,
    lifecycle_state: &str,
) -> Value {
    json!({
        "_docID": doc_id,
        "request_id": parent_request_id,
        "request_doc_id": parent_doc_id,
        "session_id": parent_session_id,
        "agent_did": parent_agent_did,
        "requester_did": null,
        "tool_call_id": tool_call_id,
        "args": format!(r#"{{"name":"{child_request_id}"}}"#),
        "result": if lifecycle_state == "completed" { "done" } else { "" },
        "status": lifecycle_state,
        "lifecycle_state": lifecycle_state,
        "started_at": "2026-08-01T00:00:00Z",
        "completed_at": if lifecycle_state == "completed" {
            Some("2026-08-01T00:00:01Z")
        } else {
            None
        },
        "await_mode": await_mode,
        "cancel_policy": "cascade",
        "child_request_id": child_request_id,
        "spawn_target_did": null,
        "unclaimed_deadline_at": null
    })
}

#[allow(clippy::too_many_arguments)]
fn canonical_child_row(
    doc_id: &str,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    behavior_id: &str,
    lifecycle_state: &str,
    parent_request_id: &str,
    parent_doc_id: &str,
    parent_tool_call_id: &str,
    parent_tool_doc_id: &str,
) -> Value {
    json!({
        "_docID": doc_id,
        "request_id": request_id,
        "agent_did": agent_did,
        "requester_did": null,
        "behavior_id": behavior_id,
        "session_id": session_id,
        "lifecycle_state": lifecycle_state,
        "caused_by_parent_request_id": parent_request_id,
        "caused_by_parent_request_doc_id": parent_doc_id,
        "caused_by_parent_tool_call_id": parent_tool_call_id,
        "caused_by_parent_tool_call_doc_id": parent_tool_doc_id
    })
}

fn canonical_bridges_response(rows: Vec<Value>) -> Value {
    json!({ "data": { "AgentToolCall": rows } })
}

fn canonical_children_response(rows: Vec<Value>) -> Value {
    json!({ "data": { "AgentRequest": rows } })
}

fn canonical_messages_empty() -> Value {
    json!({ "data": { "AgentMessage": [] } })
}

fn canonical_standard_walk_responses() -> Vec<Value> {
    vec![
        root_response(),
        canonical_root_response(
            "req-root",
            "doc-root",
            "sess-root",
            "deployment-a",
            "amy-general",
            "processing",
        ),
        canonical_bridges_response(vec![canonical_bridge_row(
            "doc-tc-bridge",
            "req-root",
            "doc-root",
            "sess-root",
            "deployment-a",
            "tc-bridge",
            "req-child",
            "background",
            "running",
        )]),
        canonical_children_response(vec![canonical_child_row(
            "doc-child",
            "req-child",
            "sess-child",
            "deployment-b",
            "amy-code",
            "processing",
            "req-root",
            "doc-root",
            "tc-bridge",
            "doc-tc-bridge",
        )]),
        canonical_bridges_response(Vec::new()),
        canonical_messages_empty(),
    ]
}

#[test]
fn effective_subagent_tree_max_depth_defaults_and_clamps() {
    assert_eq!(
        effective_subagent_tree_max_depth(None),
        DEFAULT_SUBAGENT_TREE_MAX_DEPTH
    );
    assert_eq!(effective_subagent_tree_max_depth(Some(3)), 3);
    assert_eq!(
        effective_subagent_tree_max_depth(Some(1_000)),
        HARD_SUBAGENT_TREE_MAX_DEPTH
    );
}

#[tokio::test]
async fn tree_aggregates_labeled_accesses_and_records_partial_error_for_dead_peer(
) -> anyhow::Result<()> {
    let local = spawn_mock_graphql(canonical_standard_walk_responses()).await?;
    let dead_peer = dead_graphql_endpoint().await?;

    let accesses = vec![
        SubagentTreeAccess {
            label: None,
            access: ConfigAccess::Graphql(local),
        },
        SubagentTreeAccess {
            label: Some("peer-b".to_string()),
            access: ConfigAccess::Graphql(dead_peer),
        },
    ];

    let tree = build_subagent_tree(&accesses, "req-root", false, 4).await?;

    assert_eq!(tree.nodes.len(), 2, "the healthy access still resolves");
    assert!(
        !tree.truncated,
        "the healthy access's shallow tree is not truncated"
    );
    assert_eq!(
        tree.partial_errors.len(),
        1,
        "the dead peer should be recorded once, not once per query"
    );
    assert!(
        tree.partial_errors[0].contains("peer-b"),
        "partial error should identify the dead access by label: {:?}",
        tree.partial_errors
    );

    let root = tree
        .nodes
        .iter()
        .find(|node| node.request_id == "req-root")
        .expect("root node");
    assert_eq!(
        root.resolved_via, None,
        "root resolved through the unlabeled local access"
    );
    let child = tree
        .nodes
        .iter()
        .find(|node| node.request_id == "req-child")
        .expect("child node");
    assert_eq!(child.agent_did.as_deref(), Some("deployment-b"));

    Ok(())
}

async fn create_root_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    behavior_id: &str,
) {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let agent_did = escape_graphql_string(agent_did);
    let behavior_id = escape_graphql_string(behavior_id);
    let now = chrono::Utc::now().to_rfc3339();
    let response = node
        .execute(&format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    session_id: "{session_id}",
                    retry_parent_request: "",
                    retry_root_request: "{request_id}",
                    superseded_by_request: "",
                    content: "subagent tree root fixture",
                    lifecycle_state: "processing",
                    backend_id: "",
                    execution_origin: "interactive",
                    metadata: "",
                    failure_reason: "",
                    created_at: "{now}",
                    retry_count: 0,
                    max_retries: 3,
                    subagent_depth: 0
                }}) {{ _docID }}
            }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

#[tokio::test]
async fn build_local_subagent_tree_resolves_the_root_from_the_embedded_node() -> anyhow::Result<()>
{
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    crate::schema::ensure_runtime_schemas(node.as_ref()).await?;
    create_root_request(
        node.as_ref(),
        "req-root",
        "sess-root",
        "did:test:local",
        "amy-general",
    )
    .await;

    let tree =
        build_local_subagent_tree(node, "req-root", true, DEFAULT_SUBAGENT_TREE_MAX_DEPTH).await?;

    assert_eq!(tree.root_request_id, "req-root");
    assert!(tree.partial_errors.is_empty());
    assert!(!tree.truncated);
    assert_eq!(tree.edges.len(), 0);
    assert_eq!(tree.nodes.len(), 1);
    let root = &tree.nodes[0];
    assert_eq!(root.request_id, "req-root");
    assert_eq!(root.agent_did.as_deref(), Some("did:test:local"));
    assert_eq!(root.behavior_id.as_deref(), Some("amy-general"));
    assert_eq!(root.resolved_via, None);

    Ok(())
}
