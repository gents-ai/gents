//! Descendant tree walk for cascade preview and cascade interrupt.
//!
//! Mirrors `interrupt_request_local` in
//! `crates/defra-agent-cli/src/commands/subagent.rs:327`, but stays in the
//! bridge so both `desktop_preview_interrupt_cascade` and
//! `desktop_interrupt_request` can share the walk.

use std::collections::BTreeSet;
use std::sync::Arc;

use defra_agent::defra_node::EmbeddedNode;
use defra_agent::graphql::escape_graphql_string;
use defra_agent_desktop_core::client::ClientCore;
use serde_json::Value;

use crate::bridge::snapshot::{
    compute_preview_signature, PreviewSignatureInput, PreviewSignatureRow,
};
use crate::bridge::types::{
    CascadeAffectedRequest, CascadeCancelPreview, DesktopPreviewInterruptCascadeRequest,
};

/// Maximum descent depth to match the CLI walker's safety limit.
const MAX_CASCADE_DEPTH: usize = 8;

/// Terminal lifecycle states — requests in these states are classified as
/// `AlreadyTerminal` regardless of their `cancel_policy`.
const TERMINAL_STATES: &[&str] = &[
    "completed",
    "failed",
    "cancelled",
    "superseded",
    "dead",
    "interrupted",
];

#[derive(Debug, Clone)]
pub(crate) struct CascadeWalkRequest {
    pub root_request_id: String,
    pub agent_did: Option<String>,
    pub include_terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CascadeClassification {
    WillInterrupt,
    WillDetach,
    AlreadyTerminal,
    UnknownPolicy,
}

#[derive(Debug, Clone)]
pub(crate) struct CascadeWalkRow {
    pub request_id: String,
    pub session_id: Option<String>,
    pub behavior_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub parent_request_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub classification: CascadeClassification,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CascadeWalkResult {
    pub root_state: Option<String>,
    pub root_interrupt_requested_at: Option<String>,
    pub rows: Vec<CascadeWalkRow>,
}

/// Walks `AgentToolCall.child_request_id` edges from `root_request_id` down,
/// classifying each descendant by the nearest bridge row's `cancel_policy`.
/// Filters terminal rows when `include_terminal == false`, except as
/// AlreadyTerminal evidence.
pub(crate) async fn walk(
    core: &Arc<ClientCore>,
    req: &CascadeWalkRequest,
) -> Result<CascadeWalkResult, String> {
    let node = core.node();

    // Load root request.
    let root = fetch_request(node, &req.root_request_id)
        .await
        .map_err(|e| format!("cascade::walk: root request not found: {e}"))?;

    let root_lifecycle_state = string_field(&root, "lifecycle_state");
    let root_interrupt_requested_at = string_field(&root, "interrupt_requested_at");

    let mut result = CascadeWalkResult {
        root_state: root_lifecycle_state,
        root_interrupt_requested_at,
        rows: Vec::new(),
    };

    let mut seen_requests: BTreeSet<String> = BTreeSet::new();
    seen_requests.insert(req.root_request_id.clone());

    bfs(
        node,
        &req.root_request_id,
        req.include_terminal,
        0,
        &mut seen_requests,
        &mut result.rows,
    )
    .await?;

    Ok(result)
}

/// BFS descent over AgentToolCall edges. `parent_request_id` is the node whose
/// children we're expanding at this call level. `depth` starts at 0.
async fn bfs(
    node: &EmbeddedNode,
    parent_request_id: &str,
    include_terminal: bool,
    depth: usize,
    seen_requests: &mut BTreeSet<String>,
    rows: &mut Vec<CascadeWalkRow>,
) -> Result<(), String> {
    if depth >= MAX_CASCADE_DEPTH {
        return Err(format!(
            "cascade depth exceeded at {parent_request_id}"
        ));
    }

    // Query all AgentToolCall rows where request_id == parent AND child_request_id is set.
    let escaped_parent = escape_graphql_string(parent_request_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    request_id: {{ _eq: "{escaped_parent}" }},
                    child_request_id: {{ _ne: "" }}
                }},
                order: [{{ message_sequence: ASC }}, {{ tool_call_id: ASC }}]
            ) {{
                tool_call_id
                tool_name
                await_mode
                cancel_policy
                child_request_id
            }}
        }}"#
    );

    let response = node.execute(&query).await;
    if response.has_errors() {
        return Err(format!(
            "cascade::walk: AgentToolCall query for {parent_request_id} failed: {:?}",
            response.errors
        ));
    }

    let data = response.data.unwrap_or(Value::Null);
    let tool_calls = data
        .get("AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for tc in &tool_calls {
        let child_id = match string_field(tc, "child_request_id") {
            Some(id) if !id.is_empty() => id,
            _ => continue,
        };

        // Cycle guard.
        if !seen_requests.insert(child_id.clone()) {
            continue;
        }

        let tool_call_id = string_field(tc, "tool_call_id");
        let tool_name = string_field(tc, "tool_name");
        let await_mode = string_field(tc, "await_mode");
        let cancel_policy = string_field(tc, "cancel_policy");

        // Fetch child request to determine lifecycle state.
        let child_row = fetch_request(node, &child_id).await.map_err(|e| {
            format!("cascade::walk: child request {child_id} not found: {e}")
        })?;

        let child_lifecycle_state = string_field(&child_row, "lifecycle_state");
        let child_session_id = string_field(&child_row, "session_id");
        let child_behavior_id = string_field(&child_row, "behavior_id");

        let is_terminal = child_lifecycle_state
            .as_deref()
            .map(|s| TERMINAL_STATES.contains(&s))
            .unwrap_or(false);

        let classification = if is_terminal {
            CascadeClassification::AlreadyTerminal
        } else {
            match cancel_policy.as_deref() {
                Some("cascade") => CascadeClassification::WillInterrupt,
                Some("detach") => CascadeClassification::WillDetach,
                _ => CascadeClassification::UnknownPolicy,
            }
        };

        rows.push(CascadeWalkRow {
            request_id: child_id.clone(),
            session_id: child_session_id,
            behavior_id: child_behavior_id,
            lifecycle_state: child_lifecycle_state,
            parent_request_id: Some(parent_request_id.to_string()),
            parent_tool_call_id: tool_call_id,
            tool_name,
            await_mode,
            cancel_policy,
            classification,
        });

        // Recurse only for cascade non-terminal children (and terminals when
        // include_terminal is set — the flag controls further descent into terminal
        // subtrees, not whether the terminal row itself is emitted).
        let should_recurse = match classification {
            CascadeClassification::WillInterrupt => true,
            CascadeClassification::AlreadyTerminal => include_terminal,
            CascadeClassification::WillDetach | CascadeClassification::UnknownPolicy => false,
        };

        if should_recurse {
            Box::pin(bfs(
                node,
                &child_id,
                include_terminal,
                depth + 1,
                seen_requests,
                rows,
            ))
            .await?;
        }
    }

    Ok(())
}

/// Fetch a single AgentRequest row by `request_id`. Returns Err if not found.
async fn fetch_request(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Value, String> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                lifecycle_state
                interrupt_requested_at
            }}
        }}"#
    );

    let response = node.execute(&query).await;
    if response.has_errors() {
        return Err(format!(
            "AgentRequest query for {request_id} failed: {:?}",
            response.errors
        ));
    }

    let data = response.data.unwrap_or(Value::Null);
    data.get("AgentRequest")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or_else(|| format!("request {request_id} not found in AgentRequest collection"))
}

/// Builds a `CascadeCancelPreview` by walking the descendant tree of
/// `req.request_id` and grouping rows into the four classification buckets,
/// then computing a BLAKE3 preview signature over the result.
///
/// This is the bridge-level helper called by both `desktop_preview_interrupt_cascade`
/// and any tests that need the full preview pipeline.
pub(crate) async fn build_cascade_preview(
    core: &Arc<ClientCore>,
    req: &DesktopPreviewInterruptCascadeRequest,
) -> Result<CascadeCancelPreview, String> {
    let walk_req = CascadeWalkRequest {
        root_request_id: req.request_id.clone(),
        agent_did: req.agent_did.clone(),
        include_terminal: req.include_terminal.unwrap_or(true),
    };
    let result = walk(core, &walk_req).await?;

    let mut will_interrupt = Vec::new();
    let mut will_detach = Vec::new();
    let mut already_terminal = Vec::new();
    let mut unknown_policy = Vec::new();
    let mut sig_rows = Vec::new();

    for row in &result.rows {
        let view = CascadeAffectedRequest {
            request_id: row.request_id.clone(),
            session_id: row.session_id.clone(),
            behavior_id: row.behavior_id.clone(),
            lifecycle_state: row.lifecycle_state.clone(),
            parent_request_id: row.parent_request_id.clone(),
            parent_tool_call_id: row.parent_tool_call_id.clone(),
            tool_name: row.tool_name.clone(),
            await_mode: row.await_mode.clone(),
            cancel_policy: row.cancel_policy.clone(),
        };
        sig_rows.push(PreviewSignatureRow {
            request_id: row.request_id.clone(),
            lifecycle_state: row.lifecycle_state.clone(),
            await_mode: row.await_mode.clone(),
            cancel_policy: row.cancel_policy.clone(),
            parent_tool_call_id: row.parent_tool_call_id.clone(),
        });
        match row.classification {
            CascadeClassification::WillInterrupt => will_interrupt.push(view),
            CascadeClassification::WillDetach => will_detach.push(view),
            CascadeClassification::AlreadyTerminal => already_terminal.push(view),
            CascadeClassification::UnknownPolicy => unknown_policy.push(view),
        }
    }

    let preview_signature = compute_preview_signature(&PreviewSignatureInput {
        root_request_id: req.request_id.clone(),
        root_state: result.root_state.clone(),
        root_interrupt_requested_at: result.root_interrupt_requested_at.clone(),
        affected: sig_rows,
    });

    Ok(CascadeCancelPreview {
        root_request_id: req.request_id.clone(),
        preview_signature,
        root_state: result.root_state,
        will_interrupt,
        will_detach,
        already_terminal,
        unknown_policy,
    })
}

/// Extract a non-empty string field from a JSON object.
fn string_field(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}
