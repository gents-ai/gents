use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::{CancelCause, CascadeDispatch, ToolCallLifecycle};
use gents::{DescendantGraphAccess, DescendantQuery, MAX_DESCENDANT_PAGE_LIMIT};
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::args::{SubagentCancelArgs, SubagentCommand, SubagentListArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::{
    graphql_rows, parse_duration_suffix, post_graphql, print_json, resolve_agent_did,
    resolve_config_access, resolve_request_id,
};

const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(250);
const AGENT_REQUEST_FIELDS: &str = r#"
    request_id
    agent_did
    behavior_id
    lifecycle_state
    created_at
    claimed_at
    caused_by_parent_request_id
"#;

pub(crate) async fn dispatch(command: SubagentCommand) -> Result<()> {
    match command {
        SubagentCommand::List(args) => subagent_list(args).await,
        SubagentCommand::Cancel(args) => subagent_cancel(args).await,
    }
}

async fn subagent_cancel(args: SubagentCancelArgs) -> Result<()> {
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let cause = parse_cancel_cause(&args.cause)?;
    let wait_timeout = resolve_wait_timeout(args.wait, args.timeout.as_deref())?;

    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;

    let snapshots = match access {
        ConfigAccess::Graphql(graphql) => {
            let affected = cancel_subagent_graphql(&graphql, &request_id, args.cascade).await?;
            if let Some(timeout) = wait_timeout {
                wait_for_terminal_graphql(&graphql, &affected, timeout).await?
            } else {
                snapshot_requests_graphql(&graphql, &affected).await?
            }
        }
        ConfigAccess::Local(node) => {
            let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())
                .context("resolving local agent_did for cascade ownership checks")?;
            let affected =
                cancel_subagent_local(node.clone(), &agent_did, &request_id, args.cascade, cause)
                    .await?;
            if let Some(timeout) = wait_timeout {
                wait_for_terminal_local(node.as_ref(), &affected, timeout).await?
            } else {
                snapshot_requests_local(node.as_ref(), &affected).await?
            }
        }
    };

    render_cancel_output(
        args.output,
        SubagentCancelRender {
            request_id,
            cascade: args.cascade,
            cause: cause.as_str().to_string(),
            wait: args.wait,
            requests: snapshots,
        },
    )
}

fn parse_cancel_cause(raw: &str) -> Result<CancelCause> {
    let value = raw.trim();
    CancelCause::from_persisted(value).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid --cause {value:?}; expected one of: interrupted, deadline, userCancelled"
        )
    })
}

fn resolve_wait_timeout(wait: bool, timeout: Option<&str>) -> Result<Option<Duration>> {
    if timeout.is_some() && !wait {
        anyhow::bail!("--timeout is only valid with --wait");
    }
    if !wait {
        return Ok(None);
    }
    timeout
        .map(parse_duration_suffix)
        .transpose()
        .map(|duration| Some(duration.unwrap_or(DEFAULT_WAIT_TIMEOUT)))
}

async fn cancel_subagent_graphql(
    graphql: &str,
    request_id: &str,
    cascade: bool,
) -> Result<Vec<String>> {
    let mut affected = Vec::new();
    let mut seen = BTreeSet::new();
    push_unique(&mut affected, &mut seen, request_id.to_string());

    if cascade {
        let target = fetch_request_row_graphql(graphql, request_id).await?;
        if let Some(session_id) = target.session_id.as_deref() {
            collect_descendant_request_ids_graphql(graphql, session_id, &mut affected, &mut seen)
                .await?;
        }
    }

    for request_id in &affected {
        interrupt_request_graphql(graphql, request_id).await?;
    }
    Ok(affected)
}

async fn collect_descendant_request_ids_graphql(
    graphql: &str,
    root_session_id: &str,
    affected: &mut Vec<String>,
    seen_requests: &mut BTreeSet<String>,
) -> Result<()> {
    let mut seen_sessions = BTreeSet::new();
    let mut queue = VecDeque::from([root_session_id.to_string()]);
    while let Some(session_id) = queue.pop_front() {
        if !seen_sessions.insert(session_id.clone()) {
            continue;
        }
        for bridge in running_subagent_bridges_graphql(graphql, &session_id).await? {
            let Some(child_request_id) = bridge.child_request_id else {
                continue;
            };
            push_unique(affected, seen_requests, child_request_id.clone());
            if let Ok(child) = fetch_request_row_graphql(graphql, &child_request_id).await {
                if let Some(child_session_id) = child.session_id {
                    queue.push_back(child_session_id);
                }
            }
        }
    }
    Ok(())
}

async fn interrupt_request_graphql(graphql: &str, request_id: &str) -> Result<()> {
    let row = fetch_request_row_graphql(graphql, request_id).await?;
    if row.interrupt_requested_at.is_some() {
        return Ok(());
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{ interrupt_requested_at: "{now}" }}
            ) {{ _docID }}
        }}"#,
        request_id = escape_graphql_string(request_id),
        now = escape_graphql_string(&now),
    );
    post_graphql(graphql, &mutation).await?;
    Ok(())
}

async fn cancel_subagent_local(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    request_id: &str,
    cascade: bool,
    cause: CancelCause,
) -> Result<Vec<String>> {
    let target = fetch_request_row_local(node.as_ref(), request_id).await?;
    let mut affected = Vec::new();
    let mut seen_requests = BTreeSet::new();

    if cascade {
        cancel_parent_bridge_local(node.clone(), cause, agent_did, &target).await?;
    }
    interrupt_request_local(node.as_ref(), &mut affected, &mut seen_requests, request_id).await?;

    if cascade {
        if let Some(session_id) = target.session_id.as_deref() {
            cancel_descendant_bridges_local(
                node.clone(),
                cause,
                agent_did,
                session_id,
                &mut affected,
                &mut seen_requests,
            )
            .await?;
        }
    }

    Ok(affected)
}

async fn cancel_parent_bridge_local(
    node: Arc<EmbeddedNode>,
    cause: CancelCause,
    agent_did: &str,
    target: &AgentRequestRow,
) -> Result<()> {
    let Some(parent_request_id) = target.caused_by_parent_request_id.as_deref() else {
        return Ok(());
    };
    let Some(parent_tool_call_id) = target.caused_by_parent_tool_call_id.as_deref() else {
        return Ok(());
    };
    let parent = fetch_request_row_local(node.as_ref(), parent_request_id).await?;
    let Some(parent_session_id) = parent.session_id.as_deref() else {
        return Ok(());
    };
    cancel_bridge_local(
        node,
        cause,
        agent_did,
        parent_session_id,
        parent_tool_call_id,
        BridgeKind::Parent,
    )
    .await?;
    Ok(())
}

async fn cancel_descendant_bridges_local(
    node: Arc<EmbeddedNode>,
    cause: CancelCause,
    agent_did: &str,
    root_session_id: &str,
    affected: &mut Vec<String>,
    seen_requests: &mut BTreeSet<String>,
) -> Result<()> {
    let mut seen_sessions = BTreeSet::new();
    let mut queue = VecDeque::from([root_session_id.to_string()]);
    while let Some(session_id) = queue.pop_front() {
        if !seen_sessions.insert(session_id.clone()) {
            continue;
        }
        for bridge in running_subagent_bridges_local(node.as_ref(), &session_id).await? {
            let dispatch = cancel_bridge_local(
                node.clone(),
                cause,
                agent_did,
                &session_id,
                &bridge.tool_call_id,
                BridgeKind::Descendant,
            )
            .await?;
            let Some(child_request_id) = dispatch else {
                continue;
            };
            interrupt_request_local(node.as_ref(), affected, seen_requests, &child_request_id)
                .await?;
            if let Ok(child) = fetch_request_row_local(node.as_ref(), &child_request_id).await {
                if let Some(child_session_id) = child.session_id {
                    queue.push_back(child_session_id);
                }
            }
        }
    }
    Ok(())
}

async fn cancel_bridge_local(
    node: Arc<EmbeddedNode>,
    cause: CancelCause,
    agent_did: &str,
    session_id: &str,
    tool_call_id: &str,
    bridge_kind: BridgeKind,
) -> Result<Option<String>> {
    if tool_lifecycle_state_local(node.as_ref(), session_id, tool_call_id)
        .await?
        .as_deref()
        != Some("running")
    {
        return Ok(None);
    }

    let Some(mut lifecycle) =
        ToolCallLifecycle::load(node.clone(), session_id, tool_call_id).await?
    else {
        return Ok(None);
    };
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(cause, agent_did)
        .await
        .with_context(|| {
            format!(
                "cancelling {} subagent bridge {session_id}:{tool_call_id}",
                bridge_kind.as_str()
            )
        })?;
    Ok(match dispatch {
        Some(CascadeDispatch::Local(intent)) => Some(intent.child_request_id),
        Some(CascadeDispatch::RemoteIntentWritten) | None => None,
    })
}

async fn interrupt_request_local(
    node: &EmbeddedNode,
    affected: &mut Vec<String>,
    seen_requests: &mut BTreeSet<String>,
    request_id: &str,
) -> Result<()> {
    gents::interrupt_request(node, request_id).await?;
    push_unique(affected, seen_requests, request_id.to_string());
    Ok(())
}

fn push_unique(values: &mut Vec<String>, seen: &mut BTreeSet<String>, value: String) {
    if seen.insert(value.clone()) {
        values.push(value);
    }
}

async fn wait_for_terminal_graphql(
    graphql: &str,
    request_ids: &[String],
    timeout: Duration,
) -> Result<Vec<RequestCancelSnapshot>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshots = snapshot_requests_graphql(graphql, request_ids).await?;
        if snapshots.iter().all(|row| {
            row.lifecycle_state
                .is_some_and(RequestLifecycleState::is_terminal)
        }) {
            return Ok(snapshots);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for subagent cancel after {}s; last states: {}",
                timeout.as_secs(),
                format_snapshot_states(&snapshots)
            );
        }
        tokio::time::sleep(WAIT_POLL_INTERVAL).await;
    }
}

async fn wait_for_terminal_local(
    node: &EmbeddedNode,
    request_ids: &[String],
    timeout: Duration,
) -> Result<Vec<RequestCancelSnapshot>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshots = snapshot_requests_local(node, request_ids).await?;
        if snapshots.iter().all(|row| {
            row.lifecycle_state
                .is_some_and(RequestLifecycleState::is_terminal)
        }) {
            return Ok(snapshots);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for subagent cancel after {}s; last states: {}",
                timeout.as_secs(),
                format_snapshot_states(&snapshots)
            );
        }
        tokio::time::sleep(WAIT_POLL_INTERVAL).await;
    }
}

async fn snapshot_requests_graphql(
    graphql: &str,
    request_ids: &[String],
) -> Result<Vec<RequestCancelSnapshot>> {
    let mut rows = Vec::with_capacity(request_ids.len());
    for request_id in request_ids {
        let row = fetch_request_row_graphql(graphql, request_id).await?;
        rows.push(request_cancel_snapshot(row));
    }
    Ok(rows)
}

async fn snapshot_requests_local(
    node: &EmbeddedNode,
    request_ids: &[String],
) -> Result<Vec<RequestCancelSnapshot>> {
    let mut rows = Vec::with_capacity(request_ids.len());
    for request_id in request_ids {
        let row = fetch_request_row_local(node, request_id).await?;
        rows.push(request_cancel_snapshot(row));
    }
    Ok(rows)
}

fn format_snapshot_states(snapshots: &[RequestCancelSnapshot]) -> String {
    snapshots
        .iter()
        .map(|row| {
            format!(
                "{}={}",
                row.request_id,
                row.lifecycle_state
                    .map(RequestLifecycleState::as_str)
                    .unwrap_or("missing")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

async fn fetch_request_row_graphql(graphql: &str, request_id: &str) -> Result<AgentRequestRow> {
    let query = request_row_query(request_id);
    let response = post_graphql(graphql, &query).await?;
    request_row_from_response(&response, request_id)
}

async fn fetch_request_row_local(node: &EmbeddedNode, request_id: &str) -> Result<AgentRequestRow> {
    let query = request_row_query(request_id);
    let response = execute_node_json(node, &query).await?;
    request_row_from_response(&response, request_id)
}

fn request_row_query(request_id: &str) -> String {
    format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                limit: 1
            ) {{
                request_id
                agent_did
                session_id
                lifecycle_state
                interrupt_requested_at
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#,
        request_id = escape_graphql_string(request_id),
    )
}

fn request_row_from_response(response: &Value, request_id: &str) -> Result<AgentRequestRow> {
    let row = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))?;
    serde_json::from_value(row.clone())
        .with_context(|| format!("decoding AgentRequest {request_id}"))
}

async fn running_subagent_bridges_graphql(
    graphql: &str,
    session_id: &str,
) -> Result<Vec<BridgeRow>> {
    let response = post_graphql(graphql, &running_subagent_bridges_query(session_id)).await?;
    bridge_rows_from_response(&response)
}

async fn running_subagent_bridges_local(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Vec<BridgeRow>> {
    let response = execute_node_json(node, &running_subagent_bridges_query(session_id)).await?;
    bridge_rows_from_response(&response)
}

fn running_subagent_bridges_query(session_id: &str) -> String {
    format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    lifecycle_state: {{ _eq: "running" }},
                    cancel_policy: {{ _eq: "cascade" }}
                }},
                order: [{{ started_at: ASC }}, {{ tool_call_id: ASC }}]
            ) {{
                tool_call_id
                child_request_id
            }}
        }}"#,
        session_id = escape_graphql_string(session_id),
    )
}

fn bridge_rows_from_response(response: &Value) -> Result<Vec<BridgeRow>> {
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(rows
        .iter()
        .filter_map(|row| {
            let tool_call_id = string_field(row, "tool_call_id")?;
            let child_request_id = string_field(row, "child_request_id");
            Some(BridgeRow {
                tool_call_id,
                child_request_id,
            })
        })
        .collect())
}

async fn tool_lifecycle_state_local(
    node: &EmbeddedNode,
    session_id: &str,
    tool_call_id: &str,
) -> Result<Option<String>> {
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{session_id}" }},
                    tool_call_id: {{ _eq: "{tool_call_id}" }}
                }},
                limit: 1
            ) {{
                lifecycle_state
            }}
        }}"#,
        session_id = escape_graphql_string(session_id),
        tool_call_id = escape_graphql_string(tool_call_id),
    );
    let response = execute_node_json(node, &query).await?;
    Ok(response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| string_field(row, "lifecycle_state")))
}

async fn execute_node_json(node: &EmbeddedNode, query: &str) -> Result<Value> {
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!("graphql returned errors: {:?}", response.errors);
    }
    Ok(json!({
        "data": response.data.unwrap_or(Value::Null),
    }))
}

fn string_field(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn render_cancel_output(output: OutputFormat, render: SubagentCancelRender) -> Result<()> {
    match output.ensure_supported("subagent cancel", &[OutputFormat::Text, OutputFormat::Json])? {
        OutputFormat::Text => {
            for request in &render.requests {
                println!("{}", request.request_id);
            }
            Ok(())
        }
        OutputFormat::Json => print_json(&json!({
            "request_id": render.request_id,
            "cascade": render.cascade,
            "cause": render.cause,
            "wait": render.wait,
            "interrupted_request_ids": render.requests.iter().map(|row| row.request_id.as_str()).collect::<Vec<_>>(),
            "requests": render.requests,
        })),
        _ => unreachable!("ensure_supported restricts subagent cancel output formats"),
    }
}

fn request_cancel_snapshot(row: AgentRequestRow) -> RequestCancelSnapshot {
    RequestCancelSnapshot {
        request_id: row.request_id,
        agent_did: row.agent_did,
        lifecycle_state: row.lifecycle_state,
        interrupt_requested_at: row.interrupt_requested_at,
    }
}

#[derive(Debug, Clone)]
struct BridgeRow {
    tool_call_id: String,
    child_request_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BridgeKind {
    Parent,
    Descendant,
}

impl BridgeKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Parent => "parent",
            Self::Descendant => "descendant",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct RequestCancelSnapshot {
    request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_did: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lifecycle_state: Option<RequestLifecycleState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interrupt_requested_at: Option<String>,
}

struct SubagentCancelRender {
    request_id: String,
    cascade: bool,
    cause: String,
    wait: bool,
    requests: Vec<RequestCancelSnapshot>,
}

async fn subagent_list(args: SubagentListArgs) -> Result<()> {
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let rows = match args.root.as_deref().and_then(non_empty_str) {
        Some(root) => load_rooted_lineage(&access, root, args.depth).await?,
        None => load_lineage_forest(&access, args.depth).await?,
    };

    match args.output.ensure_supported(
        "subagent list",
        &[OutputFormat::Tree, OutputFormat::Table, OutputFormat::Json],
    )? {
        OutputFormat::Tree => print_tree(&rows),
        OutputFormat::Table => print_table(&rows),
        OutputFormat::Json => print_lineage_json(args.root.as_deref(), args.depth, &rows),
        _ => unreachable!("ensure_supported restricts subagent list output formats"),
    }
}

async fn load_rooted_lineage(
    access: &ConfigAccess,
    root_request_id: &str,
    max_depth: Option<usize>,
) -> Result<Vec<LineageNode>> {
    let root = load_request_by_id(access, root_request_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("AgentRequest {root_request_id} not found"))?;
    let max_depth = max_depth.unwrap_or(usize::MAX);
    let mut descendants_by_parent = BTreeMap::<String, Vec<LineageNode>>::new();
    let mut after = None;
    loop {
        let page = gents::resolve_descendant_graph(
            DescendantGraphAccess::Config(access),
            &DescendantQuery {
                after: after.clone(),
                limit: MAX_DESCENDANT_PAGE_LIMIT,
                ..DescendantQuery::all(root_request_id)
            },
        )
        .await?;
        for edge in page.edges {
            if edge.depth > max_depth {
                continue;
            }
            let parent_request_id = edge.immediate_parent_request_id.clone();
            descendants_by_parent
                .entry(parent_request_id)
                .or_default()
                .push(LineageNode {
                    depth: edge.depth,
                    row: serde_json::from_value(json!({
                        "request_id": edge.child_request_id,
                        "agent_did": edge.principal_did,
                        "behavior_id": edge.behavior_id,
                        "lifecycle_state": edge.lifecycle_state,
                        "created_at": edge.created_at,
                        "caused_by_parent_request_id": edge.immediate_parent_request_id,
                    }))
                    .context("decoding descendant edge as canonical AgentRequest row")?,
                });
        }
        if !page.has_more {
            break;
        }
        after = page.next_cursor;
    }

    // The resolver pages breadth-first, but tree/table rendering assumes a
    // child's subtree precedes later siblings; flatten depth-first (sibling
    // order preserved from the resolver's started_at/tool_call_id ordering).
    let mut rows = Vec::new();
    let mut stack = vec![LineageNode {
        row: root,
        depth: 0,
    }];
    while let Some(node) = stack.pop() {
        let request_id = node.row.request_id.clone();
        rows.push(node);
        if let Some(mut children) = descendants_by_parent.remove(&request_id) {
            children.reverse();
            stack.extend(children);
        }
    }
    // A durable bridge can name a parent with no materialized row; keep such
    // edges visible instead of silently dropping them.
    for children in descendants_by_parent.into_values() {
        rows.extend(children);
    }

    Ok(rows)
}

async fn load_lineage_forest(
    access: &ConfigAccess,
    max_depth: Option<usize>,
) -> Result<Vec<LineageNode>> {
    let all_rows = load_all_requests(access).await?;
    let mut rows_by_id = BTreeMap::new();
    let mut children_by_parent = BTreeMap::<String, Vec<String>>::new();
    let mut included_ids = BTreeSet::<String>::new();

    for row in all_rows {
        let request_id = row.request_id.clone();
        if let Some(parent_request_id) = request_parent_id(&row) {
            included_ids.insert(parent_request_id.clone());
            included_ids.insert(request_id.clone());
            children_by_parent
                .entry(parent_request_id)
                .or_default()
                .push(request_id.clone());
        }
        rows_by_id.insert(request_id, row);
    }

    let mut roots = included_ids
        .iter()
        .filter_map(|request_id| {
            let row = rows_by_id.get(request_id)?;
            let has_included_parent = request_parent_id(row).is_some_and(|parent| {
                included_ids.contains(&parent) && rows_by_id.contains_key(&parent)
            });
            (!has_included_parent).then(|| request_id.clone())
        })
        .collect::<Vec<_>>();
    if roots.is_empty() {
        roots = included_ids
            .iter()
            .filter(|request_id| rows_by_id.contains_key(*request_id))
            .cloned()
            .collect();
    }
    sort_request_ids(&mut roots, &rows_by_id);
    for children in children_by_parent.values_mut() {
        sort_request_ids(children, &rows_by_id);
    }

    let mut output = Vec::new();
    let mut seen = HashSet::new();
    let max_depth = max_depth.unwrap_or(usize::MAX);
    for root in roots {
        append_forest_node(
            &root,
            0,
            max_depth,
            &rows_by_id,
            &children_by_parent,
            &mut seen,
            &mut output,
        );
    }

    Ok(output)
}

fn append_forest_node(
    request_id: &str,
    depth: usize,
    max_depth: usize,
    rows_by_id: &BTreeMap<String, AgentRequestRow>,
    children_by_parent: &BTreeMap<String, Vec<String>>,
    seen: &mut HashSet<String>,
    output: &mut Vec<LineageNode>,
) {
    if !seen.insert(request_id.to_string()) {
        return;
    }
    let Some(row) = rows_by_id.get(request_id) else {
        return;
    };
    output.push(LineageNode {
        row: row.clone(),
        depth,
    });
    if depth >= max_depth {
        return;
    }
    if let Some(children) = children_by_parent.get(request_id) {
        for child in children {
            append_forest_node(
                child,
                depth + 1,
                max_depth,
                rows_by_id,
                children_by_parent,
                seen,
                output,
            );
        }
    }
}

fn sort_request_ids(request_ids: &mut [String], rows_by_id: &BTreeMap<String, AgentRequestRow>) {
    request_ids.sort_by(|left, right| {
        let left_key = rows_by_id
            .get(left)
            .map(request_sort_key)
            .unwrap_or(("", ""));
        let right_key = rows_by_id
            .get(right)
            .map(request_sort_key)
            .unwrap_or(("", ""));
        left_key.cmp(&right_key)
    });
}

async fn load_request_by_id(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<Option<AgentRequestRow>> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{
                {AGENT_REQUEST_FIELDS}
            }}
        }}"#
    );
    let mut rows = load_request_rows(access, &query).await?;
    Ok(rows.pop())
}

async fn load_all_requests(access: &ConfigAccess) -> Result<Vec<AgentRequestRow>> {
    let query = format!(
        r#"{{
            AgentRequest(order: [{{ created_at: ASC }}, {{ request_id: ASC }}]) {{
                {AGENT_REQUEST_FIELDS}
            }}
        }}"#
    );
    load_request_rows(access, &query).await
}

async fn load_request_rows(access: &ConfigAccess, query: &str) -> Result<Vec<AgentRequestRow>> {
    graphql_rows(access, "AgentRequest", query)
        .await?
        .into_iter()
        .map(|value| {
            serde_json::from_value(value).context("decoding AgentRequest lineage row from GraphQL")
        })
        .collect()
}

fn print_tree(rows: &[LineageNode]) -> Result<()> {
    let output_rows = output_rows(rows, true);
    if output_rows.is_empty() {
        println!("No subagent requests found.");
        return Ok(());
    }
    print!("{}", render_table(&output_rows));
    Ok(())
}

fn print_table(rows: &[LineageNode]) -> Result<()> {
    let output_rows = output_rows(rows, false);
    if output_rows.is_empty() {
        println!("No subagent requests found.");
        return Ok(());
    }
    print!("{}", render_table(&output_rows));
    Ok(())
}

fn print_lineage_json(
    root: Option<&str>,
    max_depth: Option<usize>,
    rows: &[LineageNode],
) -> Result<()> {
    let output_rows = output_rows(rows, false);
    let tree = tree_from_rows(&output_rows);
    print_json(&serde_json::json!({
        "root_request_id": root.and_then(non_empty_str),
        "max_depth": max_depth,
        "rows": output_rows,
        "tree": tree,
    }))
}

fn output_rows(rows: &[LineageNode], indent: bool) -> Vec<LineageOutputRow> {
    rows.iter()
        .map(|node| LineageOutputRow::from_node(node, indent))
        .collect()
}

fn render_table(rows: &[LineageOutputRow]) -> String {
    const HEADERS: [&str; 6] = [
        "CHILD_REQUEST_ID",
        "PARENT_REQUEST_ID",
        "DEPLOYMENT",
        "BEHAVIOR_ID",
        "STATE",
        "STARTED_AT",
    ];
    let mut table_rows = Vec::<[String; 6]>::new();
    for row in rows {
        table_rows.push([
            row.display_request_id.clone(),
            row.parent_request_id
                .clone()
                .unwrap_or_else(|| "-".to_string()),
            row.deployment.clone(),
            row.behavior_id.clone(),
            row.state.clone(),
            row.started_at.clone(),
        ]);
    }

    let mut widths = HEADERS.map(|header| header.chars().count());
    for row in &table_rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.chars().count());
        }
    }

    let mut output = String::new();
    push_cells(&mut output, &HEADERS.map(ToOwned::to_owned), &widths);
    for row in table_rows {
        push_cells(&mut output, &row, &widths);
    }
    output
}

fn push_cells(output: &mut String, cells: &[String; 6], widths: &[usize; 6]) {
    for (idx, cell) in cells.iter().enumerate() {
        if idx > 0 {
            output.push_str("  ");
        }
        output.push_str(cell);
        for _ in cell.chars().count()..widths[idx] {
            output.push(' ');
        }
    }
    output.push('\n');
}

fn tree_from_rows(rows: &[LineageOutputRow]) -> Vec<LineageTreeNode> {
    let rows_by_id = rows
        .iter()
        .map(|row| (row.request_id.clone(), row.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut children_by_parent = BTreeMap::<String, Vec<String>>::new();
    let mut root_ids = Vec::new();

    for row in rows {
        if let Some(parent_request_id) = row.parent_request_id.as_deref() {
            if rows_by_id.contains_key(parent_request_id) {
                children_by_parent
                    .entry(parent_request_id.to_string())
                    .or_default()
                    .push(row.request_id.clone());
                continue;
            }
        }
        root_ids.push(row.request_id.clone());
    }

    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    for root_id in root_ids {
        append_tree_node(
            &root_id,
            &rows_by_id,
            &children_by_parent,
            &mut seen,
            &mut roots,
        );
    }
    roots
}

fn append_tree_node(
    request_id: &str,
    rows_by_id: &BTreeMap<String, LineageOutputRow>,
    children_by_parent: &BTreeMap<String, Vec<String>>,
    seen: &mut HashSet<String>,
    output: &mut Vec<LineageTreeNode>,
) {
    if !seen.insert(request_id.to_string()) {
        return;
    }
    let Some(row) = rows_by_id.get(request_id) else {
        return;
    };
    let mut children = Vec::new();
    if let Some(child_ids) = children_by_parent.get(request_id) {
        for child_id in child_ids {
            append_tree_node(
                child_id,
                rows_by_id,
                children_by_parent,
                seen,
                &mut children,
            );
        }
    }
    output.push(LineageTreeNode {
        row: row.clone(),
        children,
    });
}

#[derive(Debug, Clone)]
struct LineageNode {
    row: AgentRequestRow,
    depth: usize,
}

fn request_parent_id(row: &AgentRequestRow) -> Option<String> {
    row.caused_by_parent_request_id
        .as_deref()
        .and_then(non_empty_str)
        .map(ToOwned::to_owned)
}

fn request_sort_key(row: &AgentRequestRow) -> (&str, &str) {
    (
        row.created_at
            .as_deref()
            .and_then(non_empty_str)
            .unwrap_or_default(),
        row.request_id.as_str(),
    )
}

#[derive(Debug, Clone, Serialize)]
struct LineageOutputRow {
    child_request_id: String,
    request_id: String,
    parent_request_id: Option<String>,
    deployment: String,
    agent_did: Option<String>,
    behavior_id: String,
    state: String,
    started_at: String,
    depth: usize,
    #[serde(skip_serializing)]
    display_request_id: String,
}

impl LineageOutputRow {
    fn from_node(node: &LineageNode, indent: bool) -> Self {
        let request_id = node.row.request_id.clone();
        let display_request_id = if indent {
            format!("{}{}", "  ".repeat(node.depth), request_id)
        } else {
            request_id.clone()
        };
        let agent_did = node
            .row
            .agent_did
            .as_deref()
            .and_then(non_empty_str)
            .map(ToOwned::to_owned);
        let deployment = agent_did.clone().unwrap_or_else(|| "-".to_string());
        let behavior_id = node
            .row
            .behavior_id
            .as_deref()
            .and_then(non_empty_str)
            .unwrap_or("-")
            .to_string();
        let state = node
            .row
            .lifecycle_state
            .map(RequestLifecycleState::as_str)
            .unwrap_or("unknown")
            .to_string();
        let started_at = node
            .row
            .claimed_at
            .as_deref()
            .and_then(non_empty_str)
            .or_else(|| node.row.created_at.as_deref().and_then(non_empty_str))
            .unwrap_or("-")
            .to_string();

        Self {
            child_request_id: request_id.clone(),
            request_id,
            parent_request_id: request_parent_id(&node.row),
            deployment,
            agent_did,
            behavior_id,
            state,
            started_at,
            depth: node.depth,
            display_request_id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct LineageTreeNode {
    #[serde(flatten)]
    row: LineageOutputRow,
    children: Vec<LineageTreeNode>,
}

fn non_empty_str(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(request_id: &str, parent: Option<&str>, created_at: &str) -> AgentRequestRow {
        serde_json::from_value(json!({
            "request_id": request_id,
            "agent_did": "did:key:zTest",
            "behavior_id": request_id,
            "lifecycle_state": "pending",
            "created_at": created_at,
            "caused_by_parent_request_id": parent,
        }))
        .expect("canonical AgentRequest test row")
    }

    #[test]
    fn table_renderer_indents_tree_request_column_only() {
        let rows = vec![
            LineageNode {
                row: row("parent", None, "2026-05-20T00:00:00Z"),
                depth: 0,
            },
            LineageNode {
                row: row("child", Some("parent"), "2026-05-20T00:00:01Z"),
                depth: 1,
            },
        ];
        let rendered = render_table(&output_rows(&rows, true));
        assert!(rendered.contains("CHILD_REQUEST_ID"));
        assert!(rendered.contains("parent"));
        assert!(rendered.contains("  child"));
        assert!(rendered.contains("parent"));
    }
}
