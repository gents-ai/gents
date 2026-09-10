use super::Task;
use crate::template::{
    catalog::{default_catalog, Site},
    parse_template_for_validation,
};
use anyhow::{ensure, Context, Result};

impl Task {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.goal_objective_template
                .as_deref()
                .is_none_or(|value| !value.trim().is_empty()),
            "task {} goal_objective_template must be non-empty when set",
            self.task_id
        );
        ensure!(
            self.goal_token_budget.is_none() || self.goal_objective_template.is_some(),
            "task {} goal_token_budget requires goal_objective_template",
            self.task_id
        );
        ensure!(
            self.goal_token_budget.is_none_or(|value| value > 0),
            "task {} goal_token_budget must be positive",
            self.task_id
        );
        // TaskHooks.admitHooks validates all occurrences before any execution.
        let mut hook_ids = std::collections::BTreeSet::new();
        for hook in &self.hooks {
            ensure!(
                hook_ids.insert(hook.hook_id.as_str()),
                "task {} has duplicate hook_id {:?}",
                self.task_id,
                hook.hook_id
            );
            ensure!(
                !hook.command.is_empty(),
                "task {} hook {:?} command must be nonempty",
                self.task_id,
                hook.hook_id
            );
            ensure!(
                hook.timeout_secs.is_none_or(|timeout| timeout > 0),
                "task {} hook {:?} timeout_secs must be positive",
                self.task_id,
                hook.hook_id
            );
        }
        let catalog = default_catalog();
        for (field, template) in std::iter::once(("prompt_template", self.prompt_template.as_str()))
            .chain(
                self.goal_objective_template
                    .as_deref()
                    .map(|value| ("goal_objective_template", value)),
            )
        {
            for reference in parse_template_for_validation(template)
                .with_context(|| format!("task {} {field} failed to parse", self.task_id))?
            {
                if matches!(reference.root(), Some("node" | "ctx")) {
                    let path = reference.path.join(".");
                    ensure!(
                        catalog.is_available_at(&path, Site::Task),
                        "task {} {field} references unavailable template variable {path}",
                        self.task_id
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
