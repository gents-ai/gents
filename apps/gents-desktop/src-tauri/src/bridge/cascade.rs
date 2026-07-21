// Descendant tree walk for cascade preview and cascade interrupt.
//
// Mirrors `interrupt_request_local` in
// `crates/gents-cli/src/commands/subagent.rs:327`, but stays in the
// bridge so both `desktop_preview_interrupt_cascade` and
// `desktop_interrupt_request` can share the walk.

use std::collections::BTreeSet;
use std::sync::Arc;

use gents_desktop_core::client::ClientCore;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use serde_json::Value;

use crate::bridge::snapshot::{
    compute_preview_signature, PreviewSignatureInput, PreviewSignatureRow,
};
use crate::bridge::types::{
    CascadeAffectedRequest, CascadeCancelPreview, DesktopInterruptRequest,
    DesktopPreviewInterruptCascadeRequest, InterruptRequestResult,
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
///
/// When `req.agent_did` is `Some(did)`, the root AgentRequest query is scoped
/// to that operator's documents. Descendants are then authorized by persisted
/// AgentToolCall.child_request_id edges, because linked subagents may be owned
/// by a different deployment DID than the root request.
pub(crate) async fn walk(
    core: &Arc<ClientCore>,
    req: &CascadeWalkRequest,
) -> Result<CascadeWalkResult, String> {
    let node = core.node();
    let agent_did = req.agent_did.as_deref();

    // Load root request.
    let root = fetch_request(node, &req.root_request_id, agent_did)
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
        return Err(format!("cascade depth exceeded at {parent_request_id}"));
    }

    // Query all AgentToolCall rows where request_id == parent AND child_request_id is set.
    // AgentToolCall does not carry agent_did. Operator scoping is enforced on
    // the root request before the walk starts; child reachability is the bridge
    // edge itself, which supports cross-deployment subagents.
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

        // Fetch child request to determine lifecycle state. Do not apply the
        // root agent_did filter here: a valid subagent edge can point at a child
        // request owned by a different deployment DID.
        let child_row = match fetch_request(node, &child_id, None).await {
            Ok(row) => row,
            Err(e) => {
                return Err(format!(
                    "cascade::walk: child request {child_id} not found: {e}"
                ));
            }
        };

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
///
/// When `agent_did` is `Some(did)`, an additional `agent_did` filter is applied
/// so only rows owned by that operator are visible.
async fn fetch_request(
    node: &EmbeddedNode,
    request_id: &str,
    agent_did: Option<&str>,
) -> Result<Value, String> {
    let escaped = escape_graphql_string(request_id);
    let agent_did_clause = agent_did
        .map(|did| {
            let escaped_did = escape_graphql_string(did);
            format!(r#", agent_did: {{ _eq: "{escaped_did}" }}"#)
        })
        .unwrap_or_default();
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }}{agent_did_clause} }},
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

/// Result returned by `latch_root_interrupt`.
#[derive(Debug, Clone)]
pub(crate) struct LatchResult {
    /// The RFC-3339 timestamp stored (or already present) in
    /// `interrupt_requested_at`.
    pub interrupt_requested_at: String,
    /// `true` if this call was the first to write the field; `false` if it
    /// was already set (idempotent no-op path).
    pub was_first: bool,
}

/// Latches `interrupt_requested_at` on the root `AgentRequest` identified by
/// `request_id`.
///
/// - If the field is already present, returns `LatchResult { was_first: false,
///   interrupt_requested_at: <existing> }` without issuing a mutation.
/// - Otherwise writes `chrono::Utc::now().to_rfc3339()` and returns
///   `LatchResult { was_first: true, interrupt_requested_at: <now> }`.
///
/// Mirrors `interrupt_request_graphql` in
/// `crates/gents-cli/src/commands/subagent.rs:167-200`.
pub(crate) async fn latch_root_interrupt(
    core: &Arc<ClientCore>,
    request_id: &str,
) -> Result<LatchResult, String> {
    let node = core.node();

    // 1. Read the current row. No agent_did filter: latch operates on a specific
    //    request_id and doesn't need operator scoping here.
    let row = fetch_request(node, request_id, None)
        .await
        .map_err(|e| format!("latch_root_interrupt: {e}"))?;

    // 2. If already interrupted, return idempotent result.
    if let Some(existing) = string_field(&row, "interrupt_requested_at") {
        return Ok(LatchResult {
            interrupt_requested_at: existing,
            was_first: false,
        });
    }

    // 3. Compute timestamp and write.
    let now = chrono::Utc::now().to_rfc3339();
    let escaped_id = escape_graphql_string(request_id);
    let escaped_now = escape_graphql_string(&now);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped_now}" }}
            ) {{ _docID }}
        }}"#
    );

    let response = node.execute(&mutation).await;
    if response.has_errors() {
        return Err(format!(
            "latch_root_interrupt: update_AgentRequest failed: {:?}",
            response.errors
        ));
    }

    Ok(LatchResult {
        interrupt_requested_at: now,
        was_first: true,
    })
}

/// Orchestrates a non-cascade or cascade interrupt request from the operator.
///
/// Only `"userCancelled"` is an operator-authentic cause. Any other value is
/// rejected — the runtime owns deadline/interrupted derivation.
///
/// For the non-cascade path (`req.cascade == false`): latches
/// `interrupt_requested_at` on the root request and returns an
/// `InterruptRequestResult` reflecting whether this call was the first to set
/// the field (`accepted`) or the field was already set (`already_interrupted`).
///
pub(crate) async fn interrupt_request(
    core: &Arc<ClientCore>,
    req: &DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    // Only "userCancelled" is operator-authentic. Other causes must be rejected
    // — the runtime owns deadline/interrupted derivation.
    if req.cause != "userCancelled" {
        return Err(format!(
            "operator may only authentically produce cause=\"userCancelled\", got {:?}",
            req.cause
        ));
    }

    if !req.cascade {
        let latched = latch_root_interrupt(core, &req.request_id).await?;
        return Ok(InterruptRequestResult {
            request_id: req.request_id.clone(),
            accepted: true, // idempotent success — always accepted when latched or already latched
            interrupt_requested_at: Some(latched.interrupt_requested_at),
            already_interrupted: !latched.was_first,
            stale_preview: false,
            preview: None,
        });
    }

    // Cascade path:
    let expected_sig = req
        .expected_preview_signature
        .clone()
        .ok_or_else(|| "cascade=true requires expectedPreviewSignature".to_string())?;
    let preview = build_cascade_preview(
        core,
        &DesktopPreviewInterruptCascadeRequest {
            request_id: req.request_id.clone(),
            agent_did: None,
            include_terminal: Some(true),
        },
    )
    .await?;
    if preview.preview_signature != expected_sig {
        return Ok(InterruptRequestResult {
            request_id: req.request_id.clone(),
            accepted: false,
            interrupt_requested_at: None,
            already_interrupted: false,
            stale_preview: true,
            preview: Some(preview),
        });
    }

    // Signature matches — latch the root and every descendant that the
    // preview classified as cascade-interruptible. This is the Rust bridge
    // counterpart to Lean's bridge_cancel_cascade step: set
    // interrupt_requested_at on the child so the child daemon can lift
    // interrupt_processing to the interrupted terminal state.
    let latched = latch_root_interrupt(core, &req.request_id).await?;
    latch_cascade_descendants(core, &preview).await?;
    Ok(InterruptRequestResult {
        request_id: req.request_id.clone(),
        accepted: true, // idempotent success — always accepted when latched or already latched
        interrupt_requested_at: Some(latched.interrupt_requested_at),
        already_interrupted: !latched.was_first,
        stale_preview: false,
        preview: None,
    })
}

async fn latch_cascade_descendants(
    core: &Arc<ClientCore>,
    preview: &CascadeCancelPreview,
) -> Result<(), String> {
    for child in &preview.will_interrupt {
        gents::interrupt_request(core.node(), &child.request_id)
            .await
            .map_err(|error| {
                format!(
                    "cascade interrupt failed to latch child request {}: {error}",
                    child.request_id
                )
            })?;
        tracing::info!(
            root_request_id = %preview.root_request_id,
            child_request_id = %child.request_id,
            "cascade interrupt latched descendant request"
        );
    }
    Ok(())
}

/// Extract a non-empty string field from a JSON object.
fn string_field(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}
