use serde::{Deserialize, Serialize};

use super::NextCall;
use crate::goal_control::GoalRevision;
use crate::goal_status::GoalStatus;

/// Lean `GoalAutomation.Operations.Actor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalActor {
    Person,
    WorkingAgent,
    OtherAgent,
}

/// Lean `GoalAutomation.Operations.Session`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalSession {
    Missing,
    Idle,
    Busy,
}

/// Lean `GoalAutomation.Operations.Existing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalExisting {
    None,
    Live,
    Finished,
}

impl GoalExisting {
    pub fn of(status: Option<GoalStatus>) -> Self {
        match status {
            None => Self::None,
            Some(GoalStatus::Complete | GoalStatus::Stopped) => Self::Finished,
            Some(_) => Self::Live,
        }
    }
}

/// Lean `GoalAutomation.Operations.Claim`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalClaim {
    None,
    Same,
    Different,
}

/// Lean `GoalAutomation.Operations.StartDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStartDecision {
    Fresh,
    Idempotent,
    Replace,
    Denied,
    SessionMissing,
    Busy,
    LiveGoal,
    Stale,
    Conflict,
}

/// Lean `GoalAutomation.Operations.Target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalTarget {
    Keep,
    Activate,
    Pause,
    Stop,
    Complete,
    Block,
    UsageLimited,
    BudgetLimited,
}

impl GoalTarget {
    pub fn of(status: GoalStatus) -> Self {
        match status {
            GoalStatus::Active => Self::Activate,
            GoalStatus::Paused => Self::Pause,
            GoalStatus::Blocked => Self::Block,
            GoalStatus::UsageLimited => Self::UsageLimited,
            GoalStatus::BudgetLimited => Self::BudgetLimited,
            GoalStatus::Complete => Self::Complete,
            GoalStatus::Stopped => Self::Stop,
        }
    }
}

/// Lean `GoalAutomation.Operations.FieldChange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalFieldChange {
    None,
    Edit,
    ClearBudget,
}

/// Lean `GoalAutomation.Operations.Reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalReason {
    Absent,
    Blank,
    Given,
}

/// Lean `GoalAutomation.Operations.SetDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalSetDecision {
    Apply,
    Denied,
    ReasonNotAccepted,
    NothingToChange,
    PersonOnly,
    FieldsNeedKeep,
    NotEditable,
    ResumeRequired,
    Illegal,
    ReasonRequired,
    ObligationsUnmet,
}

/// The Goal the caller observed before asking. A set or start whose
/// observation is no longer current is refused as stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalExpected {
    /// The Goal's assignment identity.
    Goal(String),
    /// The Goal document's composite revision.
    Revision(GoalRevision),
    /// The working Agent's assignment: its Goal, status and continuation.
    Assignment {
        goal_doc_id: String,
        goal_id: String,
        assignment_root_request_doc_id: Option<String>,
        status: GoalStatus,
        continuation_sequence: i64,
    },
}

/// What a caller supplies to set a session's Goal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalSetInput {
    pub objective: Option<String>,
    /// `Some(None)` removes the budget.
    pub token_budget: Option<Option<i64>>,
    pub status: Option<GoalStatus>,
    /// Completion evidence with `complete`, the repeated condition with
    /// `blocked`.
    pub reason: Option<String>,
    pub auto_resume: Option<bool>,
    pub expected: Option<GoalExpected>,
}

impl GoalSetInput {
    pub fn target(&self) -> GoalTarget {
        self.status.map(GoalTarget::of).unwrap_or(GoalTarget::Keep)
    }

    pub fn reason(&self) -> GoalReason {
        match self.reason.as_deref().map(str::trim) {
            None => GoalReason::Absent,
            Some("") => GoalReason::Blank,
            Some(_) => GoalReason::Given,
        }
    }

    pub fn field_change(&self) -> GoalFieldChange {
        match (&self.objective, self.token_budget) {
            (_, Some(None)) => GoalFieldChange::ClearBudget,
            (None, None) => GoalFieldChange::None,
            _ => GoalFieldChange::Edit,
        }
    }
}

/// What a caller supplies to start a session's Goal; the client signs the
/// first request that carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalStartInput {
    pub session_id: String,
    pub objective: String,
    pub token_budget: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum GoalOperationError {
    #[error("only a person or the session's working Agent may change its Goal")]
    Denied,
    #[error("session {0} is not available; create the session before starting its goal")]
    SessionMissing(String),
    #[error("session {0} has no goal")]
    NoGoal(String),
    #[error("wait for this session to finish before starting a goal")]
    Busy,
    #[error("only a completed or removed goal can be replaced")]
    LiveGoal,
    #[error("goal changed; refresh before changing it")]
    Stale,
    #[error("the current session already has a goal with a different objective or budget")]
    Conflict,
    #[error("goal objective must be non-empty")]
    InvalidObjective,
    #[error("goal token budget must be positive")]
    InvalidBudget,
    #[error("provide objective, token_budget, status or auto_resume to change the goal")]
    NothingToChange,
    #[error("only a person may pause, stop, mark limits, clear the budget or choose reset resume")]
    PersonOnly,
    #[error("objective and token_budget can only be edited while status is active")]
    FieldsNeedKeep,
    #[error("goal edits require an editable goal; the current goal status is {} and {}", .0.as_str(), resume_path(*.0))]
    NotEditable(GoalStatus),
    #[error("reactivating a goal is a resume, not a set; the current goal status is {} and {}", .0.as_str(), resume_path(*.0))]
    ResumeRequired(GoalStatus),
    #[error("{0}")]
    ObligationsUnmet(String),
    #[error("the goal's first request is not valid: {0}")]
    FirstRequestInvalid(&'static str),
    #[error("the goal's first request does not match its session and author")]
    FirstRequestMismatch,
    #[error("reason is only accepted with status complete or blocked")]
    ReasonNotAccepted,
    #[error("blocked requires a non-empty reason identifying the repeated condition")]
    ReasonRequired,
    #[error("{target} is not legal from the goal's current status {status}")]
    IllegalTransition {
        target: &'static str,
        status: &'static str,
    },
    /// Another owner's failure (request staging, the store), unchanged.
    #[error(transparent)]
    Passthrough(Box<dyn std::error::Error + Send + Sync>),
}

/// How a Goal in `status` runs again, as the text a person or model reads.
pub fn resume_path(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "it is already active",
        GoalStatus::Blocked | GoalStatus::Paused | GoalStatus::UsageLimited => {
            "an operator resumes it with `gents automation goal resume-request --session SESSION --from REQUEST_ID` (REQUEST_ID is the terminal latest predecessor request)"
        }
        GoalStatus::Complete => "a complete goal is terminal and has no resume path",
        GoalStatus::Stopped => "the goal was removed by its operator and cannot be resumed",
        GoalStatus::BudgetLimited => {
            "a budget_limited goal cannot be reactivated and has no resume path"
        }
    }
}

impl GoalOperationError {
    /// The call that lets the caller correct this failure.
    pub fn recovery(&self) -> NextCall {
        match self {
            Self::Denied | Self::SessionMissing(_) => NextCall::List {
                resource: "session",
            },
            Self::NoGoal(_) => NextCall::Verb {
                resource: "goal",
                verb: "start",
            },
            Self::FirstRequestInvalid(_) | Self::FirstRequestMismatch => NextCall::VerbHelp {
                resource: "goal",
                verb: "start",
            },
            Self::Busy | Self::LiveGoal | Self::Stale | Self::Conflict | Self::NotEditable(_) => {
                NextCall::Inspect { resource: "goal" }
            }
            Self::ResumeRequired(_) => NextCall::VerbHelp {
                resource: "goal",
                verb: "resume-request",
            },
            Self::IllegalTransition { .. } => NextCall::Inspect { resource: "goal" },
            Self::InvalidObjective
            | Self::InvalidBudget
            | Self::NothingToChange
            | Self::PersonOnly
            | Self::FieldsNeedKeep
            | Self::ObligationsUnmet(_)
            | Self::ReasonNotAccepted
            | Self::ReasonRequired
            | Self::Passthrough(_) => NextCall::VerbHelp {
                resource: "goal",
                verb: "set",
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
