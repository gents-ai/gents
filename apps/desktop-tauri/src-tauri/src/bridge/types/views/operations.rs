//! Operator-surfaces view types per
//! docs/superpowers/specs/2026-05-20-desktop-operator-surfaces-design.md
//! "Operations Snapshot Type" (line ~799). Stubs only — the panels in their
//! own PRs (#276/#277/#278/#281/#283/#284/#285/#286/#288) build and populate
//! these structs.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopOperationsSnapshot {
    pub fetched_at: String,
    pub agent_did: Option<String>,
    pub liveness: Option<RuntimeLivenessView>,
    pub liveness_unavailable_reason: Option<String>,
    pub backgrounded_tools: Vec<BackgroundedToolView>,
    pub stuck_diagnostics: Vec<StuckWorkDiagnosticView>,
    pub lineage: Option<SubagentTreeView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeLivenessView {
    pub expired_processing_count: i64,
    pub requests: Vec<ActiveRequestView>,
    pub active_tool_calls: Vec<ActiveToolCallView>,
    pub active_native_executors_available: bool,
    pub active_native_executors: Vec<NativeExecutorStatusView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActiveRequestView {
    pub request_id: String,
    pub claimed_at: Option<String>,
    pub deadline: Option<String>,
    pub deadline_expired: bool,
    pub deadline_age_ms: Option<i64>,
    pub last_progress_age_ms: i64,
    pub subagent_depth: i64,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_trigger_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActiveToolCallView {
    pub request_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub started_at: Option<String>,
    pub deadline_at: Option<String>,
    pub await_mode: Option<String>,
    pub running_age_ms: i64,
    pub deadline_expired: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeExecutorStatusView {
    pub id: i64,
    pub pid: u32,
    pub argv0: String,
    pub tool_name: Option<String>,
    pub started_at: String,
    pub age_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackgroundedToolView {
    pub request_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub lifecycle_state: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<String>,
    pub age_ms: Option<i64>,
    pub deadline_at: Option<String>,
    pub deadline_expired: bool,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub child_request_id: Option<String>,
    pub native_executor: Option<NativeExecutorStatusView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StuckWorkDiagnosticView {
    pub request_id: String,
    pub session_id: Option<String>,
    pub severity: String,
    pub reason: String,
    pub deadline_age_ms: Option<i64>,
    pub last_progress_age_ms: Option<i64>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub stuck_since: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentTreeView {
    pub root_request_id: String,
    pub nodes: Vec<SubagentNodeView>,
    pub edges: Vec<SubagentEdgeView>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentNodeView {
    pub request_id: String,
    pub session_id: Option<String>,
    pub agent_did: Option<String>,
    pub behavior_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub status: Option<String>,
    pub subagent_depth: Option<i64>,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SubagentEdgeView {
    pub parent_request_id: String,
    pub child_request_id: String,
    pub parent_tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub lifecycle_state: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CascadeCancelPreview {
    pub root_request_id: String,
    pub preview_signature: String,
    pub root_state: Option<String>,
    pub will_interrupt: Vec<CascadeAffectedRequest>,
    pub will_detach: Vec<CascadeAffectedRequest>,
    pub already_terminal: Vec<CascadeAffectedRequest>,
    pub unknown_policy: Vec<CascadeAffectedRequest>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CascadeAffectedRequest {
    pub request_id: String,
    pub session_id: Option<String>,
    pub behavior_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub parent_request_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
}

/// Result envelope for `desktop_interrupt_request`. Field semantics are
/// normative per the design spec line 922–942:
/// - `accepted = true` iff the bridge latched (or confirmed already-latched)
///   `interrupt_requested_at` for `request_id`.
/// - `already_interrupted = true` iff the field was non-null prior to the
///   call; `accepted` is still `true` in that case.
/// - `stale_preview = true` is mutually exclusive with `accepted = true`.
///   On signature mismatch the bridge returns `accepted: false`,
///   `stale_preview: true`, and a fresh `preview` for the UI to redraw.
/// - `interrupt_requested_at` is the canonical timestamp the bridge observed
///   on the document after the call. Null only on a non-already-interrupted
///   failure.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InterruptRequestResult {
    pub request_id: String,
    pub accepted: bool,
    pub interrupt_requested_at: Option<String>,
    pub already_interrupted: bool,
    pub stale_preview: bool,
    pub preview: Option<CascadeCancelPreview>,
}
