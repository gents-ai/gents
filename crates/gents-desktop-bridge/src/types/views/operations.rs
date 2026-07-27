//! Operator-surfaces view types per
//! docs/superpowers/specs/2026-05-20-desktop-operator-surfaces-design.md (git history)
//! "Operations Snapshot Type" (line ~799). Stubs only — the panels in their
//! own PRs (#276/#277/#278/#281/#283/#284/#285/#286/#288) build and populate
//! these structs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopOperationsSnapshot {
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
pub struct RuntimeLivenessView {
    pub expired_processing_count: i64,
    pub requests: Vec<ActiveRequestView>,
    pub active_tool_calls: Vec<ActiveToolCallView>,
    pub active_native_executors_available: bool,
    pub active_native_executors: Vec<NativeExecutorStatusView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRequestView {
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
pub struct ActiveToolCallView {
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
pub struct NativeExecutorStatusView {
    pub id: i64,
    pub pid: u32,
    pub argv0: String,
    pub tool_name: Option<String>,
    pub started_at: String,
    pub age_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundedToolView {
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
    pub stuck_since: Option<String>,
    pub cancel_pending_remote_ack: bool,
    pub native_executor: Option<NativeExecutorStatusView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StuckWorkDiagnosticView {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTreeView {
    pub root_request_id: String,
    pub nodes: Vec<SubagentNodeView>,
    pub edges: Vec<SubagentEdgeView>,
    pub truncated: bool,
    /// Deployments that could not be queried this walk; the tree may be
    /// missing their branches. Empty when every access answered.
    #[serde(default)]
    pub partial_errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentNodeView {
    pub request_id: String,
    /// Peer label the row was resolved from; None = the local node.
    #[serde(default)]
    pub resolved_via: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub behavior_id: Option<String>,
    #[serde(default)]
    pub lifecycle_state: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub subagent_depth: Option<i64>,
    #[serde(default)]
    pub caused_by_parent_request_id: Option<String>,
    #[serde(default)]
    pub caused_by_parent_tool_call_id: Option<String>,
    #[serde(default)]
    pub backend_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentEdgeView {
    pub parent_request_id: String,
    pub child_request_id: String,
    #[serde(default)]
    pub parent_tool_call_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub await_mode: Option<String>,
    #[serde(default)]
    pub cancel_policy: Option<String>,
    #[serde(default)]
    pub lifecycle_state: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CascadeCancelPreview {
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
pub struct CascadeAffectedRequest {
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
pub struct InterruptRequestResult {
    pub request_id: String,
    pub accepted: bool,
    pub interrupt_requested_at: Option<String>,
    pub already_interrupted: bool,
    pub stale_preview: bool,
    pub preview: Option<CascadeCancelPreview>,
}

/// One tool call held in `awaitingApproval`, as shown in the Holds strip.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldToolCallView {
    pub tool_call_id: String,
    pub request_id: Option<String>,
    pub session_id: Option<String>,
    pub agent_did: Option<String>,
    pub tool_name: Option<String>,
    pub args: Option<String>,
    pub deadline_at: Option<String>,
}

/// Result of writing an AgentToolApproval decision for a held call.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveHoldResult {
    pub approval_id: String,
    pub tool_call_id: String,
    pub decision: String,
}

/// One backend's persisted health + recent admission outcomes. Read-only
/// projection of `InferenceBackend` joined with the last N `InferenceCall`
/// rows for that backend. `display_state` is derived from
/// `(enabled, probe_status)` per the prototype's mapping (matches
/// `InferenceBackend::is_available` and the Lean `backendAvailable`
/// witness in `BoundaryRuntime.lean`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendHealthView {
    pub backend_id: String,
    pub name: String,
    pub provider_kind: String,
    pub endpoint: String,
    pub enabled: bool,
    pub probe_status: String,
    pub display_state: String,
    pub last_probe: Option<String>,
    pub max_concurrent: i64,
    pub max_queue_depth: i64,
    pub models: Vec<String>,
    pub recent_calls: Vec<InferenceCallSummaryView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InferenceCallSummaryView {
    pub call_id: String,
    pub call_seq: i64,
    pub call_kind: String,
    pub call_state: String,
    pub failure_reason: Option<String>,
    pub queued_at: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub queue_depth_at_enqueue: Option<i64>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
}

/// One row in the MCP health status panel (panel-278).
///
/// Mirrors the persisted `ToolServiceHealthState` collection — the agent's
/// `health_checker` upserts these every cycle (default 30 s) so the
/// desktop sees the K-model state evolve over time without needing an
/// in-process agent runtime.
///
/// `status` is the internal `HealthStateInternal` projection
/// (`healthy` / `stale` / `evicted` / `reconnecting`) so the operator UI
/// can distinguish back-off from in-flight retry; the public three-state
/// `HealthStatus` collapses `evicted` and `reconnecting` to `unreachable`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MCPServiceHealthView {
    pub service_id: String,
    pub agent_did: Option<String>,
    pub endpoint: Option<String>,
    pub status: Option<String>,
    pub tool_count: Option<i64>,
    pub failure_count: Option<i64>,
    pub k_max: Option<i64>,
    pub backoff_until: Option<String>,
    pub last_probe_at: Option<String>,
    pub last_seen: Option<String>,
    pub last_error_class: Option<String>,
    pub last_error_message: Option<String>,
    pub updated_at: Option<String>,
}

/// Result envelope for `desktop_probe_mcp_service`. The probe runs a
/// one-shot `run_health_check_cycle` against the named service against
/// a fresh `McpPool` (mirrors `gents mcp probe`) — `failure_count`
/// always reports `0` here because the cycle starts from an initial
/// `ServiceModel`. For accumulated K-state, the panel reads the persisted
/// `ToolServiceHealthState` row via `desktop_list_mcp_services_with_health`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServiceProbeResult {
    pub service_id: String,
    pub status: String,
    pub latency_ms: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DerivedCancelCauseView {
    pub cause: String,      // "userCancelled" | "interrupted" | "deadline" | "unknown"
    pub source: String, // "requestInterrupt" | "parentCascade" | "deadline" | "toolLifecycle" | "responseInterruptedAt" | "unresolved"
    pub confidence: String, // "direct" | "derived"
    pub at: Option<String>,
    pub evidence: Vec<String>,
}
