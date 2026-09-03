use std::collections::{BTreeMap, BTreeSet};

use std::sync::Arc;

use anyhow::{Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::{
    ConfigAccess, DescendantEdge, DescendantGraphAccess, DescendantQuery, MAX_DESCENDANT_PAGE_LIMIT,
};
use gents_protocol::graphql::{execute_graphql_async, GraphqlRequestOptions};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;
use serde_json::Value;

use crate::types::{SubagentEdgeView, SubagentNodeView, SubagentTreeView};

pub const DEFAULT_SUBAGENT_TREE_MAX_DEPTH: usize = 8;
pub const HARD_SUBAGENT_TREE_MAX_DEPTH: usize = 32;

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
    subagent_depth: Option<i64>,
    #[serde(default)]
    caused_by_parent_request_id: Option<String>,
    #[serde(default)]
    caused_by_parent_tool_call_id: Option<String>,
    #[serde(default)]
    backend_id: Option<String>,
}

pub fn effective_subagent_tree_max_depth(max_depth: Option<u32>) -> usize {
    max_depth
        .map(|value| (value as usize).min(HARD_SUBAGENT_TREE_MAX_DEPTH))
        .unwrap_or(DEFAULT_SUBAGENT_TREE_MAX_DEPTH)
}

pub struct SubagentTreeAccess {
    pub label: Option<String>,
    pub access: TreeQueryAccess,
}

pub enum TreeQueryAccess {
    Local(Arc<EmbeddedNode>),
    Graphql(String),
}

impl TreeQueryAccess {
    async fn execute(&self, query: &str) -> Result<Value> {
        match self {
            Self::Local(node) => {
                let response = node.execute(query).await;
                if response.has_errors() {
                    let errors = response
                        .errors
                        .iter()
                        .map(|error| error.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; ");
                    anyhow::bail!("graphql returned errors: {errors}");
                }
                Ok(serde_json::json!({
                    "data": response.data.unwrap_or(Value::Null),
                }))
            }
            Self::Graphql(endpoint) => {
                execute_graphql_async(endpoint, query, GraphqlRequestOptions::default()).await
            }
        }
    }
}

#[allow(dead_code)]
pub async fn build_local_subagent_tree(
    node: Arc<EmbeddedNode>,
    root_request_id: &str,
    include_terminal: bool,
    max_depth: usize,
) -> Result<SubagentTreeView> {
    build_subagent_tree(
        &[SubagentTreeAccess {
            label: None,
            access: TreeQueryAccess::Local(node),
        }],
        root_request_id,
        include_terminal,
        max_depth,
    )
    .await
}

pub async fn build_subagent_tree(
    accesses: &[SubagentTreeAccess],
    root_request_id: &str,
    include_terminal: bool,
    max_depth: usize,
) -> Result<SubagentTreeView> {
    let mut nodes: BTreeMap<String, SubagentNodeView> = BTreeMap::new();
    let mut partial_errors: Vec<String> = Vec::new();
    let mut dead_accesses: BTreeSet<usize> = BTreeSet::new();

    for (index, entry) in accesses.iter().enumerate() {
        match fetch_root_request(&entry.access, root_request_id).await {
            Ok(Some(root)) => {
                let mut node = request_row_into_node(root);
                node.resolved_via = entry.label.clone();
                nodes.entry(node.request_id.clone()).or_insert(node);
            }
            Ok(None) => {}
            Err(error) => {
                record_dead_access(
                    &mut partial_errors,
                    &mut dead_accesses,
                    index,
                    entry,
                    &error,
                );
            }
        }
    }

    let mut canonical =
        BTreeMap::<(String, String, String), (Option<String>, DescendantEdge)>::new();
    for (index, entry) in accesses.iter().enumerate() {
        if dead_accesses.contains(&index) {
            continue;
        }
        let access = match &entry.access {
            TreeQueryAccess::Local(node) => ConfigAccess::Local(node.clone()),
            TreeQueryAccess::Graphql(endpoint) => ConfigAccess::Graphql(endpoint.clone()),
        };
        let mut after = None;
        loop {
            let page = match gents::resolve_descendant_graph(
                DescendantGraphAccess::Config(&access),
                &DescendantQuery {
                    after: after.clone(),
                    limit: MAX_DESCENDANT_PAGE_LIMIT,
                    ..DescendantQuery::all(root_request_id)
                },
            )
            .await
            {
                Ok(page) => page,
                Err(error) => {
                    record_dead_access(
                        &mut partial_errors,
                        &mut dead_accesses,
                        index,
                        entry,
                        &error,
                    );
                    break;
                }
            };
            for edge in page.edges {
                let key = (
                    edge.immediate_parent_request_id.clone(),
                    edge.immediate_parent_tool_call_id.clone(),
                    edge.child_request_id.clone(),
                );
                match canonical.get(&key) {
                    Some((_, existing)) if existing.readable() || !edge.readable() => {}
                    _ => {
                        canonical.insert(key, (entry.label.clone(), edge));
                    }
                }
            }
            if !page.has_more {
                break;
            }
            after = page.next_cursor;
        }
    }

    let truncated = canonical.values().any(|(_, edge)| edge.depth > max_depth);
    let mut edges = Vec::new();
    for (resolved_via, edge) in canonical
        .into_values()
        .filter(|(_, edge)| edge.depth <= max_depth)
    {
        nodes.insert(
            edge.child_request_id.clone(),
            SubagentNodeView {
                request_id: edge.child_request_id.clone(),
                resolved_via,
                session_id: edge.child_session_id.clone(),
                agent_did: edge.principal_did.clone(),
                behavior_id: edge.behavior_id.clone(),
                lifecycle_state: Some(edge.lifecycle_state.clone()),
                subagent_depth: Some(edge.depth as i64),
                caused_by_parent_request_id: Some(edge.immediate_parent_request_id.clone()),
                caused_by_parent_tool_call_id: Some(edge.immediate_parent_tool_call_id.clone()),
                backend_id: None,
            },
        );
        edges.push(SubagentEdgeView {
            parent_request_id: edge.immediate_parent_request_id,
            child_request_id: edge.child_request_id,
            parent_tool_call_id: Some(edge.immediate_parent_tool_call_id),
            tool_name: Some("spawn_subagent".to_string()),
            await_mode: Some(edge.await_mode),
            cancel_policy: edge.cancel_policy,
            lifecycle_state: Some(edge.lifecycle_state),
        });
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
        partial_errors,
    })
}

fn record_dead_access(
    partial_errors: &mut Vec<String>,
    dead_accesses: &mut BTreeSet<usize>,
    index: usize,
    entry: &SubagentTreeAccess,
    error: &anyhow::Error,
) {
    if dead_accesses.insert(index) {
        let who = entry.label.as_deref().unwrap_or("local node");
        partial_errors.push(format!("{who}: {error:#}"));
    }
}

fn request_row_into_node(row: RequestRow) -> SubagentNodeView {
    SubagentNodeView {
        request_id: clean_string(&row.request_id),
        resolved_via: None,
        session_id: clean_optional_string(row.session_id.as_deref()),
        agent_did: clean_optional_string(row.agent_did.as_deref()),
        behavior_id: clean_optional_string(row.behavior_id.as_deref()),
        lifecycle_state: clean_optional_string(row.lifecycle_state.as_deref()),
        subagent_depth: row.subagent_depth,
        caused_by_parent_request_id: clean_optional_string(
            row.caused_by_parent_request_id.as_deref(),
        ),
        caused_by_parent_tool_call_id: clean_optional_string(
            row.caused_by_parent_tool_call_id.as_deref(),
        ),
        backend_id: clean_optional_string(row.backend_id.as_deref()),
    }
}

async fn fetch_root_request(
    access: &TreeQueryAccess,
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
                subagent_depth
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
                backend_id
            }}
        }}"#
    );
    let envelope: RootRequestEnvelope =
        execute_access_query(access, &query, "root request lookup").await?;
    Ok(envelope.requests.into_iter().next())
}

async fn execute_access_query<T: serde::de::DeserializeOwned>(
    access: &TreeQueryAccess,
    query: &str,
    context: &str,
) -> Result<T> {
    let response = access
        .execute(query)
        .await
        .with_context(|| format!("{context} query failed"))?;
    let data = response
        .get("data")
        .cloned()
        .with_context(|| format!("{context} response missing object data"))?;
    serde_json::from_value(data).with_context(|| format!("decoding {context}"))
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
            .map(|node| !RequestLifecycleState::is_terminal_str(node.lifecycle_state.as_deref()))
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

fn clean_optional_string(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn clean_string(value: &str) -> String {
    value.trim().to_string()
}
