use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalRequestTerminal {
    Completed,
    Failed,
    Dead,
    Interrupted,
    Superseded,
}

impl GoalRequestTerminal {
    pub fn parse(value: &str) -> Option<Self> {
        Self::from_state(RequestLifecycleState::parse(value).ok()?)
    }

    pub fn from_state(state: RequestLifecycleState) -> Option<Self> {
        match state {
            RequestLifecycleState::Completed => Some(Self::Completed),
            RequestLifecycleState::Failed => Some(Self::Failed),
            RequestLifecycleState::Dead => Some(Self::Dead),
            RequestLifecycleState::Interrupted => Some(Self::Interrupted),
            RequestLifecycleState::Superseded => Some(Self::Superseded),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalDecision {
    None,
    Continue,
    Retry,
    Pause,
    Wrapup,
    AbandonWrapup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalAction {
    Pause,
    Resume,
    Stop,
    Complete,
    /// One blocked report (Lean `Goals.Action.block`).
    Block,
    UsageLimit,
    BudgetExhausted,
    WrapupFinished,
    WrapupAbandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalState {
    pub status: GoalStatus,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
}

impl GoalState {
    /// Lean `Goals.step?`.
    pub fn step(self, action: GoalAction) -> Option<Self> {
        match action {
            GoalAction::Pause
                if matches!(self.status, GoalStatus::Active | GoalStatus::BudgetLimited) =>
            {
                Some(Self {
                    status: GoalStatus::Paused,
                    ..self
                })
            }
            GoalAction::Resume
                if matches!(
                    self.status,
                    GoalStatus::Paused | GoalStatus::Blocked | GoalStatus::UsageLimited
                ) || (self.status == GoalStatus::BudgetLimited && self.wrapup_completed) =>
            {
                let owed = self.wrapup_requested && !self.wrapup_completed;
                Some(Self {
                    status: if owed {
                        GoalStatus::BudgetLimited
                    } else {
                        GoalStatus::Active
                    },
                    wrapup_requested: owed,
                    wrapup_completed: false,
                })
            }
            GoalAction::Complete
                if !matches!(self.status, GoalStatus::Complete | GoalStatus::Stopped) =>
            {
                Some(Self {
                    status: GoalStatus::Complete,
                    wrapup_completed: true,
                    ..self
                })
            }
            GoalAction::Stop
                if !matches!(self.status, GoalStatus::Complete | GoalStatus::Stopped) =>
            {
                Some(Self {
                    status: GoalStatus::Stopped,
                    ..self
                })
            }
            GoalAction::Block if self.status == GoalStatus::Active => Some(Self {
                status: GoalStatus::Blocked,
                ..self
            }),
            GoalAction::UsageLimit if self.status == GoalStatus::Active => Some(Self {
                status: GoalStatus::UsageLimited,
                ..self
            }),
            GoalAction::BudgetExhausted
                if self.status == GoalStatus::Active && !self.wrapup_requested =>
            {
                Some(Self {
                    status: GoalStatus::BudgetLimited,
                    wrapup_requested: true,
                    ..self
                })
            }
            GoalAction::WrapupFinished | GoalAction::WrapupAbandoned
                if self.status == GoalStatus::BudgetLimited
                    && self.wrapup_requested
                    && !self.wrapup_completed =>
            {
                Some(Self {
                    wrapup_completed: true,
                    ..self
                })
            }
            _ => None,
        }
    }

    /// Lean `Goals.resumeOnMessage`: a user's message into the session of a
    /// blocked Goal resumes it.
    pub fn resume_on_message(self) -> Option<Self> {
        (self.status == GoalStatus::Blocked)
            .then(|| self.step(GoalAction::Resume))
            .flatten()
    }
}

/// Lean `Goals.budgetReached`: charged tokens reached the token budget; an
/// absent budget is lifted.
pub fn goal_budget_reached(token_budget: Option<i64>, tokens_used: i64) -> bool {
    token_budget.is_some_and(|budget| budget <= tokens_used)
}

/// Lean `Goals.initialBudget`: the budget a new Goal starts with.
pub fn initial_goal_budget(requested: Option<i64>) -> Option<i64> {
    Some(requested.unwrap_or(gents_protocol::goal::DEFAULT_GOAL_TOKEN_BUDGET))
}

pub fn decide_goal_continuation(facts: GoalContinuationFacts) -> GoalDecision {
    let GoalContinuationFacts {
        status,
        terminal,
        session_idle,
        child_exists,
        budget_reached,
        has_activity,
        request_is_wrapup,
        infrastructure_retries,
        wrapup_requested,
        wrapup_completed,
        max_infrastructure_retries,
    } = facts;
    if !session_idle || child_exists {
        return GoalDecision::None;
    }
    match status {
        GoalStatus::Active => match terminal {
            GoalRequestTerminal::Interrupted | GoalRequestTerminal::Superseded => {
                GoalDecision::Pause
            }
            GoalRequestTerminal::Failed | GoalRequestTerminal::Dead => {
                if budget_reached {
                    GoalDecision::Wrapup
                } else if infrastructure_retries.max(0) < max_infrastructure_retries {
                    GoalDecision::Retry
                } else {
                    GoalDecision::Pause
                }
            }
            GoalRequestTerminal::Completed => {
                if !has_activity {
                    GoalDecision::Pause
                } else if budget_reached {
                    GoalDecision::Wrapup
                } else {
                    GoalDecision::Continue
                }
            }
        },
        GoalStatus::BudgetLimited => {
            if !wrapup_requested || wrapup_completed {
                GoalDecision::None
            } else if !request_is_wrapup {
                GoalDecision::Wrapup
            } else {
                match terminal {
                    GoalRequestTerminal::Completed => GoalDecision::None,
                    GoalRequestTerminal::Interrupted | GoalRequestTerminal::Superseded => {
                        GoalDecision::Pause
                    }
                    GoalRequestTerminal::Failed | GoalRequestTerminal::Dead => {
                        if infrastructure_retries.max(0) < max_infrastructure_retries {
                            GoalDecision::Retry
                        } else {
                            GoalDecision::AbandonWrapup
                        }
                    }
                }
            }
        }
        GoalStatus::Paused
        | GoalStatus::Blocked
        | GoalStatus::UsageLimited
        | GoalStatus::Complete
        | GoalStatus::Stopped => GoalDecision::None,
    }
}

fn operator_action_for_status(current: GoalStatus, target: GoalStatus) -> Option<GoalAction> {
    if current == target {
        return None;
    }
    Some(match target {
        GoalStatus::Active => GoalAction::Resume,
        GoalStatus::Paused => GoalAction::Pause,
        GoalStatus::Blocked => GoalAction::Block,
        GoalStatus::UsageLimited => GoalAction::UsageLimit,
        GoalStatus::BudgetLimited => GoalAction::BudgetExhausted,
        GoalStatus::Complete => GoalAction::Complete,
        GoalStatus::Stopped => GoalAction::Stop,
    })
}

pub fn apply_operator_status_transition(state: GoalState, target: GoalStatus) -> Result<GoalState> {
    let Some(action) = operator_action_for_status(state.status, target) else {
        return Ok(state);
    };
    state.step(action).with_context(|| {
        format!(
            "illegal durable Goal transition {} -> {}",
            state.status.as_str(),
            target.as_str()
        )
    })
}
