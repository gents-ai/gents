use super::shared::wire_enum;
use super::AdditionalPermissionProfile;
use super::ExecPolicyAmendment;
use super::McpToolCallError;
use super::McpToolCallResult;
use super::NetworkApprovalContext;
use super::NetworkApprovalProtocol;
use super::NetworkPolicyAmendment;
use super::RequestPermissionProfile;
use super::UserInput;
use rapidhash::RapidHashMap;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_with::serde_as;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CommandExecutionApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptWithExecpolicyAmendment {
        execpolicy_amendment: ExecPolicyAmendment,
    },
    ApplyNetworkPolicyAmendment {
        network_policy_amendment: NetworkPolicyAmendment,
    },
    Decline,
    Cancel,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FileChangeApprovalDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CommandAction {
    Read {
        command: String,
        name: String,
        path: AbsolutePathBuf,
    },
    ListFiles {
        command: String,
        path: Option<String>,
    },
    Search {
        command: String,
        query: Option<String>,
        path: Option<String>,
    },
    Unknown {
        command: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryCitation {
    pub entries: Vec<MemoryCitationEntry>,
    pub thread_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryCitationEntry {
    pub path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub note: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadItem {
    #[serde(rename_all = "camelCase")]
    UserMessage { id: String, content: Vec<UserInput> },
    #[serde(rename_all = "camelCase")]
    HookPrompt {
        id: String,
        fragments: Vec<HookPromptFragment>,
    },
    #[serde(rename_all = "camelCase")]
    AgentMessage {
        id: String,
        text: String,
        #[serde(default)]
        phase: Option<MessagePhase>,
        #[serde(default)]
        memory_citation: Option<MemoryCitation>,
    },
    #[serde(rename_all = "camelCase")]
    Plan { id: String, text: String },
    #[serde(rename_all = "camelCase")]
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<String>,
        #[serde(default)]
        content: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    CommandExecution {
        id: String,
        command: String,
        cwd: AbsolutePathBuf,
        process_id: Option<String>,
        #[serde(default)]
        source: CommandExecutionSource,
        status: CommandExecutionStatus,
        command_actions: Vec<CommandAction>,
        aggregated_output: Option<String>,
        exit_code: Option<i32>,
        duration_ms: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    FileChange {
        id: String,
        changes: Vec<FileUpdateChange>,
        status: PatchApplyStatus,
    },
    #[serde(rename_all = "camelCase")]
    McpToolCall {
        id: String,
        server: String,
        tool: String,
        status: McpToolCallStatus,
        arguments: JsonValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mcp_app_resource_uri: Option<String>,
        plugin_id: Option<String>,
        result: Option<Box<McpToolCallResult>>,
        error: Option<McpToolCallError>,
        duration_ms: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    DynamicToolCall {
        id: String,
        namespace: Option<String>,
        tool: String,
        arguments: JsonValue,
        status: DynamicToolCallStatus,
        content_items: Option<Vec<DynamicToolCallOutputContentItem>>,
        success: Option<bool>,
        duration_ms: Option<i64>,
    },
    #[serde(rename_all = "camelCase")]
    CollabAgentToolCall {
        id: String,
        tool: CollabAgentTool,
        status: CollabAgentToolCallStatus,
        sender_thread_id: String,
        receiver_thread_ids: Vec<String>,
        prompt: Option<String>,
        model: Option<String>,
        reasoning_effort: Option<ReasoningEffort>,
        agents_states: RapidHashMap<String, CollabAgentState>,
    },
    #[serde(rename_all = "camelCase")]
    WebSearch {
        id: String,
        query: String,
        action: Option<WebSearchAction>,
    },
    #[serde(rename_all = "camelCase")]
    ImageView { id: String, path: AbsolutePathBuf },
    #[serde(rename_all = "camelCase")]
    ImageGeneration {
        id: String,
        status: String,
        revised_prompt: Option<String>,
        result: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        saved_path: Option<AbsolutePathBuf>,
    },
    #[serde(rename_all = "camelCase")]
    EnteredReviewMode { id: String, review: String },
    #[serde(rename_all = "camelCase")]
    ExitedReviewMode { id: String, review: String },
    #[serde(rename_all = "camelCase")]
    ContextCompaction { id: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HookPromptFragment {
    pub text: String,
    pub hook_run_id: String,
}

impl ThreadItem {
    pub fn id(&self) -> &str {
        match self {
            ThreadItem::UserMessage { id, .. }
            | ThreadItem::HookPrompt { id, .. }
            | ThreadItem::AgentMessage { id, .. }
            | ThreadItem::Plan { id, .. }
            | ThreadItem::Reasoning { id, .. }
            | ThreadItem::CommandExecution { id, .. }
            | ThreadItem::FileChange { id, .. }
            | ThreadItem::McpToolCall { id, .. }
            | ThreadItem::DynamicToolCall { id, .. }
            | ThreadItem::CollabAgentToolCall { id, .. }
            | ThreadItem::WebSearch { id, .. }
            | ThreadItem::ImageView { id, .. }
            | ThreadItem::ImageGeneration { id, .. }
            | ThreadItem::EnteredReviewMode { id, .. }
            | ThreadItem::ExitedReviewMode { id, .. }
            | ThreadItem::ContextCompaction { id, .. } => id,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
/// [UNSTABLE] Lifecycle state for an approval auto-review.
pub enum GuardianApprovalReviewStatus {
    InProgress,
    Approved,
    Denied,
    TimedOut,
    Aborted,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AutoReviewDecisionSource {
    Agent,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GuardianRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GuardianUserAuthorization {
    Unknown,
    Low,
    Medium,
    High,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GuardianApprovalReview {
    pub status: GuardianApprovalReviewStatus,
    pub risk_level: Option<GuardianRiskLevel>,
    pub user_authorization: Option<GuardianUserAuthorization>,
    pub rationale: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GuardianCommandSource {
    Shell,
    UnifiedExec,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GuardianCommandReviewAction {
    pub source: GuardianCommandSource,
    pub command: String,
    pub cwd: AbsolutePathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GuardianExecveReviewAction {
    pub source: GuardianCommandSource,
    pub program: String,
    pub argv: Vec<String>,
    pub cwd: AbsolutePathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GuardianApplyPatchReviewAction {
    pub cwd: AbsolutePathBuf,
    pub files: Vec<AbsolutePathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GuardianNetworkAccessReviewAction {
    pub target: String,
    pub host: String,
    pub protocol: NetworkApprovalProtocol,
    pub port: u16,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GuardianMcpToolCallReviewAction {
    pub server: String,
    pub tool_name: String,
    pub connector_id: Option<String>,
    pub connector_name: Option<String>,
    pub tool_title: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GuardianRequestPermissionsReviewAction {
    pub reason: Option<String>,
    pub permissions: RequestPermissionProfile,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum GuardianApprovalReviewAction {
    #[serde(rename_all = "camelCase")]
    Command {
        source: GuardianCommandSource,
        command: String,
        cwd: AbsolutePathBuf,
    },
    #[serde(rename_all = "camelCase")]
    Execve {
        source: GuardianCommandSource,
        program: String,
        argv: Vec<String>,
        cwd: AbsolutePathBuf,
    },
    #[serde(rename_all = "camelCase")]
    ApplyPatch {
        cwd: AbsolutePathBuf,
        files: Vec<AbsolutePathBuf>,
    },
    #[serde(rename_all = "camelCase")]
    NetworkAccess {
        target: String,
        host: String,
        protocol: NetworkApprovalProtocol,
        port: u16,
    },
    #[serde(rename_all = "camelCase")]
    McpToolCall {
        server: String,
        tool_name: String,
        connector_id: Option<String>,
        connector_name: Option<String>,
        tool_title: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    RequestPermissions {
        reason: Option<String>,
        permissions: RequestPermissionProfile,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WebSearchAction {
    Search {
        query: Option<String>,
        queries: Option<Vec<String>>,
    },
    OpenPage {
        url: Option<String>,
    },
    FindInPage {
        url: Option<String>,
        pattern: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum CommandExecutionStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

wire_enum! {
    #[derive(Default)]
    pub enum CommandExecutionSource {
        #[default]
        Agent,
        UserShell,
        UnifiedExecStartup,
        UnifiedExecInteraction,
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum CollabAgentTool {
    SpawnAgent,
    SendInput,
    ResumeAgent,
    Wait,
    CloseAgent,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileUpdateChange {
    pub path: String,
    pub kind: PatchChangeKind,
    pub diff: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PatchChangeKind {
    Add,
    Delete,
    Update { move_path: Option<PathBuf> },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum PatchApplyStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum McpToolCallStatus {
    InProgress,
    Completed,
    Failed,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum DynamicToolCallStatus {
    InProgress,
    Completed,
    Failed,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CollabAgentToolCallStatus {
    InProgress,
    Completed,
    Failed,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CollabAgentStatus {
    PendingInit,
    Running,
    Interrupted,
    Completed,
    Errored,
    Shutdown,
    NotFound,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CollabAgentState {
    pub status: CollabAgentStatus,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemStartedNotification {
    pub item: ThreadItem,
    pub thread_id: String,
    pub turn_id: String,
    pub started_at_ms: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemGuardianApprovalReviewStartedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub started_at_ms: i64,
    pub review_id: String,
    /// In most cases, one review maps to one target item; network policy
    /// reviews have no target item and use `None`.
    pub target_item_id: Option<String>,
    pub review: GuardianApprovalReview,
    pub action: GuardianApprovalReviewAction,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemGuardianApprovalReviewCompletedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub review_id: String,
    /// In most cases, one review maps to one target item; network policy
    /// reviews have no target item and use `None`.
    pub target_item_id: Option<String>,
    pub decision_source: AutoReviewDecisionSource,
    pub review: GuardianApprovalReview,
    pub action: GuardianApprovalReviewAction,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemCompletedNotification {
    pub item: ThreadItem,
    pub thread_id: String,
    pub turn_id: String,
    pub completed_at_ms: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RawResponseItemCompletedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item: ResponseItem,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlanDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningSummaryTextDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    pub summary_index: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningSummaryPartAddedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub summary_index: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningTextDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    pub content_index: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInteractionNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub process_id: String,
    pub stdin: String,
}

#[serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionOutputDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}
/// Deprecated legacy notification for `apply_patch` textual output.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeOutputDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileChangePatchUpdatedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub changes: Vec<FileUpdateChange>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionRequestApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_approval_context: Option<NetworkApprovalContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<AbsolutePathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_actions: Option<Vec<CommandAction>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_permissions: Option<AdditionalPermissionProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_execpolicy_amendment: Option<ExecPolicyAmendment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_network_policy_amendments: Option<Vec<NetworkPolicyAmendment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_decisions: Option<Vec<CommandExecutionApprovalDecision>>,
}

impl CommandExecutionRequestApprovalParams {
    pub fn strip_experimental_fields(&mut self) {
        self.additional_permissions = None;
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionRequestApprovalResponse {
    pub decision: CommandExecutionApprovalDecision,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeRequestApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub started_at_ms: i64,
    pub reason: Option<String>,
    pub grant_root: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FileChangeRequestApprovalResponse {
    pub decision: FileChangeApprovalDecision,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallParams {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub namespace: Option<String>,
    pub tool: String,
    pub arguments: JsonValue,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallResponse {
    pub content_items: Vec<DynamicToolCallOutputContentItem>,
    pub success: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DynamicToolCallOutputContentItem {
    #[serde(rename_all = "camelCase")]
    InputText { text: String },
    #[serde(rename_all = "camelCase")]
    InputImage { image_url: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequestUserInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequestUserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub is_other: bool,
    #[serde(default)]
    pub is_secret: bool,
    pub options: Option<Vec<ToolRequestUserInputOption>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequestUserInputParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub questions: Vec<ToolRequestUserInputQuestion>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequestUserInputAnswer {
    pub answers: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequestUserInputResponse {
    pub answers: RapidHashMap<String, ToolRequestUserInputAnswer>,
}
use crate::core_types::{AbsolutePathBuf, MessagePhase, ReasoningEffort, ResponseItem};
