use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::MAX_SUBAGENT_DEPTH;
use gents_codex_protocol as codex;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::bound_behavior::model_selection_id;
use super::progress::{gents_tool_call_status, GentsToolCallProgress};
use super::store::query_node_json;
use super::ShimState;

const SUBAGENT_PROJECTION_COLLECTIONS: [&str; 3] =
    ["AgentRequest", "AgentToolCall", "AgentBehavior"];

#[derive(Clone, Debug)]
pub(super) struct SubagentProjectionUpdateFilter {
    collection_ids: HashSet<String>,
    match_all_updates: bool,
}

impl SubagentProjectionUpdateFilter {
    pub(super) fn from_state(state: &ShimState) -> Self {
        let mut collection_ids = HashSet::new();
        let mut match_all_updates = false;
        for collection_name in SUBAGENT_PROJECTION_COLLECTIONS {
            match state.node.get_collection(collection_name) {
                Ok(Some(definition)) => {
                    collection_ids.insert(definition.collection_id);
                }
                Ok(None) => {
                    match_all_updates = true;
                    tracing::warn!(
                        collection_name,
                        "Codex shim could not resolve a subagent projection collection; \
                         falling back to invalidation on every document update"
                    );
                }
                Err(error) => {
                    match_all_updates = true;
                    tracing::warn!(
                        collection_name,
                        %error,
                        "Codex shim failed to resolve a subagent projection collection; \
                         falling back to invalidation on every document update"
                    );
                }
            }
        }
        Self {
            collection_ids,
            match_all_updates,
        }
    }

    pub(super) fn affects_collection_id(&self, collection_id: &str) -> bool {
        self.match_all_updates || self.collection_ids.contains(collection_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LinkedSubagentThread {
    pub(super) request_id: String,
    pub(super) latest_request_id: String,
    pub(super) latest_request_content: String,
    pub(super) latest_request_created_at: Option<String>,
    pub(super) session_id: String,
    pub(super) parent_request_id: String,
    pub(super) parent_tool_call_id: String,
    pub(super) parent_session_id: String,
    pub(super) root_session_id: String,
    pub(super) depth: u32,
    pub(super) agent_did: String,
    pub(super) behavior_id: String,
    pub(super) model: Option<String>,
    pub(super) nickname: String,
    pub(super) lifecycle_state: String,
    pub(super) failure_reason: Option<String>,
    pub(super) created_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CollabProjection {
    pub(super) status: codex::CollabAgentToolCallStatus,
    pub(super) tool: codex::CollabAgentTool,
    pub(super) receiver_thread_id: String,
    pub(super) child_model: Option<String>,
    pub(super) child_lifecycle_state: String,
    pub(super) child_failure_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct RequestRow {
    request_id: String,
    #[serde(default)]
    content: String,
    session_id: String,
    agent_did: String,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    metadata: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    failure_reason: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    subagent_depth: Option<u32>,
    #[serde(default)]
    caused_by_parent_request_id: Option<String>,
    #[serde(default)]
    caused_by_parent_tool_call_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ToolLinkRow {
    request_id: String,
    session_id: String,
    agent_did: String,
    tool_call_id: String,
    tool_name: String,
    #[serde(default)]
    child_request_id: Option<String>,
    #[serde(default)]
    spawn_target_did: Option<String>,
    #[serde(default)]
    args: String,
}

#[derive(Clone, Debug, Deserialize)]
struct BehaviorPresentationRow {
    behavior_id: String,
    #[serde(default)]
    backend_id: Option<String>,
    #[serde(default)]
    model_name: Option<String>,
}

#[derive(Clone, Debug)]
struct AuthorizedRequest {
    row_index: usize,
    root_session_id: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct RequestContextKey {
    session_id: String,
    agent_did: String,
    behavior_id: Option<String>,
    depth: u32,
}

const REQUEST_ROW_FIELDS: &str = r#"
    request_id
    content
    session_id
    agent_did
    behavior_id
    metadata
    lifecycle_state
    failure_reason
    created_at
    subagent_depth
    caused_by_parent_request_id
    caused_by_parent_tool_call_id
"#;

const TOOL_LINK_ROW_FIELDS: &str = r#"
    request_id
    session_id
    agent_did
    tool_call_id
    tool_name
    child_request_id
    spawn_target_did
    args
"#;

/// Load the subagent request graph reachable from a Codex-shim-owned root.
///
/// ACP still decides which rows are visible.  On top of that boundary, this
/// verifies both halves of every bridge edge before exposing a child session:
/// the child must point at the parent request/tool call, and that tool call must
/// point back at the child request.  This prevents `thread/read` from becoming
/// an unscoped foreign-session lookup.
pub(super) async fn load_authorized_subagent_threads(
    state: &ShimState,
) -> Result<Vec<LinkedSubagentThread>> {
    load_authorized_subagent_threads_for_roots(state, None).await
}

pub(super) async fn load_authorized_subagent_threads_for_root(
    state: &ShimState,
    root_session_id: &str,
) -> Result<Vec<LinkedSubagentThread>> {
    load_authorized_subagent_threads_for_roots(state, Some(&[root_session_id.to_string()])).await
}

pub(super) async fn load_authorized_subagent_threads_for_root_ids(
    state: &ShimState,
    root_session_ids: &[String],
) -> Result<Vec<LinkedSubagentThread>> {
    if root_session_ids.is_empty() {
        return Ok(Vec::new());
    }
    load_authorized_subagent_threads_for_roots(state, Some(root_session_ids)).await
}

async fn load_authorized_subagent_threads_for_roots(
    state: &ShimState,
    root_session_ids: Option<&[String]>,
) -> Result<Vec<LinkedSubagentThread>> {
    let response = query_node_json(
        state.node.as_ref(),
        &root_requests_query(state, root_session_ids),
    )
    .await?;
    let mut requests = decode_rows::<RequestRow>(&response, "AgentRequest")
        .context("decoding Codex root AgentRequest rows")?;
    let mut seen_request_ids = requests
        .iter()
        .map(|row| row.request_id.clone())
        .collect::<HashSet<_>>();
    let mut tools = Vec::<ToolLinkRow>::new();
    let mut seen_tool_edges = HashSet::<(String, String, String)>::new();
    let mut scanned_sessions = HashSet::<String>::new();
    let mut scanned_parent_requests = HashSet::<String>::new();

    // Walk only the graph frontier reachable from scoped, Codex-stamped roots.
    // Each linked session is scanned once; each request's spawn edges are
    // scanned once. This keeps the hot path proportional to the visible graph
    // rather than to every request and tool row on the fleet node.
    for _ in 0..=MAX_SUBAGENT_DEPTH {
        let links = resolve_authorized_subagent_threads(
            &requests,
            &tools,
            state.agent_did.as_ref(),
            state.behavior_id.as_ref(),
        );
        let mut frontier_sessions = requests
            .iter()
            .filter(|row| is_codex_root(row, &state.agent_did, &state.behavior_id))
            .map(|row| row.session_id.clone())
            .chain(links.iter().map(|link| link.session_id.clone()))
            .filter(|session_id| scanned_sessions.insert(session_id.clone()))
            .collect::<Vec<_>>();
        frontier_sessions.sort();
        frontier_sessions.dedup();

        if !frontier_sessions.is_empty() {
            let response = query_node_json(
                state.node.as_ref(),
                &requests_for_sessions_query(&frontier_sessions),
            )
            .await?;
            let rows = decode_rows::<RequestRow>(&response, "AgentRequest")
                .context("decoding linked-session AgentRequest rows")?;
            extend_unique_requests(&mut requests, &mut seen_request_ids, rows);
        }

        let mut parent_request_ids = requests
            .iter()
            .filter(|row| scanned_sessions.contains(&row.session_id))
            .map(|row| row.request_id.clone())
            .filter(|request_id| scanned_parent_requests.insert(request_id.clone()))
            .collect::<Vec<_>>();
        parent_request_ids.sort();
        parent_request_ids.dedup();

        let mut child_request_ids = Vec::<String>::new();
        if !parent_request_ids.is_empty() {
            let response = query_node_json(
                state.node.as_ref(),
                &spawn_tools_for_requests_query(&parent_request_ids),
            )
            .await?;
            for tool in decode_rows::<ToolLinkRow>(&response, "AgentToolCall")
                .context("decoding scoped spawn AgentToolCall rows")?
            {
                if let Some(child_request_id) = nonempty(tool.child_request_id.as_deref()) {
                    child_request_ids.push(child_request_id.to_string());
                }
                let key = (
                    tool.request_id.clone(),
                    tool.tool_call_id.clone(),
                    tool.child_request_id.clone().unwrap_or_default(),
                );
                if seen_tool_edges.insert(key) {
                    tools.push(tool);
                }
            }
        }

        child_request_ids.retain(|request_id| !seen_request_ids.contains(request_id));
        child_request_ids.sort();
        child_request_ids.dedup();
        if !child_request_ids.is_empty() {
            let response = query_node_json(
                state.node.as_ref(),
                &requests_by_id_query(&child_request_ids),
            )
            .await?;
            let rows = decode_rows::<RequestRow>(&response, "AgentRequest")
                .context("decoding child AgentRequest frontier")?;
            extend_unique_requests(&mut requests, &mut seen_request_ids, rows);
        }

        if frontier_sessions.is_empty()
            && parent_request_ids.is_empty()
            && child_request_ids.is_empty()
        {
            break;
        }
    }

    let mut links = resolve_authorized_subagent_threads(
        &requests,
        &tools,
        state.agent_did.as_ref(),
        state.behavior_id.as_ref(),
    );
    attach_runtime_models(state, &mut links).await;
    Ok(links)
}

async fn attach_runtime_models(state: &ShimState, links: &mut [LinkedSubagentThread]) {
    let mut behavior_ids = links
        .iter()
        .map(|link| link.behavior_id.clone())
        .filter(|behavior_id| !behavior_id.trim().is_empty())
        .collect::<Vec<_>>();
    behavior_ids.sort_unstable();
    behavior_ids.dedup();
    if behavior_ids.is_empty() {
        return;
    }

    let query = format!(
        r#"{{
            AgentBehavior(filter: {{ behavior_id: {{ _in: [{}] }} }}) {{
                behavior_id
                backend_id
                model_name
            }}
        }}"#,
        graphql_string_list(behavior_ids.iter().map(String::as_str)),
    );
    let response = match query_node_json(state.node.as_ref(), &query).await {
        Ok(response) => response,
        Err(error) => {
            tracing::debug!(%error, "unable to load child behavior model metadata");
            return;
        }
    };
    let rows = match decode_rows::<BehaviorPresentationRow>(&response, "AgentBehavior") {
        Ok(rows) => rows,
        Err(error) => {
            tracing::debug!(%error, "unable to decode child behavior model metadata");
            return;
        }
    };
    let models = rows
        .into_iter()
        .filter_map(|row| {
            let model_name = nonempty(row.model_name.as_deref())?;
            let model = nonempty(row.backend_id.as_deref())
                .map(|backend_id| model_selection_id(backend_id, model_name))
                .unwrap_or_else(|| model_name.to_string());
            Some((row.behavior_id, model))
        })
        .collect::<HashMap<_, _>>();
    for link in links {
        link.model = models.get(&link.behavior_id).cloned();
    }
}

fn root_requests_query(state: &ShimState, root_session_ids: Option<&[String]>) -> String {
    let session_filter = root_session_ids
        .filter(|ids| !ids.is_empty())
        .map(|ids| {
            format!(
                ", session_id: {{ _in: [{}] }}",
                graphql_string_list(ids.iter().map(String::as_str))
            )
        })
        .unwrap_or_default();
    format!(
        r#"{{
            AgentRequest(
                filter: {{
                    agent_did: {{ _eq: "{agent_did}" }},
                    behavior_id: {{ _eq: "{behavior_id}" }},
                    execution_origin: {{ _eq: "interactive" }}{session_filter}
                }},
                order: {{ created_at: ASC }}
            ) {{ {REQUEST_ROW_FIELDS} }}
        }}"#,
        agent_did = escape_graphql_string(&state.agent_did),
        behavior_id = escape_graphql_string(&state.behavior_id),
    )
}

fn requests_for_sessions_query(session_ids: &[String]) -> String {
    format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _in: [{}] }} }},
                order: {{ created_at: ASC }}
            ) {{ {REQUEST_ROW_FIELDS} }}
        }}"#,
        graphql_string_list(session_ids.iter().map(String::as_str)),
    )
}

fn spawn_tools_for_requests_query(request_ids: &[String]) -> String {
    format!(
        r#"{{
            AgentToolCall(filter: {{
                request_id: {{ _in: [{}] }},
                tool_name: {{ _eq: "spawn_subagent" }},
                child_request_id: {{ _ne: "" }}
            }}) {{ {TOOL_LINK_ROW_FIELDS} }}
        }}"#,
        graphql_string_list(request_ids.iter().map(String::as_str)),
    )
}

fn requests_by_id_query(request_ids: &[String]) -> String {
    format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _in: [{}] }} }}) {{
                {REQUEST_ROW_FIELDS}
            }}
        }}"#,
        graphql_string_list(request_ids.iter().map(String::as_str)),
    )
}

fn graphql_string_list<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    values
        .into_iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn extend_unique_requests(
    requests: &mut Vec<RequestRow>,
    seen_request_ids: &mut HashSet<String>,
    rows: Vec<RequestRow>,
) {
    requests.extend(
        rows.into_iter()
            .filter(|row| seen_request_ids.insert(row.request_id.clone())),
    );
}

fn resolve_authorized_subagent_threads(
    requests: &[RequestRow],
    tools: &[ToolLinkRow],
    shim_agent_did: &str,
    shim_behavior_id: &str,
) -> Vec<LinkedSubagentThread> {
    let roots = requests
        .iter()
        .enumerate()
        .filter(|(_, row)| is_codex_root(row, shim_agent_did, shim_behavior_id))
        .map(|(row_index, row)| AuthorizedRequest {
            row_index,
            root_session_id: row.session_id.clone(),
        })
        .collect::<Vec<_>>();
    let mut authorized = roots
        .iter()
        .map(|entry| (entry.row_index, entry.root_session_id.clone()))
        .collect::<HashMap<_, _>>();
    let mut authorized_contexts = roots
        .iter()
        .map(|entry| {
            (
                request_context_key(&requests[entry.row_index]),
                entry.root_session_id.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    let request_indices_by_id = requests.iter().enumerate().fold(
        HashMap::<String, Vec<usize>>::new(),
        |mut by_id, (index, row)| {
            by_id.entry(row.request_id.clone()).or_default().push(index);
            by_id
        },
    );
    let tools_by_parent_call = tools.iter().fold(
        HashMap::<(String, String, String, String), Vec<&ToolLinkRow>>::new(),
        |mut by_call, tool| {
            by_call
                .entry((
                    tool.request_id.clone(),
                    tool.session_id.clone(),
                    tool.agent_did.clone(),
                    tool.tool_call_id.clone(),
                ))
                .or_default()
                .push(tool);
            by_call
        },
    );
    let mut links = Vec::new();

    for _ in 0..MAX_SUBAGENT_DEPTH {
        let mut added = false;

        for (row_index, row) in requests.iter().enumerate() {
            if authorized.contains_key(&row_index) {
                continue;
            }
            if let Some(root_session_id) =
                authorized_contexts.get(&request_context_key(row)).cloned()
            {
                authorized.insert(row_index, root_session_id);
                added = true;
            }
        }

        for (row_index, child) in requests.iter().enumerate() {
            if authorized.contains_key(&row_index) {
                continue;
            }
            let Some(parent_request_id) = nonempty(child.caused_by_parent_request_id.as_deref())
            else {
                continue;
            };
            let Some(parent_tool_call_id) =
                nonempty(child.caused_by_parent_tool_call_id.as_deref())
            else {
                continue;
            };
            let child_depth = child.subagent_depth.unwrap_or_default();
            if child_depth == 0 || child_depth > MAX_SUBAGENT_DEPTH {
                continue;
            }
            if Uuid::parse_str(&child.session_id).is_err() {
                continue;
            }

            let Some((parent_index, root_session_id, tool)) = request_indices_by_id
                .get(parent_request_id)
                .into_iter()
                .flatten()
                .find_map(|parent_index| {
                    let root_session_id = authorized.get(parent_index)?;
                    let parent = &requests[*parent_index];
                    (parent.subagent_depth.unwrap_or_default() + 1 == child_depth).then_some(())?;
                    let key = (
                        parent.request_id.clone(),
                        parent.session_id.clone(),
                        parent.agent_did.clone(),
                        parent_tool_call_id.to_string(),
                    );
                    let tool = tools_by_parent_call.get(&key)?.iter().find(|tool| {
                        tool.tool_name == "spawn_subagent"
                            && nonempty(tool.child_request_id.as_deref())
                                == Some(child.request_id.as_str())
                            && nonempty(tool.spawn_target_did.as_deref())
                                .is_none_or(|target| target == child.agent_did)
                    })?;
                    Some((*parent_index, root_session_id.clone(), *tool))
                })
            else {
                continue;
            };
            let parent = &requests[parent_index];
            let behavior_id = nonempty(child.behavior_id.as_deref())
                .unwrap_or("subagent")
                .to_string();
            let nickname = spawn_nickname(&tool.args).unwrap_or_else(|| behavior_id.clone());
            links.push(LinkedSubagentThread {
                request_id: child.request_id.clone(),
                latest_request_id: child.request_id.clone(),
                latest_request_content: child.content.clone(),
                latest_request_created_at: child.created_at.clone(),
                session_id: child.session_id.clone(),
                parent_request_id: parent.request_id.clone(),
                parent_tool_call_id: parent_tool_call_id.to_string(),
                parent_session_id: parent.session_id.clone(),
                root_session_id: root_session_id.clone(),
                depth: child_depth,
                agent_did: child.agent_did.clone(),
                behavior_id,
                model: None,
                nickname,
                lifecycle_state: nonempty(child.lifecycle_state.as_deref())
                    .unwrap_or("")
                    .to_string(),
                failure_reason: child
                    .failure_reason
                    .as_deref()
                    .and_then(|value| nonempty(Some(value)))
                    .map(ToOwned::to_owned),
                created_at: child.created_at.clone(),
            });
            authorized.insert(row_index, root_session_id.clone());
            authorized_contexts.insert(request_context_key(child), root_session_id);
            added = true;
        }
        if !added {
            break;
        }
    }

    links.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    let mut seen_sessions = HashSet::new();
    links.retain(|link| seen_sessions.insert(link.session_id.clone()));
    let latest_by_context = requests.iter().enumerate().fold(
        HashMap::<RequestContextKey, usize>::new(),
        |mut latest, (index, row)| {
            let key = request_context_key(row);
            let replace = latest.get(&key).is_none_or(|previous| {
                let previous = &requests[*previous];
                (&row.created_at, &row.request_id) > (&previous.created_at, &previous.request_id)
            });
            if replace {
                latest.insert(key, index);
            }
            latest
        },
    );
    let requests_by_id = requests
        .iter()
        .map(|row| (row.request_id.as_str(), row))
        .collect::<HashMap<_, _>>();
    for link in &mut links {
        let Some(spawn_request) = requests_by_id.get(link.request_id.as_str()) else {
            continue;
        };
        let Some(latest_index) = latest_by_context.get(&request_context_key(spawn_request)) else {
            continue;
        };
        let latest = &requests[*latest_index];
        link.latest_request_id = latest.request_id.clone();
        link.latest_request_content = latest.content.clone();
        link.latest_request_created_at = latest.created_at.clone();
        link.lifecycle_state = nonempty(latest.lifecycle_state.as_deref())
            .unwrap_or("")
            .to_string();
        link.failure_reason = latest
            .failure_reason
            .as_deref()
            .and_then(|value| nonempty(Some(value)))
            .map(ToOwned::to_owned);
    }
    links
}

fn request_context_key(row: &RequestRow) -> RequestContextKey {
    RequestContextKey {
        session_id: row.session_id.clone(),
        agent_did: row.agent_did.clone(),
        behavior_id: nonempty(row.behavior_id.as_deref()).map(ToOwned::to_owned),
        depth: row.subagent_depth.unwrap_or_default(),
    }
}

fn is_codex_root(row: &RequestRow, agent_did: &str, behavior_id: &str) -> bool {
    row.agent_did == agent_did
        && nonempty(row.behavior_id.as_deref()) == Some(behavior_id)
        && row.subagent_depth.unwrap_or_default() == 0
        && nonempty(row.caused_by_parent_request_id.as_deref()).is_none()
        && nonempty(row.caused_by_parent_tool_call_id.as_deref()).is_none()
        && row
            .metadata
            .as_deref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .is_some_and(|metadata| metadata.get("codex_shim").is_some())
}

fn spawn_nickname(args: &str) -> Option<String> {
    serde_json::from_str::<Value>(args)
        .ok()?
        .get("name")?
        .as_str()
        .and_then(|value| nonempty(Some(value)))
        .map(ToOwned::to_owned)
}

fn decode_rows<T>(response: &Value, collection: &str) -> serde_json::Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    response
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(serde_json::from_value)
        .collect()
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(super) fn is_subagent_control_tool(tool_name: &str) -> bool {
    collab_tool(tool_name).is_some()
}

fn collab_tool(tool_name: &str) -> Option<codex::CollabAgentTool> {
    match tool_name {
        "spawn_subagent" => Some(codex::CollabAgentTool::SpawnAgent),
        "wait_subagent" => Some(codex::CollabAgentTool::Wait),
        "steer_subagent" => Some(codex::CollabAgentTool::SendInput),
        "cancel_subagent" => Some(codex::CollabAgentTool::CloseAgent),
        _ => None,
    }
}

pub(super) fn attach_subagent_link(
    tool: &mut GentsToolCallProgress,
    links: &[LinkedSubagentThread],
) {
    let Some(child_request_id) = tool_child_request_id(tool) else {
        return;
    };
    tool.subagent_link = links
        .iter()
        .find(|link| link.request_id == child_request_id)
        .cloned();
}

fn tool_child_request_id(tool: &GentsToolCallProgress) -> Option<String> {
    nonempty(tool.child_request_id.as_deref())
        .map(ToOwned::to_owned)
        .or_else(|| {
            serde_json::from_str::<Value>(&tool.args)
                .ok()?
                .get("child_request_id")?
                .as_str()
                .and_then(|value| nonempty(Some(value)))
                .map(ToOwned::to_owned)
        })
}

pub(super) fn collab_projection(tool: &GentsToolCallProgress) -> Option<CollabProjection> {
    let collab_tool = collab_tool(&tool.tool_name)?;
    let link = tool.subagent_link.as_ref()?;
    let runtime_status = gents_tool_call_status(tool);
    let status = match (&collab_tool, runtime_status) {
        (_, codex::McpToolCallStatus::Failed) => codex::CollabAgentToolCallStatus::Failed,
        (codex::CollabAgentTool::SpawnAgent, _) => {
            // The reciprocal edge proves the spawn operation succeeded. GENTS
            // deliberately keeps the bridge row running while a background
            // child works; Codex represents that child lifecycle separately
            // in agentsStates, so the collaboration operation is complete.
            codex::CollabAgentToolCallStatus::Completed
        }
        (_, codex::McpToolCallStatus::InProgress) => codex::CollabAgentToolCallStatus::InProgress,
        (_, codex::McpToolCallStatus::Completed) => codex::CollabAgentToolCallStatus::Completed,
    };
    Some(CollabProjection {
        status,
        tool: collab_tool,
        receiver_thread_id: link.session_id.clone(),
        child_model: link.model.clone(),
        child_lifecycle_state: link.lifecycle_state.clone(),
        child_failure_reason: link.failure_reason.clone(),
    })
}

pub(super) fn collab_agent_status(lifecycle_state: &str) -> codex::CollabAgentStatus {
    match lifecycle_state.trim() {
        "pending" => codex::CollabAgentStatus::PendingInit,
        "claimed" | "processing" | "inputRequired" => codex::CollabAgentStatus::Running,
        "completed" => codex::CollabAgentStatus::Completed,
        "failed" | "dead" => codex::CollabAgentStatus::Errored,
        "superseded" | "interrupted" => codex::CollabAgentStatus::Interrupted,
        _ => codex::CollabAgentStatus::NotFound,
    }
}

pub(super) fn collab_tool_item(
    sender_thread_id: &str,
    tool: &GentsToolCallProgress,
    projection: &CollabProjection,
) -> codex::ThreadItem {
    let prompt = serde_json::from_str::<Value>(&tool.args)
        .ok()
        .and_then(|args| match projection.tool {
            codex::CollabAgentTool::SpawnAgent => args
                .get("prompt")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            codex::CollabAgentTool::SendInput => args
                .get("message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            _ => None,
        });
    let mut agents_states = HashMap::new();
    agents_states.insert(
        projection.receiver_thread_id.clone(),
        codex::CollabAgentState {
            status: collab_agent_status(&projection.child_lifecycle_state),
            message: projection.child_failure_reason.clone(),
        },
    );
    codex::ThreadItem::CollabAgentToolCall {
        id: tool.tool_call_key.clone(),
        tool: projection.tool.clone(),
        status: projection.status.clone(),
        sender_thread_id: sender_thread_id.to_string(),
        receiver_thread_ids: vec![projection.receiver_thread_id.clone()],
        prompt,
        model: (projection.tool == codex::CollabAgentTool::SpawnAgent)
            .then(|| projection.child_model.clone())
            .flatten(),
        reasoning_effort: None,
        agents_states,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(
        request_id: &str,
        session_id: &str,
        depth: u32,
        parent_request_id: Option<&str>,
        parent_tool_call_id: Option<&str>,
    ) -> RequestRow {
        RequestRow {
            request_id: request_id.to_string(),
            content: format!("content for {request_id}"),
            session_id: session_id.to_string(),
            agent_did: if depth == 0 { "did:root" } else { "did:child" }.to_string(),
            behavior_id: Some(if depth == 0 { "root" } else { "reviewer" }.to_string()),
            metadata: (depth == 0).then(|| r#"{"codex_shim":{"cwd":"/tmp"}}"#.to_string()),
            lifecycle_state: Some(
                if depth == 0 {
                    "processing"
                } else {
                    "completed"
                }
                .to_string(),
            ),
            failure_reason: None,
            created_at: None,
            subagent_depth: Some(depth),
            caused_by_parent_request_id: parent_request_id.map(ToOwned::to_owned),
            caused_by_parent_tool_call_id: parent_tool_call_id.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn only_symmetric_edges_reachable_from_codex_roots_are_exposed() {
        let root_session = Uuid::new_v4().to_string();
        let child_session = Uuid::new_v4().to_string();
        let orphan_session = Uuid::new_v4().to_string();
        let requests = vec![
            request("root-request", &root_session, 0, None, None),
            request(
                "child-request",
                &child_session,
                1,
                Some("root-request"),
                Some("spawn-call"),
            ),
            request(
                "orphan-request",
                &orphan_session,
                1,
                Some("root-request"),
                Some("missing-call"),
            ),
        ];
        let tools = vec![ToolLinkRow {
            request_id: "root-request".to_string(),
            session_id: root_session.clone(),
            agent_did: "did:root".to_string(),
            tool_call_id: "spawn-call".to_string(),
            tool_name: "spawn_subagent".to_string(),
            child_request_id: Some("child-request".to_string()),
            spawn_target_did: Some("did:child".to_string()),
            args: r#"{"name":"reviewer"}"#.to_string(),
        }];

        let links = resolve_authorized_subagent_threads(&requests, &tools, "did:root", "root");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].session_id, child_session);
        assert_eq!(links[0].root_session_id, root_session);
        assert_eq!(links[0].nickname, "reviewer");
    }

    #[test]
    fn nested_spawn_from_later_request_in_linked_session_is_exposed() {
        let root_session = Uuid::new_v4().to_string();
        let child_session = Uuid::new_v4().to_string();
        let grandchild_session = Uuid::new_v4().to_string();
        let requests = vec![
            request("root-request", &root_session, 0, None, None),
            request(
                "grandchild-request",
                &grandchild_session,
                2,
                Some("child-followup"),
                Some("nested-spawn-call"),
            ),
            request(
                "child-request",
                &child_session,
                1,
                Some("root-request"),
                Some("spawn-call"),
            ),
            request("child-followup", &child_session, 1, None, None),
        ];
        let tools = vec![
            ToolLinkRow {
                request_id: "root-request".to_string(),
                session_id: root_session.clone(),
                agent_did: "did:root".to_string(),
                tool_call_id: "spawn-call".to_string(),
                tool_name: "spawn_subagent".to_string(),
                child_request_id: Some("child-request".to_string()),
                spawn_target_did: Some("did:child".to_string()),
                args: r#"{"name":"reviewer"}"#.to_string(),
            },
            ToolLinkRow {
                request_id: "child-followup".to_string(),
                session_id: child_session.clone(),
                agent_did: "did:child".to_string(),
                tool_call_id: "nested-spawn-call".to_string(),
                tool_name: "spawn_subagent".to_string(),
                child_request_id: Some("grandchild-request".to_string()),
                spawn_target_did: Some("did:child".to_string()),
                args: r#"{"name":"nested-reviewer"}"#.to_string(),
            },
        ];

        let links = resolve_authorized_subagent_threads(&requests, &tools, "did:root", "root");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].session_id, child_session);
        assert_eq!(links[1].session_id, grandchild_session);
        assert_eq!(links[1].parent_session_id, child_session);
        assert_eq!(links[1].root_session_id, root_session);
    }

    #[test]
    fn link_status_tracks_latest_request_in_authorized_child_session() {
        let root_session = Uuid::new_v4().to_string();
        let child_session = Uuid::new_v4().to_string();
        let mut child = request(
            "child-request",
            &child_session,
            1,
            Some("root-request"),
            Some("spawn-call"),
        );
        child.created_at = Some("2026-01-01T00:00:01Z".to_string());
        child.lifecycle_state = Some("completed".to_string());
        let mut followup = request("child-followup", &child_session, 1, None, None);
        followup.created_at = Some("2026-01-01T00:00:02Z".to_string());
        followup.lifecycle_state = Some("processing".to_string());
        let requests = vec![
            request("root-request", &root_session, 0, None, None),
            child,
            followup,
        ];
        let tools = vec![ToolLinkRow {
            request_id: "root-request".to_string(),
            session_id: root_session,
            agent_did: "did:root".to_string(),
            tool_call_id: "spawn-call".to_string(),
            tool_name: "spawn_subagent".to_string(),
            child_request_id: Some("child-request".to_string()),
            spawn_target_did: Some("did:child".to_string()),
            args: r#"{"name":"reviewer"}"#.to_string(),
        }];

        let links = resolve_authorized_subagent_threads(&requests, &tools, "did:root", "root");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].request_id, "child-request");
        assert_eq!(links[0].latest_request_id, "child-followup");
        assert_eq!(
            links[0].latest_request_content,
            "content for child-followup"
        );
        assert_eq!(links[0].lifecycle_state, "processing");
    }

    #[test]
    fn graph_frontier_queries_are_scoped_and_escape_values() {
        let sessions = vec!["session-a".to_string(), "session-\"b".to_string()];
        let requests = vec!["request-a".to_string(), "request-\"b".to_string()];
        let session_query = requests_for_sessions_query(&sessions);
        assert!(session_query.contains("session_id: { _in:"));
        assert!(session_query.contains(r#""session-\"b""#));

        let tool_query = spawn_tools_for_requests_query(&requests);
        assert!(tool_query.contains("request_id: { _in:"));
        assert!(tool_query.contains(r#"tool_name: { _eq: "spawn_subagent" }"#));
        assert!(tool_query.contains(r#"child_request_id: { _ne: "" }"#));
        assert!(tool_query.contains(r#""request-\"b""#));
    }

    #[test]
    fn lean_fenced_tool_and_status_mappings_match_codex_protocol() {
        assert_eq!(
            collab_tool("spawn_subagent"),
            Some(codex::CollabAgentTool::SpawnAgent)
        );
        assert_eq!(
            collab_tool("wait_subagent"),
            Some(codex::CollabAgentTool::Wait)
        );
        assert_eq!(
            collab_tool("steer_subagent"),
            Some(codex::CollabAgentTool::SendInput)
        );
        assert_eq!(
            collab_tool("cancel_subagent"),
            Some(codex::CollabAgentTool::CloseAgent)
        );
        assert_eq!(collab_tool("list_subagents"), None);
        assert_eq!(collab_tool("read_subagent"), None);

        assert_eq!(
            collab_agent_status("pending"),
            codex::CollabAgentStatus::PendingInit
        );
        assert_eq!(
            collab_agent_status("processing"),
            codex::CollabAgentStatus::Running
        );
        assert_eq!(
            collab_agent_status("completed"),
            codex::CollabAgentStatus::Completed
        );
        assert_eq!(
            collab_agent_status("failed"),
            codex::CollabAgentStatus::Errored
        );
        assert_eq!(
            collab_agent_status("interrupted"),
            codex::CollabAgentStatus::Interrupted
        );
    }

    #[test]
    fn spawn_item_uses_child_session_as_receiver_thread() {
        let child_session_id = Uuid::new_v4().to_string();
        let mut tool = GentsToolCallProgress {
            tool_call_key: "parent:spawn-call".to_string(),
            tool_name: "spawn_subagent".to_string(),
            status: "running".to_string(),
            lifecycle_state: Some("running".to_string()),
            await_mode: Some("background".to_string()),
            child_request_id: Some("child-request".to_string()),
            args: r#"{"name":"reviewer","prompt":"Inspect the patch"}"#.to_string(),
            result: String::new(),
            subagent_link: None,
            ..Default::default()
        };
        tool.subagent_link = Some(LinkedSubagentThread {
            request_id: "child-request".to_string(),
            latest_request_id: "child-request".to_string(),
            latest_request_content: "Inspect the patch".to_string(),
            latest_request_created_at: None,
            session_id: child_session_id.clone(),
            parent_request_id: "parent-request".to_string(),
            parent_tool_call_id: "spawn-call".to_string(),
            parent_session_id: Uuid::new_v4().to_string(),
            root_session_id: Uuid::new_v4().to_string(),
            depth: 1,
            agent_did: "did:child".to_string(),
            behavior_id: "code-review".to_string(),
            model: Some("child-model".to_string()),
            nickname: "reviewer".to_string(),
            lifecycle_state: "processing".to_string(),
            failure_reason: None,
            created_at: None,
        });
        let projection = collab_projection(&tool).expect("authorized spawn projection");
        assert_eq!(
            projection.status,
            codex::CollabAgentToolCallStatus::Completed,
            "the linked spawn operation completes while agentsStates tracks the running child"
        );
        let item = collab_tool_item("parent-thread", &tool, &projection);
        let value = serde_json::to_value(&item).expect("serialize collab item");
        serde_json::from_value::<codex::ThreadItem>(value.clone())
            .expect("collab projection must be a valid pinned Codex ThreadItem");

        assert_eq!(value["type"], "collabAgentToolCall");
        assert_eq!(value["tool"], "spawnAgent");
        assert_eq!(value["status"], "completed");
        assert_eq!(value["senderThreadId"], "parent-thread");
        assert_eq!(value["receiverThreadIds"], json!([child_session_id]));
        assert_eq!(value["prompt"], "Inspect the patch");
        assert_eq!(value["model"], "child-model");
        assert_eq!(value["reasoningEffort"], Value::Null);
        assert_eq!(
            value.pointer(&format!(
                "/agentsStates/{}/status",
                tool.subagent_link
                    .as_ref()
                    .expect("link")
                    .session_id
                    .replace('~', "~0")
                    .replace('/', "~1")
            )),
            Some(&Value::String("running".to_string()))
        );

        let mut completed = tool.clone();
        let link = completed.subagent_link.as_mut().expect("link");
        link.lifecycle_state = "completed".to_string();
        let completed_projection = collab_projection(&completed).expect("completed projection");
        assert_ne!(
            projection, completed_projection,
            "child lifecycle changes must refresh agentsStates even after the tool status settles"
        );
    }

    #[test]
    fn subagent_update_filter_ignores_unrelated_document_updates() {
        let filter = SubagentProjectionUpdateFilter {
            collection_ids: HashSet::from([
                "agent-request-id".to_string(),
                "agent-tool-call-id".to_string(),
                "agent-behavior-id".to_string(),
            ]),
            match_all_updates: false,
        };

        assert!(filter.affects_collection_id("agent-request-id"));
        assert!(filter.affects_collection_id("agent-tool-call-id"));
        assert!(filter.affects_collection_id("agent-behavior-id"));
        assert!(!filter.affects_collection_id("agent-message-id"));
        assert!(!filter.affects_collection_id("inference-call-id"));
    }

    #[test]
    fn incomplete_subagent_update_filter_fails_open() {
        let filter = SubagentProjectionUpdateFilter {
            collection_ids: HashSet::new(),
            match_all_updates: true,
        };

        assert!(filter.affects_collection_id("any-collection-id"));
    }
}
