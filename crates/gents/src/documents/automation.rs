use anyhow::Result;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rapidhash::{RapidHashMap, RapidHashSet};
use serde_json::{json, Value};

use super::schedule_cron::{next_cron_run_after, CronMissedRunPolicy};
use super::{ConcurrencyMode, EventSource, ScheduleCadence, TaskHook};

#[derive(Debug, Clone)]
pub struct ResolvedTask {
    pub emit_outcome: bool,
    pub task_id: String,
    pub name: Option<String>,
    pub agent_did: String,
    pub prompt_template: String,
    pub goal_objective_template: Option<String>,
    pub goal_token_budget: Option<i64>,
    #[allow(dead_code)]
    pub output_schema_ref: Option<String>,
    pub hooks: Vec<TaskHook>,
}

impl ResolvedTask {
    pub(crate) fn display_label(&self) -> &str {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.task_id)
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedSchedule {
    pub session_id_template: Option<String>,
    pub trigger_doc_id: String,
    pub schedule_doc_id: String,
    pub schedule_id: String,
    #[allow(dead_code)]
    pub task_id: String,
    pub task: ResolvedTask,
    pub cadence: ScheduleCadence,
    #[allow(dead_code)]
    pub enabled: bool,
    pub concurrency: ConcurrencyMode,
}

pub const MAX_EVENT_TRIGGER_GROUP_DOCS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventTriggerFireMode {
    PerDocument,
    PerGroup,
}

#[derive(Debug, Clone)]
pub struct ResolvedEventTrigger {
    pub event_source_id: String,
    pub event_source_doc_id: String,
    pub task_doc_id: String,
    pub session_id_template: Option<String>,
    pub trigger_doc_id: String,
    pub trigger_id: String,
    #[allow(dead_code)]
    pub task_id: String,
    pub task: ResolvedTask,
    pub source_collection: String,
    pub event_kind: String,
    pub filter: Option<String>,
    #[allow(dead_code)]
    pub enabled: bool,
    pub concurrency: ConcurrencyMode,
    pub fire_mode: EventTriggerFireMode,
    pub correlation_field: Option<String>,
    pub expected_count: Option<usize>,
    pub expected_count_field: Option<String>,
    pub group_timeout_secs: Option<u64>,
    pub group_min_count: usize,
    pub workspace_authority: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ResolvedAutomation {
    pub(crate) tasks: RapidHashMap<String, ResolvedTask>,
    pub(crate) schedules: RapidHashMap<String, ResolvedSchedule>,
    pub(crate) unavailable_schedules: RapidHashSet<String>,
    pub(crate) event_triggers: RapidHashMap<String, ResolvedEventTrigger>,
    pub(crate) unavailable_event_triggers: RapidHashSet<String>,
}

pub(crate) fn seed_schedule_next_run_at(
    cadence: &ScheduleCadence,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    match cadence {
        ScheduleCadence::Interval { .. } => Ok(now),
        ScheduleCadence::Cron {
            expression,
            timezone,
            ..
        } => next_cron_run_after(expression, timezone, now),
    }
}

pub(crate) fn advance_schedule_next_run_at(
    cadence: &ScheduleCadence,
    parsed_next_run_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    match cadence {
        ScheduleCadence::Interval { interval_secs } => {
            Ok(parsed_next_run_at + ChronoDuration::seconds(*interval_secs))
        }
        ScheduleCadence::Cron {
            expression,
            timezone,
            missed_run_policy,
        } => match missed_run_policy {
            None | Some(CronMissedRunPolicy::LatestOnly) => {
                next_cron_run_after(expression, timezone, now)
            }
        },
    }
}

pub(crate) fn resolved_task_from(task: &super::Task) -> ResolvedTask {
    ResolvedTask {
        emit_outcome: task.emit_outcome,
        task_id: task.task_id.clone(),
        name: task.display_name.clone(),
        agent_did: task.agent_did.clone(),
        prompt_template: task.prompt_template.clone(),
        goal_objective_template: task.goal_objective_template.clone(),
        goal_token_budget: task.goal_token_budget,
        output_schema_ref: task.output_schema_ref.clone(),
        hooks: task.hooks.clone(),
    }
}

pub(crate) fn event_source_generation(source: &EventSource) -> Result<Value> {
    source.validate()?;
    let (expected_count, expected_field, timeout, minimum) = source.group_projection();
    Ok(json!({
        "collection": source.source_collection,
        "event_kind": source.event_kind.as_deref().map(str::trim).filter(|value| !value.is_empty()).unwrap_or("created"),
        "filter": source.filter,
        "correlation": source.correlation_field.as_deref().filter(|value| !value.trim().is_empty()),
        "grouped": source.group.is_some(),
        "expected_count": expected_count,
        "expected_field": expected_field,
        "timeout": timeout,
        "minimum": minimum,
        "workspace_authority": source.workspace_authority.as_ref().map(|authority| authority.as_str()),
    }))
}

pub(crate) fn resolved_event_source_generation(trigger: &ResolvedEventTrigger) -> Value {
    json!({
        "collection": trigger.source_collection,
        "event_kind": trigger.event_kind,
        "filter": trigger.filter,
        "correlation": trigger.correlation_field,
        "grouped": trigger.fire_mode == EventTriggerFireMode::PerGroup,
        "expected_count": trigger.expected_count,
        "expected_field": trigger.expected_count_field,
        "timeout": trigger.group_timeout_secs,
        "minimum": trigger.group_min_count,
        "workspace_authority": trigger.workspace_authority,
    })
}

pub(crate) struct EventConfigDocIds<'a> {
    pub trigger: &'a str,
    pub source: &'a str,
    pub task: &'a str,
}

fn execution_generation(
    task: &ResolvedTask,
    source_id: &str,
    session_template: Option<&str>,
    concurrency: ConcurrencyMode,
    ids: &EventConfigDocIds<'_>,
) -> Value {
    json!({
        "task_id": task.task_id, "agent": task.agent_did, "name": task.name,
        "prompt": task.prompt_template, "objective": task.goal_objective_template,
        "budget": task.goal_token_budget, "output": task.output_schema_ref,
        "hooks": serde_json::to_string(&task.hooks).expect("Task hooks contain serializable fields"),
        "emit": task.emit_outcome,
        "source_id": source_id, "trigger_doc": ids.trigger, "source_doc": ids.source,
        "task_doc": ids.task, "session_template": session_template,
        "concurrency": concurrency,
    })
}

pub(crate) fn event_execution_generation(
    trigger: &super::Trigger,
    task: &super::Task,
    ids: &EventConfigDocIds<'_>,
) -> Result<Value> {
    let super::TriggerSource::Event { event_source_id } = &trigger.source else {
        anyhow::bail!("event execution requires an event trigger");
    };
    Ok(execution_generation(
        &resolved_task_from(task),
        event_source_id,
        trigger.session_id_template.as_deref(),
        trigger.concurrency.unwrap_or_default(),
        ids,
    ))
}

pub(crate) fn resolved_event_execution_generation(trigger: &ResolvedEventTrigger) -> Value {
    execution_generation(
        &trigger.task,
        &trigger.event_source_id,
        trigger.session_id_template.as_deref(),
        trigger.concurrency,
        &EventConfigDocIds {
            trigger: &trigger.trigger_doc_id,
            source: &trigger.event_source_doc_id,
            task: &trigger.task_doc_id,
        },
    )
}

pub(crate) fn event_admission_generation(
    source: &EventSource,
    trigger: &super::Trigger,
    task: &super::Task,
    ids: &EventConfigDocIds<'_>,
) -> Result<Value> {
    Ok(json!({"source":event_source_generation(source)?,
        "execution":event_execution_generation(trigger, task, ids)?}))
}

pub(crate) fn resolved_event_admission_generation(trigger: &ResolvedEventTrigger) -> Value {
    json!({"source":resolved_event_source_generation(trigger),
        "execution":resolved_event_execution_generation(trigger)})
}
