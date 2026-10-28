//! Typed invocation inputs. Configuration remains on the agent/context/profile;
//! these values describe one request and never widen its configured capabilities.

use serde::{Deserialize, Serialize};

use crate::session::SessionTitle;

mod attachment;
pub use attachment::*;
mod captured_context;
pub use captured_context::*;
#[cfg(test)]
mod tests;

pub const REQUEST_INPUT_CONTRACT_VERSION: u32 = 2;
pub const STATUS_INPUT_CONTRACT_FIELD: &str = "request_input_contract_version";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestInput {
    /// Explicit activation within AgentContext.skill_ids, not an extra skill grant.
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub selected_skill_ids: Vec<String>,
    /// Optional invocation directory, validated by existing host/workspace policy.
    /// Absence uses the configured or workspace-bound directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Creation intent consumed only when the session is first materialized.
    /// Later title changes belong to the session title owner, not this input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_title: Option<SessionTitle>,
    /// Source session carried by a signed session message because its receiver
    /// may not hold the parent request. Consumed only when creating a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<crate::session::SessionReference>,
    /// Existing queue/steering/wake input, subject to request admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<RequestQueue>,
    /// Original facts of an authenticated runtime-issued goal continuation.
    /// Goal/parent identities remain in the existing signed request lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_continuation: Option<GoalContinuationInput>,
    /// Signed assignment attribution; authorization remains with admission and ACP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_assignment: Option<GoalAssignmentInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_reply: Option<crate::mailbox_question::MailboxQuestionReply>,
    /// Submitted snapshots, distinct from authored prose and configuration grants.
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub captured_context: Vec<CapturedContext>,
    #[serde(
        default,
        deserialize_with = "crate::row::deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub attachments: Vec<RequestAttachment>,
}

impl RequestInput {
    /// The serving runtime must advertise support before a client publishes
    /// typed captures. This observation grants no request or workspace authority.
    pub fn captured_input_compatible(&self, runtime_version: Option<u64>) -> bool {
        (self.attachments.is_empty() || runtime_version == Some(2))
            && (self.captured_context.is_empty() || matches!(runtime_version, Some(1 | 2)))
    }

    pub fn goal_assignment_compatible(&self, runtime_version: Option<u64>) -> bool {
        self.goal_assignment.is_none()
            || runtime_version == Some(u64::from(crate::goal_control::GOAL_STOP_CONTRACT_VERSION))
    }

    pub fn question_reply_compatible(&self, runtime_version: Option<u64>) -> bool {
        self.question_reply.as_ref().is_none_or(|reply| {
            let required = u64::from(reply.question_version.unwrap_or(2));
            runtime_version.is_some_and(|version| {
                required >= 2
                    && required <= version
                    && version <= u64::from(crate::mailbox_question::MAILBOX_QUESTION_VERSION)
            })
        })
    }

    pub fn has_authored_input(&self, prose: &str) -> bool {
        !prose.trim().is_empty()
            || !self.captured_context.is_empty()
            || !self.attachments.is_empty()
    }

    /// A retry or queue edit is a new physical request in the same session.
    /// Resolve an opening request's self reference before signing the successor.
    pub fn reissued(&self, source_doc_id: &str) -> Self {
        Self {
            goal_assignment: self
                .goal_assignment
                .as_ref()
                .map(|reference| reference.continuation(source_doc_id)),
            ..self.clone()
        }
    }

    pub fn for_fresh_session(&self) -> Self {
        Self {
            started_by: None,
            question_reply: None,
            queue: None,
            goal_continuation: None,
            goal_assignment: None,
            ..self.clone()
        }
    }

    pub fn authored_message(&self, prose: &str) -> anyhow::Result<crate::message::Message> {
        use crate::message::{Message, UserContent};
        let mut content = Vec::with_capacity(self.captured_context.len() + 1);
        if !prose.is_empty() || (self.captured_context.is_empty() && self.attachments.is_empty()) {
            content.push(UserContent::text(prose));
        }
        for context in &self.captured_context {
            content.push(UserContent::Document(context.to_document()?));
        }
        content.extend(self.attachments.iter().map(RequestAttachment::to_content));
        Ok(Message::User { content })
    }

    /// Runtime continuations retain the invocation directory; the host still
    /// validates it against the current workspace and configured tool ceiling.
    pub fn continuation(
        &self,
        queue: RequestQueue,
        goal_continuation: Option<GoalContinuationInput>,
        parent_doc_id: &str,
    ) -> Self {
        Self {
            cwd: self.cwd.clone(),
            queue: Some(queue),
            goal_continuation,
            goal_assignment: self
                .goal_assignment
                .as_ref()
                .map(|reference| reference.continuation(parent_doc_id)),
            ..Self::default()
        }
    }
}

/// The opening invocation uses no root to denote itself without a recursive
/// document hash. Later invocations name the authenticated opening document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalAssignmentInput {
    pub goal_id: String,
    #[serde(default)]
    pub root_request_doc_id: Option<String>,
}

impl GoalAssignmentInput {
    pub fn continuation(&self, parent_doc_id: &str) -> Self {
        Self {
            goal_id: self.goal_id.clone(),
            root_request_doc_id: Some(
                self.root_request_doc_id
                    .as_deref()
                    .unwrap_or(parent_doc_id)
                    .to_owned(),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestQueue {
    pub source: QueueSource,
    pub policy: QueuePolicy,
    #[serde(default, skip_serializing_if = "QueueDelivery::is_queue")]
    pub delivery: QueueDelivery,
    /// A replacement retains an existing queue slot. Admission checks the
    /// predecessor's terminal replacement link before accepting this claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<QueuePosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_after_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_request_id: Option<String>,
    /// Existing durable wake format marker; not a configuration version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_completion_wake_version: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum QueueDelivery {
    #[default]
    Queue,
    Steer,
}

impl QueueDelivery {
    pub fn is_queue(&self) -> bool {
        matches!(self, Self::Queue)
    }
}

/// Native first-arrival identity, independent of replacement issuance time.
/// The slot is immutable across edits; the predecessor is the physical request
/// atomically superseded when this replacement was signed and published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueuePosition {
    pub slot_request_doc_id: String,
    pub replaces_request_doc_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueSource {
    User,
    BackgroundCompletion,
    Steering,
    Goal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuePolicy {
    Append,
    Coalesce,
}

/// Immutable issuance facts; mutable Goal state cannot reconstruct old requests.
/// Presence grants no goal authority: admission verifies the existing local-control
/// receipt, goal/parent lineage, and deterministic continuation identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalContinuationInput {
    /// Positive original continuation sequence, validated by the goal owner.
    pub sequence: i64,
    pub wrapup: bool,
}
