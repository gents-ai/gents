use std::error::Error;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::agent::NextCall;
use crate::request_input::{QueueDelivery, RequestInput};
use crate::request_lifecycle::RequestLifecycleState;

/// Lean `SessionOperations.DeliveryChoice`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryChoice {
    #[default]
    Unspecified,
    Queue,
    Steer,
}

impl DeliveryChoice {
    /// Lean `SessionOperations.resolveDelivery`.
    pub fn resolve(self) -> QueueDelivery {
        match self {
            Self::Unspecified | Self::Queue => QueueDelivery::Queue,
            Self::Steer => QueueDelivery::Steer,
        }
    }

    pub fn steer_when(steer: bool) -> Self {
        if steer {
            Self::Steer
        } else {
            Self::Unspecified
        }
    }
}

impl From<Option<QueueDelivery>> for DeliveryChoice {
    fn from(delivery: Option<QueueDelivery>) -> Self {
        match delivery {
            None => Self::Unspecified,
            Some(QueueDelivery::Queue) => Self::Queue,
            Some(QueueDelivery::Steer) => Self::Steer,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MessageSendInput {
    pub node_did: String,
    pub session_id: Option<String>,
    pub agent_did: Option<String>,
    pub request_id: Option<String>,
    pub content: String,
    pub input: RequestInput,
    pub delivery: DeliveryChoice,
    pub valid_until: Option<DateTime<Utc>>,
    pub mailbox_item_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RequestReceipt {
    pub request_id: String,
    pub request_doc_id: String,
    pub session_id: String,
    pub node_did: String,
    pub requester_did: Option<String>,
    pub agent_did: Option<String>,
    pub input: Option<RequestInput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRef {
    pub node_did: String,
    pub requester_did: Option<String>,
    pub request_id: String,
}

/// Lean `SessionOperations.Matches`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestMatches {
    None,
    One,
    Many,
}

impl RequestMatches {
    pub fn of(count: usize) -> Self {
        match count {
            0 => Self::None,
            1 => Self::One,
            _ => Self::Many,
        }
    }
}

/// Lean `SessionOperations.Lookup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestLookup {
    NotFound,
    Found,
    Ambiguous,
}

/// Lean `SessionOperations.ReissueKind`: `Retry` runs a failed turn again in
/// its session; `Resend` sends a stale request again in a fresh session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReissueKind {
    Retry,
    Resend,
}

impl ReissueKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Resend => "resend",
        }
    }
}

/// Lean `SessionOperations.ReissueParent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReissueParent {
    Failed,
    StaleDead,
    OtherTerminal,
    Live,
}

impl ReissueParent {
    pub fn of(state: Option<RequestLifecycleState>, failure_reason: Option<&str>) -> Self {
        match state {
            Some(RequestLifecycleState::Failed) => Self::Failed,
            Some(RequestLifecycleState::Dead) if failure_reason == Some("Stale") => Self::StaleDead,
            Some(state) if state.is_terminal() => Self::OtherTerminal,
            Some(_) | None => Self::Live,
        }
    }
}

/// Lean `SessionOperations.ReissueDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReissueDecision {
    Admit,
    RefuseParentState,
    RefuseNotInteractive,
    RefuseBudgetExhausted,
    RefuseDeadlineClosed,
    RefuseNotLatest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestReissueInput {
    pub kind: ReissueKind,
    pub parent: RequestRef,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReissueReceipt {
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub agent_did: Option<String>,
    pub retry_parent_request: String,
    pub retry_root_request: String,
    pub retry_count: i64,
    pub max_retries: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestWaitInput {
    pub request: RequestRef,
    pub timeout_secs: u64,
    pub poll_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestInterruptInput {
    pub request: RequestRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestInterruptReceipt {
    pub request_id: String,
    pub request_doc_id: String,
    pub node_did: String,
    pub interrupt_requested_at: Option<String>,
    pub already_interrupted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxCauseRefusal {
    NotOneItem,
    Closed,
    NotCompose,
    OtherAgent,
    OtherRequester,
    OtherSession,
}

impl MailboxCauseRefusal {
    fn describe(self) -> &'static str {
        match self {
            Self::NotOneItem => "it does not identify exactly one item",
            Self::Closed => "the item is no longer open",
            Self::NotCompose => "the item does not ask for a message",
            Self::OtherAgent => "the item is addressed to another agent",
            Self::OtherRequester => "the item belongs to another requester",
            Self::OtherSession => "the item belongs to another session",
        }
    }
}

impl std::fmt::Display for MailboxCauseRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.describe())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RequestOperationError {
    #[error("{0} is required")]
    FieldRequired(&'static str),
    #[error("a message needs content, captured context or an attachment")]
    ContentRequired,
    #[error("this operation needs an authenticated client identity")]
    IdentityRequired,
    #[error("the message's queue entry is set by delivery; send it without input.queue")]
    QueueEntryOwned,
    #[error("session {session_id} has more than one active request; interrupt one before sending")]
    ConflictingActiveRequests { session_id: String },
    #[error("node {node_did} is not one enabled Node this client can read")]
    NodeUnavailable { node_did: String },
    #[error("node {node_did} has no default agent; name an agent")]
    NoDefaultAgent { node_did: String },
    #[error("agent {agent_did} is not one enabled Agent of node {node_did}")]
    AgentUnavailable { node_did: String, agent_did: String },
    #[error("session {session_id} runs agent {session_agent}, not {requested}; send without an agent or start a new session")]
    SessionAgentMismatch {
        session_id: String,
        session_agent: String,
        requested: String,
    },
    #[error("mailbox item {item_id} cannot cause this message: {refusal}")]
    MailboxCause {
        item_id: String,
        refusal: MailboxCauseRefusal,
    },
    #[error("request {request_id} was not found on node {node_did}")]
    NotFound { node_did: String, request_id: String },
    #[error("request id {request_id} names more than one request on node {node_did}; name its requester")]
    Ambiguous { node_did: String, request_id: String },
    #[error("request {request_id} is {state} ({failure_reason}); {kind} needs {needs}")]
    ReissueParentState {
        kind: &'static str,
        request_id: String,
        state: &'static str,
        failure_reason: String,
        needs: &'static str,
    },
    #[error("request {request_id} was not started by a person; only an interactive turn can be retried")]
    ReissueNotInteractive { request_id: String },
    #[error("request {request_id} has used its retry budget ({retry_count} of {max_retries}); send the message again as a new request")]
    RetryBudgetExhausted {
        request_id: String,
        retry_count: i64,
        max_retries: i64,
    },
    #[error("request {request_id} passed its deadline {deadline}; send the message again as a new request")]
    ReissueDeadlineClosed { request_id: String, deadline: String },
    #[error("request {request_id} is not the latest turn of session {session_id} ({latest_request_id} is); retry that one or send a new message")]
    ReissueNotLatest {
        request_id: String,
        session_id: String,
        latest_request_id: String,
    },
    #[error("request {request_id} is already {state} and cannot be interrupted")]
    InterruptTerminal {
        request_id: String,
        state: &'static str,
    },
    #[error("request {request_id} did not reach a terminal state within {timeout_secs}s; it is still {state}")]
    WaitTimedOut {
        request_id: String,
        timeout_secs: u64,
        state: String,
    },
    #[error("the submission of request {request_id} is unconfirmed; send the same request id again")]
    Unconfirmed {
        request_id: String,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error("request id {request_id} is held by another request; send under a new request id")]
    IdentityConflict {
        request_id: String,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error(transparent)]
    Passthrough(Box<dyn Error + Send + Sync>),
}

impl RequestOperationError {
    pub fn passthrough(error: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self::Passthrough(error.into())
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::FieldRequired(_) => "field_required",
            Self::ContentRequired => "content_required",
            Self::IdentityRequired => "identity_required",
            Self::QueueEntryOwned => "queue_entry_owned",
            Self::ConflictingActiveRequests { .. } => "conflicting_active_requests",
            Self::NodeUnavailable { .. } => "node_unavailable",
            Self::NoDefaultAgent { .. } => "no_default_agent",
            Self::AgentUnavailable { .. } => "agent_unavailable",
            Self::SessionAgentMismatch { .. } => "session_agent_mismatch",
            Self::MailboxCause { .. } => "mailbox_cause",
            Self::NotFound { .. } => "request_not_found",
            Self::Ambiguous { .. } => "request_ambiguous",
            Self::ReissueParentState { .. } => "reissue_parent_state",
            Self::ReissueNotInteractive { .. } => "reissue_not_interactive",
            Self::RetryBudgetExhausted { .. } => "retry_budget_exhausted",
            Self::ReissueDeadlineClosed { .. } => "reissue_deadline_closed",
            Self::ReissueNotLatest { .. } => "reissue_not_latest",
            Self::InterruptTerminal { .. } => "interrupt_terminal",
            Self::WaitTimedOut { .. } => "wait_timed_out",
            Self::Unconfirmed { .. } => "submission_unconfirmed",
            Self::IdentityConflict { .. } => "request_identity_conflict",
            Self::Passthrough(_) => "failed",
        }
    }

    pub fn next_call(&self) -> NextCall {
        match self {
            Self::FieldRequired(_)
            | Self::ContentRequired
            | Self::QueueEntryOwned
            | Self::RetryBudgetExhausted { .. }
            | Self::ReissueDeadlineClosed { .. }
            | Self::Unconfirmed { .. }
            | Self::IdentityConflict { .. }
            | Self::Passthrough(_) => NextCall::VerbHelp {
                resource: "request",
                verb: "submit",
            },
            Self::IdentityRequired | Self::NodeUnavailable { .. } => {
                NextCall::Inspect { resource: "node" }
            }
            Self::NoDefaultAgent { .. }
            | Self::AgentUnavailable { .. }
            | Self::SessionAgentMismatch { .. } => NextCall::List { resource: "agent" },
            Self::MailboxCause { .. } => NextCall::List {
                resource: "mailbox",
            },
            Self::ConflictingActiveRequests { .. }
            | Self::ReissueNotLatest { .. }
            | Self::NotFound { .. }
            | Self::Ambiguous { .. } => NextCall::List {
                resource: "session",
            },
            Self::ReissueParentState { .. }
            | Self::ReissueNotInteractive { .. }
            | Self::InterruptTerminal { .. }
            | Self::WaitTimedOut { .. } => NextCall::Inspect {
                resource: "request",
            },
        }
    }

    pub fn source_passthrough(&self) -> Option<&(dyn Error + Send + Sync + 'static)> {
        match self {
            Self::Passthrough(error) => Some(error.as_ref()),
            Self::Unconfirmed { source, .. } | Self::IdentityConflict { source, .. } => {
                Some(source.as_ref())
            }
            _ => None,
        }
    }
}
