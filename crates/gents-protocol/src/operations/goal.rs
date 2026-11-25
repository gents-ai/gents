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
    RuntimeOnly,
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

/// What a person supplies to change a Goal's stop policy. `Some(None)`
/// removes a ceiling; an absent field keeps what the policy holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalStopPolicyInput {
    pub on_blocked: Option<crate::goal::OnBlocked>,
    pub token_ceiling: Option<Option<i64>>,
    pub turn_ceiling: Option<Option<i64>>,
    pub lifted: Option<bool>,
}

impl GoalStopPolicyInput {
    /// The policy after this change, from the one stored (the default when the
    /// Goal has none).
    pub fn apply(
        &self,
        stored: &crate::goal::GoalStopPolicy,
    ) -> Result<crate::goal::GoalStopPolicy, GoalOperationError> {
        if self == &Self::default() {
            return Err(GoalOperationError::NothingToChange);
        }
        let positive = |value: Option<i64>| value.is_none_or(|value| value > 0);
        if !positive(self.token_ceiling.flatten()) || !positive(self.turn_ceiling.flatten()) {
            return Err(GoalOperationError::InvalidCeiling);
        }
        Ok(crate::goal::GoalStopPolicy {
            on_blocked: self.on_blocked.unwrap_or(stored.on_blocked),
            token_ceiling: self.token_ceiling.unwrap_or(stored.token_ceiling),
            turn_ceiling: self.turn_ceiling.unwrap_or(stored.turn_ceiling),
            lifted: self.lifted.unwrap_or(stored.lifted),
        })
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
    #[error("a goal policy ceiling must be a positive whole number")]
    InvalidCeiling,
    #[error("your user asked to confirm a block once: the report is recorded and the goal stays active. Check once more that you cannot go on without the user; if you still cannot, report blocked again and the goal pauses for the user")]
    ConfirmBlocked,
    #[error("your user asked you to keep trying: the report is recorded for them and the goal stays active. Continue with another approach; the goal pauses on its own if turns stop making progress")]
    KeepTrying,
    #[error("provide objective, token_budget, status or auto_resume to change the goal")]
    NothingToChange,
    #[error("usage_limited and budget_limited are set by the runtime when a limit is reached")]
    RuntimeOnly,
    #[error("only a person may pause, stop or choose reset resume")]
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
    #[error("blocked requires a non-empty reason saying what you need from the user")]
    ReasonRequired,
    #[error("{target} is not legal from the goal's current status {status}")]
    IllegalTransition {
        target: &'static str,
        status: &'static str,
    },
    /// Another owner's failure (request staging, the store), unchanged.
    #[error(transparent)]
    Passthrough(crate::error_chain::PassedThrough),
}

/// How a Goal in `status` runs again, as the text a person or model reads.
pub fn resume_path(status: GoalStatus) -> &'static str {
    match status {
        GoalStatus::Active => "it is already active",
        GoalStatus::Blocked => {
            "it is waiting for its user: send a message in the session to answer it, or resume it with `gents automation goal resume-request --session SESSION --from REQUEST_ID` (REQUEST_ID is the terminal latest predecessor request)"
        }
        GoalStatus::Paused | GoalStatus::UsageLimited => {
            "an operator resumes it with `gents automation goal resume-request --session SESSION --from REQUEST_ID` (REQUEST_ID is the terminal latest predecessor request)"
        }
        GoalStatus::Complete => "a complete goal is terminal and has no resume path",
        GoalStatus::Stopped => "the goal was removed by its operator and cannot be resumed",
        GoalStatus::BudgetLimited => {
            "it reached its token budget: raise it with `gents automation goal set --session SESSION --token-budget N` or lift it with `--clear-token-budget`, then resume it with `gents automation goal resume-request --session SESSION --from REQUEST_ID`"
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
            | Self::RuntimeOnly
            | Self::PersonOnly
            | Self::FieldsNeedKeep
            | Self::ObligationsUnmet(_)
            | Self::ReasonNotAccepted
            | Self::ReasonRequired
            | Self::Passthrough(_) => NextCall::VerbHelp {
                resource: "goal",
                verb: "set",
            },
            Self::ConfirmBlocked | Self::KeepTrying => NextCall::Inspect { resource: "goal" },
            Self::InvalidCeiling => NextCall::VerbHelp {
                resource: "goal",
                verb: "policy",
            },
        }
    }

    pub fn passthrough(&self) -> Option<&(dyn std::error::Error + Send + Sync + 'static)> {
        match self {
            Self::Passthrough(error) => Some(error.get()),
            _ => None,
        }
    }
}
