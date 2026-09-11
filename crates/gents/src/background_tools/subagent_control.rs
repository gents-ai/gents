//! Shared, authorized cancellation for native tools and external adapters.
//! All transitions remain owned by the existing tool/request lifecycle APIs.

use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use super::ChildEdge;
use crate::descendant_graph::{resolve_session_descendant_edge, DescendantGraphAccess};
use crate::graphql::ensure_no_errors;
use crate::tool_call_lifecycle::{CancelCause, CascadeDispatch, ToolCallLifecycle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentCancellation {
    pub child_request_id: String,
    pub child_session_id: String,
    pub behavior_id: String,
    pub parent_session_id: String,
    pub parent_tool_call_id: String,
    pub active_interrupted: bool,
    pub descendants_cancelled: usize,
    pub queued_drained: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelSubagentOutcome {
    Unavailable { diagnostic: String, retryable: bool },
    NotAuthorized,
    Cancelled(SubagentCancellation),
    AlreadyTerminal(SubagentCancellation),
}

/// Cancel a child controlled by this caller's session principal, including a
/// child spawned by an earlier turn. Authorization is resolved here so wire
/// adapters cannot bypass the canonical descendant relationship checks.
pub async fn cancel_session_subagent(
    node: Arc<EmbeddedNode>,
    caller_request_id: &str,
    child_request_id: &str,
    reason: Option<&str>,
) -> Result<CancelSubagentOutcome> {
    let Some(canonical) = resolve_session_descendant_edge(
        DescendantGraphAccess::Local(&node),
        caller_request_id,
        child_request_id,
    )
    .await?
    else {
        return Ok(CancelSubagentOutcome::Unavailable {
            diagnostic: "child subagent request is not available to this session principal".into(),
            retryable: false,
        });
    };
    if !canonical.readable() {
        return Ok(CancelSubagentOutcome::Unavailable {
            diagnostic: canonical
                .diagnostic
                .clone()
                .unwrap_or_else(|| format!("child request {child_request_id} is not materialized")),
            retryable: canonical.retryable(),
        });
    }
    if !canonical.controllable() {
        return Ok(CancelSubagentOutcome::NotAuthorized);
    }
    let edge = ChildEdge::from_descendant(&canonical)
        .context("authorized descendant edge lacks materialized child identity")?;
    let Some(lifecycle) = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &edge.parent_tool_call_doc_id,
        &edge.parent_agent_did,
        &edge.parent_session_id,
        edge.parent_requester_did.as_deref(),
    )
    .await?
    else {
        return Ok(CancelSubagentOutcome::Unavailable {
            diagnostic: "authorized subagent bridge is no longer available".into(),
            retryable: true,
        });
    };
    // A logical tool ID can collide with another persisted receipt. Never
    // let the lifecycle loader substitute a different edge after authorization.
    if lifecycle.request_id() != edge.parent_request_id
        || lifecycle.request_doc_id() != Some(edge.parent_request_doc_id.as_str())
        || lifecycle.tool_call_id() != edge.parent_tool_call_id
        || lifecycle.child_request_id.as_deref() != Some(edge.child_request_id.as_str())
    {
        return Ok(CancelSubagentOutcome::Unavailable {
            diagnostic: "stored subagent lifecycle does not match the authorized edge".into(),
            retryable: false,
        });
    }
    let local_did = lifecycle.agent_did().to_owned();
    let was_running = lifecycle.is_running();
    let reason = reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or("subagent cancelled by parent request");

    // Preserve the native tool's drain/cancel/interrupt/cascade/drain order.
    // The second drain catches work admitted concurrently with interruption.
    let mut queued_drained = crate::interrupt::cancel_subagent_session_queue(
        &node,
        &edge.child_session_id,
        &edge.child_agent_did,
        edge.child_requester_did.as_deref(),
        reason,
    )
    .await?;
    cancel_bridge_lifecycle(
        &node,
        lifecycle,
        &local_did,
        "root",
        CancelCause::UserCancelled,
    )
    .await?;
    let active_interrupted = crate::interrupt::interrupt_active_session_request(
        &node,
        &edge.child_session_id,
        &edge.child_agent_did,
        edge.child_requester_did.as_deref(),
    )
    .await?;
    let descendants_cancelled = cancel_live_subagent_descendants(
        node.clone(),
        &edge.child_session_id,
        &edge.child_agent_did,
        edge.child_requester_did.as_deref(),
        &local_did,
        CancelCause::UserCancelled,
    )
    .await?;
    queued_drained += crate::interrupt::cancel_subagent_session_queue(
        &node,
        &edge.child_session_id,
        &edge.child_agent_did,
        edge.child_requester_did.as_deref(),
        reason,
    )
    .await?;
    let cancelled =
        was_running || active_interrupted || descendants_cancelled != 0 || queued_drained != 0;
    let receipt = SubagentCancellation {
        child_request_id: edge.child_request_id,
        child_session_id: edge.child_session_id,
        behavior_id: edge.behavior_id,
        parent_session_id: edge.parent_session_id,
        parent_tool_call_id: edge.parent_tool_call_id,
        active_interrupted,
        descendants_cancelled,
        queued_drained,
    };
    Ok(if cancelled {
        CancelSubagentOutcome::Cancelled(receipt)
    } else {
        CancelSubagentOutcome::AlreadyTerminal(receipt)
    })
}

pub(crate) async fn cancel_live_subagent_descendants(
    node: Arc<EmbeddedNode>,
    child_session_id: &str,
    child_agent_did: &str,
    child_requester_did: Option<&str>,
    local_did: &str,
    cause: CancelCause,
) -> Result<usize> {
    let ids = running_subagent_bridge_ids(
        &node,
        child_session_id,
        child_agent_did,
        child_requester_did,
    )
    .await?;
    let mut cancelled = 0;
    for id in ids {
        if let Some(lifecycle) = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &id,
            child_agent_did,
            child_session_id,
            child_requester_did,
        )
        .await?
        {
            if cancel_bridge_lifecycle(&node, lifecycle, local_did, "descendant", cause).await? {
                cancelled += 1;
            }
        }
    }
    Ok(cancelled)
}

async fn cancel_bridge_lifecycle(
    node: &EmbeddedNode,
    mut lifecycle: ToolCallLifecycle,
    local_did: &str,
    bridge_kind: &str,
    cause: CancelCause,
) -> Result<bool> {
    if !lifecycle.is_running() {
        return Ok(false);
    }
    let tool_call_id = lifecycle.tool_call_id().to_owned();
    let dispatch = lifecycle
        .cancel_during_run_with_cascade_dispatch(cause, local_did)
        .await?;
    if !lifecycle.is_cancelled() {
        return Ok(false);
    }
    let Some(dispatch) = dispatch else {
        return Ok(false);
    };
    if let CascadeDispatch::Local { intent, child } = dispatch {
        crate::interrupt::interrupt_request_by_doc_id(node, child.doc_id.as_deref().expect("verified physical cascade child"), child.agent_did.as_deref().expect("verified local child principal"), child.requester_did.as_deref()).await
            .with_context(|| format!("failed to cascade cancel_subagent {bridge_kind} bridge {tool_call_id} cancellation to child request {}", intent.child_request_id))?;
    }
    Ok(true)
}

#[derive(Deserialize)]
struct RunningSubagentBridgeRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    child_request_id: Option<String>,
}

async fn running_subagent_bridge_ids(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<String>> {
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
    let response = node.execute(&format!(r#"{{ AgentToolCall(
        filter: {{ {scope}, lifecycle_state: {{ _eq: "running" }}, cancel_policy: {{ _eq: "cascade" }} }},
        order: [{{ started_at: ASC }}, {{ tool_call_id: ASC }}]
    ) {{ _docID child_request_id }} }}"#)).await;
    ensure_no_errors(&response, "query running subagent bridges")?;
    anyhow::ensure!(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .is_some(),
        "running bridge query omitted rows"
    );
    let rows: Vec<RunningSubagentBridgeRow> = crate::graphql::rows(&response, "AgentToolCall")?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            row.child_request_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty())
        })
        .map(|row| row.doc_id)
        .collect())
}
