use anyhow::Result;
use gents_protocol::goal::GoalLimits;

/// Lean `GoalAutomation.validTaskGoalDeclaration`.
#[doc(hidden)]
pub fn validate_task_goal_declaration(
    objective: Option<&str>,
    token_budget: Option<i64>,
    limits: GoalLimits,
) -> Result<()> {
    match objective {
        None => {
            anyhow::ensure!(
                token_budget.is_none(),
                "goal token budget requires a goal objective template"
            );
            anyhow::ensure!(
                limits.max_infrastructure_retries.is_none(),
                "goal infrastructure retry limit requires a goal objective template"
            );
        }
        Some(objective) => {
            anyhow::ensure!(
                !objective.trim().is_empty(),
                "goal objective must be non-empty"
            );
            anyhow::ensure!(
                token_budget.is_none_or(|budget| budget > 0),
                "goal token budget must be positive"
            );
            anyhow::ensure!(
                limits
                    .max_infrastructure_retries
                    .is_none_or(|retries| retries >= 0),
                "goal infrastructure retry limit must not be negative"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod pushdown_tests;
