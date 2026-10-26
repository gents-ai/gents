use std::num::ParseIntError;

use gents_protocol::mailbox_question::MailboxQuestionError;

#[derive(Debug, thiserror::Error)]
pub enum BridgeCommandError {
    #[error("{name} is required")]
    Required { name: &'static str },
    #[error("token budget must be a positive whole number within the runtime limit")]
    GoalBudgetNotNumeric(#[source] ParseIntError),
    #[error("token budget must be positive")]
    GoalBudgetNotPositive,
    #[error("The chat folder must be an existing folder on this computer.")]
    ChatFolderNotFound,
    #[error("schedule {id} was not found")]
    ScheduleNotFound { id: String },
    #[error("schedule {id} is ambiguous across agent scopes")]
    ScheduleAmbiguous { id: String },
    #[error("task {id} was not found")]
    TaskNotFound { id: String },
    #[error("task {id} is ambiguous across agent scopes")]
    TaskAmbiguous { id: String },
    #[error("MailboxItem not found")]
    MailboxItemNotFound,
    #[error("only requester_did may act on a MailboxItem")]
    MailboxRequesterMismatch,
    #[error("MailboxItem is no longer open")]
    MailboxItemClosed,
    #[error("MailboxItem action does not open a compose surface")]
    MailboxActionNotComposable,
    #[error("an answer requires causedBySourceDocId")]
    AnswerRequiresItem,
    #[error("MailboxItem is not a question")]
    MailboxItemNotQuestion,
    #[error("an answer must be sent to the question's session")]
    AnswerSessionMismatch,
    #[error("an answer must be sent to the agent that asked")]
    AnswerAgentMismatch,
    #[error(transparent)]
    MailboxQuestion(#[from] MailboxQuestionError),
    #[error("mcp_port is required")]
    McpPortRequired,
    #[error("mcp_port must be between 1 and 65535")]
    McpPortOutOfRange,
    #[error("hostname, tailscale_ip, or lan_ip is required")]
    EndpointHostRequired,
    #[error("parsing ToolServiceHealthState rows: {error}")]
    ParseHealthRows { error: serde_json::Error },
    #[error("ToolServiceHealthState row for service_id={service_id:?} has missing/unrecognized status {status:?}")]
    HealthStatusUnrecognized {
        service_id: String,
        status: Option<String>,
    },
    #[error("an answer renders its own content; send empty content")]
    AnswerHasContent,
    #[error("resolving an earlier send requires its session_id")]
    ResolveRequiresSessionId,
    #[error("manual task run request {request_id} was not found")]
    ManualRunRequestNotFound { request_id: String },
    #[error("manual schedule run request has no _docID")]
    ManualScheduleRunNoDocId,
    #[error("manual task run request has no _docID")]
    ManualTaskRunNoDocId,
    #[error("MCP list_tools timed out")]
    McpListToolsTimedOut(#[source] tokio::time::error::Elapsed),
    #[error(transparent)]
    Core(#[from] gents_desktop_core::client::ClientCoreError),
    #[error(transparent)]
    McpRegistry(#[from] super::mcp_health::McpRegistryError),
    #[error(transparent)]
    Receipt(#[from] gents_desktop_core::client::ReceiptError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    ConfigClient(#[from] gents::config_client::ConfigClientError),
    #[error(transparent)]
    BackgroundTools(#[from] gents::background_tools::BackgroundToolsError),
    #[error(transparent)]
    McpPool(#[from] gents::mcp_pool::McpPoolError),
}

pub(crate) type Result<T, E = BridgeCommandError> = std::result::Result<T, E>;

pub(super) fn trim_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn require_trimmed(
    name: &'static str,
    value: impl AsRef<str>,
) -> Result<String, BridgeCommandError> {
    let value = value.as_ref().trim().to_string();
    if value.is_empty() {
        return Err(BridgeCommandError::Required { name });
    }
    Ok(value)
}
