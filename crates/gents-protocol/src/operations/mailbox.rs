use serde::{Deserialize, Serialize};

use super::agent::NextCall;
use crate::mailbox_question::{
    MailboxQuestionAnswer, MailboxQuestionError, MailboxQuestionReply, MailboxQuestionReplyError,
};
use crate::row::MailboxItemRow;

/// Lean `Mailbox.Operations.ListScope`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxListScope {
    #[default]
    Waiting,
    All,
}

/// What a list asks for. ACP bounds the rows; `node_did` and `requester_did`
/// only narrow them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailboxListInput {
    pub scope: MailboxListScope,
    pub node_did: Option<String>,
    pub requester_did: Option<String>,
}

/// Lean `Mailbox.Operations.DismissDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxDismissDecision {
    AlreadyClosed,
    NeedsAnswer,
    Dismissed,
}

/// Lean `Mailbox.Operations.ReplyKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxReplyKind {
    Text,
    Answer,
}

/// Lean `Mailbox.Operations.ReplyDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxReplyDecision {
    Admitted,
    NotOpen,
    NotReplyable,
    NotAQuestion,
    RouteDiffers,
    QuestionChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxReply {
    Text(String),
    Answer(MailboxQuestionAnswer),
}

impl MailboxReply {
    pub fn kind(&self) -> MailboxReplyKind {
        match self {
            Self::Text(_) => MailboxReplyKind::Text,
            Self::Answer(_) => MailboxReplyKind::Answer,
        }
    }
}

/// The route the caller addressed, when its surface lets a person choose one.
/// Each stated part must equal the item's own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailboxReplyRoute {
    pub session_id: Option<String>,
    pub node_did: Option<String>,
    pub agent_did: Option<String>,
}

/// The item fields a reply is decided from, as any client's copy holds them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxReplySource<'a> {
    pub doc_id: &'a str,
    pub status: &'a str,
    pub kind: &'a str,
    pub action: &'a str,
    pub title: &'a str,
    pub summary: Option<&'a str>,
    pub payload: Option<&'a str>,
    pub request_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub target_node_did: &'a str,
    pub target_agent_did: &'a str,
}

impl<'a> From<&'a MailboxItemRow> for MailboxReplySource<'a> {
    fn from(row: &'a MailboxItemRow) -> Self {
        Self {
            doc_id: &row.doc_id,
            status: &row.status,
            kind: &row.kind,
            action: &row.action,
            title: &row.title,
            summary: row.summary.as_deref(),
            payload: row.payload.as_deref(),
            request_id: row.request_id.as_deref(),
            session_id: row.session_id.as_deref(),
            target_node_did: &row.target_node_did,
            target_agent_did: &row.target_agent_did,
        }
    }
}

/// An admitted reply: the request the caller sends, on the item's route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedMailboxReply {
    pub mailbox_item_id: String,
    pub node_did: String,
    pub agent_did: String,
    pub session_id: Option<String>,
    pub content: String,
    pub question_reply: Option<MailboxQuestionReply>,
}

#[derive(Debug, thiserror::Error)]
pub enum MailboxOperationError {
    #[error("MailboxItem not found")]
    ItemMissing,
    #[error("MailboxItem _docID lookup returned more than one row")]
    ItemAmbiguous,
    #[error("MailboxItem has unknown status {0:?}")]
    UnknownStatus(String),
    #[error("MailboxItem has unknown action {0:?}")]
    UnknownAction(String),
    #[error("MailboxItem is no longer open")]
    NotOpen,
    #[error("MailboxItem does not accept request replies")]
    NotReplyable,
    #[error("MailboxItem is not a question")]
    NotAQuestion,
    #[error("a reply goes to the session and agent that filed the item")]
    RouteDiffers,
    #[error("the question changed; reload its current prompt before replying")]
    QuestionChanged,
    #[error("reply to this question through its original item; use an answer or explicit Skip when optional")]
    NeedsAnswer,
    #[error("question payload is unavailable")]
    QuestionPayloadMissing,
    #[error("mailbox dismissal requires an authenticated actor")]
    ActorRequired,
    #[error("mailbox dismissal did not consume an open item")]
    DismissalLost,
    #[error(transparent)]
    Question(#[from] MailboxQuestionError),
    #[error(transparent)]
    QuestionReply(#[from] MailboxQuestionReplyError),
    /// The store's failure, unchanged.
    #[error(transparent)]
    Passthrough(Box<dyn std::error::Error + Send + Sync>),
}

impl MailboxOperationError {
    /// The call that lets the caller correct this failure.
    pub fn recovery(&self) -> NextCall {
        match self {
            Self::ItemMissing
            | Self::ItemAmbiguous
            | Self::NotOpen
            | Self::UnknownStatus(_)
            | Self::UnknownAction(_)
            | Self::DismissalLost => NextCall::List {
                resource: "mailbox",
            },
            Self::NotReplyable | Self::RouteDiffers | Self::QuestionPayloadMissing => {
                NextCall::Inspect {
                    resource: "mailbox",
                }
            }
            Self::NotAQuestion | Self::NeedsAnswer => NextCall::VerbHelp {
                resource: "mailbox",
                verb: "reply",
            },
            Self::QuestionChanged | Self::Question(_) | Self::QuestionReply(_) => {
                NextCall::Inspect {
                    resource: "mailbox",
                }
            }
            Self::ActorRequired | Self::Passthrough(_) => NextCall::ResourceHelp {
                resource: "mailbox",
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

impl MailboxReplyDecision {
    pub fn admit(self) -> Result<(), MailboxOperationError> {
        match self {
            Self::Admitted => Ok(()),
            Self::NotOpen => Err(MailboxOperationError::NotOpen),
            Self::NotReplyable => Err(MailboxOperationError::NotReplyable),
            Self::NotAQuestion => Err(MailboxOperationError::NotAQuestion),
            Self::RouteDiffers => Err(MailboxOperationError::RouteDiffers),
            Self::QuestionChanged => Err(MailboxOperationError::QuestionChanged),
        }
    }
}
