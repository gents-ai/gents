use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopOperationsSnapshot {
    pub fetched_at: String,
    pub agent_did: Option<String>,
    pub liveness: Option<RuntimeLivenessView>,
    pub liveness_unavailable_reason: Option<String>,
    pub backgrounded_tools: Vec<BackgroundedToolView>,
    pub stuck_diagnostics: Vec<StuckWorkDiagnosticView>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeLivenessView {
    pub expired_processing_count: i64,
    pub requests: Vec<ActiveRequestView>,
    pub active_tool_calls: Vec<ActiveToolCallView>,
    pub active_native_executors_available: bool,
    pub active_native_executors: Vec<NativeExecutorStatusView>,
}

#[derive(Debug, Clone, Serialize, TS)]
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

#[derive(Debug, Clone, Serialize, TS)]
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

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NativeExecutorStatusView {
    pub id: i64,
    pub pid: u32,
    pub argv0: String,
    pub tool_name: Option<String>,
    pub started_at: String,
    pub age_ms: i64,
}

#[derive(Debug, Clone, Serialize, TS)]
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
    pub stuck_since: Option<String>,
    pub native_executor: Option<NativeExecutorStatusView>,
}

#[derive(Debug, Clone, Serialize, TS)]
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

/// Session-message provenance for one session, read from the immutable
/// `AgentRequest.caused_by_parent_*` lineage. It is provenance only: no
/// hierarchy, cascade or authority follows from it.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionProvenanceView {
    pub session_id: String,
    /// Requests in this session that another session's tool call caused.
    pub received: Vec<CausedRequestView>,
    /// Requests in other sessions that this session's tool calls caused.
    pub sent: Vec<CausedRequestView>,
    /// A bound was reached; older links may be missing.
    pub truncated: bool,
}

/// One caused request and the call that caused it. `caused_by_session_id` is
/// null when the causing request is not visible on this node.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CausedRequestView {
    pub request_id: String,
    pub request_doc_id: String,
    pub session_id: Option<String>,
    pub agent_did: Option<String>,
    pub behavior_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub interrupt_requested_at: Option<String>,
    pub created_at: Option<String>,
    /// `subagent_depth`: the causal hop.
    pub hop: Option<i64>,
    pub caused_by_request_id: Option<String>,
    pub caused_by_request_doc_id: Option<String>,
    pub caused_by_tool_call_id: Option<String>,
    pub caused_by_session_id: Option<String>,
}

/// Result envelope for `desktop_interrupt_request`:
/// - `accepted = true` iff the bridge latched (or confirmed already-latched)
///   `interrupt_requested_at` for `request_id`.
/// - `already_interrupted = true` iff the field was non-null prior to the
///   call; `accepted` is still `true` in that case.
/// - `interrupt_requested_at` is the canonical timestamp the bridge observed
///   on the document after the call.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InterruptRequestResult {
    pub request_id: String,
    pub accepted: bool,
    pub interrupt_requested_at: Option<String>,
    pub already_interrupted: bool,
}

/// One backend's persisted health + recent admission outcomes. Read-only
/// projection of `InferenceBackend` joined with the last N `InferenceCall`
/// rows for that backend. `display_state` is derived from
/// `(enabled, probe_status)` per the prototype's mapping (matches
/// `InferenceBackend::is_available` and the Lean `backendAvailable`
/// witness in `BoundaryRuntime.lean`).
#[derive(Debug, Clone, Serialize, TS)]
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

#[derive(Debug, Clone, Serialize, TS)]
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
/// `status` is the internal `ToolServiceHealthState` vocabulary
/// (`healthy` / `degraded` / `evicted` / `reconnecting`) so the operator UI
/// can distinguish back-off from in-flight retry; `display_state` is the
/// collapsed three-state projection (`healthy` / `stale` / `unreachable`),
/// projection owned by `ToolServiceHealthState::project` — the panel and
/// table classify against `display_state` only, never `status`.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MCPServiceHealthView {
    pub service_id: String,
    pub agent_did: Option<String>,
    pub endpoint: Option<String>,
    pub status: Option<String>,
    pub display_state: String,
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
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpServiceProbeResult {
    pub service_id: String,
    pub status: String,
    pub latency_ms: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DerivedCancelCauseView {
    pub cause: String,
    pub source: String,
    pub confidence: String,
    pub at: Option<String>,
    pub evidence: Vec<String>,
}
