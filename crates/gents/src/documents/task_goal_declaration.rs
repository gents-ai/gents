use anyhow::Result;

#[doc(hidden)]
pub fn validate_task_goal_declaration(
    objective: Option<&str>,
    token_budget: Option<i64>,
) -> Result<()> {
    match objective {
        None => anyhow::ensure!(
            token_budget.is_none(),
            "goal token budget requires a goal objective template"
        ),
        Some(objective) => {
            anyhow::ensure!(
                !objective.trim().is_empty(),
                "goal objective must be non-empty"
            );
            anyhow::ensure!(
                token_budget.is_none_or(|budget| budget > 0),
                "goal token budget must be positive"
            );
        }
    }
    Ok(())
}
