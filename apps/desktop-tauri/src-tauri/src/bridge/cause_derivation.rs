// Pure-Rust classifier that maps a cancelled tool call or response onto one of
// four `CancelCause` variants with an evidence trail.
//
// Derivation precedence (operator-surfaces spec §470-491):
//  1. `deadline`   — tool lifecycle_state == "timedOut" (or timed_out flag).
//  2. `interrupted` — request has caused_by_parent_request_id AND tool's
//                     cancel_policy == "cascade" (parent cascade wins over
//                     user-cancel evidence on the child).
//  3. `userCancelled` — root request has interrupt_requested_at and no parent.
//  4. `unknown`    — cancelled terminal row without attributable evidence;
//                     evidence field enumerates what was checked and found empty.
//
// Inner doc-comments (//!) intentionally avoided here because this file is
// `include!`-ed into the bin's manually-assembled bridge module
// (apps/desktop-tauri/src-tauri/src/bin/bridge_runner.rs) and inner docs at
// the top of an include! body trip E0753 in that context.
// No IO. All inputs are plain Rust structs.

use crate::bridge::types::DerivedCancelCauseView;

// ---------------------------------------------------------------------------
// Evidence input structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct RequestEvidence {
    pub request_id: String,
    pub interrupt_requested_at: Option<String>,
    pub caused_by_parent_request_id: Option<String>,
    pub deadline_breached: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolCallEvidence {
    pub tool_call_id: String,
    pub lifecycle_state: Option<String>,
    pub deadline_at: Option<String>,
    pub cancel_policy: Option<String>,
    pub completed_at: Option<String>,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ResponseEvidence {
    pub interrupted_at: Option<String>,
    pub completed_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Returns `true` when the lifecycle_state string represents a cancelled
/// terminal — i.e., one that warrants cause derivation.
fn is_cancelled_terminal(state: &Option<String>) -> bool {
    matches!(
        state.as_deref(),
        Some("cancelled") | Some("interrupted") | Some("timedOut")
    )
}

// ---------------------------------------------------------------------------
// Public derivation functions
// ---------------------------------------------------------------------------

/// Classify why a tool call ended in a cancelled terminal state.
///
/// Returns `None` when the tool call is not in a cancelled terminal state
/// (e.g., it completed normally).
pub(crate) fn derive_tool_call_cause(
    req: &RequestEvidence,
    tool: &ToolCallEvidence,
) -> Option<DerivedCancelCauseView> {
    if !is_cancelled_terminal(&tool.lifecycle_state) {
        return None;
    }

    // ---- Precedence 1: deadline ----
    if tool.timed_out || tool.lifecycle_state.as_deref() == Some("timedOut") {
        return Some(DerivedCancelCauseView {
            cause: "deadline".into(),
            source: "toolLifecycle".into(),
            confidence: "derived".into(),
            at: tool.completed_at.clone(),
            evidence: vec![
                format!("AgentToolCall.lifecycle_state = \"timedOut\""),
                format!(
                    "deadline_at = {:?}",
                    tool.deadline_at.as_deref().unwrap_or("(unset)")
                ),
                format!(
                    "completed_at = {:?}",
                    tool.completed_at.as_deref().unwrap_or("(unset)")
                ),
            ],
        });
    }

    // ---- Precedence 2: interrupted (parent cascade wins over user-cancel on child) ----
    if req.caused_by_parent_request_id.is_some() && tool.cancel_policy.as_deref() == Some("cascade")
    {
        let parent = req.caused_by_parent_request_id.clone().unwrap_or_default();
        return Some(DerivedCancelCauseView {
            cause: "interrupted".into(),
            source: "parentCascade".into(),
            confidence: "derived".into(),
            at: tool.completed_at.clone(),
            evidence: vec![
                format!("AgentRequest.caused_by_parent_request_id = {parent}"),
                "AgentToolCall.cancel_policy = \"cascade\"".into(),
            ],
        });
    }

    // ---- Precedence 3: userCancelled ----
    if req.interrupt_requested_at.is_some() && req.caused_by_parent_request_id.is_none() {
        let at = req.interrupt_requested_at.clone();
        return Some(DerivedCancelCauseView {
            cause: "userCancelled".into(),
            source: "requestInterrupt".into(),
            confidence: "direct".into(),
            at: at.clone(),
            evidence: vec![
                format!(
                    "AgentRequest.interrupt_requested_at = {}",
                    at.as_deref().unwrap_or("(unset)"),
                ),
                "no parent cascade (caused_by_parent_request_id is null)".into(),
            ],
        });
    }

    // ---- Precedence 4: unknown — enumerate all checks ----
    Some(DerivedCancelCauseView {
        cause: "unknown".into(),
        source: "unresolved".into(),
        confidence: "derived".into(),
        at: tool.completed_at.clone(),
        evidence: vec![
            "checked: no parent cascade (caused_by_parent_request_id is null)".into(),
            "checked: no deadline (lifecycle_state is not timedOut)".into(),
            "checked: no interrupt_requested_at on root".into(),
            "schema has no persisted AgentToolCall.cancel_cause".into(),
        ],
    })
}

/// Classify why a streaming response was interrupted.
///
/// Returns `Some` only when `resp.interrupted_at` is present — meaning the
/// response stream was cut short. Returns `None` for completed responses.
pub(crate) fn derive_response_cause(
    _req: &RequestEvidence,
    resp: &ResponseEvidence,
) -> Option<DerivedCancelCauseView> {
    if let Some(at) = &resp.interrupted_at {
        return Some(DerivedCancelCauseView {
            cause: "interrupted".into(),
            source: "responseInterruptedAt".into(),
            confidence: "direct".into(),
            at: Some(at.clone()),
            evidence: vec![format!("AgentResponse.interrupted_at = {at}")],
        });
    }
    None
}
