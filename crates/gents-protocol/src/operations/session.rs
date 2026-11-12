use std::cmp::Reverse;
use std::error::Error;

use serde::Serialize;

use super::NextCall;
use crate::session::{AgentSession, SessionRowError};

pub type SessionOrderKey = (
    Reverse<Option<String>>,
    Reverse<Option<String>>,
    String,
    Option<String>,
);

pub fn session_order_key(
    updated_at: Option<&str>,
    created_at: Option<&str>,
    session_id: &str,
    requester_did: Option<&str>,
) -> SessionOrderKey {
    (
        Reverse(updated_at.map(str::to_owned)),
        Reverse(created_at.map(str::to_owned)),
        session_id.to_owned(),
        requester_did.map(str::to_owned),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Open,
    Closed,
    Archived,
}

impl SessionState {
    pub fn of(session: &AgentSession) -> Self {
        if session.archived_at.is_some() {
            Self::Archived
        } else if session.closed_at.is_some() {
            Self::Closed
        } else {
            Self::Open
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Archived => "archived",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionListInput {
    pub node_did: Option<String>,
    pub requester_did: Option<String>,
    pub session_id: Option<String>,
    pub include_archived: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionEntry {
    #[serde(flatten)]
    pub session: AgentSession,
    pub state: SessionState,
    pub request_count: u64,
}

/// An absent `requester_did` selects the session of that node and id whatever
/// its requester; a present one must match the session's requester exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    pub node_did: String,
    pub session_id: String,
    pub requester_did: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRenameInput {
    pub session: SessionRef,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRenameReceipt {
    pub node_did: String,
    pub session_id: String,
    pub title: String,
}

/// An absent `expected_revision` fences the change at the revision the
/// operation read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionArchiveInput {
    pub session: SessionRef,
    pub expected_revision: Option<u64>,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionForkBoundary {
    UserTurn(u32),
    BeforeMessage {
        message_key: String,
        child_session_id: String,
        client_did: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionForkInput {
    pub source: SessionRef,
    pub boundary: SessionForkBoundary,
    pub target_agent_did: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SessionForkCopied {
    pub messages: u32,
    pub tool_calls: u32,
    pub tool_results: u32,
    pub compaction_entries: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionForkReceipt {
    pub node_did: String,
    pub source_session_id: String,
    pub session_id: String,
    pub copied: Option<SessionForkCopied>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStopStatus {
    Stopping,
    StopRequested,
}

impl SessionStopStatus {
    pub fn of(interrupted_a_request: bool) -> Self {
        if interrupted_a_request {
            Self::Stopping
        } else {
            Self::StopRequested
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopping => "stopping",
            Self::StopRequested => "stop_requested",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionStopReceipt {
    pub node_did: String,
    pub session_id: String,
    pub status: SessionStopStatus,
    pub interrupted_request_id: Option<String>,
    pub drained_request_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionResumeReceipt {
    pub node_did: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeBlocker {
    Requests,
    ToolCalls,
    BackgroundExecutions,
    Processes,
    PluginJobs,
}

impl ResumeBlocker {
    fn describe(self) -> &'static str {
        match self {
            Self::Requests => "session work is still settling",
            Self::ToolCalls => "a tool call of the session is still settling",
            Self::BackgroundExecutions => "a background execution of the session is still active",
            Self::Processes => "a process of the session has not been observed exited",
            Self::PluginJobs => "a plugin job step of the session is still executing",
        }
    }
}

impl std::fmt::Display for ResumeBlocker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.describe())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionOperationError {
    #[error("{0} is required")]
    FieldRequired(&'static str),
    #[error("this operation needs an authenticated client identity")]
    IdentityRequired,
    #[error("session {session_id} was not found on node {node_did} in this client's scope")]
    NotFound { node_did: String, session_id: String },
    #[error("session id {session_id} names more than one session on node {node_did}; name its requester")]
    Ambiguous { node_did: String, session_id: String },
    #[error("session {session_id} is not a valid session document: {source}")]
    InvalidDocument {
        session_id: String,
        #[source]
        source: SessionRowError,
    },
    #[error("session {session_id} has no requester, so a client cannot change it")]
    RequesterMissing { session_id: String },
    #[error("archive revision {revision} is out of range; read the session for its current revision")]
    RevisionOutOfRange { revision: u64 },
    #[error("session {session_id} belongs to another requester than this caller")]
    RequesterMismatch { session_id: String },
    #[error("session {session_id} has an active request and cannot be forked; interrupt it or fork an earlier turn after it settles")]
    ForkSourceBusy { session_id: String },
    #[error("user turn {at_user_turn} is out of range; session {session_id} has {user_messages} user messages")]
    ForkTurnOutOfRange {
        session_id: String,
        at_user_turn: u32,
        user_messages: u32,
    },
    #[error("the selected message {message_key} is missing, ambiguous or not editable: {reason}")]
    ForkMessageNotEditable { message_key: String, reason: String },
    #[error("the saved edit belongs to another client {client_did}; fork with this client's own identity")]
    ForkClientMismatch { client_did: String },
    #[error("child session id {child_session_id:?} must be nonblank and keep its exact canonical form")]
    ForkChildNotCanonical { child_session_id: String },
    #[error("forking to a different agent is unavailable; fork without naming an agent")]
    CrossAgentForkUnavailable,
    #[error("target agent {agent_did} was not found; name one of the node's enabled agents")]
    AgentUnavailable { agent_did: String },
    #[error("fork {session_id} was created but its access relationships are incomplete; fork recovery completes them")]
    ForkRelationshipsIncomplete {
        session_id: String,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error("fork {session_id} has incomplete relationships and its retained creation intent is required to finish it")]
    ForkRecoveryIntentRequired { session_id: String },
    #[error("session control is supported only on this runtime's own node; {node_did} is another node")]
    RemoteNodeUnsupported { node_did: String },
    #[error("session {session_id} cannot resume yet: {blocker}; read the session and resume after it settles")]
    ResumeBlocked {
        session_id: String,
        blocker: ResumeBlocker,
    },
    #[error("session {session_id} cannot resume: its retained process ownership could not be verified; retry after runtime recovery")]
    ProcessOwnershipUnverifiable {
        session_id: String,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error(transparent)]
    Passthrough(Box<dyn Error + Send + Sync>),
}

impl SessionOperationError {
    pub fn passthrough(error: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self::Passthrough(error.into())
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::FieldRequired(_) => "field_required",
            Self::IdentityRequired => "identity_required",
            Self::NotFound { .. } => "session_not_found",
            Self::Ambiguous { .. } => "session_ambiguous",
            Self::InvalidDocument { .. } => "session_invalid_document",
            Self::RequesterMissing { .. } => "session_requester_missing",
            Self::RevisionOutOfRange { .. } => "archive_revision_out_of_range",
            Self::RequesterMismatch { .. } => "session_requester_mismatch",
            Self::ForkSourceBusy { .. } => "fork_source_busy",
            Self::ForkTurnOutOfRange { .. } => "fork_turn_out_of_range",
            Self::ForkMessageNotEditable { .. } => "fork_message_not_editable",
            Self::ForkClientMismatch { .. } => "fork_client_mismatch",
            Self::ForkChildNotCanonical { .. } => "fork_child_not_canonical",
            Self::CrossAgentForkUnavailable => "cross_agent_fork_unavailable",
            Self::AgentUnavailable { .. } => "agent_unavailable",
            Self::ForkRelationshipsIncomplete { .. } => "fork_relationships_incomplete",
            Self::ForkRecoveryIntentRequired { .. } => "fork_recovery_intent_required",
            Self::RemoteNodeUnsupported { .. } => "remote_node_unsupported",
            Self::ResumeBlocked { .. } => "resume_blocked",
            Self::ProcessOwnershipUnverifiable { .. } => "process_ownership_unverifiable",
            Self::Passthrough(_) => "failed",
        }
    }

    pub fn next_call(&self) -> NextCall {
        match self {
            Self::FieldRequired(_)
            | Self::RevisionOutOfRange { .. }
            | Self::ForkTurnOutOfRange { .. }
            | Self::ForkMessageNotEditable { .. }
            | Self::ForkChildNotCanonical { .. }
            | Self::CrossAgentForkUnavailable
            | Self::Passthrough(_) => NextCall::VerbHelp {
                resource: "session",
                verb: "fork",
            },
            Self::IdentityRequired
            | Self::ForkClientMismatch { .. }
            | Self::RemoteNodeUnsupported { .. } => NextCall::Inspect { resource: "node" },
            Self::NotFound { .. }
            | Self::Ambiguous { .. }
            | Self::RequesterMismatch { .. } => NextCall::List {
                resource: "session",
            },
            Self::InvalidDocument { .. }
            | Self::RequesterMissing { .. }
            | Self::ForkRelationshipsIncomplete { .. }
            | Self::ForkRecoveryIntentRequired { .. } => NextCall::Inspect {
                resource: "session",
            },
            Self::ForkSourceBusy { .. }
            | Self::ResumeBlocked { .. }
            | Self::ProcessOwnershipUnverifiable { .. } => NextCall::Inspect {
                resource: "request",
            },
            Self::AgentUnavailable { .. } => NextCall::List { resource: "agent" },
        }
    }

    pub fn source_passthrough(&self) -> Option<&(dyn Error + Send + Sync + 'static)> {
        match self {
            Self::Passthrough(error) => Some(error.as_ref()),
            Self::ForkRelationshipsIncomplete { source, .. }
            | Self::ProcessOwnershipUnverifiable { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}
