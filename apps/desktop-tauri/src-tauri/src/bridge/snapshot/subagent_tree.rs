use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use serde::Deserialize;
use serde_json::Value;

use crate::bridge::types::{SubagentEdgeView, SubagentNodeView, SubagentTreeView};

pub(crate) const DEFAULT_SUBAGENT_TREE_MAX_DEPTH: usize = 8;
pub(crate) const HARD_SUBAGENT_TREE_MAX_DEPTH: usize = 32;

const TERMINAL_STATES: &[&str] = &[
    "completed",
    "failed",
    "cancelled",
    "interrupted",
    "superseded",
    "dead",
];

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

pub(crate) fn effective_subagent_tree_max_depth(max_depth: Option<u32>) -> usize {
    max_depth
        .map(|value| (value as usize).min(HARD_SUBAGENT_TREE_MAX_DEPTH))
        .unwrap_or(DEFAULT_SUBAGENT_TREE_MAX_DEPTH)
}

pub(crate) async fn build_local_subagent_tree(
    node: &EmbeddedNode,
    root_request_id: &str,
    include_terminal: bool,
    max_depth: usize,
) -> Result<SubagentTreeView> {
    let mut nodes: BTreeMap<String, SubagentNodeView> = BTreeMap::new();
    let mut edges: Vec<SubagentEdgeView> = Vec::new();
    let mut seen_edges: BTreeSet<(String, String)> = BTreeSet::new();

    if let Some(root) = fetch_root_request(node, root_request_id).await? {
        nodes.insert(root.request_id.clone(), request_row_into_node(root));
    }

    let mut frontier: VecDeque<String> = VecDeque::from([root_request_id.to_string()]);
    let mut depth = 0;
    let mut truncated = false;

    while !frontier.is_empty() && depth < max_depth {
        let level = frontier.drain(..).collect::<Vec<_>>();
        let envelope = fetch_level(node, &level).await?;

        for request in envelope.requests {
            if request.request_id.trim().is_empty() {
                continue;
            }
            let node = request_row_into_node(request);
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
            edges.push(SubagentEdgeView {
                parent_request_id,
                child_request_id: child_request_id.clone(),
                parent_tool_call_id: clean_optional_string(bridge.tool_call_id.as_deref()),
                tool_name: clean_optional_string(bridge.tool_name.as_deref()),
                await_mode,
                cancel_policy,
                lifecycle_state: bridge_state,
            });
            next_frontier.insert(child_request_id);
        }

        frontier.extend(next_frontier);
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

    Ok(SubagentTreeView {
        root_request_id: root_request_id.to_string(),
        nodes: nodes.into_values().collect(),
        edges,
        truncated,
    })
}

fn request_row_into_node(row: RequestRow) -> SubagentNodeView {
    SubagentNodeView {
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

async fn fetch_root_request(
    node: &EmbeddedNode,
    root_request_id: &str,
) -> Result<Option<RequestRow>> {
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
    let envelope: RootRequestEnvelope =
        execute_local_query(node, &query, "root request lookup").await?;
    Ok(envelope.requests.into_iter().next())
}

async fn fetch_level(
    node: &EmbeddedNode,
    parent_request_ids: &[String],
) -> Result<LevelQueryEnvelope> {
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
    execute_local_query(node, &query, "subagent tree level fetch").await
}

async fn execute_local_query<T: serde::de::DeserializeOwned>(
    node: &EmbeddedNode,
    query: &str,
    context: &str,
) -> Result<T> {
    let response = node.execute(query).await;
    if response.has_errors() {
        let errors = response
            .errors
            .iter()
            .map(|error| error.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::bail!("{context} query failed: {errors}");
    }
    let data: Value = response
        .data
        .with_context(|| format!("{context} response missing object data"))?;
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
    nodes: &mut BTreeMap<String, SubagentNodeView>,
    edges: &mut Vec<SubagentEdgeView>,
    root_request_id: &str,
) {
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
        nodes: &BTreeMap<String, SubagentNodeView>,
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
