use serde::{Deserialize, Serialize};

/// One source document delivered to one owner's trigger. Human-readable trigger
/// IDs are unique only within an owner; source IDs are scoped by collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireIdentity {
    pub owner_did: String,
    pub trigger_id: String,
    pub source_collection: String,
    pub source_doc_id: String,
}

impl FireIdentity {
    /// Character-count framing is shared with `Triggers.Durable.Identity`.
    /// Every component, including the owner, participates in admission identity.
    pub fn fire_key(&self) -> String {
        [
            &self.owner_did,
            &self.trigger_id,
            &self.source_collection,
            &self.source_doc_id,
        ]
        .into_iter()
        .map(|value| format!("{}:{value}", value.chars().count()))
        .collect()
    }

    pub fn request_id(&self) -> String {
        format!("trigger-request:{}", self.fire_key())
    }

    pub fn session_id(&self) -> String {
        format!("trigger-session:{}", self.fire_key())
    }

    pub fn outcome_id(&self) -> String {
        format!("outcome:{}", self.fire_key())
    }
}

/// Admission fields are immutable and committed atomically with their AgentRequest.
/// The sole mutable claim marker is set with the deferred Goal assignment; it
/// records which assignment owns the outcome, not a second execution lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerFire {
    pub fire_key: String,
    #[serde(flatten)]
    pub identity: FireIdentity,
    pub task_id: String,
    pub request_id: String,
    pub session_id: String,
    pub goal_id: Option<String>,
    pub goal_objective: Option<String>,
    pub goal_token_budget: Option<i64>,
    /// Only the winning request claim may mark the queued assignment applied,
    /// in the transaction that updates its Goal and acquires execution.
    #[serde(default)]
    pub goal_assignment_applied: bool,
    pub emit_outcome: bool,
    pub queued_serial: bool,
    pub source_handoff_id: Option<String>,
    pub reply_session_id: Option<String>,
    pub shard_id: Option<String>,
    pub attempt: Option<i64>,
    pub created_at: String,
}

/// Immutable terminal observation for an opted-in fire. A goal-backed fire
/// observes its Goal assignment after claim; before claim, request termination
/// ends the handoff. Ordinary continuing request boundaries do not end a Goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FireOutcome {
    pub handoff_id: String,
    pub fire_key: String,
    #[serde(flatten)]
    pub identity: FireIdentity,
    pub request_id: String,
    pub session_id: String,
    pub goal_id: Option<String>,
    pub terminal_state: String,
    pub reason: String,
    pub source_handoff_id: String,
    pub reply_session_id: Option<String>,
    pub shard_id: Option<String>,
    pub attempt: Option<i64>,
    pub created_at: String,
}

/// Position in DefraDB's receiving-node document-arrival journal. Each
/// per-document consumer (trigger or callback binding) checkpoints
/// independently in a non-branchable runtime document. Advancement requires a
/// complete prefix of admitted or explicitly excluded arrivals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSourceCursor {
    pub cursor_key: String,
    pub owner_did: String,
    pub consumer: crate::event_delivery::EventConsumer,
    pub source_collection: String,
    pub after: String,
}
