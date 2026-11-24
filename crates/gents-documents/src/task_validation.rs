use super::{Task, TaskHookPhase};
use crate::template::{
    catalog::{default_catalog, Site},
    check_template_vocabulary, parse_template_for_validation, TemplateError,
};
use anyhow::{ensure, Context, Result};

/// `TaskHooks.maxHookTimeoutSecs`.
pub(crate) const MAX_TASK_HOOK_TIMEOUT_SECS: i64 = 86_400;

impl Task {
    pub fn validate(&self) -> Result<()> {
        if let Some(command) = &self.command {
            return self.validate_command(command);
        }
        ensure!(
            !self.agent_did.trim().is_empty(),
            "task {} needs an agent (agent_did and prompt_template) or a command",
            self.task_id
        );
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
        ensure!(
            self.goal_max_infrastructure_retries.is_none()
                || self.goal_objective_template.is_some(),
            "task {} goal_max_infrastructure_retries requires goal_objective_template",
            self.task_id
        );
        ensure!(
            self.goal_max_infrastructure_retries
                .is_none_or(|value| value >= 0),
            "task {} goal_max_infrastructure_retries must not be negative",
            self.task_id
        );
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
                hook.timeout_secs
                    .is_none_or(|timeout| (1..=MAX_TASK_HOOK_TIMEOUT_SECS).contains(&timeout)),
                "task {} hook {:?} timeout_secs must be between 1 and {MAX_TASK_HOOK_TIMEOUT_SECS}",
                self.task_id,
                hook.hook_id
            );
            ensure!(
                hook.skip_exit_codes.is_empty()
                    || (hook.phase == TaskHookPhase::Before
                        && hook.skip_exit_codes.iter().all(|code| (1..=255).contains(code))),
                "task {} hook {:?} skip_exit_codes require a before hook and values between 1 and 255",
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
            let references = parse_template_for_validation(template)
                .with_context(|| format!("task {} {field} failed to parse", self.task_id))?;
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
                anyhow::Error::new(error)
                    .context(format!("task {} {field} {judgement}", self.task_id))
            })?;
            for reference in references {
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

    /// A command Task runs its command and nothing else: the fields that shape
    /// an Agent turn or its outcome have no meaning without one.
    fn validate_command(&self, command: &super::TaskCommand) -> Result<()> {
        let agent_fields = [
            ("agent_did", !self.agent_did.is_empty()),
            ("prompt_template", !self.prompt_template.is_empty()),
            (
                "goal_objective_template",
                self.goal_objective_template.is_some(),
            ),
            ("goal_token_budget", self.goal_token_budget.is_some()),
            ("emit_outcome", self.emit_outcome),
            ("outcome_readers", !self.outcome_readers.is_empty()),
            ("hooks", !self.hooks.is_empty()),
            ("output_schema_ref", self.output_schema_ref.is_some()),
        ];
        let set = agent_fields
            .iter()
            .filter_map(|(field, set)| set.then_some(*field))
            .collect::<Vec<_>>();
        ensure!(
            set.is_empty(),
            "task {} has a command and {}; a Task runs either an agent or a command, so drop {}",
            self.task_id,
            set.join(", "),
            if set.len() == 1 { "it" } else { "them" }
        );
        ensure!(
            command
                .argv
                .first()
                .is_some_and(|program| !program.trim().is_empty()),
            "task {} command argv must start with a nonempty program",
            self.task_id
        );
        ensure!(
            command
                .timeout_secs
                .is_none_or(|timeout| (1..=MAX_TASK_HOOK_TIMEOUT_SECS).contains(&timeout)),
            "task {} command timeout_secs must be between 1 and {MAX_TASK_HOOK_TIMEOUT_SECS}",
            self.task_id
        );
        Ok(())
    }
}
