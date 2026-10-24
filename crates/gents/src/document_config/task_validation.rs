use super::error::DocumentConfigError;
use super::Task;
use crate::template::{
    catalog::{default_catalog, Site},
    check_template_vocabulary, parse_template_for_validation, TemplateError,
};

/// `TaskHooks.maxHookTimeoutSecs`.
pub(crate) const MAX_TASK_HOOK_TIMEOUT_SECS: i64 = 86_400;

impl Task {
    pub fn validate(&self) -> Result<(), DocumentConfigError> {
        if self
            .goal_objective_template
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(DocumentConfigError::TaskGoalObjectiveBlank {
                task_id: self.task_id.clone(),
            });
        }
        if self.goal_token_budget.is_some() && self.goal_objective_template.is_none() {
            return Err(DocumentConfigError::TaskGoalBudgetWithoutObjective {
                task_id: self.task_id.clone(),
            });
        }
        if self.goal_token_budget.is_some_and(|value| value <= 0) {
            return Err(DocumentConfigError::TaskGoalBudgetNotPositive {
                task_id: self.task_id.clone(),
            });
        }
        let mut hook_ids = std::collections::BTreeSet::new();
        for hook in &self.hooks {
            if !hook_ids.insert(hook.hook_id.as_str()) {
                return Err(DocumentConfigError::TaskDuplicateHook {
                    task_id: self.task_id.clone(),
                    hook_id: hook.hook_id.clone(),
                });
            }
            if hook.command.is_empty() {
                return Err(DocumentConfigError::TaskHookCommandEmpty {
                    task_id: self.task_id.clone(),
                    hook_id: hook.hook_id.clone(),
                });
            }
            if !hook
                .timeout_secs
                .is_none_or(|timeout| (1..=MAX_TASK_HOOK_TIMEOUT_SECS).contains(&timeout))
            {
                return Err(DocumentConfigError::TaskHookTimeout {
                    task_id: self.task_id.clone(),
                    hook_id: hook.hook_id.clone(),
                });
            }
        }
        let catalog = default_catalog();
        for (field, template) in std::iter::once(("prompt_template", self.prompt_template.as_str()))
            .chain(
                self.goal_objective_template
                    .as_deref()
                    .map(|value| ("goal_objective_template", value)),
            )
        {
            let references = parse_template_for_validation(template).map_err(|source| {
                DocumentConfigError::TaskTemplateParse {
                    task_id: self.task_id.clone(),
                    field,
                    source,
                }
            })?;
            check_template_vocabulary(template).map_err(|error| {
                let judgement = match &error {
                    TemplateError::UnknownName { kind: "variable", .. } => {
                        "names an unknown variable; variables start at a template root: doc, event, args, session, request, group, node or ctx (e.g. {{ doc.message }})"
                    }
                    TemplateError::UnknownName { .. } => {
                        "names an unknown filter, test or function"
                    }
                    _ => "failed to parse",
                };
                DocumentConfigError::TaskTemplateVocabulary {
                    task_id: self.task_id.clone(),
                    field,
                    judgement,
                    source: error,
                }
            })?;
            for reference in references {
                if matches!(reference.root(), Some("node" | "ctx")) {
                    let path = reference.path.join(".");
                    if !catalog.is_available_at(&path, Site::Task) {
                        return Err(DocumentConfigError::TaskTemplateUnavailableVariable {
                            task_id: self.task_id.clone(),
                            field,
                            path,
                        });
                    }
                }
            }
        }
        Ok(())
    }
}
