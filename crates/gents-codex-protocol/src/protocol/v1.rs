use rapidhash::RapidHashMap;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use crate::protocol::common::AuthMode;
use crate::protocol::v2::ForcedChatgptWorkspaceIds;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<InitializeCapabilities>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub title: Option<String>,
    pub version: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct InitializeCapabilities {
    #[serde(default)]
    pub experimental_api: bool,
    #[serde(default)]
    pub request_attestation: bool,
    pub opt_out_notification_methods: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub user_agent: String,
    pub codex_home: AbsolutePathBuf,
    pub platform_family: String,
    pub platform_os: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum GetConversationSummaryParams {
    RolloutPath {
        #[serde(rename = "rolloutPath")]
        rollout_path: PathBuf,
    },
    ThreadId {
        #[serde(rename = "conversationId")]
        conversation_id: ThreadId,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct GetConversationSummaryResponse {
    pub summary: ConversationSummary,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    pub conversation_id: ThreadId,
    pub path: PathBuf,
    pub preview: String,
    pub timestamp: Option<String>,
    pub updated_at: Option<String>,
    pub model_provider: String,
    pub cwd: PathBuf,
    pub cli_version: String,
    pub source: SessionSource,
    pub git_info: Option<ConversationGitInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ConversationGitInfo {
    pub sha: Option<String>,
    pub branch: Option<String>,
    pub origin_url: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoginApiKeyParams {
    pub api_key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffToRemoteResponse {
    pub sha: GitSha,
    pub diff: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPatchApprovalParams {
    pub conversation_id: ThreadId,
    pub call_id: String,
    pub file_changes: RapidHashMap<PathBuf, FileChange>,
    pub reason: Option<String>,
    pub grant_root: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPatchApprovalResponse {
    pub decision: ReviewDecision,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExecCommandApprovalParams {
    pub conversation_id: ThreadId,
    pub call_id: String,
    pub approval_id: Option<String>,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub reason: Option<String>,
    pub parsed_cmd: Vec<ParsedCommand>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ExecCommandApprovalResponse {
    pub decision: ReviewDecision,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffToRemoteParams {
    pub cwd: PathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetAuthStatusParams {
    pub include_token: Option<bool>,
    pub refresh_token: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExecOneOffCommandParams {
    pub command: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub cwd: Option<PathBuf>,
    pub sandbox_policy: Option<SandboxPolicy>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetAuthStatusResponse {
    pub auth_method: Option<AuthMode>,
    pub auth_token: Option<String>,
    pub requires_openai_auth: Option<bool>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSavedConfig {
    pub approval_policy: Option<AskForApproval>,
    pub sandbox_mode: Option<SandboxMode>,
    pub sandbox_settings: Option<SandboxSettings>,
    pub forced_chatgpt_workspace_id: Option<ForcedChatgptWorkspaceIds>,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub model: Option<String>,
    pub model_reasoning_effort: Option<ReasoningEffort>,
    pub model_reasoning_summary: Option<ReasoningSummary>,
    pub model_verbosity: Option<Verbosity>,
    pub tools: Option<Tools>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tools {
    pub web_search: Option<bool>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSettings {
    #[serde(default)]
    pub writable_roots: Vec<AbsolutePathBuf>,
    pub network_access: Option<bool>,
    pub exclude_tmpdir_env_var: Option<bool>,
    pub exclude_slash_tmp: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InterruptConversationResponse {
    pub abort_reason: TurnAbortReason,
}
use crate::core_types::{
    AbsolutePathBuf, FileChange, ForcedLoginMethod, GitSha, ParsedCommand, ReasoningEffort,
    ReasoningSummary, ReviewDecision, SessionSource, ThreadId, TurnAbortReason, Verbosity,
};
use crate::{AskForApproval, SandboxMode, SandboxPolicy};
