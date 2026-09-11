use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::MAX_SUBAGENT_DEPTH;
use gents_codex_protocol as codex;
use gents_protocol::client_protocol::{
    project_persisted_attempt, ClientHeadProjection, ClientTurnState, RequestLifecycleState,
};
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::bound_behavior::load_bound_model_selection_id;
use super::progress::{observed_tool_status, GentsToolCallProgress};
use super::projection_state::{ChildStatus, CollabProjection, CollabTool, ProjectionStatus};
use super::store::query_node_json;
use super::ShimState;

const SUBAGENT_PROJECTION_COLLECTIONS: [&str; 6] = [
    "AgentRequest",
    "AgentResponse",
    "AgentToolCall",
    "AgentBehavior",
    "InferenceProfile",
    "InferenceBackend",
];

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
    pub(super) request_doc_id: String,
    pub(super) requester_did: Option<String>,
    pub(super) latest_request_doc_id: String,
    pub(super) latest_request_id: String,
    pub(super) latest_request_content: String,
    pub(super) latest_request_created_at: Option<String>,
    pub(super) session_id: String,
    pub(super) parent_request_id: String,
    pub(super) parent_request_doc_id: String,
    pub(super) parent_agent_did: String,
    pub(super) parent_requester_did: Option<String>,
    pub(super) parent_tool_call_id: String,
    pub(super) parent_session_id: String,
    pub(super) root_session_id: String,
    pub(super) depth: u32,
    pub(super) agent_did: String,
    pub(super) behavior_id: String,
    pub(super) model: Option<String>,
    pub(super) nickname: String,
    pub(super) client_projection: Option<ClientHeadProjection>,
    pub(super) failure_reason: Option<String>,
    pub(super) created_at: Option<String>,
}

#[derive(Clone, Debug)]
struct RequestProjectionRow(AgentRequestRow);

impl std::ops::Deref for RequestProjectionRow {
    type Target = AgentRequestRow;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for RequestProjectionRow {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl RequestProjectionRow {
    fn decode(row: Value) -> Result<Self> {
        let row: AgentRequestRow =
            serde_json::from_value(row).context("decoding canonical AgentRequest row")?;
        row.doc_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .context("AgentRequest row missing physical identity")?;
        row.session_id
            .as_deref()
            .context("AgentRequest row missing session_id")?;
        row.agent_did
            .as_deref()
            .context("AgentRequest row missing agent_did")?;
        if let Some(depth) = row.subagent_depth {
            u32::try_from(depth).context("AgentRequest subagent_depth is outside u32 range")?;
        }
        Ok(Self(row))
    }

    fn session_id(&self) -> &str {
        self.0
            .session_id
            .as_deref()
            .expect("request projection validates session_id")
    }

    fn agent_did(&self) -> &str {
        self.0
            .agent_did
            .as_deref()
            .expect("request projection validates agent_did")
    }

    fn content(&self) -> &str {
        self.0.content.as_deref().unwrap_or_default()
    }

    fn depth(&self) -> u32 {
        self.0
            .subagent_depth
            .map(u32::try_from)
            .transpose()
            .expect("request projection validates subagent_depth")
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ToolLinkRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_doc_id: String,
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
struct ResponseRow {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Clone, Debug)]
struct AuthorizedRequest {
    row_index: usize,
    root_session_id: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct RequestContextKey {
    requester_did: Option<String>,
    session_id: String,
    agent_did: String,
    behavior_id: Option<String>,
    depth: u32,
}

const REQUEST_ROW_FIELDS: &str = r#"
    _docID
    requester_did
    request_id
    content
    session_id
    agent_did
    behavior_id
    lifecycle_state
    superseded_by_request
    failure_reason
    created_at
    subagent_depth
    caused_by_parent_request_id
    caused_by_parent_request_doc_id
    caused_by_parent_tool_call_id
    caused_by_parent_tool_call_doc_id
"#;

const TOOL_LINK_ROW_FIELDS: &str = r#"
    _docID
    request_doc_id
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
    let mut requests =
        decode_request_rows(&response).context("decoding Codex root AgentRequest rows")?;
    let mut seen_request_doc_ids = requests
        .iter()
        .map(|row| row.doc_id.clone().expect("validated physical request"))
        .collect::<HashSet<_>>();
    let mut tools = Vec::<ToolLinkRow>::new();
    let mut seen_tool_edges = HashSet::<(String, String, String)>::new();
    let mut scanned_sessions = HashSet::<RequestContextKey>::new();
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
        )?;
        let frontier_sessions = requests
            .iter()
            .filter(|row| is_projectable_root(row, &state.agent_did, &state.behavior_id))
            .map(request_context_key)
            .chain(links.iter().map(|link| RequestContextKey {
                session_id: link.session_id.clone(),
                agent_did: link.agent_did.clone(),
                requester_did: link.requester_did.clone(),
                behavior_id: Some(link.behavior_id.clone()),
                depth: link.depth,
            }))
            .filter(|session_id| scanned_sessions.insert(session_id.clone()))
            .collect::<Vec<_>>();

        if !frontier_sessions.is_empty() {
            let response = query_node_json(
                state.node.as_ref(),
                &requests_for_sessions_query(&frontier_sessions),
            )
            .await?;
            let rows = decode_request_rows(&response)
                .context("decoding linked-session AgentRequest rows")?;
            extend_unique_requests(&mut requests, &mut seen_request_doc_ids, rows);
        }

        let mut parent_request_ids = requests
            .iter()
            .filter(|row| scanned_sessions.contains(&request_context_key(row)))
            .map(|row| row.doc_id.clone().expect("validated physical parent"))
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
                    tool.request_doc_id.clone(),
                    tool.doc_id.clone(),
                    tool.child_request_id.clone().unwrap_or_default(),
                );
                if seen_tool_edges.insert(key) {
                    tools.push(tool);
                }
            }
        }

        child_request_ids.sort();
        child_request_ids.dedup();
        if !child_request_ids.is_empty() {
            let response = query_node_json(
                state.node.as_ref(),
                &requests_by_id_query(&child_request_ids),
            )
            .await?;
            let rows =
                decode_request_rows(&response).context("decoding child AgentRequest frontier")?;
            extend_unique_requests(&mut requests, &mut seen_request_doc_ids, rows);
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
    )?;
    attach_canonical_request_heads(state, &requests, &mut links).await?;
    attach_runtime_models(state, &mut links).await?;
    Ok(links)
}

async fn attach_canonical_request_heads(
    state: &ShimState,
    _requests: &[RequestProjectionRow],
    links: &mut [LinkedSubagentThread],
) -> Result<()> {
    for link in links {
        let owner = link.agent_did.clone();
        let session = link.session_id.clone();
        let requester = link.requester_did.clone();
        let updated = gents::config_client::ConfigAccess::transact_local(&state.node,None,"codex.subagent.head",|txn| {
            let owner = owner.clone(); let session=session.clone(); let requester=requester.clone();
            Box::pin(async move {
                let Some(head) = gents::session::load_latest_request_in_txn(txn,&owner,&session,Some(requester.as_deref())).await? else { return Ok(None); };
                let scope = gents::session::session_scope_filter(&owner,&session,requester.as_deref());
                let physical = escape_graphql_string(&head.observed.request_doc_id);
                let response = txn.execute(&format!(r#"{{AgentRequest(filter:{{{scope},_docID:{{_eq:"{physical}"}}}}){{{REQUEST_ROW_FIELDS}}} AgentResponse(filter:{{{scope},request_doc_id:{{_eq:"{physical}"}}}}){{status}}}}"#)).await?;
                let mut requests = decode_request_rows(&response)?;
                let responses = decode_rows::<ResponseRow>(&response,"AgentResponse")?;
                anyhow::ensure!(requests.len()==1 && responses.len()<=1,"ambiguous subagent physical head");
                Ok(Some((requests.remove(0),responses.into_iter().next())))
            })
        }).await?;
        let Some((latest, response)) = updated else {
            anyhow::bail!("authorized subagent head disappeared");
        };
        anyhow::ensure!(
            latest.behavior_id.as_deref() == Some(link.behavior_id.as_str())
                && latest.depth() == link.depth,
            "subagent head crossed behavior/depth"
        );
        apply_latest_request(link, &latest);
        link.client_projection = project_persisted_attempt(
            latest
                .lifecycle_state
                .map(|state| state.as_str())
                .unwrap_or(""),
            nonempty(latest.superseded_by_request.as_deref()).is_some(),
            response.as_ref().and_then(|row| row.status.as_deref()),
        );
    }
    Ok(())
}

async fn attach_runtime_models(
    state: &ShimState,
    links: &mut [LinkedSubagentThread],
) -> Result<()> {
    let mut models = HashMap::new();
    for link in links {
        let key = (link.agent_did.clone(), link.behavior_id.clone());
        if !models.contains_key(&key) {
            models.insert(
                key.clone(),
                load_bound_model_selection_id(&state.node, &key.0, &key.1).await?,
            );
        }
        link.model = models.get(&key).cloned();
    }
    Ok(())
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
                    requester_did: {{ _eq: "{agent_did}" }},
                    execution_origin: {{ _eq: "interactive" }}{session_filter}
                }},
                order: {{ created_at: ASC }}
            ) {{ {REQUEST_ROW_FIELDS} }}
        }}"#,
        agent_did = escape_graphql_string(&state.agent_did),
        behavior_id = escape_graphql_string(&state.behavior_id),
    )
}

fn requests_for_sessions_query(scopes: &[RequestContextKey]) -> String {
    let filters = scopes
        .iter()
        .map(|scope| {
            format!(
                "{{{}}}",
                gents::session::session_scope_filter(
                    &scope.agent_did,
                    &scope.session_id,
                    scope.requester_did.as_deref()
                )
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{AgentRequest(filter:{{_or:[{filters}]}},order:{{created_at:ASC}}){{{REQUEST_ROW_FIELDS}}}}}")
}

fn spawn_tools_for_requests_query(request_ids: &[String]) -> String {
    format!(
        r#"{{
            AgentToolCall(filter: {{
                request_doc_id: {{ _in: [{}] }},
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
    requests: &mut Vec<RequestProjectionRow>,
    seen_request_doc_ids: &mut HashSet<String>,
    rows: Vec<RequestProjectionRow>,
) {
    requests.extend(rows.into_iter().filter(|row| {
        seen_request_doc_ids.insert(row.doc_id.clone().expect("validated physical request"))
    }));
}

fn resolve_authorized_subagent_threads(
    requests: &[RequestProjectionRow],
    tools: &[ToolLinkRow],
    shim_agent_did: &str,
    shim_behavior_id: &str,
) -> Result<Vec<LinkedSubagentThread>> {
    let roots = requests
        .iter()
        .enumerate()
        .filter(|(_, row)| is_projectable_root(row, shim_agent_did, shim_behavior_id))
        .map(|(row_index, row)| AuthorizedRequest {
            row_index,
            root_session_id: row.session_id().to_string(),
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
            let child_depth = child.depth();
            if child_depth == 0 || child_depth > MAX_SUBAGENT_DEPTH {
                continue;
            }
            if Uuid::parse_str(child.session_id()).is_err() {
                continue;
            }

            let Some((parent_index, root_session_id, tool)) = request_indices_by_id
                .get(parent_request_id)
                .into_iter()
                .flatten()
                .find_map(|parent_index| {
                    let root_session_id = authorized.get(parent_index)?;
                    let parent = &requests[*parent_index];
                    (parent.depth() + 1 == child_depth).then_some(())?;
                    (child.requester_did.as_deref() == Some(parent.agent_did())
                        && child.caused_by_parent_request_doc_id == parent.doc_id)
                        .then_some(())?;
                    let key = (
                        parent.request_id.clone(),
                        parent.session_id().to_string(),
                        parent.agent_did().to_string(),
                        parent_tool_call_id.to_string(),
                    );
                    let tool = tools_by_parent_call.get(&key)?.iter().find(|tool| {
                        tool.tool_name == "spawn_subagent"
                            && Some(tool.request_doc_id.as_str()) == parent.doc_id.as_deref()
                            && Some(tool.doc_id.as_str())
                                == child.caused_by_parent_tool_call_doc_id.as_deref()
                            && nonempty(tool.child_request_id.as_deref())
                                == Some(child.request_id.as_str())
                            && nonempty(tool.spawn_target_did.as_deref())
                                .is_none_or(|target| target == child.agent_did())
                    })?;
                    Some((*parent_index, root_session_id.clone(), *tool))
                })
            else {
                continue;
            };
            let parent = &requests[parent_index];
            let Some(behavior_id) = child
                .behavior_id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            let nickname = spawn_nickname(&tool.args).unwrap_or_else(|| behavior_id.clone());
            links.push(LinkedSubagentThread {
                request_id: child.request_id.clone(),
                request_doc_id: child
                    .doc_id
                    .clone()
                    .expect("validated physical child identity"),
                latest_request_doc_id: child
                    .doc_id
                    .clone()
                    .expect("validated physical child identity"),
                requester_did: child.requester_did.clone(),
                latest_request_id: child.request_id.clone(),
                latest_request_content: child.content().to_string(),
                latest_request_created_at: child.created_at.clone(),
                session_id: child.session_id().to_string(),
                parent_request_id: parent.request_id.clone(),
                parent_request_doc_id: parent.doc_id.clone().expect("verified physical parent"),
                parent_agent_did: parent.agent_did().to_owned(),
                parent_requester_did: parent.requester_did.clone(),
                parent_tool_call_id: parent_tool_call_id.to_string(),
                parent_session_id: parent.session_id().to_string(),
                root_session_id: root_session_id.clone(),
                depth: child_depth,
                agent_did: child.agent_did().to_string(),
                behavior_id,
                model: None,
                nickname,
                client_projection: project_persisted_attempt(
                    child.lifecycle_state.map(|s| s.as_str()).unwrap_or(""),
                    nonempty(child.superseded_by_request.as_deref()).is_some(),
                    None,
                ),
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
    let mut seen_sessions = HashMap::new();
    let mut unique = Vec::new();
    for link in links {
        let scope = (link.agent_did.clone(), link.requester_did.clone());
        for root in &roots {
            let row = &requests[root.row_index];
            if row.session_id() == link.session_id {
                anyhow::ensure!(
                    row.agent_did() == link.agent_did && row.requester_did == link.requester_did,
                    "ambiguous Codex root/child thread label across canonical scopes: {}",
                    link.session_id
                );
            }
        }
        if let Some(existing) = seen_sessions.get(&link.session_id) {
            anyhow::ensure!(
                existing == &scope,
                "ambiguous Codex thread label across canonical session scopes: {}",
                link.session_id
            );
        } else {
            seen_sessions.insert(link.session_id.clone(), scope);
            unique.push(link);
        }
    }
    Ok(unique)
}

fn apply_latest_request(link: &mut LinkedSubagentThread, latest: &RequestProjectionRow) {
    link.latest_request_id = latest.request_id.clone();
    link.latest_request_doc_id = latest
        .doc_id
        .clone()
        .expect("validated physical request identity");
    link.latest_request_content = latest.content().to_string();
    link.latest_request_created_at = latest.created_at.clone();
    link.client_projection = project_persisted_attempt(
        latest.lifecycle_state.map(|s| s.as_str()).unwrap_or(""),
        nonempty(latest.superseded_by_request.as_deref()).is_some(),
        None,
    );
    link.failure_reason = latest
        .failure_reason
        .as_deref()
        .and_then(|value| nonempty(Some(value)))
        .map(ToOwned::to_owned);
}

fn request_context_key(row: &RequestProjectionRow) -> RequestContextKey {
    RequestContextKey {
        requester_did: row.requester_did.clone(),
        session_id: row.session_id().to_string(),
        agent_did: row.agent_did().to_string(),
        behavior_id: row.behavior_id.clone(),
        depth: row.depth(),
    }
}

fn is_projectable_root(row: &RequestProjectionRow, agent_did: &str, behavior_id: &str) -> bool {
    row.agent_did() == agent_did
        && row.requester_did.as_deref() == Some(agent_did)
        && row.behavior_id.as_deref() == Some(behavior_id)
        && row.depth() == 0
        && nonempty(row.caused_by_parent_request_id.as_deref()).is_none()
        && nonempty(row.caused_by_parent_tool_call_id.as_deref()).is_none()
}

fn spawn_nickname(args: &str) -> Option<String> {
    serde_json::from_str::<Value>(args)
        .ok()?
        .get("name")?
        .as_str()
        .and_then(|value| nonempty(Some(value)))
        .map(ToOwned::to_owned)
}

fn decode_rows<T>(response: &Value, collection: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    response
        .pointer(&format!("/data/{collection}"))
        .and_then(Value::as_array)
        .cloned()
        .with_context(|| format!("missing {collection} rows"))?
        .into_iter()
        .map(|row| serde_json::from_value(row).map_err(Into::into))
        .collect()
}

fn decode_request_rows(response: &Value) -> Result<Vec<RequestProjectionRow>> {
    response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .context("missing AgentRequest rows")?
        .into_iter()
        .map(RequestProjectionRow::decode)
        .collect()
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(super) fn is_subagent_control_tool(tool_name: &str) -> bool {
    collab_tool(tool_name).is_some()
}

fn collab_tool(tool_name: &str) -> Option<CollabTool> {
    match tool_name {
        "spawn_subagent" => Some(CollabTool::SpawnAgent),
        "wait_subagent" => Some(CollabTool::Wait),
        "steer_subagent" => Some(CollabTool::SendInput),
        "cancel_subagent" => Some(CollabTool::CloseAgent),
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
    let runtime_status = observed_tool_status(tool);
    let status = match (&collab_tool, runtime_status) {
        (_, ProjectionStatus::Failed) => ProjectionStatus::Failed,
        (CollabTool::SpawnAgent, _) => {
            // The reciprocal edge proves the spawn operation succeeded. GENTS
            // deliberately keeps the bridge row running while a background
            // child works; Codex represents that child lifecycle separately
            // in agentsStates, so the collaboration operation is complete.
            ProjectionStatus::Completed
        }
        (_, ProjectionStatus::InProgress) => ProjectionStatus::InProgress,
        (_, ProjectionStatus::Completed) => ProjectionStatus::Completed,
    };
    Some(CollabProjection {
        status,
        tool: collab_tool,
        receiver_thread_id: link.session_id.clone(),
        child_model: link.model.clone(),
        child_status: collab_agent_status(link.client_projection),
        child_failure_reason: link.failure_reason.clone(),
    })
}

pub(super) fn collab_agent_status(head: Option<ClientHeadProjection>) -> ChildStatus {
    match head {
        Some(ClientHeadProjection {
            turn_state: ClientTurnState::Completed,
            ..
        }) => ChildStatus::Completed,
        Some(ClientHeadProjection {
            turn_state: ClientTurnState::Failed,
            ..
        }) => ChildStatus::Errored,
        Some(ClientHeadProjection {
            turn_state: ClientTurnState::Superseded | ClientTurnState::Interrupted,
            ..
        }) => ChildStatus::Interrupted,
        Some(ClientHeadProjection {
            turn_state: ClientTurnState::WaitingForClaim,
            request_state: RequestLifecycleState::Pending,
        }) => ChildStatus::Pending,
        Some(_) => ChildStatus::Running,
        None => ChildStatus::NotFound,
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
            CollabTool::SpawnAgent => args
                .get("prompt")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            CollabTool::SendInput => args
                .get("message")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            _ => None,
        });
    let mut agents_states = HashMap::new();
    agents_states.insert(
        projection.receiver_thread_id.clone(),
        codex::CollabAgentState {
            status: codex_child_status(projection.child_status),
            message: projection.child_failure_reason.clone(),
        },
    );
    codex::ThreadItem::CollabAgentToolCall {
        id: tool.tool_call_key.clone(),
        tool: codex_collab_tool(projection.tool),
        status: codex_collab_status(projection.status),
        sender_thread_id: sender_thread_id.to_string(),
        receiver_thread_ids: vec![projection.receiver_thread_id.clone()],
        prompt,
        model: (projection.tool == CollabTool::SpawnAgent)
            .then(|| projection.child_model.clone())
            .flatten(),
        reasoning_effort: None,
        agents_states,
    }
}

fn codex_collab_tool(tool: CollabTool) -> codex::CollabAgentTool {
    match tool {
        CollabTool::SpawnAgent => codex::CollabAgentTool::SpawnAgent,
        CollabTool::ResumeAgent => codex::CollabAgentTool::ResumeAgent,
        CollabTool::Wait => codex::CollabAgentTool::Wait,
        CollabTool::SendInput => codex::CollabAgentTool::SendInput,
        CollabTool::CloseAgent => codex::CollabAgentTool::CloseAgent,
    }
}

fn codex_child_status(status: ChildStatus) -> codex::CollabAgentStatus {
    match status {
        ChildStatus::Pending => codex::CollabAgentStatus::PendingInit,
        ChildStatus::Running => codex::CollabAgentStatus::Running,
        ChildStatus::Interrupted => codex::CollabAgentStatus::Interrupted,
        ChildStatus::Completed => codex::CollabAgentStatus::Completed,
        ChildStatus::Errored => codex::CollabAgentStatus::Errored,
        ChildStatus::Shutdown => codex::CollabAgentStatus::Shutdown,
        ChildStatus::NotFound => codex::CollabAgentStatus::NotFound,
    }
}

fn codex_collab_status(status: ProjectionStatus) -> codex::CollabAgentToolCallStatus {
    match status {
        ProjectionStatus::InProgress => codex::CollabAgentToolCallStatus::InProgress,
        ProjectionStatus::Completed => codex::CollabAgentToolCallStatus::Completed,
        ProjectionStatus::Failed => codex::CollabAgentToolCallStatus::Failed,
    }
}

pub(super) fn observed_collab_status(
    status: &codex::CollabAgentToolCallStatus,
) -> ProjectionStatus {
    match status {
        codex::CollabAgentToolCallStatus::InProgress => ProjectionStatus::InProgress,
        codex::CollabAgentToolCallStatus::Completed => ProjectionStatus::Completed,
        codex::CollabAgentToolCallStatus::Failed => ProjectionStatus::Failed,
    }
}

pub(super) fn observed_collab_tool(tool: &codex::CollabAgentTool) -> CollabTool {
    match tool {
        codex::CollabAgentTool::SpawnAgent => CollabTool::SpawnAgent,
        codex::CollabAgentTool::ResumeAgent => CollabTool::ResumeAgent,
        codex::CollabAgentTool::Wait => CollabTool::Wait,
        codex::CollabAgentTool::SendInput => CollabTool::SendInput,
        codex::CollabAgentTool::CloseAgent => CollabTool::CloseAgent,
    }
}

pub(super) fn observed_child_status(status: &codex::CollabAgentStatus) -> ChildStatus {
    match status {
        codex::CollabAgentStatus::PendingInit => ChildStatus::Pending,
        codex::CollabAgentStatus::Running => ChildStatus::Running,
        codex::CollabAgentStatus::Interrupted => ChildStatus::Interrupted,
        codex::CollabAgentStatus::Completed => ChildStatus::Completed,
        codex::CollabAgentStatus::Errored => ChildStatus::Errored,
        codex::CollabAgentStatus::Shutdown => ChildStatus::Shutdown,
        codex::CollabAgentStatus::NotFound => ChildStatus::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn signed_local_self_request_matches_shim_root_selection() {
        use gents::AgentIdentity;
        let temp = tempfile::tempdir().unwrap();
        let identity =
            gents::KeyIdentity::load_or_create(temp.path().join("agent.key"), None).unwrap();
        let did = identity.did().to_owned();
        let create = gents::build_signed_request(
            gents::RequestSpec::new(
                gents::RequestIdentity {
                    request_id: "local-root".into(),
                    agent_did: did.clone(),
                    requester_did: None,
                    behavior_id: "behavior".into(),
                    session_id: "session".into(),
                    content: "hello".into(),
                    execution_origin: gents::lifecycle::ExecutionOrigin::Interactive,
                    created_at: "2026-09-01T00:00:00Z".into(),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&did),
            ),
            gents::RequestSigner::Identity(&identity),
        )
        .await
        .unwrap();
        assert_eq!(create.requester_did, did);
        let mut row = RequestProjectionRow::decode(serde_json::json!({"_docID":"physical-root","request_id":create.request_id,"agent_did":create.agent_did,"requester_did":create.requester_did,"behavior_id":create.behavior_id,"session_id":create.session_id,"subagent_depth":0})).unwrap();
        assert!(is_projectable_root(&row, &did, "behavior"));
        row.requester_did = None;
        assert!(
            !is_projectable_root(&row, &did, "behavior"),
            "exact absent scope is not this signed local session"
        );
        row.requester_did = Some("foreign-requester".into());
        assert!(!is_projectable_root(&row, &did, "behavior"));
    }

    fn request(
        request_id: &str,
        session_id: &str,
        depth: u32,
        parent_request_id: Option<&str>,
        parent_tool_call_id: Option<&str>,
    ) -> RequestProjectionRow {
        RequestProjectionRow::decode(json!({
            "_docID": format!("doc:{request_id}"),
            "request_id": request_id,
            "requester_did": if depth == 0 { Some("did:root") } else if depth == 1 { Some("did:root") } else { Some("did:child") },
            "content": format!("content for {request_id}"),
            "session_id": session_id,
            "agent_did": if depth == 0 { "did:root" } else { "did:child" },
            "behavior_id": if depth == 0 { "root" } else { "reviewer" },
            "lifecycle_state": if depth == 0 { "processing" } else { "completed" },
            "subagent_depth": depth,
            "caused_by_parent_request_id": parent_request_id,
            "caused_by_parent_request_doc_id": parent_request_id.map(|id|format!("doc:{id}")),
            "caused_by_parent_tool_call_id": parent_tool_call_id,
            "caused_by_parent_tool_call_doc_id": parent_tool_call_id.map(|id|format!("tool:{id}")),
        }))
        .expect("canonical AgentRequest test row")
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
            doc_id: "tool:spawn-call".into(),
            request_doc_id: "doc:root-request".into(),
            request_id: "root-request".to_string(),
            session_id: root_session.clone(),
            agent_did: "did:root".to_string(),
            tool_call_id: "spawn-call".to_string(),
            tool_name: "spawn_subagent".to_string(),
            child_request_id: Some("child-request".to_string()),
            spawn_target_did: Some("did:child".to_string()),
            args: r#"{"name":"reviewer"}"#.to_string(),
        }];

        let links =
            resolve_authorized_subagent_threads(&requests, &tools, "did:root", "root").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].session_id, child_session);
        assert_eq!(links[0].root_session_id, root_session);
        assert_eq!(links[0].nickname, "reviewer");
    }

    #[test]
    fn matching_logical_edges_cannot_substitute_physical_parent_or_requester() {
        let root_session = Uuid::new_v4().to_string();
        let child_session = Uuid::new_v4().to_string();
        let root = request("parent", &root_session, 0, None, None);
        let child = request("child", &child_session, 1, Some("parent"), Some("spawn"));
        let tool = ToolLinkRow {
            doc_id: "tool:spawn".into(),
            request_doc_id: "doc:parent".into(),
            request_id: "parent".into(),
            session_id: root_session,
            agent_did: "did:root".into(),
            tool_call_id: "spawn".into(),
            tool_name: "spawn_subagent".into(),
            child_request_id: Some("child".into()),
            spawn_target_did: Some("did:child".into()),
            args: "{}".into(),
        };
        let resolve = |child: RequestProjectionRow, tool: ToolLinkRow| {
            resolve_authorized_subagent_threads(&[root.clone(), child], &[tool], "did:root", "root")
                .unwrap()
        };
        assert_eq!(resolve(child.clone(), tool.clone()).len(), 1);
        let mut foreign_parent = child.clone();
        foreign_parent.caused_by_parent_request_doc_id = Some("foreign-parent-doc".into());
        assert!(resolve(foreign_parent, tool.clone()).is_empty());
        let mut foreign_requester = child.clone();
        foreign_requester.requester_did = Some("did:unrelated".into());
        assert!(resolve(foreign_requester, tool.clone()).is_empty());
        let mut foreign_tool = tool.clone();
        foreign_tool.doc_id = "foreign-tool-doc".into();
        assert!(resolve(child.clone(), foreign_tool).is_empty());
        let mut foreign_tool_parent = tool;
        foreign_tool_parent.request_doc_id = "foreign-parent-doc".into();
        assert!(resolve(child, foreign_tool_parent).is_empty());
    }

    #[test]
    fn identical_wire_labels_cannot_choose_between_distinct_principals() {
        let root_session = Uuid::new_v4().to_string();
        let child_session = Uuid::new_v4().to_string();
        let root = request("parent", &root_session, 0, None, None);
        let child = request("child", &child_session, 1, Some("parent"), Some("spawn"));
        let mut other = request(
            "other",
            &child_session,
            1,
            Some("parent"),
            Some("other-spawn"),
        );
        other.agent_did = Some("did:other-child".into());
        let tool = |tool_id: &str, child_id: &str, target: &str| ToolLinkRow {
            doc_id: format!("tool:{tool_id}"),
            request_doc_id: "doc:parent".into(),
            request_id: "parent".into(),
            session_id: root_session.clone(),
            agent_did: "did:root".into(),
            tool_call_id: tool_id.into(),
            tool_name: "spawn_subagent".into(),
            child_request_id: Some(child_id.into()),
            spawn_target_did: Some(target.into()),
            args: "{}".into(),
        };
        let tools = [
            tool("spawn", "child", "did:child"),
            tool("other-spawn", "other", "did:other-child"),
        ];
        assert!(resolve_authorized_subagent_threads(
            &[root, child, other],
            &tools,
            "did:root",
            "root"
        )
        .is_err());
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
                doc_id: "tool:spawn-call".into(),
                request_doc_id: "doc:root-request".into(),
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
                doc_id: "tool:nested-spawn-call".into(),
                request_doc_id: "doc:child-followup".into(),
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

        let links =
            resolve_authorized_subagent_threads(&requests, &tools, "did:root", "root").unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].session_id, child_session);
        assert_eq!(links[1].session_id, grandchild_session);
        assert_eq!(links[1].parent_session_id, child_session);
        assert_eq!(links[1].root_session_id, root_session);
    }

    #[test]
    fn verified_head_updates_status_and_physical_request_identity() {
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
        child.lifecycle_state = Some(RequestLifecycleState::Completed);
        let mut followup = request("child-followup", &child_session, 1, None, None);
        followup.created_at = Some("2026-01-01T00:00:02Z".to_string());
        followup.lifecycle_state = Some(RequestLifecycleState::Processing);
        let requests = vec![
            request("root-request", &root_session, 0, None, None),
            child,
            followup,
        ];
        let tools = vec![ToolLinkRow {
            doc_id: "tool:spawn-call".into(),
            request_doc_id: "doc:root-request".into(),
            request_id: "root-request".to_string(),
            session_id: root_session,
            agent_did: "did:root".to_string(),
            tool_call_id: "spawn-call".to_string(),
            tool_name: "spawn_subagent".to_string(),
            child_request_id: Some("child-request".to_string()),
            spawn_target_did: Some("did:child".to_string()),
            args: r#"{"name":"reviewer"}"#.to_string(),
        }];

        let mut links =
            resolve_authorized_subagent_threads(&requests, &tools, "did:root", "root").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].request_id, "child-request");
        apply_latest_request(&mut links[0], &requests[2]);
        assert_eq!(links[0].latest_request_doc_id, "doc:child-followup");
        assert_eq!(links[0].latest_request_id, "child-followup");
        assert_eq!(
            links[0].latest_request_content,
            "content for child-followup"
        );
        assert_eq!(
            links[0].client_projection,
            project_persisted_attempt("processing", false, None)
        );
    }

    #[test]
    fn graph_frontier_queries_are_scoped_and_escape_values() {
        let sessions = ["session-a", "session-\"b"]
            .into_iter()
            .map(|session_id| RequestContextKey {
                session_id: session_id.into(),
                agent_did: "did:owner".into(),
                requester_did: Some("did:requester".into()),
                behavior_id: Some("bound".into()),
                depth: 1,
            })
            .collect::<Vec<_>>();
        let requests = vec!["request-a".to_string(), "request-\"b".to_string()];
        let session_query = requests_for_sessions_query(&sessions);
        assert!(session_query.contains("session_id:"));
        assert!(session_query.contains("did:owner"));
        assert!(session_query.contains("did:requester"));
        assert!(session_query.contains(r#""session-\"b""#));

        let tool_query = spawn_tools_for_requests_query(&requests);
        assert!(tool_query.contains("request_doc_id: { _in:"));
        assert!(tool_query.contains(r#"tool_name: { _eq: "spawn_subagent" }"#));
        assert!(tool_query.contains(r#"child_request_id: { _ne: "" }"#));
        assert!(tool_query.contains(r#""request-\"b""#));
    }

    #[test]
    fn lean_fenced_tool_and_status_mappings_match_codex_protocol() {
        assert_eq!(collab_tool("spawn_subagent"), Some(CollabTool::SpawnAgent));
        assert_eq!(collab_tool("wait_subagent"), Some(CollabTool::Wait));
        assert_eq!(collab_tool("steer_subagent"), Some(CollabTool::SendInput));
        assert_eq!(collab_tool("cancel_subagent"), Some(CollabTool::CloseAgent));
        assert_eq!(collab_tool("list_subagents"), None);
        assert_eq!(collab_tool("read_subagent"), None);

        let status = |lifecycle, response| {
            collab_agent_status(project_persisted_attempt(lifecycle, false, response))
        };
        assert_eq!(status("pending", None), ChildStatus::Pending);
        assert_eq!(status("claimed", None), ChildStatus::Running);
        assert_eq!(status("processing", None), ChildStatus::Running);
        assert_eq!(status("inputRequired", None), ChildStatus::Running);
        assert_eq!(status("completed", None), ChildStatus::Completed);
        assert_eq!(status("failed", None), ChildStatus::Errored);
        assert_eq!(status("interrupted", None), ChildStatus::Interrupted);
        assert_eq!(
            status("processing", Some("complete")),
            ChildStatus::Completed
        );
        assert_eq!(status("processing", Some("error")), ChildStatus::Errored);
        assert_eq!(collab_agent_status(None), ChildStatus::NotFound);
    }

    #[test]
    fn spawn_item_uses_child_session_as_receiver_thread() {
        let child_session_id = Uuid::new_v4().to_string();
        let mut tool = GentsToolCallProgress {
            tool_call_key: "parent:spawn-call".to_string(),
            tool_name: "spawn_subagent".to_string(),
            lifecycle_state: Some("running".to_string()),
            await_mode: Some("background".to_string()),
            child_request_id: Some("child-request".to_string()),
            args: r#"{"name":"reviewer","prompt":"Inspect the patch"}"#.to_string(),
            result: String::new(),
            subagent_link: None,
            ..Default::default()
        };
        tool.subagent_link = Some(LinkedSubagentThread {
            parent_request_doc_id: "test-parent-doc".into(),
            parent_agent_did: "did:parent".into(),
            parent_requester_did: None,
            request_doc_id: "test-request-doc".into(),
            latest_request_doc_id: "test-request-doc".into(),
            requester_did: Some("did:parent".into()),
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
            client_projection: project_persisted_attempt("processing", false, None),
            failure_reason: None,
            created_at: None,
        });
        let projection = collab_projection(&tool).expect("authorized spawn projection");
        assert_eq!(
            projection.status,
            ProjectionStatus::Completed,
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

        let mut claimed = tool.clone();
        claimed
            .subagent_link
            .as_mut()
            .expect("link")
            .client_projection = project_persisted_attempt("claimed", false, None);
        let claimed_projection = collab_projection(&claimed).expect("claimed projection");
        assert_eq!(
            projection, claimed_projection,
            "storage lifecycle aliases with the same Codex status must not emit duplicate items"
        );

        let mut completed = tool.clone();
        let link = completed.subagent_link.as_mut().expect("link");
        link.client_projection = project_persisted_attempt("completed", false, None);
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
                "agent-response-id".to_string(),
                "agent-tool-call-id".to_string(),
                "agent-behavior-id".to_string(),
            ]),
            match_all_updates: false,
        };

        assert!(filter.affects_collection_id("agent-request-id"));
        assert!(filter.affects_collection_id("agent-response-id"));
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
