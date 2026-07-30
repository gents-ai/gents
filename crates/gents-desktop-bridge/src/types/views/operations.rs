use serde::{Deserialize, Serialize};
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
    pub lineage: Option<SubagentTreeView>,
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
    pub cancel_policy: Option<String>,
    pub child_request_id: Option<String>,
    pub stuck_since: Option<String>,
    pub cancel_pending_remote_ack: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTreeView {
    pub root_request_id: String,
    pub nodes: Vec<SubagentNodeView>,
    pub edges: Vec<SubagentEdgeView>,
    pub truncated: bool,
    #[serde(default)]
    pub partial_errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SubagentNodeView {
    pub request_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
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

#[derive(Debug, Clone, Serialize, TS)]
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

#[derive(Debug, Clone, Serialize, TS)]
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

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InterruptRequestResult {
    pub request_id: String,
    pub accepted: bool,
    pub interrupt_requested_at: Option<String>,
    pub already_interrupted: bool,
    pub stale_preview: bool,
    pub preview: Option<CascadeCancelPreview>,
}

#[derive(Debug, Clone, Serialize, TS)]
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

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ResolveHoldResult {
    pub approval_id: String,
    pub tool_call_id: String,
    pub decision: String,
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

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
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
