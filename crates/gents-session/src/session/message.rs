use anyhow::Result;
#[cfg(test)]
use gents_documents::AgentToolConfig;
#[cfg(test)]
use gents_protocol::request_lifecycle::{SessionMessageCause, SessionMessageTarget};
#[cfg(test)]
use gents_protocol::session::SessionScope;
#[cfg(test)]
use gents_store::config_client::ConfigAccess;
#[cfg(test)]
use gents_store::graphql::escape_graphql_string;
use serde::Deserialize;

#[cfg(test)]
mod agent_tools_tests;
#[cfg(test)]
mod delivery_conformance;
#[cfg(test)]
mod goal_input_tests;
#[cfg(test)]
mod hop_tests;

mod body;
pub mod commit;
mod discovery;
mod inspection;
mod interrupt;
pub mod plan;
mod target;

pub use body::{load_configured_task, render_body, render_task_body};
pub(crate) use commit::commit;
pub use commit::{
    load_receipt, stage_planned_write_in_txn, CommitError, SessionMessageCall,
    SessionMessageReceipt, ToolCallTerminal,
};
pub use discovery::{agent_session_list, AgentSessionListArgs};
pub use inspection::{agent_session_read, AgentSessionReadArgs};
pub use interrupt::{
    agent_interrupt_allowed, interrupt_refusal, interrupt_session, interrupt_session_with_access,
};
#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) use plan::plan;
pub use plan::{plan_with_delivery, Plan};
pub use target::{
    agent_list, load_caller_session_tools, resolve_send_target, target_agent_exists, target_scope,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskBody {
    pub task_id: String,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentNewArgs {
    pub agent: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub task: Option<TaskBody>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentMessageArgs {
    pub session_id: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub task: Option<TaskBody>,
    #[serde(default)]
    pub interrupt: Option<bool>,
    #[serde(default)]
    pub delivery: Option<AgentMessageDelivery>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessageDelivery {
    Queue,
    Steer,
    Interrupt,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInterruptArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentListArgs {}

pub(crate) enum MessageBody<'a> {
    Prompt(&'a str),
    Task(&'a TaskBody),
}

pub fn message_body<'a>(
    field: &str,
    prompt: Option<&'a String>,
    task: Option<&'a TaskBody>,
) -> Result<MessageBody<'a>, String> {
    match (prompt.map(|prompt| prompt.trim()), task) {
        (Some(prompt), None) if !prompt.is_empty() => Ok(MessageBody::Prompt(prompt)),
        (None, Some(task)) if !task.task_id.trim().is_empty() => Ok(MessageBody::Task(task)),
        (Some(_), None) => Err(format!("{field} must be non-empty")),
        (None, Some(_)) => Err("task.task_id must be non-empty".to_owned()),
        _ => Err(format!("provide exactly one of {field} or task")),
    }
}

impl AgentNewArgs {
    pub(crate) fn body(&self) -> Result<MessageBody<'_>, String> {
        message_body("prompt", self.prompt.as_ref(), self.task.as_ref())
    }
}

impl AgentMessageArgs {
    pub(crate) fn body(&self) -> Result<MessageBody<'_>, String> {
        message_body("message", self.message.as_ref(), self.task.as_ref())
    }

    pub(crate) fn delivery(&self) -> Result<AgentMessageDelivery, String> {
        match (self.delivery, self.interrupt) {
            (None, None | Some(false)) => Ok(AgentMessageDelivery::Queue),
            (None, Some(true)) | (Some(AgentMessageDelivery::Interrupt), Some(true)) => {
                Ok(AgentMessageDelivery::Interrupt)
            }
            (Some(delivery), None) => Ok(delivery),
            (
                Some(delivery @ (AgentMessageDelivery::Queue | AgentMessageDelivery::Steer)),
                Some(false),
            ) => Ok(delivery),
            (Some(_), Some(_)) => Err("interrupt conflicts with delivery".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    Request,
    Queued,
    Steering,
}

impl Delivery {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Queued => "queue",
            Self::Steering => "steer",
        }
    }
}

pub struct RenderedBody {
    pub content: String,
    pub goal: Option<(String, Option<i64>)>,
}
