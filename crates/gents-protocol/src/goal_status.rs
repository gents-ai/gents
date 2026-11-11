use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod conformance {
    use super::GoalStatus;

    #[test]
    fn goal_statuses_are_the_lean_goal_statuses() {
        gents_lean_vocab::families::goals_status_vocabulary::replay(&[
            GoalStatus::Active,
            GoalStatus::Paused,
            GoalStatus::Blocked,
            GoalStatus::UsageLimited,
            GoalStatus::BudgetLimited,
            GoalStatus::Complete,
            GoalStatus::Stopped,
        ]);
    }
}
