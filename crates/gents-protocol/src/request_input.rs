//! Typed invocation inputs. Configuration remains on the agent/context/profile;
//! these values describe one request and never widen its configured capabilities.

use serde::{Deserialize, Serialize};

use crate::session::SessionTitle;

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
    /// Existing queue/steering/wake input, subject to request admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<RequestQueue>,
    /// Original facts of an authenticated runtime-issued goal continuation.
    /// Goal/parent identities remain in the existing signed request lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_continuation: Option<GoalContinuationInput>,
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
