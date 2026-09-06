use std::collections::BTreeSet;

use gents::template::catalog::{default_catalog, Site};
use gents::{parse_template_for_validation, schedule_cron::validate_cron_schedule, VariableRef};

use super::super::{DesiredSchedule, DesiredStateManifest, DesiredTask};

pub(super) fn validate_tasks(
    manifest: &DesiredStateManifest,
    behavior_ids: &BTreeSet<String>,
    errors: &mut Vec<String>,
) -> BTreeSet<String> {
    let mut task_ids = BTreeSet::new();
    for task in &manifest.tasks {
        let task_id = task.task_id.trim();
        if task_id.is_empty() {
            errors.push("tasks manifest contains a task with an empty task_id".to_string());
        } else if !task_ids.insert(task_id.to_string()) {
            errors.push(format!("duplicate task_id in tasks manifest: {task_id}"));
        }

        if task.name.trim().is_empty() {
            errors.push(format!(
                "task {} in tasks manifest must contain a non-empty name",
                task.task_id
            ));
        }

        let behavior_id = task.behavior_id.trim();
        if behavior_id.is_empty() {
            errors.push(format!(
                "task {} in tasks manifest must contain a non-empty behavior_id",
                task.task_id
            ));
        } else if !behavior_ids.contains(behavior_id) {
            errors.push(format!(
                "task {} references missing behavior_id {}",
                task.task_id, behavior_id
            ));
        }

        let goal_objective_template = task
            .goal_objective_template
            .as_ref()
            .and_then(Option::as_ref);
        let goal_token_budget = task.goal_token_budget.as_ref().and_then(|value| *value);
        match (goal_objective_template, goal_token_budget) {
            (Some(objective), _) if objective.trim().is_empty() => errors.push(format!(
                "task {} goal_objective_template must be non-empty when set",
                task.task_id
            )),
            (None, Some(_)) => errors.push(format!(
                "task {} goal_token_budget requires goal_objective_template",
                task.task_id
            )),
            _ => {}
        }
        if goal_token_budget.is_some_and(|budget| budget <= 0) {
            errors.push(format!(
                "task {} goal_token_budget must be positive",
                task.task_id
            ));
        }

        validate_task_template_catalog_scope(task, errors);
    }
    task_ids
}

pub(super) fn validate_schedules(
    manifest: &DesiredStateManifest,
    task_ids: &BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    let mut schedule_ids = BTreeSet::new();
    for schedule in &manifest.schedules {
        let schedule_id = schedule.schedule_id.trim();
        if schedule_id.is_empty() {
            errors.push(
                "schedules manifest contains a schedule with an empty schedule_id".to_string(),
            );
        } else if !schedule_ids.insert(schedule_id.to_string()) {
            errors.push(format!(
                "duplicate schedule_id in schedules manifest: {schedule_id}"
            ));
        }

        let task_id = schedule.task_id.trim();
        if task_id.is_empty() {
            errors.push(format!(
                "schedule {} in schedules manifest must contain a non-empty task_id",
                schedule.schedule_id
            ));
        } else if !task_ids.contains(task_id) {
            errors.push(format!(
                "schedule {} references missing task_id {}",
                schedule.schedule_id, task_id
            ));
        }

        validate_schedule_cadence(schedule, errors);

        match schedule.concurrency.trim() {
            "parallel" | "serial" | "latest_only" => {}
            other => errors.push(format!(
                "schedule {} in schedules manifest has unknown concurrency {}",
                schedule.schedule_id, other
            )),
        }

        if !task_id.is_empty() {
            if let Some(task) = manifest.tasks.iter().find(|task| task.task_id == task_id) {
                for (field, template) in task_templates(task) {
                    match parse_template_for_validation(template) {
                        Ok(refs) => {
                            let mut reported: BTreeSet<&str> = BTreeSet::new();
                            for var in &refs {
                                if let Some(root) = var.root() {
                                    if (root == "doc" || root == "args" || root == "group")
                                        && reported.insert(root)
                                    {
                                        errors.push(format!(
                                            "schedule {} {} references forbidden scope: {}; schedule scope only permits event.*, node.*, and ctx.now",
                                            schedule.schedule_id,
                                            field,
                                            format_variable_ref(var),
                                        ));
                                    }
                                }
                            }
                        }
                        Err(err) => errors.push(format!(
                            "schedule {} {} failed to parse: {}",
                            schedule.schedule_id, field, err
                        )),
                    }
                }
            }
        }
    }
}

pub(super) fn validate_event_triggers(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    let mut event_trigger_ids = BTreeSet::new();
    for trigger in &manifest.event_triggers {
        let trigger_id = trigger.trigger_id.trim();
        if trigger_id.is_empty() {
            errors.push(
                "event_triggers manifest contains a trigger with an empty trigger_id".to_string(),
            );
            continue;
        }
        if !event_trigger_ids.insert(trigger_id.to_string()) {
            errors.push(format!(
                "duplicate trigger_id in event_triggers manifest: {trigger_id}"
            ));
        }

        let task_id = trigger.task_id.trim();
        if task_id.is_empty() {
            errors.push(format!(
                "event_trigger {} in event_triggers manifest must contain a non-empty task_id",
                trigger.trigger_id
            ));
        }

        if trigger.source_collection.trim().is_empty() {
            errors.push(format!(
                "event_trigger {} in event_triggers manifest must contain a non-empty source_collection",
                trigger.trigger_id
            ));
        } else if let Err(error) =
            gents::graphql::validate_collection_identifier(trigger.source_collection.trim())
        {
            errors.push(format!(
                "event_trigger {} has invalid source_collection {:?}: {}",
                trigger.trigger_id, trigger.source_collection, error
            ));
        }

        if trigger.event_kind != "created" {
            errors.push(format!(
                "event_trigger {} uses unsupported event_kind {:?} (v1 supports only \"created\")",
                trigger.trigger_id, trigger.event_kind
            ));
        }

        if let Some(authority) = trigger
            .workspace_authority
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if let Err(error) = gents::toolset::WorkspaceAuthority::parse(authority) {
                errors.push(format!(
                    "event_trigger {} has invalid workspace_authority {authority:?}: {error}",
                    trigger.trigger_id
                ));
            }
        }

        match trigger.concurrency.trim() {
            "parallel" | "serial" | "latest_only" => {}
            other => errors.push(format!(
                "event_trigger {} in event_triggers manifest has unknown concurrency {}; expected parallel|serial|latest_only",
                trigger.trigger_id, other
            )),
        }

        let fire_mode = trigger
            .fire_mode
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("per_document");
        if !matches!(fire_mode, "per_document" | "per_group") {
            errors.push(format!(
                "event_trigger {} has unknown fire_mode {:?}; expected per_document|per_group",
                trigger.trigger_id, fire_mode
            ));
        }
        for (label, field) in [
            ("correlation_field", trigger.correlation_field.as_deref()),
            (
                "expected_count_field",
                trigger.expected_count_field.as_deref(),
            ),
        ] {
            if let Some(field) = field.map(str::trim).filter(|value| !value.is_empty()) {
                if let Err(error) = gents::graphql::validate_graphql_name(field) {
                    errors.push(format!(
                        "event_trigger {} has invalid {} {:?}: {}",
                        trigger.trigger_id, label, field, error
                    ));
                }
            }
        }
        let has_correlation = trigger
            .correlation_field
            .as_deref()
            .is_some_and(|field| !field.trim().is_empty());
        let has_expected_field = trigger
            .expected_count_field
            .as_deref()
            .is_some_and(|field| !field.trim().is_empty());
        let has_timeout = trigger.group_timeout_secs.is_some();
        if trigger
            .expected_count
            .is_some_and(|count| count <= 0 || count as usize > gents::MAX_EVENT_TRIGGER_GROUP_DOCS)
        {
            errors.push(format!(
                "event_trigger {} expected_count must be in 1..={}",
                trigger.trigger_id,
                gents::MAX_EVENT_TRIGGER_GROUP_DOCS
            ));
        }
        if trigger
            .group_timeout_secs
            .is_some_and(|seconds| seconds <= 0)
        {
            errors.push(format!(
                "event_trigger {} group_timeout_secs must be positive",
                trigger.trigger_id
            ));
        }
        if trigger
            .group_min_count
            .is_some_and(|count| count <= 0 || count as usize > gents::MAX_EVENT_TRIGGER_GROUP_DOCS)
        {
            errors.push(format!(
                "event_trigger {} group_min_count must be in 1..={}",
                trigger.trigger_id,
                gents::MAX_EVENT_TRIGGER_GROUP_DOCS
            ));
        }
        if trigger.group_min_count.is_some() && !has_timeout {
            errors.push(format!(
                "event_trigger {} group_min_count requires group_timeout_secs",
                trigger.trigger_id
            ));
        }
        if let (Some(minimum), Some(expected)) = (trigger.group_min_count, trigger.expected_count) {
            if minimum > expected {
                errors.push(format!(
                    "event_trigger {} group_min_count cannot exceed expected_count",
                    trigger.trigger_id
                ));
            }
        }
        match fire_mode {
            "per_document" => {
                if trigger.expected_count.is_some()
                    || has_expected_field
                    || has_timeout
                    || trigger.group_min_count.is_some()
                {
                    errors.push(format!(
                        "event_trigger {} per_document mode cannot configure group count or timeout fields",
                        trigger.trigger_id
                    ));
                }
            }
            "per_group" => {
                if !has_correlation {
                    errors.push(format!(
                        "event_trigger {} per_group mode requires correlation_field",
                        trigger.trigger_id
                    ));
                }
                if trigger.expected_count.is_some() && has_expected_field {
                    errors.push(format!(
                        "event_trigger {} must configure only one of expected_count or expected_count_field",
                        trigger.trigger_id
                    ));
                }
                if trigger.expected_count.is_none() && !has_expected_field && !has_timeout {
                    errors.push(format!(
                        "event_trigger {} per_group mode requires a count source or group_timeout_secs",
                        trigger.trigger_id
                    ));
                }
            }
            _ => {}
        }

        if !task_id.is_empty() && !manifest.tasks.iter().any(|t| t.task_id == task_id) {
            errors.push(format!(
                "event_trigger {} references unknown task_id {}",
                trigger.trigger_id, trigger.task_id
            ));
        }

        if !task_id.is_empty() {
            if let Some(task) = manifest.tasks.iter().find(|t| t.task_id == task_id) {
                for (field, template) in task_templates(task) {
                    match parse_template_for_validation(template) {
                        Ok(refs) => {
                            let mut reported: BTreeSet<&str> = BTreeSet::new();
                            for vref in &refs {
                                if let Some(root) = vref.root() {
                                    if root == "args" && reported.insert("args") {
                                        errors.push(format!(
                                            "event_trigger {} {} references forbidden scope: args; event scope only permits event.*, doc.*, node.*, and ctx.now",
                                            trigger.trigger_id, field
                                        ));
                                    }
                                    if root == "group"
                                        && fire_mode != "per_group"
                                        && reported.insert("group")
                                    {
                                        errors.push(format!(
                                            "event_trigger {} {} references group.* outside per_group mode",
                                            trigger.trigger_id, field
                                        ));
                                    }
                                }
                            }
                        }
                        Err(err) => errors.push(format!(
                            "event_trigger {} {} failed to parse: {}",
                            trigger.trigger_id, field, err
                        )),
                    }
                }
            }
        }
    }
}

pub(super) fn validate_callback_bindings(
    manifest: &DesiredStateManifest,
    errors: &mut Vec<String>,
) {
    for binding in &manifest.callback_bindings {
        let binding_id = binding.binding_id.trim();
        if binding_id.is_empty() {
            errors.push(
                "callback_bindings manifest contains a binding with an empty binding_id"
                    .to_string(),
            );
            continue;
        }
        if binding.source_collection.trim().is_empty() {
            errors.push(format!(
                "callback_binding {binding_id} must contain a non-empty source_collection"
            ));
        } else if let Err(error) =
            gents::graphql::validate_collection_identifier(binding.source_collection.trim())
        {
            errors.push(format!(
                "callback_binding {binding_id} has invalid source_collection {:?}: {error}",
                binding.source_collection
            ));
        }
        if binding.event_kind.trim() != "created" {
            errors.push(format!(
                "callback_binding {binding_id} uses unsupported event_kind {:?} (v1 supports only \"created\")",
                binding.event_kind
            ));
        }
        if binding
            .builtin_emitter
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
            && binding
                .module_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_none()
        {
            errors.push(format!(
                "callback_binding {binding_id} needs builtin_emitter or module_id"
            ));
        }
        if let Err(error) = gents::reject_secret_bearing_callback_fields(
            binding_id,
            binding.filter.as_deref(),
            binding.source_fields.as_deref(),
        ) {
            errors.push(error.to_string());
        }
    }
}

pub(super) fn validate_repository_placements(
    manifest: &DesiredStateManifest,
    errors: &mut Vec<String>,
) {
    for placement in &manifest.repository_placements {
        if placement.repository_id.trim().is_empty() {
            errors.push(
                "repository_placements manifest contains a placement with an empty repository_id"
                    .to_string(),
            );
        }
        if placement.host_path.trim().is_empty() {
            errors.push(format!(
                "repository_placement {} must contain a non-empty host_path",
                placement.repository_id
            ));
        }
    }
}

fn validate_task_template_catalog_scope(task: &DesiredTask, errors: &mut Vec<String>) {
    for (field, template) in task_templates(task) {
        validate_template_catalog_scope(task, field, template, errors);
    }
}

fn task_templates<'a>(task: &'a DesiredTask) -> impl Iterator<Item = (&'static str, &'a str)> + 'a {
    std::iter::once(("prompt_template", task.prompt_template.as_str())).chain(
        task.goal_objective_template
            .as_ref()
            .and_then(Option::as_deref)
            .map(|template| ("goal_objective_template", template)),
    )
}

fn validate_template_catalog_scope(
    task: &DesiredTask,
    field: &str,
    template: &str,
    errors: &mut Vec<String>,
) {
    let refs = match parse_template_for_validation(template) {
        Ok(refs) => refs,
        Err(error) => {
            errors.push(format!(
                "task {} {} failed to parse: {}",
                task.task_id, field, error
            ));
            return;
        }
    };
    let catalog = default_catalog();
    let mut reported: BTreeSet<String> = BTreeSet::new();
    for var in refs {
        let Some(root) = var.root() else {
            continue;
        };
        if root != "node" && root != "ctx" {
            continue;
        }
        let full_ref = format_variable_ref(&var);
        if catalog.is_available_at(&full_ref, Site::Task) {
            continue;
        }
        if reported.insert(full_ref.clone()) {
            errors.push(format!(
                "task {} {} references unavailable template variable {}; task scope permits node.node_did, node.behavior_id, and ctx.now",
                task.task_id, field, full_ref
            ));
        }
    }
}

fn validate_schedule_cadence(schedule: &DesiredSchedule, errors: &mut Vec<String>) {
    let interval_secs = schedule.interval_secs;
    let cron = schedule
        .cron
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match (interval_secs, cron) {
        (Some(interval_secs), None) if interval_secs >= 1 => {}
        (Some(_), Some(_)) => errors.push(format!(
            "schedule {} in schedules manifest must contain exactly one of interval_secs or cron",
            schedule.schedule_id
        )),
        (Some(_), None) => errors.push(format!(
            "schedule {} in schedules manifest must contain an interval_secs >= 1",
            schedule.schedule_id
        )),
        (None, Some(expression)) => {
            let timezone = schedule
                .timezone
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let Some(timezone) = timezone else {
                errors.push(format!(
                    "schedule {} in schedules manifest must contain a timezone when cron is set",
                    schedule.schedule_id
                ));
                return;
            };
            if let Err(error) =
                validate_cron_schedule(expression, timezone, schedule.missed_run_policy.as_deref())
            {
                errors.push(format!(
                    "schedule {} in schedules manifest has invalid cron schedule: {}",
                    schedule.schedule_id, error
                ));
            }
        }
        (None, None) => errors.push(format!(
            "schedule {} in schedules manifest must contain exactly one of interval_secs or cron",
            schedule.schedule_id
        )),
    }
}

fn format_variable_ref(var: &VariableRef) -> String {
    if var.path.is_empty() {
        String::new()
    } else {
        var.path.join(".")
    }
}
