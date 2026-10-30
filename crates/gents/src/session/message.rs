#[cfg(test)]
use crate::config_client::ConfigAccess;
#[cfg(test)]
use crate::graphql::escape_graphql_string;
#[cfg(test)]
use crate::request::lifecycle::{SessionMessageCause, SessionMessageTarget};
#[cfg(test)]
use crate::session::origin::SessionScope;
#[cfg(test)]
use crate::tool_surface::AgentToolConfig;
use anyhow::Result;
#[cfg(test)]
use defra_node::EmbeddedNode;
use serde::Deserialize;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
mod agent_tools_tests;
#[cfg(test)]
mod delivery_conformance;
#[cfg(test)]
mod goal_input_tests;
#[cfg(test)]
mod hop_tests;

mod body;
mod caused;
mod commit;
mod discovery;
mod inspection;
mod interrupt;
mod plan;
mod target;

pub(crate) use body::{load_configured_task, render_body, render_task_body};
pub(crate) use caused::{kill, observe_caused_request, CausedObservation, KillOutcome};
#[cfg(test)]
pub(crate) use commit::load_receipt;
pub(crate) use commit::{commit, stage_planned_write_in_txn, CommitError, SessionMessageReceipt};
pub(crate) use discovery::{agent_session_list, AgentSessionListArgs};
pub(crate) use inspection::{agent_session_read, AgentSessionReadArgs};
pub use interrupt::{agent_interrupt_allowed, interrupt_refusal, interrupt_session};
#[cfg(test)]
pub(crate) use plan::plan;
pub(crate) use plan::{plan_with_delivery, Plan};
pub(crate) use target::{agent_list, load_caller_session_tools, resolve_send_target, target_scope};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskBody {
    pub task_id: String,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentNewArgs {
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
pub(crate) struct AgentMessageArgs {
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
pub(crate) enum AgentMessageDelivery {
    Queue,
    Steer,
    Interrupt,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentInterruptArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentListArgs {}

pub(crate) enum MessageBody<'a> {
    Prompt(&'a str),
    Task(&'a TaskBody),
}

pub(crate) fn message_body<'a>(
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

pub(crate) struct RenderedBody {
    pub content: String,
    pub goal: Option<(String, Option<i64>)>,
}
