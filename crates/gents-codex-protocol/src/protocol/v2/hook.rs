use super::shared::wire_enum;
use serde::Deserialize;
use serde::Serialize;

wire_enum!(
    pub enum HookEventName {
        PreToolUse,
        PermissionRequest,
        PostToolUse,
        PreCompact,
        PostCompact,
        SessionStart,
        UserPromptSubmit,
        SubagentStart,
        SubagentStop,
        Stop,
    }
);

wire_enum!(
    pub enum HookHandlerType {
        Command,
        Prompt,
        Agent,
    }
);

wire_enum!(
    pub enum HookExecutionMode {
        Sync,
        Async,
    }
);

wire_enum!(
    pub enum HookScope {
        Thread,
        Turn,
    }
);

wire_enum!(
    pub enum HookSource {
        System,
        User,
        Project,
        Mdm,
        SessionFlags,
        Plugin,
        CloudRequirements,
        LegacyManagedConfigFile,
        LegacyManagedConfigMdm,
        Unknown,
    }
);

wire_enum!(
    pub enum HookTrustStatus {
        Managed,
        Untrusted,
        Trusted,
        Modified,
    }
);

fn default_hook_source() -> HookSource {
    HookSource::Unknown
}

wire_enum!(
    pub enum HookRunStatus {
        Running,
        Completed,
        Failed,
        Blocked,
        Stopped,
    }
);

wire_enum!(
    pub enum HookOutputEntryKind {
        Warning,
        Stop,
        Feedback,
        Context,
        Error,
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HookOutputEntry {
    pub kind: HookOutputEntryKind,
    pub text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HookRunSummary {
    pub id: String,
    pub event_name: HookEventName,
    pub handler_type: HookHandlerType,
    pub execution_mode: HookExecutionMode,
    pub scope: HookScope,
    pub source_path: AbsolutePathBuf,
    #[serde(default = "default_hook_source")]
    pub source: HookSource,
    pub display_order: i64,
    pub status: HookRunStatus,
    pub status_message: Option<String>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub entries: Vec<HookOutputEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HookStartedNotification {
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub run: HookRunSummary,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HookCompletedNotification {
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub run: HookRunSummary,
}
use crate::core_types::AbsolutePathBuf;
