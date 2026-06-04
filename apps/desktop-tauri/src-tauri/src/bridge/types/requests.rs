use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopInitRequest {
    pub agent_home: Option<PathBuf>,
    pub desktop_home: Option<PathBuf>,
    pub label: Option<String>,
    pub dangerously_overwrite: bool,
    pub reset: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PeerAddRequest {
    pub label: String,
    pub agent_did: String,
    pub addr: String,
    #[serde(default)]
    pub graphql: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PeerStatusFetchRequest {
    pub server_address: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatSendRequest {
    pub agent_did: String,
    pub behavior_id: Option<String>,
    pub session_id: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationRenameRequest {
    pub session_id: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgentConfigSaveRequest {
    pub agent_did: String,
    pub display_name: String,
    pub default_behavior_id: String,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BehaviorSaveRequest {
    pub agent_did: String,
    pub behavior_id: String,
    pub display_name: String,
    pub system_prompt: String,
    pub backend_id: Option<String>,
    pub tool_selection_id: Option<String>,
    pub inference_profile_id: Option<String>,
    pub compaction_strategy: Option<String>,
    pub compaction_threshold: Option<f64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackendSaveRequest {
    pub backend_id: String,
    pub name: String,
    pub provider_kind: String,
    pub endpoint: String,
    pub api_key: Option<String>,
    pub api_key_env_var: Option<String>,
    pub clear_api_key: Option<bool>,
    pub models: Vec<String>,
    pub max_concurrent: Option<i64>,
    pub max_queue_depth: Option<i64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InferenceProfileSaveRequest {
    pub profile_id: String,
    pub display_name: String,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub max_turns: Option<i64>,
    pub temperature: Option<f64>,
    pub stream_batch_ms: Option<i64>,
    pub deadline_duration_secs: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolSelectionSaveRequest {
    pub agent_did: String,
    pub selection_id: String,
    pub display_name: String,
    pub enable_file_tools: Option<bool>,
    pub file_tools_mode: Option<String>,
    pub file_tool_root: Option<String>,
    pub enable_bash: Option<bool>,
    pub bash_mode: Option<String>,
    #[serde(default)]
    pub command_execution_policy: Option<String>,
    #[serde(default)]
    pub command_allowed_argv_prefixes: Vec<String>,
    #[serde(default)]
    pub command_forbidden_argv_prefixes: Vec<String>,
    #[serde(default)]
    pub command_network_mode: Option<String>,
    pub cli_tool_names: Vec<String>,
    pub enable_meta_tools: Option<bool>,
    #[serde(default)]
    pub allowed_mcp_service_ids: Vec<String>,
    pub delegate_to: Vec<String>,
    #[serde(default)]
    pub backgroundable_tool_names: Vec<String>,
    #[serde(default)]
    pub subagent_targets: Vec<String>,
    pub subagent_spawn_enabled: Option<bool>,
    pub subagent_steering_enabled: Option<bool>,
    pub subagent_background_enabled: Option<bool>,
    pub subagent_allow_cross_deployment: Option<bool>,
    pub cross_deployment_spawn_timeout_seconds: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolServiceSaveRequest {
    pub service_id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub hostname: Option<String>,
    pub tailscale_ip: Option<String>,
    pub lan_ip: Option<String>,
    pub mcp_port: Option<i64>,
    pub mcp_path: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolServiceTestRequest {
    pub service_id: String,
    pub hostname: Option<String>,
    pub tailscale_ip: Option<String>,
    pub lan_ip: Option<String>,
    pub mcp_port: Option<i64>,
    pub mcp_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskSaveRequest {
    pub task_id: String,
    pub name: String,
    pub description: Option<String>,
    pub behavior_id: String,
    pub prompt_template: String,
    pub enabled: Option<bool>,
    pub output_schema_ref: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskRunRequest {
    pub task_id: String,
    pub args: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScheduleSaveRequest {
    pub schedule_id: String,
    pub task_id: String,
    pub interval_secs: Option<i64>,
    pub enabled: Option<bool>,
    pub concurrency: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScheduleRunRequest {
    pub schedule_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EventTriggerSaveRequest {
    pub trigger_id: String,
    pub task_id: String,
    pub source_collection: String,
    pub event_kind: String,
    pub filter: Option<String>,
    pub enabled: Option<bool>,
    pub concurrency: Option<String>,
}

// --- operator-surfaces request params (issue #302) ---

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopOperationsSnapshotRequest {
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub root_request_id: Option<String>,
    #[serde(default)]
    pub include_terminal: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopListSubagentTreeRequest {
    pub root_request_id: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub include_terminal: Option<bool>,
    #[serde(default)]
    pub max_depth: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopPreviewInterruptCascadeRequest {
    pub request_id: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub include_terminal: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopInterruptRequest {
    pub request_id: String,
    /// Currently always `"userCancelled"` per spec line 907. Kept as a String
    /// so future cause variants don't require an enum migration here.
    pub cause: String,
    pub cascade: bool,
    #[serde(default)]
    pub expected_preview_signature: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopProbeMcpServiceRequest {
    pub service_id: String,
}
