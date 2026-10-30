use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalDecision {
    None,
    Continue,
    Retry,
    Pause,
    Wrapup,
    AbandonWrapup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalAuditObservation {
    SameRequest,
    SameCondition,
    NewCondition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalAction {
    Pause,
    Resume,
    Stop,
    Complete,
    BlockedAudit(GoalAuditObservation),
    OperatorBlock,
    UsageLimit,
    BudgetExhausted,
    WrapupFinished,
    WrapupAbandoned,
    CleanTurn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalState {
    pub status: GoalStatus,
    pub blocked_audits: i64,
    pub wrapup_requested: bool,
    pub wrapup_completed: bool,
}

impl GoalState {
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
                ) =>
            {
                Some(Self {
                    status: if self.wrapup_requested && !self.wrapup_completed {
                        GoalStatus::BudgetLimited
                    } else {
                        GoalStatus::Active
                    },
                    blocked_audits: 0,
                    ..self
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
            GoalAction::BlockedAudit(observation) if self.status == GoalStatus::Active => {
                let blocked_audits = match observation {
                    GoalAuditObservation::SameRequest => self.blocked_audits.max(0),
                    GoalAuditObservation::SameCondition => {
                        self.blocked_audits.max(0).saturating_add(1)
                    }
                    GoalAuditObservation::NewCondition => 1,
                };
                Some(Self {
                    status: if blocked_audits >= BLOCKED_AUDIT_THRESHOLD {
                        GoalStatus::Blocked
                    } else {
                        GoalStatus::Active
                    },
                    blocked_audits,
                    ..self
                })
            }
            GoalAction::OperatorBlock if self.status == GoalStatus::Active => Some(Self {
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
            GoalAction::CleanTurn if self.status == GoalStatus::Active => Some(Self {
                blocked_audits: 0,
                ..self
            }),
            _ => None,
        }
    }
}

pub fn next_blocked_audit(
    previous_count: i64,
    previous_reason: Option<&str>,
    previous_request_id: Option<&str>,
    reason: &str,
    request_id: &str,
) -> (i64, bool) {
    let observation = if previous_request_id == Some(request_id) {
        GoalAuditObservation::SameRequest
    } else if previous_reason == Some(reason) {
        GoalAuditObservation::SameCondition
    } else {
        GoalAuditObservation::NewCondition
    };
    let state = GoalState {
        status: GoalStatus::Active,
        blocked_audits: previous_count,
        wrapup_requested: false,
        wrapup_completed: false,
    }
    .step(GoalAction::BlockedAudit(observation))
    .expect("blocked audit is legal from active");
    (state.blocked_audits, state.status == GoalStatus::Blocked)
}

#[allow(clippy::too_many_arguments)]
pub fn decide_goal_continuation(
    status: GoalStatus,
    terminal: GoalRequestTerminal,
    session_idle: bool,
    child_exists: bool,
    budget_reached: bool,
    has_activity: bool,
    request_is_wrapup: bool,
    infrastructure_retries: i64,
    wrapup_requested: bool,
    wrapup_completed: bool,
) -> GoalDecision {
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
                } else if infrastructure_retries.max(0) < MAX_INFRASTRUCTURE_RETRIES {
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
                        if infrastructure_retries.max(0) < MAX_INFRASTRUCTURE_RETRIES {
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

impl GoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::UsageLimited => "usage_limited",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
            Self::Stopped => "stopped",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "blocked" => Some(Self::Blocked),
            "usage_limited" | "usageLimited" => Some(Self::UsageLimited),
            "budget_limited" | "budgetLimited" => Some(Self::BudgetLimited),
            "complete" => Some(Self::Complete),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }

    pub fn accrues_active_time(self) -> bool {
        self == Self::Active
    }
}

fn operator_action_for_status(current: GoalStatus, target: GoalStatus) -> Option<GoalAction> {
    if current == target {
        return None;
    }
    Some(match target {
        GoalStatus::Active => GoalAction::Resume,
        GoalStatus::Paused => GoalAction::Pause,
        GoalStatus::Blocked => GoalAction::OperatorBlock,
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
