use anyhow::Result;
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use gents::config_client::ConfigAccess;
use gents::subagent_tree::{
    build_subagent_tree, effective_subagent_tree_max_depth, SubagentTreeAccess,
    SubagentTreeEdge as OwnedSubagentTreeEdge, SubagentTreeNode as OwnedSubagentTreeNode,
};
use serde::{Deserialize, Serialize};

use crate::http::router::RuntimeHttpState;

const SNAPSHOT_SOURCE: &str = "graphql.subagent_tree";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentTreeSnapshot {
    pub(crate) generated_at: String,
    pub(crate) source: String,
    pub(crate) root_request_id: String,
    pub(crate) nodes: Vec<SubagentTreeNode>,
    pub(crate) edges: Vec<SubagentTreeEdge>,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentTreeNode {
    pub(crate) request_id: String,
    pub(crate) session_id: Option<String>,
    pub(crate) agent_did: Option<String>,
    pub(crate) behavior_id: Option<String>,
    pub(crate) lifecycle_state: Option<String>,
    pub(crate) subagent_depth: Option<i64>,
    pub(crate) caused_by_parent_request_id: Option<String>,
    pub(crate) caused_by_parent_tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentTreeEdge {
    pub(crate) parent_request_id: String,
    pub(crate) child_request_id: String,
    pub(crate) parent_tool_call_id: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) await_mode: Option<String>,
    pub(crate) cancel_policy: Option<String>,
    pub(crate) lifecycle_state: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct SubagentTreeQuery {
    root_request_id: Option<String>,
    #[serde(default)]
    include_terminal: Option<bool>,
    #[serde(default)]
    max_depth: Option<usize>,
}

pub(crate) async fn subagent_tree_handler(
    State(state): State<RuntimeHttpState>,
    Query(query): Query<SubagentTreeQuery>,
) -> Response {
    let root_request_id = match clean_optional_string(query.root_request_id.as_deref()) {
        Some(value) => value,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "subagent tree request missing root_request_id".to_string(),
            )
                .into_response()
        }
    };
    let include_terminal = query.include_terminal.unwrap_or(false);
    let max_depth = effective_subagent_tree_max_depth(query.max_depth.map(|value| value as u32));

    match load_subagent_tree_snapshot(
        &state.graphql,
        &root_request_id,
        include_terminal,
        max_depth,
    )
    .await
    {
        Ok(snapshot) => (StatusCode::OK, Json(snapshot)).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("subagent tree snapshot failed: {error:#}"),
        )
            .into_response(),
    }
}

/// Delegates the graph walk to the owned `gents::subagent_tree` projector
/// (#1334) against a single GraphQL access, then maps the result into this
/// route's JSON DTO. A dead access surfaces the same way an inline query
/// failure used to: as an `Err` the handler turns into a 500, since the CLI
/// has exactly one access and nothing else can answer for it.
pub(crate) async fn load_subagent_tree_snapshot(
    graphql: &str,
    root_request_id: &str,
    include_terminal: bool,
    max_depth: usize,
) -> Result<SubagentTreeSnapshot> {
    let generated_at = Utc::now();
    let accesses = [SubagentTreeAccess {
        label: None,
        access: ConfigAccess::Graphql(graphql.to_string()),
    }];
    let tree = build_subagent_tree(&accesses, root_request_id, include_terminal, max_depth).await?;
    if !tree.partial_errors.is_empty() {
        anyhow::bail!(tree.partial_errors.join("; "));
    }

    Ok(SubagentTreeSnapshot {
        generated_at: generated_at.to_rfc3339(),
        source: SNAPSHOT_SOURCE.to_string(),
        root_request_id: tree.root_request_id,
        nodes: tree.nodes.into_iter().map(subagent_tree_node_dto).collect(),
        edges: tree.edges.into_iter().map(subagent_tree_edge_dto).collect(),
        truncated: tree.truncated,
    })
}

fn subagent_tree_node_dto(node: OwnedSubagentTreeNode) -> SubagentTreeNode {
    SubagentTreeNode {
        request_id: node.request_id,
        session_id: node.session_id,
        agent_did: node.agent_did,
        behavior_id: node.behavior_id,
        lifecycle_state: node.lifecycle_state,
        subagent_depth: node.subagent_depth,
        caused_by_parent_request_id: node.caused_by_parent_request_id,
        caused_by_parent_tool_call_id: node.caused_by_parent_tool_call_id,
    }
}

fn subagent_tree_edge_dto(edge: OwnedSubagentTreeEdge) -> SubagentTreeEdge {
    SubagentTreeEdge {
        parent_request_id: edge.parent_request_id,
        child_request_id: edge.child_request_id,
        parent_tool_call_id: edge.parent_tool_call_id,
        tool_name: edge.tool_name,
        await_mode: edge.await_mode,
        cancel_policy: edge.cancel_policy,
        lifecycle_state: edge.lifecycle_state,
    }
}

fn clean_optional_string(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

#[cfg(test)]
mod tests {
    use std::{
        net::SocketAddr,
        sync::{Arc, Mutex},
    };

    use axum::{extract::State, routing::post, Router};
    use serde_json::{json, Value};
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Clone)]
    struct MockGraphqlState {
        responses: Arc<Mutex<Vec<Value>>>,
        queries: Arc<Mutex<Vec<String>>>,
    }

    async fn mock_graphql(
        State(state): State<MockGraphqlState>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        state.queries.lock().unwrap().push(query);
        let mut responses = state.responses.lock().unwrap();
        let response = if responses.len() == 1 {
            responses[0].clone()
        } else {
            responses.remove(0)
        };
        Json(response)
    }

    async fn spawn_mock_graphql(
        responses: Vec<Value>,
    ) -> anyhow::Result<(String, Arc<Mutex<Vec<String>>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let queries = Arc::new(Mutex::new(Vec::new()));
        let responses = Arc::new(Mutex::new(responses));
        let router = Router::new()
            .route("/api/v0/graphql", post(mock_graphql))
            .with_state(MockGraphqlState {
                responses,
                queries: queries.clone(),
            });
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok((format!("http://{addr}/api/v0/graphql"), queries))
    }

    async fn spawn_runtime_router(graphql: String) -> anyhow::Result<SocketAddr> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let router = crate::http::runtime_contract_router(
            graphql,
            "subagent-tree-test-agent".to_string(),
            "did:key:z6Mksubagenttree".to_string(),
            None,
            None,
            None,
            None,
            crate::http::enrollment::empty_issuer_handle(),
            crate::http::enrollment::empty_decision_service_handle(),
        );
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(addr)
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
                        "caused_by_parent_tool_call_id": null
                    }
                ]
            }
        })
    }

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

    #[tokio::test]
    async fn tree_walks_cross_deployment_bridge_and_carries_await_mode_metadata(
    ) -> anyhow::Result<()> {
        let (graphql, _queries) = spawn_mock_graphql(canonical_standard_walk_responses()).await?;
        let snapshot = load_subagent_tree_snapshot(&graphql, "req-root", false, 4).await?;

        assert_eq!(snapshot.root_request_id, "req-root");
        assert!(!snapshot.truncated, "shallow tree should not be truncated");
        assert_eq!(snapshot.nodes.len(), 2);
        assert_eq!(snapshot.edges.len(), 1);

        let root = snapshot
            .nodes
            .iter()
            .find(|node| node.request_id == "req-root")
            .expect("root node");
        assert_eq!(root.agent_did.as_deref(), Some("deployment-a"));
        assert_eq!(root.subagent_depth, Some(0));

        let child = snapshot
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

        let edge = &snapshot.edges[0];
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
                        "lifecycle_state": "processing",
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
        let (graphql, _queries) = spawn_mock_graphql(vec![
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
        let snapshot = load_subagent_tree_snapshot(&graphql, "req-root", true, 1).await?;
        assert!(snapshot.truncated, "max_depth=1 should set truncated");
        assert_eq!(snapshot.nodes.len(), 2);
        assert_eq!(snapshot.edges.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn tree_endpoint_routes_under_runtime_router() -> anyhow::Result<()> {
        let (graphql, _queries) = spawn_mock_graphql(canonical_standard_walk_responses()).await?;
        let runtime_addr = spawn_runtime_router(graphql).await?;
        let response = reqwest::Client::new()
            .get(format!(
                "http://{runtime_addr}/subagents/tree?root_request_id=req-root&include_terminal=true"
            ))
            .send()
            .await?;
        let status = response.status();
        let snapshot = response.json::<SubagentTreeSnapshot>().await?;
        assert!(status.is_success(), "unexpected status {status}");
        assert_eq!(snapshot.root_request_id, "req-root");
        assert_eq!(snapshot.nodes.len(), 2);
        assert_eq!(snapshot.edges.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn tree_endpoint_rejects_missing_root_request_id() -> anyhow::Result<()> {
        let (graphql, _queries) = spawn_mock_graphql(vec![]).await?;
        let runtime_addr = spawn_runtime_router(graphql).await?;
        let response = reqwest::Client::new()
            .get(format!("http://{runtime_addr}/subagents/tree"))
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.text().await?;
        assert!(
            body.contains("root_request_id"),
            "error body should call out missing param: {body}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tree_prunes_fully_terminal_subtrees_when_include_terminal_is_false(
    ) -> anyhow::Result<()> {
        let root = json!({
            "data": {
                "AgentRequest": [
                    {
                        "request_id": "req-root",
                        "agent_did": "deployment-a",
                        "lifecycle_state": "processing",
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
        let (graphql, _queries) = spawn_mock_graphql(vec![
            root,
            canonical_root,
            bridges,
            children,
            canonical_bridges_response(Vec::new()),
            canonical_messages_empty(),
        ])
        .await?;
        let snapshot = load_subagent_tree_snapshot(&graphql, "req-root", false, 4).await?;
        let request_ids = snapshot
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
            snapshot
                .edges
                .iter()
                .all(|edge| edge.child_request_id != "req-dead"),
            "edges into a pruned node should also be dropped"
        );
        Ok(())
    }
}
