use serde::Serialize;

use super::bootstrap::DesktopBootstrapSummary;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct P2PHealthView {
    pub status: String,
    pub connected_peer_count: usize,
    pub replicator_count: usize,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub last_ok_at: Option<String>,
    pub last_failure_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeView {
    pub process_state: Option<String>,
    pub reconcile_phase: Option<String>,
    pub last_reconcile_result: Option<String>,
    pub last_reconcile_error: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentPrincipalView {
    pub agent_did: String,
    pub display_name: Option<String>,
    pub default_behavior_id: Option<String>,
    pub enabled: Option<bool>,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BehaviorView {
    pub behavior_id: String,
    pub display_name: String,
    pub system_prompt: Option<String>,
    pub backend_id: Option<String>,
    pub model_name: Option<String>,
    pub tool_selection_id: Option<String>,
    pub inference_profile_id: Option<String>,
    pub compaction_strategy: Option<String>,
    pub compaction_threshold: Option<f64>,
    pub enabled: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceBackendView {
    pub backend_id: String,
    pub name: Option<String>,
    pub provider_kind: Option<String>,
    pub endpoint: Option<String>,
    pub api_key_configured: bool,
    pub api_key_env_var: Option<String>,
    pub max_concurrent: Option<i64>,
    pub max_queue_depth: Option<i64>,
    pub enabled: Option<bool>,
    pub models: Vec<String>,
    pub probe_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceProfileView {
    pub profile_id: String,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub max_turns: Option<i64>,
    pub temperature: Option<f64>,
    pub stream_batch_ms: Option<i64>,
    pub deadline_duration_secs: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolSelectionView {
    pub selection_id: String,
    pub agent_did: Option<String>,
    pub display_name: Option<String>,
    pub enable_file_tools: Option<bool>,
    pub file_tools_mode: Option<String>,
    pub file_tool_root: Option<String>,
    pub enable_bash: Option<bool>,
    pub bash_mode: Option<String>,
    pub cli_tool_names: Vec<String>,
    pub enable_meta_tools: Option<bool>,
    pub delegate_to: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolServiceRegistryView {
    pub service_id: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub hostname: Option<String>,
    pub tailscale_ip: Option<String>,
    pub lan_ip: Option<String>,
    pub mcp_port: Option<i64>,
    pub mcp_path: Option<String>,
    pub status: Option<String>,
    pub version: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskView {
    pub task_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub behavior_id: Option<String>,
    pub prompt_template: Option<String>,
    pub enabled: Option<bool>,
    pub output_schema_ref: Option<String>,
    pub recent_runs: TaskRecentRunsView,
    pub run_history: Vec<TaskRunSummaryView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskRecentRunsView {
    pub total_fires: u64,
    pub last_attempt_at: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub schedule_count: usize,
    pub event_trigger_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskRunSummaryView {
    pub request_id: String,
    pub session_id: Option<String>,
    pub behavior_id: Option<String>,
    pub status: Option<String>,
    pub lifecycle_state: Option<String>,
    pub execution_origin: Option<String>,
    pub caused_by_trigger_id: Option<String>,
    pub caused_by_trigger_kind: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScheduleView {
    pub schedule_id: String,
    pub task_id: Option<String>,
    pub interval_secs: Option<i64>,
    pub enabled: Option<bool>,
    pub concurrency: Option<String>,
    pub next_run_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub fire_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventTriggerView {
    pub trigger_id: String,
    pub task_id: Option<String>,
    pub source_collection: Option<String>,
    pub event_kind: Option<String>,
    pub filter: Option<String>,
    pub enabled: Option<bool>,
    pub concurrency: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_fired_source_doc_id: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub fire_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationSummary {
    pub session_id: String,
    pub title: Option<String>,
    pub preview_text: Option<String>,
    pub status: Option<String>,
    pub behavior_id: Option<String>,
    pub latest_request_id: Option<String>,
    pub task_id: Option<String>,
    pub task_name: Option<String>,
    pub trigger_id: Option<String>,
    pub trigger_kind: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub turn_state: Option<String>,
    pub message_count: usize,
    pub tool_call_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeploymentView {
    pub peer_id: String,
    pub label: String,
    pub agent_did: String,
    pub addr: String,
    pub source: Option<String>,
    pub graphql: Option<String>,
    pub dial_succeeded: bool,
    pub last_error: Option<String>,
    pub default_behavior_id: Option<String>,
    pub agent_principal: AgentPrincipalView,
    pub runtime: Option<RuntimeView>,
    pub behaviors: Vec<BehaviorView>,
    pub inference_backends: Vec<InferenceBackendView>,
    pub inference_profiles: Vec<InferenceProfileView>,
    pub tool_selections: Vec<ToolSelectionView>,
    pub tool_service_registries: Vec<ToolServiceRegistryView>,
    pub tasks: Vec<TaskView>,
    pub schedules: Vec<ScheduleView>,
    pub event_triggers: Vec<EventTriggerView>,
    pub conversations: Vec<ConversationSummary>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRuntimeSnapshot {
    pub local_peer_id: String,
    pub listen_addresses: Vec<String>,
    pub p2p_health: P2PHealthView,
    pub bootstrap_errors: Vec<String>,
    pub last_mutation_error: Option<String>,
    pub focused_request_id: Option<String>,
    pub configured_peer_count: usize,
    pub dialed_peer_count: usize,
    pub peer_issue_count: usize,
    pub row_count: usize,
    pub approx_serialized_bytes: usize,
    pub deployments: Vec<DeploymentView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopClientSnapshot {
    pub bootstrap: DesktopBootstrapSummary,
    pub client: Option<DesktopRuntimeSnapshot>,
}
