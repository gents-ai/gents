use serde::{Deserialize, Serialize};

use super::NextCall;

/// What a caller supplies to fire one Task now.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRunInput {
    pub task_id: String,
    /// A JSON object bound as the `args.*` template scope.
    pub args: serde_json::Value,
    /// A goal-backed Task's stable invocation key; otherwise a session label.
    pub session_id: Option<String>,
    /// An existing session to continue with this Task.
    pub continue_session: Option<String>,
}

/// Lean `Triggers.ManualRun.Presence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPresence {
    Missing,
    Disabled,
    Enabled,
}

/// Lean `Triggers.ManualRun.RunDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunDecision {
    Run,
    TaskMissing,
    TaskDisabled,
    AgentMissing,
    AgentDisabled,
}

/// Lean `Triggers.ManualRun.Matches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskMatches {
    None,
    One,
    Many,
}

impl TaskMatches {
    pub fn of(count: usize) -> Self {
        match count {
            0 => Self::None,
            1 => Self::One,
            _ => Self::Many,
        }
    }
}

/// Lean `Triggers.ManualRun.ScheduleDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRunDecision {
    Run,
    ScheduleMissing,
    ScheduleAmbiguous,
    NoEnabledTrigger,
    ManyEnabledTriggers,
}

/// Lean `Triggers.ManualRun.TitleKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskTitleKind {
    Unchanged,
    Run,
    Goal,
}

#[derive(Debug, thiserror::Error)]
pub enum TaskOperationError {
    #[error("task {task_id} was not found under node {node_did}")]
    TaskMissing { task_id: String, node_did: String },
    #[error("Task {0} is disabled")]
    TaskDisabled(String),
    #[error("task agent {agent_did} was not found under node {node_did}")]
    AgentMissing { agent_did: String, node_did: String },
    #[error("Agent {0} is disabled")]
    AgentDisabled(String),
    #[error("task args must be a JSON object (got: {0})")]
    ArgsNotObject(serde_json::Value),
    #[error("Task {0} declares a durable goal; pass --session-id with a stable invocation identity so retries converge")]
    StableKeyRequired(String),
    #[error(
        "ordinary Tasks accept either a session label or an existing session to continue, not both"
    )]
    SessionLabelAndContinue,
    #[error("schedule {schedule_id} was not found for {node_did}")]
    ScheduleMissing {
        schedule_id: String,
        node_did: String,
    },
    #[error("schedule {schedule_id} is ambiguous for {node_did}")]
    ScheduleAmbiguous {
        schedule_id: String,
        node_did: String,
    },
    #[error("schedule {0} has no enabled Trigger")]
    NoEnabledTrigger(String),
    #[error("schedule {0} has multiple enabled Triggers; run a Task explicitly")]
    ManyEnabledTriggers(String),
    /// Another owner's failure (templates, request signing, Task delivery,
    /// the store), unchanged.
    #[error(transparent)]
    Passthrough(Box<dyn std::error::Error + Send + Sync>),
}

impl TaskOperationError {
    /// The call that lets the caller correct this failure.
    pub fn recovery(&self) -> NextCall {
        match self {
            Self::TaskMissing { .. } => NextCall::List { resource: "task" },
            Self::TaskDisabled(_) => NextCall::Inspect { resource: "task" },
            Self::AgentMissing { .. } | Self::AgentDisabled(_) => {
                NextCall::Inspect { resource: "agent" }
            }
            Self::ArgsNotObject(_)
            | Self::StableKeyRequired(_)
            | Self::SessionLabelAndContinue
            | Self::Passthrough(_) => NextCall::VerbHelp {
                resource: "task",
                verb: "run",
            },
            Self::ScheduleMissing { .. } | Self::ScheduleAmbiguous { .. } => NextCall::List {
                resource: "schedule",
            },
            Self::NoEnabledTrigger(_) | Self::ManyEnabledTriggers(_) => NextCall::List {
                resource: "trigger",
            },
        }
    }

    pub fn passthrough(&self) -> Option<&(dyn std::error::Error + Send + Sync + 'static)> {
        match self {
            Self::Passthrough(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}
