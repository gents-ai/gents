//! `GET /subagents/tree` — walks the parent → child request graph rooted at a
//! single `root_request_id`, returning a tree shape the desktop bridge can
//! render directly. Edges carry the `spawn_subagent` bridge metadata
//! (`await_mode`, `cancel_policy`, `tool_name`) so the panel can label the
//! routing between request nodes without a second query.
//!
//! Companion to `/subagents/dispatches` (see [`crate::http::r5_dispatch`]):
//! that endpoint returns immediate children of one parent and is the canonical
//! source for the `subagents-cross-deployment` Rust consumer. This handler
//! exists to spare the desktop bridge from issuing N+1 round trips against the
//! one-level endpoint when it just wants the closure rooted at a turn.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result};
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use defra_agent::graphql::escape_graphql_string;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{http::router::RuntimeHttpState, post_graphql};

const SNAPSHOT_SOURCE: &str = "graphql.subagent_tree";
const DEFAULT_MAX_DEPTH: usize = 8;
const HARD_MAX_DEPTH: usize = 32;
const TERMINAL_STATES: &[&str] = &[
    "completed",
    "failed",
    "cancelled",
    "interrupted",
    "superseded",
    "dead",
];

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
    pub(crate) status: Option<String>,
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

#[derive(Debug, Deserialize)]
struct LevelQueryEnvelope {
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
    #[serde(rename = "AgentToolCall", default)]
    bridges: Vec<BridgeRow>,
}

#[derive(Debug, Deserialize)]
struct RootRequestEnvelope {
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<RequestRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct RequestRow {
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    subagent_depth: Option<i64>,
    #[serde(default)]
    caused_by_parent_request_id: Option<String>,
    #[serde(default)]
    caused_by_parent_tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BridgeRow {
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    args: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    child_request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpawnSubagentArgs {
    #[serde(default)]
    await_mode: Option<String>,
    #[serde(default)]
    cancel_policy: Option<String>,
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
    let max_depth = query
        .max_depth
        .map(|value| value.min(HARD_MAX_DEPTH))
        .unwrap_or(DEFAULT_MAX_DEPTH);

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

pub(crate) async fn load_subagent_tree_snapshot(
    graphql: &str,
    root_request_id: &str,
    include_terminal: bool,
    max_depth: usize,
) -> Result<SubagentTreeSnapshot> {
    let generated_at = Utc::now();

    let mut nodes: BTreeMap<String, SubagentTreeNode> = BTreeMap::new();
    let mut edges: Vec<SubagentTreeEdge> = Vec::new();
    let mut seen_edges: BTreeSet<(String, String)> = BTreeSet::new();

    if let Some(root) = fetch_root_request(graphql, root_request_id).await? {
        nodes.insert(root.request_id.clone(), request_row_into_node(root));
    }

    let mut frontier: VecDeque<String> = VecDeque::new();
    frontier.push_back(root_request_id.to_string());

    let mut depth: usize = 0;
    let mut truncated = false;

    while !frontier.is_empty() && depth < max_depth {
        let level: Vec<String> = frontier.drain(..).collect();
        let envelope = fetch_level(graphql, &level).await?;

        for request in envelope.requests {
            if request.request_id.is_empty() {
                continue;
            }
            let node = request_row_into_node(request);
            // Prefer keeping the first node we resolved for a request id; the
            // root fetch already populated the root before the loop started.
            nodes.entry(node.request_id.clone()).or_insert(node);
        }

        let mut next_frontier: BTreeSet<String> = BTreeSet::new();
        for bridge in envelope.bridges {
            let parent_request_id = clean_string(&bridge.request_id);
            let child_request_id = match clean_optional_string(bridge.child_request_id.as_deref()) {
                Some(value) => value,
                None => continue,
            };
            if parent_request_id.is_empty() {
                continue;
            }
            if !seen_edges.insert((parent_request_id.clone(), child_request_id.clone())) {
                continue;
            }
            let (await_mode, cancel_policy) = parse_spawn_args(bridge.args.as_deref());
            let bridge_state = clean_optional_string(bridge.lifecycle_state.as_deref())
                .or_else(|| clean_optional_string(bridge.status.as_deref()));
            edges.push(SubagentTreeEdge {
                parent_request_id,
                child_request_id: child_request_id.clone(),
                parent_tool_call_id: clean_optional_string(bridge.tool_call_id.as_deref()),
                tool_name: clean_optional_string(bridge.tool_name.as_deref()),
                await_mode,
                cancel_policy,
                lifecycle_state: bridge_state,
            });
            if !nodes.contains_key(&child_request_id) {
                next_frontier.insert(child_request_id);
            } else {
                // We already have the request row; still walk descendants in
                // case the local cache was populated by an earlier sibling
                // bridge but its subtree has not been explored yet.
                next_frontier.insert(child_request_id);
            }
        }

        for child in next_frontier {
            frontier.push_back(child);
        }
        depth += 1;
    }

    if !frontier.is_empty() {
        truncated = true;
    }

    if !include_terminal {
        prune_terminal_subtrees(&mut nodes, &mut edges, root_request_id);
    }

    edges.sort_by(|left, right| {
        (
            left.parent_request_id.as_str(),
            left.child_request_id.as_str(),
        )
            .cmp(&(
                right.parent_request_id.as_str(),
                right.child_request_id.as_str(),
            ))
    });

    let nodes = nodes.into_values().collect::<Vec<_>>();

    Ok(SubagentTreeSnapshot {
        generated_at: generated_at.to_rfc3339(),
        source: SNAPSHOT_SOURCE.to_string(),
        root_request_id: root_request_id.to_string(),
        nodes,
        edges,
        truncated,
    })
}

fn request_row_into_node(row: RequestRow) -> SubagentTreeNode {
    SubagentTreeNode {
        request_id: clean_string(&row.request_id),
        session_id: clean_optional_string(row.session_id.as_deref()),
        agent_did: clean_optional_string(row.agent_did.as_deref()),
        behavior_id: clean_optional_string(row.behavior_id.as_deref()),
        lifecycle_state: clean_optional_string(row.lifecycle_state.as_deref()),
        status: clean_optional_string(row.status.as_deref()),
        subagent_depth: row.subagent_depth,
        caused_by_parent_request_id: clean_optional_string(
            row.caused_by_parent_request_id.as_deref(),
        ),
        caused_by_parent_tool_call_id: clean_optional_string(
            row.caused_by_parent_tool_call_id.as_deref(),
        ),
    }
}

async fn fetch_root_request(graphql: &str, root_request_id: &str) -> Result<Option<RequestRow>> {
    let escaped = escape_graphql_string(root_request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                agent_did
                behavior_id
                lifecycle_state
                status
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#
    );
    let response = post_graphql(graphql, &query).await?;
    let envelope: RootRequestEnvelope = decode_data_object(response, "root request lookup")?;
    Ok(envelope.requests.into_iter().next())
}

async fn fetch_level(graphql: &str, parent_request_ids: &[String]) -> Result<LevelQueryEnvelope> {
    let list = parent_request_ids
        .iter()
        .map(|value| format!("\"{}\"", escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_parent_request_id: {{ _in: [{list}] }} }},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}]
            ) {{
                request_id
                session_id
                agent_did
                behavior_id
                lifecycle_state
                status
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
            AgentToolCall(
                filter: {{
                    _and: [
                        {{ request_id: {{ _in: [{list}] }} }},
                        {{ tool_name: {{ _eq: "spawn_subagent" }} }},
                        {{ child_request_id: {{ _ne: "" }} }}
                    ]
                }},
                order: [{{ started_at: ASC }}, {{ child_request_id: ASC }}]
            ) {{
                request_id
                tool_call_id
                tool_name
                args
                status
                lifecycle_state
                child_request_id
            }}
        }}"#
    );
    let response = post_graphql(graphql, &query).await?;
    decode_data_object(response, "subagent tree level fetch")
}

fn decode_data_object<T: serde::de::DeserializeOwned>(response: Value, context: &str) -> Result<T> {
    let data = response
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| format!("{context} response missing object data: {response}"))?;
    serde_json::from_value(data).with_context(|| format!("decoding {context}"))
}

fn parse_spawn_args(args: Option<&str>) -> (Option<String>, Option<String>) {
    let args = match args {
        Some(value) => value.trim(),
        None => return (None, None),
    };
    if args.is_empty() {
        return (None, None);
    }
    let parsed = match serde_json::from_str::<SpawnSubagentArgs>(args) {
        Ok(parsed) => parsed,
        Err(_) => return (None, None),
    };
    (
        clean_optional_string(parsed.await_mode.as_deref()),
        clean_optional_string(parsed.cancel_policy.as_deref()),
    )
}

fn prune_terminal_subtrees(
    nodes: &mut BTreeMap<String, SubagentTreeNode>,
    edges: &mut Vec<SubagentTreeEdge>,
    root_request_id: &str,
) {
    // Drop request nodes whose entire subtree (including themselves) is
    // terminal. The root is always retained so the panel can render an empty
    // tree placeholder rooted at the caller's turn.
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for edge in edges.iter() {
        children
            .entry(edge.parent_request_id.clone())
            .or_default()
            .push(edge.child_request_id.clone());
    }

    let mut keep: BTreeSet<String> = BTreeSet::new();
    fn mark(
        request_id: &str,
        nodes: &BTreeMap<String, SubagentTreeNode>,
        children: &BTreeMap<String, Vec<String>>,
        keep: &mut BTreeSet<String>,
    ) -> bool {
        let live_self = nodes
            .get(request_id)
            .map(|node| !lifecycle_is_terminal(node.lifecycle_state.as_deref()))
            .unwrap_or(false);
        let mut keep_self = live_self;
        if let Some(child_ids) = children.get(request_id) {
            for child_id in child_ids {
                if mark(child_id, nodes, children, keep) {
                    keep_self = true;
                }
            }
        }
        if keep_self {
            keep.insert(request_id.to_string());
        }
        keep_self
    }
    mark(root_request_id, nodes, &children, &mut keep);
    keep.insert(root_request_id.to_string());

    nodes.retain(|request_id, _| keep.contains(request_id));
    edges.retain(|edge| {
        keep.contains(&edge.parent_request_id) && keep.contains(&edge.child_request_id)
    });
}

fn lifecycle_is_terminal(value: Option<&str>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let value = value.trim().to_ascii_lowercase();
    TERMINAL_STATES.iter().any(|terminal| value == *terminal)
}

fn clean_optional_string(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn clean_string(value: &str) -> String {
    value.trim().to_string()
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
                        "lifecycle_state": "Processing",
                        "status": "processing",
                        "subagent_depth": 0,
                        "caused_by_parent_request_id": null,
                        "caused_by_parent_tool_call_id": null
                    }
                ]
            }
        })
    }

    fn level_one_response() -> Value {
        json!({
            "data": {
                "AgentRequest": [
                    {
                        "request_id": "req-child",
                        "session_id": "sess-child",
                        "agent_did": "deployment-b",
                        "behavior_id": "amy-code",
                        "lifecycle_state": "Processing",
                        "status": "processing",
                        "subagent_depth": 1,
                        "caused_by_parent_request_id": "req-root",
                        "caused_by_parent_tool_call_id": "tc-bridge"
                    }
                ],
                "AgentToolCall": [
                    {
                        "request_id": "req-root",
                        "tool_call_id": "tc-bridge",
                        "tool_name": "spawn_subagent",
                        "args": "{\"await_mode\":\"background\",\"cancel_policy\":\"cascade\"}",
                        "status": "running",
                        "lifecycle_state": "running",
                        "child_request_id": "req-child"
                    }
                ]
            }
        })
    }

    fn level_two_empty() -> Value {
        json!({
            "data": {
                "AgentRequest": [],
                "AgentToolCall": []
            }
        })
    }

    #[tokio::test]
    async fn tree_walks_cross_deployment_bridge_and_carries_await_mode_metadata(
    ) -> anyhow::Result<()> {
        let (graphql, _queries) = spawn_mock_graphql(vec![
            root_response(),
            level_one_response(),
            level_two_empty(),
        ])
        .await?;
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
        // Build a 3-level chain: root -> a -> b -> c. We cap max_depth at 1 so
        // only root -> a should land in the tree, with truncated = true.
        let root = json!({
            "data": {
                "AgentRequest": [
                    {
                        "request_id": "req-root",
                        "agent_did": "deployment-a",
                        "lifecycle_state": "Processing",
                        "status": "processing",
                        "subagent_depth": 0
                    }
                ]
            }
        });
        let level_one = json!({
            "data": {
                "AgentRequest": [
                    {
                        "request_id": "req-a",
                        "agent_did": "deployment-a",
                        "lifecycle_state": "Processing",
                        "status": "processing",
                        "subagent_depth": 1,
                        "caused_by_parent_request_id": "req-root",
                        "caused_by_parent_tool_call_id": "tc-a"
                    }
                ],
                "AgentToolCall": [
                    {
                        "request_id": "req-root",
                        "tool_call_id": "tc-a",
                        "tool_name": "spawn_subagent",
                        "args": "{\"await_mode\":\"background\",\"cancel_policy\":\"cascade\"}",
                        "lifecycle_state": "running",
                        "child_request_id": "req-a"
                    }
                ]
            }
        });
        let (graphql, _queries) = spawn_mock_graphql(vec![root, level_one]).await?;
        let snapshot = load_subagent_tree_snapshot(&graphql, "req-root", true, 1).await?;
        assert!(snapshot.truncated, "max_depth=1 should set truncated");
        assert_eq!(snapshot.nodes.len(), 2);
        assert_eq!(snapshot.edges.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn tree_endpoint_routes_under_runtime_router() -> anyhow::Result<()> {
        let (graphql, _queries) = spawn_mock_graphql(vec![
            root_response(),
            level_one_response(),
            level_two_empty(),
        ])
        .await?;
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
                        "lifecycle_state": "Processing",
                        "status": "processing",
                        "subagent_depth": 0
                    }
                ]
            }
        });
        let level_one = json!({
            "data": {
                "AgentRequest": [
                    {
                        "request_id": "req-live",
                        "agent_did": "deployment-a",
                        "lifecycle_state": "Processing",
                        "subagent_depth": 1,
                        "caused_by_parent_request_id": "req-root",
                        "caused_by_parent_tool_call_id": "tc-live"
                    },
                    {
                        "request_id": "req-dead",
                        "agent_did": "deployment-a",
                        "lifecycle_state": "Completed",
                        "subagent_depth": 1,
                        "caused_by_parent_request_id": "req-root",
                        "caused_by_parent_tool_call_id": "tc-dead"
                    }
                ],
                "AgentToolCall": [
                    {
                        "request_id": "req-root",
                        "tool_call_id": "tc-live",
                        "tool_name": "spawn_subagent",
                        "args": "{\"await_mode\":\"background\",\"cancel_policy\":\"cascade\"}",
                        "lifecycle_state": "running",
                        "child_request_id": "req-live"
                    },
                    {
                        "request_id": "req-root",
                        "tool_call_id": "tc-dead",
                        "tool_name": "spawn_subagent",
                        "args": "{\"await_mode\":\"foreground\",\"cancel_policy\":\"cascade\"}",
                        "lifecycle_state": "completed",
                        "child_request_id": "req-dead"
                    }
                ]
            }
        });
        let level_two_empty = json!({
            "data": {
                "AgentRequest": [],
                "AgentToolCall": []
            }
        });
        let (graphql, _queries) =
            spawn_mock_graphql(vec![root, level_one, level_two_empty]).await?;
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
