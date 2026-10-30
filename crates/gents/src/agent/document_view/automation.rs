use super::DocumentRuntimeView;
use crate::runtime_snapshot::{
    ConcurrencyMode, EventTriggerFireMode, ResolvedAutomation, ResolvedEventTrigger,
    ResolvedSchedule, ResolvedTask, UnavailableAgent,
};
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};

pub(super) fn resolve_automation(
    view: &DocumentRuntimeView,
    unavailable_agents: &RapidHashMap<String, UnavailableAgent>,
) -> ResolvedAutomation {
    let tasks = resolve_tasks(view, unavailable_agents);
    let mut schedules = RapidHashMap::new();
    let mut unavailable_schedules = RapidHashSet::new();
    let mut event_triggers = RapidHashMap::new();
    let mut unavailable_event_triggers = RapidHashSet::new();
    let mut unavailable_triggers = RapidHashSet::new();

    let mut trigger_tasks: RapidHashMap<String, ResolvedTask> = RapidHashMap::new();
    for trigger_record in view.triggers.values() {
        let trigger = &trigger_record.value;
        let trigger_id = trigger.trigger_id.clone();

        if !trigger.enabled {
            unavailable_triggers.insert(trigger_id.clone());
            continue;
        }

        let task_id = trigger.task_id.as_str();
        if task_id.is_empty() {
            unavailable_triggers.insert(trigger_id.clone());
            continue;
        }
        let Some(task_record) = view.tasks.get(task_id) else {
            unavailable_triggers.insert(trigger_id.clone());
            continue;
        };
        let task = &task_record.value;
        if !task.enabled {
            unavailable_triggers.insert(trigger_id.clone());
            continue;
        }
        let agent_id = task.agent_id.as_str();
        let Some(behavior_record) = view.agents.get(agent_id) else {
            unavailable_triggers.insert(trigger_id.clone());
            continue;
        };
        if !behavior_record.value.enabled {
            unavailable_triggers.insert(trigger_id.clone());
            continue;
        }
        if unavailable_agents.contains_key(agent_id) {
            unavailable_triggers.insert(trigger_id.clone());
            continue;
        }
        trigger_tasks.insert(trigger_id, resolved_task_from(task));
    }

    for trigger_record in view.triggers.values() {
        let trigger = &trigger_record.value;
        let trigger_id = trigger.trigger_id.clone();
        if unavailable_triggers.contains(&trigger_id) {
            match &trigger.source {
                crate::documents::TriggerSource::Schedule { .. } => {
                    unavailable_schedules.insert(trigger_id);
                }
                crate::documents::TriggerSource::Event { .. } => {
                    unavailable_event_triggers.insert(trigger_id);
                }
            }
            continue;
        }
        let Some(task) = trigger_tasks.get(&trigger_id) else {
            continue;
        };

        let concurrency = trigger.concurrency.unwrap_or_default();

        match &trigger.source {
            crate::documents::TriggerSource::Schedule { schedule_id } => {
                resolve_schedule_trigger(
                    view,
                    schedule_id,
                    &trigger_record.doc_id,
                    &trigger_id,
                    concurrency,
                    task.clone(),
                    &mut schedules,
                    &mut unavailable_schedules,
                );
            }
            crate::documents::TriggerSource::Event { event_source_id } => {
                resolve_event_trigger(
                    view,
                    event_source_id,
                    &trigger_record.doc_id,
                    &trigger_id,
                    concurrency,
                    task.clone(),
                    &mut event_triggers,
                    &mut unavailable_event_triggers,
                );
            }
        }
        if let Some(schedule) = schedules.get_mut(&trigger_id) {
            schedule.session_id_template = trigger.session_id_template.clone();
        }
        if let Some(event) = event_triggers.get_mut(&trigger_id) {
            event.session_id_template = trigger.session_id_template.clone();
        }
    }

    ResolvedAutomation {
        tasks,
        schedules,
        unavailable_schedules,
        event_triggers,
        unavailable_event_triggers,
    }
}

fn resolved_task_from(task: &crate::documents::Task) -> ResolvedTask {
    ResolvedTask {
        emit_outcome: task.emit_outcome,
        task_id: task.task_id.clone(),
        name: task.display_name.clone(),
        agent_id: task.agent_id.clone(),
        prompt_template: task.prompt_template.clone(),
        goal_objective_template: task.goal_objective_template.clone(),
        goal_token_budget: task.goal_token_budget,
        output_schema_ref: task.output_schema_ref.clone(),
        hooks: task.hooks.clone(),
    }
}

fn resolve_tasks(
    view: &DocumentRuntimeView,
    unavailable_agents: &RapidHashMap<String, UnavailableAgent>,
) -> RapidHashMap<String, ResolvedTask> {
    let mut active_tasks = RapidHashMap::new();

    for (task_id, task_record) in &view.tasks {
        let task = &task_record.value;

        if !task.enabled {
            continue;
        }

        if task.agent_id.trim().is_empty() {
            continue;
        }

        let behavior_record = match view.agents.get(&task.agent_id) {
            Some(record) => record,
            None => continue,
        };
        if !behavior_record.value.enabled {
            continue;
        }
        if unavailable_agents.contains_key(&task.agent_id) {
            continue;
        }

        active_tasks.insert(task_id.clone(), resolved_task_from(task));
    }

    active_tasks
}

#[allow(clippy::too_many_arguments)]
fn resolve_schedule_trigger(
    view: &DocumentRuntimeView,
    schedule_id: &str,
    trigger_doc_id: &str,
    trigger_id: &str,
    concurrency: ConcurrencyMode,
    task: ResolvedTask,
    active_schedules: &mut RapidHashMap<String, ResolvedSchedule>,
    unavailable_schedules: &mut RapidHashSet<String>,
) {
    let Some(schedule_record) = view.schedules.get(schedule_id) else {
        tracing::warn!(
            trigger_id = %trigger_id,
            schedule_id = %schedule_id,
            "schedule quarantined: referenced schedule document is missing",
        );
        unavailable_schedules.insert(trigger_id.to_string());
        return;
    };
    let schedule = &schedule_record.value;

    if let Err(error) = schedule.cadence.validate() {
        tracing::warn!(
            trigger_id = %trigger_id,
            schedule_id = %schedule_id,
            %error,
            "schedule quarantined: invalid cadence",
        );
        unavailable_schedules.insert(trigger_id.to_string());
        return;
    }

    if task_template_references_group(&task.prompt_template) {
        tracing::warn!(
            trigger_id = %trigger_id,
            schedule_id = %schedule_id,
            "schedule quarantined: group.* template scope is only available to \
             per_group event triggers",
        );
        unavailable_schedules.insert(trigger_id.to_string());
        return;
    }

    active_schedules.insert(
        trigger_id.to_string(),
        ResolvedSchedule {
            session_id_template: None,
            trigger_doc_id: trigger_doc_id.to_string(),
            schedule_id: schedule.schedule_id.clone(),
            task_id: task.task_id.clone(),
            task,
            cadence: schedule.cadence.clone(),
            enabled: true,
            concurrency,
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn resolve_event_trigger(
    view: &DocumentRuntimeView,
    event_source_id: &str,
    trigger_doc_id: &str,
    trigger_id: &str,
    concurrency: ConcurrencyMode,
    task: ResolvedTask,
    active_event_triggers: &mut RapidHashMap<String, ResolvedEventTrigger>,
    unavailable_event_triggers: &mut RapidHashSet<String>,
) {
    let Some(source_record) = view.event_sources.get(event_source_id) else {
        tracing::warn!(
            trigger_id = %trigger_id,
            event_source_id = %event_source_id,
            "event trigger quarantined: referenced event source is missing",
        );
        unavailable_event_triggers.insert(trigger_id.to_string());
        return;
    };
    let source = &source_record.value;

    let source_collection = source.source_collection.clone();
    let event_kind = source
        .event_kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("created")
        .to_string();
    if let Err(error) = source.validate() {
        tracing::warn!(trigger_id = %trigger_id, %error,
            "event trigger quarantined: invalid event source configuration");
        unavailable_event_triggers.insert(trigger_id.to_string());
        return;
    }
    let correlation_field = source
        .correlation_field
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let group = source.group.as_ref();
    let fire_mode = if group.is_some() {
        EventTriggerFireMode::PerGroup
    } else {
        EventTriggerFireMode::PerDocument
    };
    if fire_mode != EventTriggerFireMode::PerGroup
        && task_template_references_group(&task.prompt_template)
    {
        tracing::warn!(
            trigger_id = %trigger_id,
            "event trigger quarantined: group.* template scope requires per_group mode",
        );
        unavailable_event_triggers.insert(trigger_id.to_string());
        return;
    }
    let (expected_count, expected_count_field, group_timeout_secs, group_min_count) =
        group_projection(source);

    active_event_triggers.insert(
        trigger_id.to_string(),
        ResolvedEventTrigger {
            session_id_template: None,
            trigger_doc_id: trigger_doc_id.to_string(),
            trigger_id: trigger_id.to_string(),
            task_id: task.task_id.clone(),
            task,
            source_collection,
            event_kind,
            filter: source.filter.clone(),
            enabled: true,
            concurrency,
            fire_mode,
            correlation_field,
            expected_count,
            expected_count_field,
            group_timeout_secs,
            group_min_count,
            workspace_authority: source
                .workspace_authority
                .as_ref()
                .map(|authority| authority.as_str().to_string()),
        },
    );
}

fn group_projection(
    source: &crate::documents::EventSource,
) -> (Option<usize>, Option<String>, Option<u64>, usize) {
    let Some(group) = source.group.as_ref() else {
        return (None, None, None, 1);
    };
    let expected_count = match group.expected_count {
        Some(crate::documents::EventGroupCount::Fixed(count)) => usize::try_from(count).ok(),
        Some(crate::documents::EventGroupCount::SourceField { .. }) => None,
        None => None,
    };
    let expected_count_field = match &group.expected_count {
        Some(crate::documents::EventGroupCount::Fixed(_)) => None,
        Some(crate::documents::EventGroupCount::SourceField { source_field }) => {
            Some(source_field.clone())
        }
        None => None,
    };
    let group_timeout_secs = group
        .timeout_secs
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value > 0);
    let group_min_count = group
        .min_count
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1);
    (
        expected_count,
        expected_count_field,
        group_timeout_secs,
        group_min_count,
    )
}

fn task_template_references_group(template: &str) -> bool {
    crate::documents::template::parse_template_for_validation(template).is_ok_and(|refs| {
        refs.iter()
            .any(|reference| reference.root() == Some("group"))
    })
}
