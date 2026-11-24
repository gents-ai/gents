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
    /// The Goal's pinned limit, or the default when its Task set none.
    pub max_infrastructure_retries: i64,
    pub continuation_sequence: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
    /// What the working Agent said it needs when it reported the Goal
    /// blocked; shown to the user, never read by a decision.
    pub blocked_reason: Option<String>,
    pub last_failure: Option<String>,
    pub completion_evidence: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// The limit a Goal pins when it is created: a Task's
/// `goal_max_infrastructure_retries`. Unset is the controller default. Goals
/// the model creates with goal tools, and Goals a person starts, have no Task
/// and keep the default. The creation claim records it beside the token
/// budget, so a replayed creation with another limit conflicts
/// (`GoalAutomation.decideCreate`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GoalLimits {
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
    blocked_reason
    last_continued_from_request_id
    continuation_sequence
    wrapup_requested
    wrapup_completed
    infrastructure_retry_count
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

/// Lean `GoalStopPolicy.OnBlocked`: what one blocked report does, set per Goal
/// by its user in the Goal's stop policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum OnBlocked {
    /// The report pauses the Goal as `blocked`.
    #[default]
    StopFirst,
    /// The first report gets one confirmation turn; a report in that turn
    /// pauses the Goal.
    ConfirmOnce,
    /// The report is kept for the user and the Goal continues; the
    /// no-progress pause and any budget still bound it.
    KeepTrying,
}

impl OnBlocked {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StopFirst => "stop_first",
            Self::ConfirmOnce => "confirm_once",
            Self::KeepTrying => "keep_trying",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stop_first" => Some(Self::StopFirst),
            "confirm_once" => Some(Self::ConfirmOnce),
            "keep_trying" => Some(Self::KeepTrying),
            _ => None,
        }
    }
}

/// A Goal's stop policy as its user set it. A Goal without one runs under
/// the default: `stop_first`, no ceiling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct GoalStopPolicy {
    pub on_blocked: OnBlocked,
    pub token_ceiling: Option<i64>,
    pub turn_ceiling: Option<i64>,
    pub lifted: bool,
}
