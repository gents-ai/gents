use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopOperationsSnapshot {
    pub fetched_at: String,
    pub node_did: Option<String>,
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
    pub request_hop: i64,
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
    pub starts_session: bool,
    pub job: bool,
    pub lifecycle_state: Option<String>,
    pub status: Option<String>,
    pub started_at: Option<String>,
    pub age_ms: Option<i64>,
    pub deadline_at: Option<String>,
    pub deadline_expired: bool,
    pub await_mode: Option<String>,
    pub stuck_since: Option<String>,
    pub native_executor: Option<NativeExecutorStatusView>,
    /// Shared wait-status projection (gents-protocol::wait_status): parsed
    /// lifecycle, elapsed truth, stable terminal duration. `None` fields mean
    /// unknown — never fabricated.
    pub wait_status: Option<WaitStatusProjectionView>,
}

/// ts-rs mirror of `gents_protocol::wait_status::WaitStatusProjection`.
/// Field semantics documented there; do not diverge.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct WaitStatusProjectionView {
    pub lifecycle: String,
    /// Verbatim persisted lifecycle_state token.
    pub lifecycle_state_raw: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub running_elapsed_seconds: Option<i64>,
    pub terminal_elapsed_seconds: Option<i64>,
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

/// Provenance only: no hierarchy, cascade or authority follows.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionProvenanceView {
    pub session_id: String,
    pub started_by: Option<LinkedSessionView>,
    pub started: Vec<LinkedSessionView>,
    pub sent: Vec<LinkedSessionView>,
    pub received: Vec<LinkedSessionView>,
    /// The request of this session that its starter's call opened it with
    /// (C7), while that request is visible.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub opening_request_id: Option<String>,
    pub senders: Vec<TurnSenderView>,
    pub calls: Vec<CausedCallView>,
    /// Exact-scope metadata read through the existing node observation access;
    /// related sessions need not be hydrated in the client's transcript cache.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub related_sessions: Option<Vec<super::SessionSummary>>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LinkedSessionView {
    pub node_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
    pub cause_request_doc_id: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TurnSenderView {
    pub request_id: String,
    pub sender: LinkedSessionView,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CausedCallView {
    pub request_id: String,
    pub tool_call_id: String,
    pub caused: CausedRequestView,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CausedRequestView {
    pub request_id: String,
    pub node_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
    pub lifecycle_state: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InterruptRequestResult {
    pub request_id: String,
    pub accepted: bool,
    pub interrupt_requested_at: Option<String>,
    pub already_interrupted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum BackendDisplayState {
    Available,
    Unhealthy,
    Stale,
    RateLimited,
    CircuitOpen,
    Unknown,
    Disabled,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BackendHealthView {
    pub backend_id: String,
    pub name: String,
    pub provider_kind: String,
    pub endpoint: String,
    pub enabled: bool,
    pub probe_status: String,
    pub display_state: BackendDisplayState,
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

/// Panel and table classify against `display_state` only, never `status`;
/// the projection lives on `ToolServiceHealthState::project`.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MCPServiceHealthView {
    pub service_id: String,
    pub node_did: Option<String>,
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
