use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct GoalSnapshot {
    pub goal_id: String,
    pub session_id: String,
    pub node_did: String,
    pub objective: String,
    pub status: String,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub active_time_seconds: i64,
    pub consecutive_blocked_audits: i64,
    /// The Goal's pinned threshold, or the default when its Task set none.
    pub blocked_audit_threshold: i64,
    /// The Goal's pinned limit, or the default when its Task set none.
    pub max_infrastructure_retries: i64,
    pub continuation_sequence: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
    pub last_blocked_reason: Option<String>,
    pub last_failure: Option<String>,
    pub completion_evidence: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// The limits a Goal pins when it is created: a Task's
/// `goal_blocked_audit_threshold` and `goal_max_infrastructure_retries`.
/// Unset values are the controller defaults. Goals the model creates with goal
/// tools, and Goals a person starts, have no Task and keep the defaults. The
/// creation claim records them beside the token budget, so a replayed creation
/// with other limits conflicts (`GoalAutomation.decideCreate`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GoalLimits {
    pub blocked_audit_threshold: Option<i64>,
    pub max_infrastructure_retries: Option<i64>,
}

pub const GOAL_FIELDS: &str = r#"
    _docID
    goal_id
    creation_key
    session_id
    node_did
    objective
    status
    token_budget
    tokens_used
    token_usage_baseline
    assignment_root_request_doc_id
    active_time_seconds
    active_started_at
    consecutive_blocked_audits
    last_blocked_request_id
    last_blocked_reason
    last_continued_from_request_id
    continuation_sequence
    wrapup_requested
    wrapup_completed
    infrastructure_retry_count
    blocked_audit_threshold
    max_infrastructure_retries
    last_failure
    completion_evidence
    auto_resume_at_reset
    created_at
    updated_at
"#;

pub fn deterministic_goal_id(node_did: &str, session_id: &str) -> String {
    format!("{}:{node_did}:{session_id}", node_did.len())
}

pub fn deterministic_goal_creation_key(node_did: &str, session_id: &str) -> String {
    format!("goal-create:{}:{node_did}:{session_id}", node_did.len())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalClaimFence {
    Allowed,
    Retired,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestAssignmentScope {
    Unassigned,
    Unresolved { entry_doc_id: String },
    Referenced(crate::request_input::GoalAssignmentInput),
}
