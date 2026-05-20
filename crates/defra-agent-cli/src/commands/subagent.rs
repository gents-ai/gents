use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::tool_call_lifecycle::{CancelCause, CascadeDispatch, ToolCallLifecycle};
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::args::{SubagentCancelArgs, SubagentCancelOutput, SubagentCommand};
use crate::config_writes::ConfigAccess;
use crate::{
    parse_duration_suffix, post_graphql, print_json, resolve_agent_did, resolve_config_access,
    resolve_request_id,
};

const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) async fn dispatch(command: SubagentCommand) -> Result<()> {
    match command {
        SubagentCommand::Cancel(args) => subagent_cancel(args).await,
    }
}

async fn subagent_cancel(args: SubagentCancelArgs) -> Result<()> {
    let request_id =
        resolve_request_id(args.request_id.as_deref(), args.request_id_flag.as_deref())?;
    let cause = parse_cancel_cause(&args.cause)?;
    let wait_timeout = resolve_wait_timeout(args.wait, args.timeout.as_deref())?;

    let (access, _) = resolve_config_access(
        args.home.as_deref(),
        args.graphql.as_deref(),
        /* ensure_local_schemas */ true,
    )
    .await?;

    let snapshots = match access {
        ConfigAccess::Graphql(graphql) => {
            // Live GraphQL mode latches request interrupts and lets the daemon
            // transition any in-flight bridge tool-calls it owns. Local mode
            // has no daemon process, so it performs bridge transitions itself.
            let affected = cancel_subagent_graphql(&graphql, &request_id, args.cascade).await?;
            if let Some(timeout) = wait_timeout {
                wait_for_terminal_graphql(&graphql, &affected, timeout).await?
            } else {
                snapshot_requests_graphql(&graphql, &affected).await?
            }
        }
        ConfigAccess::Local(node) => {
            let node = Arc::new(node);
            defra_agent::migration::ensure_tool_call_migrations(node.clone()).await?;
            defra_agent::migration::ensure_subagent_extensions_migrations(node.clone()).await?;
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

    // TODO: Route this through a server-side interrupt endpoint once one
    // exists, so idempotency and queue-drain behavior stay centralized with
    // defra_agent::interrupt_request.
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
    target: &RequestRow,
) -> Result<()> {
    let Some(parent_request_id) = target.parent_request_id.as_deref() else {
        return Ok(());
    };
    let Some(parent_tool_call_id) = target.parent_tool_call_id.as_deref() else {
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
    defra_agent::interrupt_request(node, request_id).await?;
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
                .as_deref()
                .is_some_and(is_terminal_state)
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
                .as_deref()
                .is_some_and(is_terminal_state)
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

fn is_terminal_state(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "superseded" | "dead" | "interrupted"
    )
}

async fn snapshot_requests_graphql(
    graphql: &str,
    request_ids: &[String],
) -> Result<Vec<RequestCancelSnapshot>> {
    let mut rows = Vec::with_capacity(request_ids.len());
    for request_id in request_ids {
        let row = fetch_request_row_graphql(graphql, request_id).await?;
        rows.push(row.into_snapshot());
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
        rows.push(row.into_snapshot());
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
                row.lifecycle_state.as_deref().unwrap_or("missing")
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

async fn fetch_request_row_graphql(graphql: &str, request_id: &str) -> Result<RequestRow> {
    let query = request_row_query(request_id);
    let response = post_graphql(graphql, &query).await?;
    request_row_from_response(&response, request_id)
}

async fn fetch_request_row_local(node: &EmbeddedNode, request_id: &str) -> Result<RequestRow> {
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

fn request_row_from_response(response: &Value, request_id: &str) -> Result<RequestRow> {
    let row = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))?;
    Ok(RequestRow {
        request_id: string_field(row, "request_id").unwrap_or_else(|| request_id.to_string()),
        agent_did: string_field(row, "agent_did"),
        session_id: string_field(row, "session_id"),
        lifecycle_state: string_field(row, "lifecycle_state"),
        interrupt_requested_at: string_field(row, "interrupt_requested_at"),
        parent_request_id: string_field(row, "caused_by_parent_request_id"),
        parent_tool_call_id: string_field(row, "caused_by_parent_tool_call_id"),
    })
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

fn render_cancel_output(output: SubagentCancelOutput, render: SubagentCancelRender) -> Result<()> {
    match output {
        SubagentCancelOutput::Text => {
            for request in &render.requests {
                println!("{}", request.request_id);
            }
            Ok(())
        }
        SubagentCancelOutput::Json => print_json(&json!({
            "request_id": render.request_id,
            "cascade": render.cascade,
            "cause": render.cause,
            "wait": render.wait,
            "interrupted_request_ids": render.requests.iter().map(|row| row.request_id.as_str()).collect::<Vec<_>>(),
            "requests": render.requests,
        })),
    }
}

#[derive(Debug, Clone)]
struct RequestRow {
    request_id: String,
    agent_did: Option<String>,
    session_id: Option<String>,
    lifecycle_state: Option<String>,
    interrupt_requested_at: Option<String>,
    parent_request_id: Option<String>,
    parent_tool_call_id: Option<String>,
}

impl RequestRow {
    fn into_snapshot(self) -> RequestCancelSnapshot {
        RequestCancelSnapshot {
            request_id: self.request_id,
            agent_did: self.agent_did,
            lifecycle_state: self.lifecycle_state,
            interrupt_requested_at: self.interrupt_requested_at,
        }
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
    lifecycle_state: Option<String>,
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
