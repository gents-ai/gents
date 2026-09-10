use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::session::session_scope_filter;
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
    // Both routes cancel within an explicit principal scope: a duplicate
    // logical request ID under a foreign owner must never be interrupted.
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())
        .context("resolving agent_did for scoped subagent cancellation")?;

    let snapshots = match &access {
        ConfigAccess::Graphql(graphql) => {
            let affected =
                cancel_subagent_graphql(&access, graphql, &agent_did, &request_id, args.cascade)
                    .await?;
            if let Some(timeout) = wait_timeout {
                wait_for_terminal_graphql(graphql, &affected, timeout).await?
            } else {
                snapshot_requests_graphql(graphql, &affected).await?
            }
        }
        ConfigAccess::Local(node) => {
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
    access: &ConfigAccess,
    graphql: &str,
    agent_did: &str,
    request_id: &str,
    cascade: bool,
) -> Result<Vec<ScopedRequestRef>> {
    // Resolve the root within its exact principal scope. Duplicate logical
    // IDs under any owner are rejected here — never first-row picked.
    let root = resolve_scoped_root_graphql(graphql, agent_did, request_id).await?;
    let mut affected = Vec::new();
    let mut seen = BTreeSet::new();
    push_scoped_ref(&mut affected, &mut seen, root.clone());

    if cascade {
        // Reuse the canonical descendant owner over the shared ConfigAccess
        // seam: it corroborates each edge against the parent-authored bridge
        // receipt and only exposes children with verified physical identity
        // (`child_request_doc_id`), so the cascade never joins a logical label.
        let mut after = None;
        let mut cascade_parents = BTreeSet::from([root
            .doc_id
            .clone()
            .context("cascade root missing physical identity")?]);
        loop {
            let page = gents::descendant_graph::resolve_descendant_graph_by_doc_id(
                DescendantGraphAccess::Config(access),
                &DescendantQuery {
                    after: after.clone(),
                    limit: MAX_DESCENDANT_PAGE_LIMIT,
                    ..DescendantQuery::all(&root.request_id)
                },
                root.doc_id
                    .as_deref()
                    .context("cascade root missing physical identity")?,
                root.agent_did
                    .as_deref()
                    .context("cascade root missing principal")?,
                root.requester_did.as_deref(),
            )
            .await?;
            for edge in &page.edges {
                if !cascade_parents.contains(&edge.immediate_parent_request_doc_id)
                    || edge.cancel_policy.as_deref() != Some("cascade")
                {
                    continue;
                }
                if let Some(child) = edge.child_request_doc_id.as_ref() {
                    cascade_parents.insert(child.clone());
                }

                let (Some(child_doc_id), Some(child_agent_did)) = (
                    edge.child_request_doc_id.as_deref(),
                    edge.principal_did.as_deref(),
                ) else {
                    // Awaiting materialization or physically uncorroborated:
                    // not interrupt-eligible, and not a cancel target.
                    continue;
                };
                push_scoped_ref(
                    &mut affected,
                    &mut seen,
                    ScopedRequestRef {
                        request_id: edge.child_request_id.clone(),
                        doc_id: Some(child_doc_id.to_string()),
                        agent_did: Some(child_agent_did.to_string()),
                        requester_did: edge.child_requester_did.clone(),
                        session_id: edge.child_session_id.clone(),
                    },
                );
            }
            if !page.has_more {
                break;
            }
            after = page.next_cursor;
        }
    }

    for target in &affected {
        interrupt_request_graphql(graphql, target).await?;
    }
    Ok(affected)
}

/// Resolve the unique AgentRequest for a logical ID under the caller's exact
/// principal scope. A duplicate logical ID under any owner is a data error,
/// never silently joined: the scoped query must return at most one row.
async fn resolve_scoped_root_graphql(
    graphql: &str,
    agent_did: &str,
    request_id: &str,
) -> Result<ScopedRequestRef> {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    request_id: {{ _eq: "{escaped_request_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }}
                }},
                limit: 2
            ) {{
                _docID
                request_id
                agent_did
                requester_did
                session_id
            }}
        }}"#
    );
    let response = post_graphql(graphql, &query).await?;
    let mut rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    ensure_unique_scoped_root(agent_did, request_id, rows.len())?;
    let row = rows
        .pop()
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found for agent {agent_did}"))?;
    scoped_ref_from_row(&row, request_id)
}

/// Duplicate logical IDs within the requested principal scope are a data
/// error, never a first-row pick: a shared logical label must never select
/// which physical document gets interrupted.
fn ensure_unique_scoped_root(agent_did: &str, request_id: &str, row_count: usize) -> Result<()> {
    anyhow::ensure!(
        row_count <= 1,
        "request {request_id} is ambiguous for agent {agent_did}; refusing to interrupt a shared logical ID"
    );
    Ok(())
}

/// Reuse the shared physical interrupt owner over HTTP transaction access.
async fn interrupt_request_graphql(graphql: &str, target: &ScopedRequestRef) -> Result<()> {
    gents::interrupt::interrupt_request_by_doc_id_with_access(
        &ConfigAccess::Graphql(graphql.to_owned()),
        target
            .doc_id
            .as_deref()
            .context("interrupt target missing physical identity")?,
        target
            .agent_did
            .as_deref()
            .context("interrupt target missing principal")?,
        target.requester_did.as_deref(),
    )
    .await
}

async fn cancel_subagent_local(
    node: Arc<EmbeddedNode>,
    agent_did: &str,
    request_id: &str,
    cascade: bool,
    cause: CancelCause,
) -> Result<Vec<ScopedRequestRef>> {
    // Resolve the unique root with the caller's exact principal scope before
    // any interrupt or cascade decision. Duplicate logical IDs under a foreign
    // owner are rejected here, never silently joined.
    let target = fetch_request_row_local_scoped(node.as_ref(), agent_did, request_id).await?;
    let target_ref = scoped_ref_from_agent_row(&target, request_id)?;
    let mut affected = Vec::new();
    let mut seen_requests = BTreeSet::new();

    if cascade {
        cancel_parent_bridge_local(node.clone(), cause, agent_did, &target).await?;
    }
    interrupt_request_local(
        node.as_ref(),
        &mut affected,
        &mut seen_requests,
        &target_ref,
    )
    .await?;

    if cascade {
        cancel_descendant_bridges_local(
            node.clone(),
            cause,
            &target_ref,
            &mut affected,
            &mut seen_requests,
        )
        .await?;
    }

    Ok(affected)
}

async fn cancel_parent_bridge_local(
    node: Arc<EmbeddedNode>,
    cause: CancelCause,
    _agent_did: &str,
    target: &AgentRequestRow,
) -> Result<()> {
    let Some(parent_request_id) = target.caused_by_parent_request_id.as_deref() else {
        return Ok(());
    };
    let Some(parent_tool_call_id) = target.caused_by_parent_tool_call_id.as_deref() else {
        return Ok(());
    };
    let parent_doc_id = target
        .caused_by_parent_request_doc_id
        .as_deref()
        .context("parent bridge lacks physical request identity")?;
    let bridge_doc_id = target
        .caused_by_parent_tool_call_doc_id
        .as_deref()
        .context("parent bridge lacks physical tool identity")?;
    let verified = gents::descendant_graph::resolve_physical_bridge_child(
        DescendantGraphAccess::Local(node.as_ref()),
        parent_doc_id,
        bridge_doc_id,
    )
    .await?
    .context("parent bridge does not corroborate child")?;
    anyhow::ensure!(
        verified.doc_id == target.doc_id
            && verified.agent_did == target.agent_did
            && verified.requester_did == target.requester_did
            && verified.session_id == target.session_id,
        "parent bridge selects a different physical child or scope"
    );
    let physical = escape_graphql_string(parent_doc_id);
    let response = execute_node_json(node.as_ref(), &format!(r#"{{AgentRequest(filter: {{_docID: {{_eq: "{physical}"}}}},limit:2){{_docID request_id agent_did requester_did session_id}}}}"#)).await?;
    let parent = request_row_from_response(&response, parent_request_id)?;
    anyhow::ensure!(
        parent.request_id == parent_request_id && parent.doc_id.as_deref() == Some(parent_doc_id),
        "parent physical and logical identities disagree"
    );
    let parent_session_id = parent
        .session_id
        .as_deref()
        .context("parent request missing session")?;
    let parent_owner = parent
        .agent_did
        .as_deref()
        .context("parent request missing principal")?;
    cancel_bridge_local_by_doc_id(
        node,
        cause,
        parent_owner,
        parent_session_id,
        parent.requester_did.as_deref(),
        parent_tool_call_id,
        bridge_doc_id,
        BridgeKind::Parent,
    )
    .await
    .map(|_| ())
}

async fn cancel_descendant_bridges_local(
    node: Arc<EmbeddedNode>,
    cause: CancelCause,
    root: &ScopedRequestRef,
    affected: &mut Vec<ScopedRequestRef>,
    seen_requests: &mut BTreeSet<String>,
) -> Result<()> {
    let mut after = None;
    let mut cascade_parents = BTreeSet::from([root
        .doc_id
        .clone()
        .context("cascade root missing physical identity")?]);
    loop {
        let page = gents::descendant_graph::resolve_descendant_graph_by_doc_id(
            DescendantGraphAccess::Local(node.as_ref()),
            &DescendantQuery {
                after: after.clone(),
                limit: MAX_DESCENDANT_PAGE_LIMIT,
                ..DescendantQuery::all(&root.request_id)
            },
            root.doc_id
                .as_deref()
                .context("cascade root missing physical identity")?,
            root.agent_did
                .as_deref()
                .context("cascade root missing principal")?,
            root.requester_did.as_deref(),
        )
        .await?;
        for edge in &page.edges {
            if !cascade_parents.contains(&edge.immediate_parent_request_doc_id)
                || edge.cancel_policy.as_deref() != Some("cascade")
            {
                continue;
            }
            if let Some(child) = edge.child_request_doc_id.as_ref() {
                cascade_parents.insert(child.clone());
            }
            let dispatch = cancel_bridge_local_by_doc_id(
                node.clone(),
                cause,
                &edge.immediate_parent_agent_did,
                &edge.immediate_parent_session_id,
                edge.immediate_parent_requester_did.as_deref(),
                &edge.immediate_parent_tool_call_id,
                &edge.immediate_parent_tool_call_doc_id,
                BridgeKind::Descendant,
            )
            .await?;
            if let Some(child) = dispatch {
                interrupt_request_local(node.as_ref(), affected, seen_requests, &child).await?;
            }
        }
        if !page.has_more {
            break;
        }
        after = page.next_cursor;
    }
    Ok(())
}

async fn cancel_bridge_local_by_doc_id(
    node: Arc<EmbeddedNode>,
    cause: CancelCause,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    tool_call_id: &str,
    bridge_doc_id: &str,
    bridge_kind: BridgeKind,
) -> Result<Option<ScopedRequestRef>> {
    if tool_lifecycle_state_local(
        node.as_ref(),
        bridge_doc_id,
        agent_did,
        session_id,
        requester_did,
    )
    .await?
    .as_deref()
        != Some("running")
    {
        return Ok(None);
    }
    // Load by physical docID within the exact session scope; a logical tool ID
    // collision can never substitute a different bridge for cancellation.
    let Some(lifecycle) = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        bridge_doc_id,
        agent_did,
        session_id,
        requester_did,
    )
    .await?
    else {
        return Ok(None);
    };
    cancel_loaded_bridge(
        lifecycle,
        cause,
        agent_did,
        session_id,
        tool_call_id,
        bridge_kind,
    )
    .await
}

async fn cancel_loaded_bridge(
    mut lifecycle: ToolCallLifecycle,
    cause: CancelCause,
    agent_did: &str,
    session_id: &str,
    tool_call_id: &str,
    bridge_kind: BridgeKind,
) -> Result<Option<ScopedRequestRef>> {
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
        // Carry the verified physical child (docID + owner + requester) so the
        // cascade interrupts the exact corroborated row, not a logical label.
        Some(CascadeDispatch::Local { intent, child }) => {
            Some(scoped_ref_from_agent_row(&child, &intent.child_request_id)?)
        }
        Some(CascadeDispatch::RemoteIntentWritten) | None => None,
    })
}

async fn interrupt_request_local(
    node: &EmbeddedNode,
    affected: &mut Vec<ScopedRequestRef>,
    seen_requests: &mut BTreeSet<String>,
    target: &ScopedRequestRef,
) -> Result<()> {
    // Reuse the existing scoped interrupt owner: it latches by physical docID
    // within the exact principal/requester scope (absent requester encodes
    // `_eq: null`, never a wildcard) and ambiguity-rejects duplicates.
    gents::interrupt::interrupt_request_by_doc_id(
        node,
        target.doc_id.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "interrupt target {} has no verified physical identity",
                target.request_id
            )
        })?,
        target.agent_did.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "interrupt target {} has no verified principal",
                target.request_id
            )
        })?,
        target.requester_did.as_deref(),
    )
    .await?;
    push_scoped_ref(affected, seen_requests, target.clone());
    Ok(())
}

fn push_scoped_ref(
    values: &mut Vec<ScopedRequestRef>,
    seen: &mut BTreeSet<String>,
    value: ScopedRequestRef,
) {
    if seen.insert(value.request_id.clone()) {
        values.push(value);
    }
}

/// Physical-identity cancel/wait/snapshot target: the verified docID plus the
/// exact owner and requester scope it was resolved under. Absent requester is
/// exact None (anonymous scope), never a wildcard.
#[derive(Debug, Clone)]
struct ScopedRequestRef {
    request_id: String,
    doc_id: Option<String>,
    agent_did: Option<String>,
    requester_did: Option<String>,
    session_id: Option<String>,
}

fn scoped_ref_from_row(row: &Value, request_id: &str) -> Result<ScopedRequestRef> {
    let doc_id = string_field(row, "_docID");
    let agent_did = string_field(row, "agent_did");
    anyhow::ensure!(
        doc_id.is_some() && agent_did.is_some(),
        "request {request_id} resolved without physical identity or principal; refusing scoped interrupt"
    );
    Ok(ScopedRequestRef {
        request_id: request_id.to_string(),
        doc_id,
        agent_did,
        requester_did: raw_requester_field(row, "requester_did"),
        session_id: string_field(row, "session_id"),
    })
}

/// Build a scoped ref from the canonical typed row. `_docID` is
/// `skip_serializing`, so the physical identity must come from the raw query
/// envelope carried on `doc_id` by the scoped local fetch.
fn scoped_ref_from_agent_row(row: &AgentRequestRow, request_id: &str) -> Result<ScopedRequestRef> {
    let doc_id = row.doc_id.clone().filter(|value| !value.trim().is_empty());
    let agent_did = row
        .agent_did
        .clone()
        .filter(|value| !value.trim().is_empty());
    anyhow::ensure!(
        doc_id.is_some() && agent_did.is_some(),
        "request {request_id} lacks physical identity or principal; refusing scoped cancel"
    );
    Ok(ScopedRequestRef {
        request_id: request_id.to_string(),
        doc_id,
        agent_did,
        requester_did: row.requester_did.clone(),
        session_id: row.session_id.clone(),
    })
}

fn raw_requester_field(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

/// Fetch the unique local root inside the exact principal scope, retaining the
/// physical `_docID` on the typed row for scoped interrupt/cascade routing.
async fn fetch_request_row_local_scoped(
    node: &EmbeddedNode,
    agent_did: &str,
    request_id: &str,
) -> Result<AgentRequestRow> {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    request_id: {{ _eq: "{escaped_request_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }}
                }},
                limit: 2
            ) {{
                _docID
                request_id
                agent_did
                requester_did
                session_id
                lifecycle_state
                interrupt_requested_at
                caused_by_parent_request_id
                caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id
                caused_by_parent_tool_call_doc_id
            }}
        }}"#
    );
    let response = execute_node_json(node, &query).await?;
    let mut rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(
        rows.len() <= 1,
        "request {request_id} is ambiguous for agent {agent_did}; refusing to cancel a shared logical ID"
    );
    let row = rows
        .pop()
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found for agent {agent_did}"))?;
    let mut typed: AgentRequestRow = serde_json::from_value(row.clone())
        .with_context(|| format!("decoding AgentRequest {request_id}"))?;
    typed.doc_id = row
        .get("_docID")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);
    anyhow::ensure!(
        typed.doc_id.is_some(),
        "request {request_id} resolved without physical identity; refusing scoped cancel"
    );
    Ok(typed)
}

async fn wait_for_terminal_graphql(
    graphql: &str,
    affected: &[ScopedRequestRef],
    timeout: Duration,
) -> Result<Vec<RequestCancelSnapshot>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshots = snapshot_requests_graphql(graphql, affected).await?;
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
    affected: &[ScopedRequestRef],
    timeout: Duration,
) -> Result<Vec<RequestCancelSnapshot>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshots = snapshot_requests_local(node, affected).await?;
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
    affected: &[ScopedRequestRef],
) -> Result<Vec<RequestCancelSnapshot>> {
    let mut rows = Vec::with_capacity(affected.len());
    for target in affected {
        let row = scoped_fetch_row_graphql(graphql, target).await?;
        rows.push(request_cancel_snapshot(row));
    }
    Ok(rows)
}

async fn snapshot_requests_local(
    node: &EmbeddedNode,
    affected: &[ScopedRequestRef],
) -> Result<Vec<RequestCancelSnapshot>> {
    let mut rows = Vec::with_capacity(affected.len());
    for target in affected {
        let row = scoped_fetch_row_local(node, target).await?;
        rows.push(request_cancel_snapshot(row));
    }
    Ok(rows)
}

/// Read one snapshot row through its verified physical identity. A duplicate
/// logical ID under a foreign owner can never be joined here: the scoped
/// lookup matches the exact `_docID` resolved at cancel time, ambiguity-rejects,
/// and surfaces failure clearly instead of swallowing a decode error.
async fn scoped_fetch_row_graphql(
    graphql: &str,
    target: &ScopedRequestRef,
) -> Result<AgentRequestRow> {
    let escaped_doc_id = escape_graphql_string(target.doc_id.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "snapshot target {} has no verified physical identity",
            target.request_id
        )
    })?);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 2
            ) {{
                _docID
                request_id
                agent_did
                session_id
                requester_did
                lifecycle_state
                interrupt_requested_at
            }}
        }}"#
    );
    let response = post_graphql(graphql, &query).await?;
    request_row_from_scoped_response(&response, target)
}

async fn scoped_fetch_row_local(
    node: &EmbeddedNode,
    target: &ScopedRequestRef,
) -> Result<AgentRequestRow> {
    let escaped_doc_id = escape_graphql_string(target.doc_id.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "snapshot target {} has no verified physical identity",
            target.request_id
        )
    })?);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 2
            ) {{
                _docID
                request_id
                agent_did
                session_id
                requester_did
                lifecycle_state
                interrupt_requested_at
            }}
        }}"#
    );
    let response = execute_node_json(node, &query).await?;
    request_row_from_scoped_response(&response, target)
}

fn request_row_from_scoped_response(
    response: &Value,
    target: &ScopedRequestRef,
) -> Result<AgentRequestRow> {
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("scoped snapshot query omitted rows"))?;
    anyhow::ensure!(
        rows.len() <= 1,
        "snapshot for request {} is ambiguous across physical documents",
        target.request_id
    );
    let row = rows
        .first()
        .ok_or_else(|| anyhow::anyhow!("request {} not found", target.request_id))?;
    anyhow::ensure!(
        row["_docID"].as_str() == target.doc_id.as_deref()
            && row["request_id"].as_str() == Some(target.request_id.as_str())
            && row["agent_did"].as_str() == target.agent_did.as_deref()
            && row["requester_did"].as_str() == target.requester_did.as_deref()
            && row["session_id"].as_str() == target.session_id.as_deref(),
        "request snapshot no longer matches selected physical identity and scope"
    );
    serde_json::from_value(row.clone())
        .with_context(|| format!("decoding AgentRequest {}", target.request_id))
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

fn request_row_from_response(response: &Value, request_id: &str) -> Result<AgentRequestRow> {
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("request row query omitted rows"))?;
    anyhow::ensure!(
        rows.len() <= 1,
        "request {request_id} is ambiguous across physical AgentRequest documents"
    );
    let row = rows
        .first()
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))?;
    serde_json::from_value(row.clone())
        .with_context(|| format!("decoding AgentRequest {request_id}"))
}

#[cfg(test)]
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
            let doc_id = string_field(row, "_docID");
            Some(BridgeRow {
                doc_id,
                tool_call_id,
                child_request_id,
            })
        })
        .collect())
}

async fn tool_lifecycle_state_local(
    node: &EmbeddedNode,
    doc_id: &str,
    owner: &str,
    session_id: &str,
    requester: Option<&str>,
) -> Result<Option<String>> {
    let physical = escape_graphql_string(doc_id);
    let scope = session_scope_filter(owner, session_id, requester);
    let response = execute_node_json(node, &format!(r#"{{AgentToolCall(filter: {{{scope},_docID: {{_eq: "{physical}"}}}},limit:2){{_docID lifecycle_state}}}}"#)).await?;
    let rows = response["data"]["AgentToolCall"]
        .as_array()
        .context("bridge state query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "physical bridge identity is ambiguous");
    Ok(rows
        .first()
        .and_then(|row| row["lifecycle_state"].as_str())
        .map(str::to_owned))
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
        .filter(|value| !value.trim().is_empty())
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
#[cfg(test)]
struct BridgeRow {
    doc_id: Option<String>,
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
        // A duplicate logical ID across owners is a data error: silently
        // overwriting one row in the map would join a foreign-owned physical
        // document into another lineage's tree.
        if rows_by_id.contains_key(&request_id) {
            anyhow::bail!(
                "request {request_id} is ambiguous across physical AgentRequest documents; refusing to render a shared logical ID"
            );
        }
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
                limit: 2
            ) {{
                {AGENT_REQUEST_FIELDS}
            }}
        }}"#
    );
    let rows = load_request_rows(access, &query).await?;
    // A duplicate logical ID is a data error, never a silent first-row pick:
    // the rooted list must not join a foreign-owned row sharing the label.
    anyhow::ensure!(
        rows.len() <= 1,
        "request {request_id} is ambiguous across {} AgentRequest documents; refusing to render a shared logical ID",
        rows.len()
    );
    Ok(rows.into_iter().next())
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

    fn scoped_row(
        request_id: &str,
        doc_id: &str,
        agent_did: &str,
        requester_did: Option<&str>,
        session_id: Option<&str>,
    ) -> Value {
        json!({
            "_docID": doc_id,
            "request_id": request_id,
            "agent_did": agent_did,
            "requester_did": requester_did,
            "session_id": session_id,
        })
    }

    #[test]
    fn scoped_ref_preserves_exact_none_requester_not_wildcard() {
        // Absent requester must resolve to None exactly, so the scoped
        // interrupt owner encodes `requester_did: {_eq: null}` — a request
        // with any requester DID is a different scope, never matched.
        let own = scoped_ref_from_row(
            &scoped_row("r1", "doc-own-1", "did:key:zOwner", None, Some("session-1")),
            "r1",
        )
        .expect("own scope ref");
        assert_eq!(own.requester_did, None);
        assert_eq!(own.doc_id.as_deref(), Some("doc-own-1"));
        assert_eq!(own.agent_did.as_deref(), Some("did:key:zOwner"));

        let foreign_requester = scoped_ref_from_row(
            &scoped_row(
                "r1",
                "doc-foreign-1",
                "did:key:zOwner",
                Some("did:key:zRequester"),
                Some("session-1"),
            ),
            "r1",
        )
        .expect("requester-scoped ref");
        assert_eq!(
            foreign_requester.requester_did.as_deref(),
            Some("did:key:zRequester"),
            "requester presence must be carried, never collapsed into the anonymous scope"
        );
    }

    #[test]
    fn scoped_ref_rejects_missing_physical_identity_or_principal() {
        // `string_field` trims empties, so an empty _docID is "missing".
        let missing_doc = json!({
            "_docID": "",
            "request_id": "r1",
            "agent_did": "did:key:zOwner",
        });
        assert!(scoped_ref_from_row(&missing_doc, "r1").is_err());

        let missing_principal = json!({
            "_docID": "doc-1",
            "request_id": "r1",
        });
        assert!(scoped_ref_from_row(&missing_principal, "r1").is_err());
    }

    #[test]
    fn session_scope_filter_encodes_exact_none_requester() {
        let anonymous = session_scope_filter("did:key:zOwner", "session-1", None);
        assert!(
            anonymous.contains("requester_did: { _eq: null }"),
            "absent requester must be the exact anonymous scope, not a wildcard: {anonymous}"
        );
        let scoped =
            session_scope_filter("did:key:zOwner", "session-1", Some("did:key:zRequester"));
        assert!(
            scoped.contains("requester_did: { _eq: \"did:key:zRequester\" }"),
            "present requester must match exactly: {scoped}"
        );
        assert_ne!(anonymous, scoped);
    }

    #[test]
    fn bridge_rows_carry_physical_doc_id_for_scoped_cancellation() {
        let response = json!({
            "data": {
                "AgentToolCall": [
                    {"_docID": "bridge-doc-1", "tool_call_id": "tc-1", "child_request_id": "child-1"},
                    {"tool_call_id": "tc-2"}
                ]
            }
        });
        let rows = bridge_rows_from_response(&response).expect("bridge rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].doc_id.as_deref(),
            Some("bridge-doc-1"),
            "physical bridge identity must survive row decoding for docID-scoped lifecycle loads"
        );
        assert_eq!(rows[0].tool_call_id, "tc-1");
        assert_eq!(rows[0].child_request_id.as_deref(), Some("child-1"));
        assert!(
            rows[1].doc_id.is_none(),
            "a row without _docID must not fabricate physical identity"
        );
    }

    #[test]
    fn scoped_root_resolution_rejects_duplicate_logical_ids_in_scope() {
        // Two physical documents sharing one logical ID within the requested
        // principal scope are ambiguous: interrupt must be refused, never
        // first-row picked.
        assert!(ensure_unique_scoped_root("did:key:zOwner", "r1", 1).is_ok());
        let error = ensure_unique_scoped_root("did:key:zOwner", "r1", 2)
            .expect_err("duplicate scoped root must be rejected");
        assert!(
            error
                .to_string()
                .contains("refusing to interrupt a shared logical ID"),
            "rejection must name the shared-logical-ID hazard: {error}"
        );
    }
}
