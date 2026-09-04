use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use tokio::net::TcpListener;

use super::*;

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
                    "lifecycle_state": "Processing",
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

fn single_access(graphql: String) -> Vec<SubagentTreeAccess> {
    vec![SubagentTreeAccess {
        label: None,
        access: ConfigAccess::Graphql(graphql),
    }]
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
async fn tree_walks_cross_deployment_bridge_and_carries_metadata() -> anyhow::Result<()> {
    let graphql = spawn_mock_graphql(canonical_standard_walk_responses()).await?;
    let tree = build_subagent_tree(&single_access(graphql), "req-root", false, 4).await?;

    assert_eq!(tree.root_request_id, "req-root");
    assert!(!tree.truncated, "shallow tree should not be truncated");
    assert!(tree.partial_errors.is_empty());
    assert_eq!(tree.nodes.len(), 2);
    assert_eq!(tree.edges.len(), 1);

    let root = tree
        .nodes
        .iter()
        .find(|node| node.request_id == "req-root")
        .expect("root node");
    assert_eq!(root.agent_did.as_deref(), Some("deployment-a"));
    assert_eq!(root.subagent_depth, Some(0));
    assert_eq!(root.resolved_via, None);

    let child = tree
        .nodes
        .iter()
        .find(|node| node.request_id == "req-child")
        .expect("child node");
    assert_eq!(child.agent_did.as_deref(), Some("deployment-b"));
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some("req-root")
    );
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some("tc-bridge")
    );

    let edge = &tree.edges[0];
    assert_eq!(edge.parent_request_id, "req-root");
    assert_eq!(edge.child_request_id, "req-child");
    assert_eq!(edge.tool_name.as_deref(), Some("spawn_subagent"));
    assert_eq!(edge.await_mode.as_deref(), Some("background"));
    assert_eq!(edge.cancel_policy.as_deref(), Some("cascade"));
    assert_eq!(edge.lifecycle_state.as_deref(), Some("running"));

    Ok(())
}

#[tokio::test]
async fn tree_respects_max_depth_and_sets_truncated_flag() -> anyhow::Result<()> {
    let root = json!({
        "data": {
            "AgentRequest": [
                {
                    "request_id": "req-root",
                    "agent_did": "deployment-a",
                    "lifecycle_state": "Processing",
                    "subagent_depth": 0
                }
            ]
        }
    });
    let canonical_root = canonical_root_response(
        "req-root",
        "doc-root",
        "sess-root",
        "deployment-a",
        "amy-general",
        "processing",
    );
    let canonical_level_one = canonical_bridges_response(vec![canonical_bridge_row(
        "doc-tc-a",
        "req-root",
        "doc-root",
        "sess-root",
        "deployment-a",
        "tc-a",
        "req-a",
        "background",
        "running",
    )]);
    let canonical_child_a = canonical_children_response(vec![canonical_child_row(
        "doc-a",
        "req-a",
        "sess-a",
        "deployment-a",
        "amy-code",
        "processing",
        "req-root",
        "doc-root",
        "tc-a",
        "doc-tc-a",
    )]);
    let canonical_level_two = canonical_bridges_response(vec![canonical_bridge_row(
        "doc-tc-b",
        "req-a",
        "doc-a",
        "sess-a",
        "deployment-a",
        "tc-b",
        "req-b",
        "foreground",
        "running",
    )]);
    let canonical_child_b = canonical_children_response(vec![canonical_child_row(
        "doc-b",
        "req-b",
        "sess-b",
        "deployment-a",
        "amy-review",
        "processing",
        "req-a",
        "doc-a",
        "tc-b",
        "doc-tc-b",
    )]);
    let graphql = spawn_mock_graphql(vec![
        root,
        canonical_root,
        canonical_level_one,
        canonical_child_a,
        canonical_level_two,
        canonical_child_b,
        canonical_bridges_response(Vec::new()),
        canonical_messages_empty(),
    ])
    .await?;
    let tree = build_subagent_tree(&single_access(graphql), "req-root", true, 1).await?;
    assert!(tree.truncated, "max_depth=1 should set truncated");
    assert_eq!(tree.nodes.len(), 2);
    assert_eq!(tree.edges.len(), 1);
    Ok(())
}

#[tokio::test]
async fn tree_prunes_fully_terminal_subtrees_when_include_terminal_is_false() -> anyhow::Result<()>
{
    let root = json!({
        "data": {
            "AgentRequest": [
                {
                    "request_id": "req-root",
                    "agent_did": "deployment-a",
                    "lifecycle_state": "Processing",
                    "subagent_depth": 0
                }
            ]
        }
    });
    let canonical_root = canonical_root_response(
        "req-root",
        "doc-root",
        "sess-root",
        "deployment-a",
        "amy-general",
        "processing",
    );
    let bridges = canonical_bridges_response(vec![
        canonical_bridge_row(
            "doc-tc-live",
            "req-root",
            "doc-root",
            "sess-root",
            "deployment-a",
            "tc-live",
            "req-live",
            "background",
            "running",
        ),
        canonical_bridge_row(
            "doc-tc-dead",
            "req-root",
            "doc-root",
            "sess-root",
            "deployment-a",
            "tc-dead",
            "req-dead",
            "foreground",
            "completed",
        ),
    ]);
    let children = canonical_children_response(vec![
        canonical_child_row(
            "doc-live",
            "req-live",
            "sess-live",
            "deployment-a",
            "amy-code",
            "processing",
            "req-root",
            "doc-root",
            "tc-live",
            "doc-tc-live",
        ),
        canonical_child_row(
            "doc-dead",
            "req-dead",
            "sess-dead",
            "deployment-a",
            "amy-code",
            "completed",
            "req-root",
            "doc-root",
            "tc-dead",
            "doc-tc-dead",
        ),
    ]);
    let graphql = spawn_mock_graphql(vec![
        root,
        canonical_root,
        bridges,
        children,
        canonical_bridges_response(Vec::new()),
        canonical_messages_empty(),
    ])
    .await?;
    let tree = build_subagent_tree(&single_access(graphql), "req-root", false, 4).await?;
    let request_ids = tree
        .nodes
        .iter()
        .map(|node| node.request_id.as_str())
        .collect::<Vec<_>>();
    assert!(request_ids.contains(&"req-root"));
    assert!(request_ids.contains(&"req-live"));
    assert!(
        !request_ids.contains(&"req-dead"),
        "terminal request without live descendants should be pruned"
    );
    assert!(
        tree.edges
            .iter()
            .all(|edge| edge.child_request_id != "req-dead"),
        "edges into a pruned node should also be dropped"
    );
    Ok(())
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
