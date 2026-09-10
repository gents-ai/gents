use std::collections::BTreeMap;

use anyhow::Result;
use gents::graphql::escape_graphql_string;
use serde_json::{json, Value};

use crate::cli::{TaskCommand, TaskListArgs, TaskShowArgs};
use crate::commands::config::task_run::{config_task_run, resolve_task_id_for};
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access};

const TASK_FIELDS: &str = "task_id agent_did display_name description behavior_id prompt_template goal_objective_template goal_token_budget hooks enabled output_schema_ref created_at updated_at tags";
const BEHAVIOR_FIELDS: &str = "behavior_id agent_did display_name description context_id inference_profile_id enabled tags created_at updated_at";
const TRIGGER_FIELDS: &str = "trigger_id agent_did display_name description task_id source enabled concurrency next_run_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count created_at updated_at tags";
const SCHEDULE_FIELDS: &str =
    "schedule_id agent_did display_name cadence created_at updated_at tags";
const EVENT_SOURCE_FIELDS: &str = "event_source_id agent_did display_name source_collection event_kind filter correlation_field group workspace_authority created_at updated_at tags";

pub(crate) async fn dispatch(command: TaskCommand) -> Result<()> {
    match command {
        TaskCommand::List(args) => task_list(args).await,
        TaskCommand::Show(args) => task_show(args).await,
        TaskCommand::Run(args) => config_task_run(args).await,
    }
}

pub(crate) async fn task_list(args: TaskListArgs) -> Result<()> {
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let inventory = load_task_inventory(&access, None).await?;
    let tasks = inventory.task_summaries();
    print_json(&json!({
        "count": tasks.len(),
        "tasks": tasks,
    }))?;
    Ok(())
}

pub(crate) async fn task_show(args: TaskShowArgs) -> Result<()> {
    let task_id = resolve_task_id_for(
        "show",
        args.task_id.as_deref(),
        args.task_id_flag.as_deref(),
    )?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let inventory = load_task_inventory(&access, Some(&task_id)).await?;
    let Some(task) = inventory.tasks.first() else {
        anyhow::bail!("no Task with task_id = {task_id}");
    };
    print_json(&inventory.task_detail(task))?;
    Ok(())
}

struct TaskInventory {
    tasks: Vec<Value>,
    behaviors_by_id: BTreeMap<String, Value>,
    triggers: Vec<Value>,
    schedules_by_id: BTreeMap<String, Value>,
    event_sources_by_id: BTreeMap<String, Value>,
}

impl TaskInventory {
    fn task_summaries(&self) -> Vec<Value> {
        self.tasks
            .iter()
            .map(|task| self.task_summary(task))
            .collect()
    }

    fn task_summary(&self, task: &Value) -> Value {
        let task_id = string_field(task, "task_id").unwrap_or_default();
        let behavior_id = string_field(task, "behavior_id");
        let behavior = behavior_id
            .as_deref()
            .and_then(|id| self.behaviors_by_id.get(id));
        let triggers = self.triggers_for_task(&task_id);
        let (runnable, unavailable_reason) = runnable_status(task, behavior);

        json!({
            "task_id": task_id,
            "display_name": task.get("display_name").cloned().unwrap_or(Value::Null),
            "description": task.get("description").cloned().unwrap_or(Value::Null),
            "behavior_id": behavior_id,
            "goal_objective_template": task.get("goal_objective_template").cloned().unwrap_or(Value::Null),
            "goal_token_budget": task.get("goal_token_budget").cloned().unwrap_or(Value::Null),
            "enabled": bool_field(task, "enabled").unwrap_or(false),
            "runnable": runnable,
            "unavailable_reason": unavailable_reason,
            "behavior": behavior.and_then(behavior_summary).unwrap_or(Value::Null),
            "trigger_count": triggers.len(),
            "trigger_ids": triggers.iter().filter_map(|row| string_field(row, "trigger_id")).collect::<Vec<_>>(),
        })
    }

    fn task_detail(&self, task: &Value) -> Value {
        let task_id = string_field(task, "task_id").unwrap_or_default();
        let behavior = string_field(task, "behavior_id")
            .as_deref()
            .and_then(|id| self.behaviors_by_id.get(id));
        let triggers = self.triggers_for_task(&task_id);
        let (runnable, unavailable_reason) = runnable_status(task, behavior);

        json!({
            "task": task,
            "behavior": behavior.cloned().unwrap_or(Value::Null),
            "runnable": runnable,
            "unavailable_reason": unavailable_reason,
            "trigger_count": triggers.len(),
            "triggers": triggers,
        })
    }

    fn triggers_for_task(&self, task_id: &str) -> Vec<Value> {
        let mut rows = self
            .triggers
            .iter()
            .filter(|row| string_field(row, "task_id").as_deref() == Some(task_id))
            .map(|row| self.enrich_trigger(row))
            .collect::<Vec<_>>();
        sort_rows_by_string_field(&mut rows, "trigger_id");
        rows
    }

    fn enrich_trigger(&self, row: &Value) -> Value {
        let mut trigger = row.clone();
        let source_config = match row.pointer("/source/kind").and_then(Value::as_str) {
            Some("schedule") => row
                .pointer("/source/schedule_id")
                .and_then(Value::as_str)
                .and_then(|id| self.schedules_by_id.get(id)),
            Some("event") => row
                .pointer("/source/event_source_id")
                .and_then(Value::as_str)
                .and_then(|id| self.event_sources_by_id.get(id)),
            _ => None,
        };
        if let Some(object) = trigger.as_object_mut() {
            object.insert(
                "source_config".into(),
                source_config.cloned().unwrap_or(Value::Null),
            );
        }
        trigger
    }
}

async fn load_task_inventory(
    access: &ConfigAccess,
    task_id_filter: Option<&str>,
) -> Result<TaskInventory> {
    let query = task_inventory_query(task_id_filter);
    let response = access.execute(&query).await?;
    ensure_no_graphql_errors(&response, "query task inventory")?;

    let mut tasks = rows(&response, "Task");
    sort_rows_by_string_field(&mut tasks, "task_id");

    let behaviors_by_id = rows(&response, "AgentBehavior")
        .into_iter()
        .filter_map(|row| string_field(&row, "behavior_id").map(|id| (id, row)))
        .collect::<BTreeMap<_, _>>();

    let mut triggers = rows(&response, "Trigger");
    sort_rows_by_string_field(&mut triggers, "trigger_id");
    let schedules_by_id = rows(&response, "Schedule")
        .into_iter()
        .filter_map(|row| string_field(&row, "schedule_id").map(|id| (id, row)))
        .collect();
    let event_sources_by_id = rows(&response, "EventSource")
        .into_iter()
        .filter_map(|row| string_field(&row, "event_source_id").map(|id| (id, row)))
        .collect();

    Ok(TaskInventory {
        tasks,
        behaviors_by_id,
        triggers,
        schedules_by_id,
        event_sources_by_id,
    })
}

fn task_inventory_query(task_id_filter: Option<&str>) -> String {
    let task_args = task_id_filter
        .map(|task_id| {
            format!(
                r#"(filter: {{ task_id: {{ _eq: "{}" }} }}, limit: 1)"#,
                escape_graphql_string(task_id)
            )
        })
        .unwrap_or_default();
    let trigger_args = task_id_filter
        .map(|task_id| {
            format!(
                r#"(filter: {{ task_id: {{ _eq: "{}" }} }})"#,
                escape_graphql_string(task_id)
            )
        })
        .unwrap_or_default();

    format!(
        r#"{{
            Task{task_args} {{
                {TASK_FIELDS}
            }}
            AgentBehavior {{
                {BEHAVIOR_FIELDS}
            }}
            Trigger{trigger_args} {{
                {TRIGGER_FIELDS}
            }}
            Schedule {{
                {SCHEDULE_FIELDS}
            }}
            EventSource {{
                {EVENT_SOURCE_FIELDS}
            }}
        }}"#
    )
}

fn ensure_no_graphql_errors(response: &Value, context: &str) -> Result<()> {
    if let Some(errors) = response
        .get("errors")
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    {
        anyhow::bail!("{context} failed: {errors:?}");
    }
    Ok(())
}

fn rows(response: &Value, collection: &str) -> Vec<Value> {
    response
        .get("data")
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn string_field(row: &Value, field: &str) -> Option<String> {
    row.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn bool_field(row: &Value, field: &str) -> Option<bool> {
    row.get(field).and_then(Value::as_bool)
}

fn sort_rows_by_string_field(rows: &mut [Value], field: &str) {
    rows.sort_by(|left, right| {
        string_field(left, field)
            .unwrap_or_default()
            .cmp(&string_field(right, field).unwrap_or_default())
    });
}

fn behavior_summary(behavior: &Value) -> Option<Value> {
    let behavior_id = string_field(behavior, "behavior_id")?;
    Some(json!({
        "behavior_id": behavior_id,
        "agent_did": behavior.get("agent_did").cloned().unwrap_or(Value::Null),
        "display_name": behavior.get("display_name").cloned().unwrap_or(Value::Null),
        "enabled": bool_field(behavior, "enabled").unwrap_or(false),
        "context_id": behavior.get("context_id").cloned().unwrap_or(Value::Null),
        "inference_profile_id": behavior.get("inference_profile_id").cloned().unwrap_or(Value::Null),
    }))
}

fn runnable_status(task: &Value, behavior: Option<&Value>) -> (bool, Option<&'static str>) {
    if !bool_field(task, "enabled").unwrap_or(false) {
        return (false, Some("task_disabled"));
    }
    if string_field(task, "behavior_id").is_none() {
        return (false, Some("missing_behavior_id"));
    }
    let Some(behavior) = behavior else {
        return (false, Some("behavior_missing"));
    };
    if !bool_field(behavior, "enabled").unwrap_or(false) {
        return (false, Some("behavior_disabled"));
    }
    (true, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_inventory() -> TaskInventory {
        TaskInventory {
            tasks: vec![
                json!({
                    "task_id": "disabled",
                    "display_name": "Disabled",
                    "description": null,
                    "behavior_id": "default",
                    "prompt_template": "noop",
                    "enabled": false,
                    "output_schema_ref": null,
                    "created_at": null,
                    "updated_at": null
                }),
                json!({
                    "task_id": "host-check",
                    "display_name": "Host check",
                    "description": "Sweep host status",
                    "behavior_id": "default",
                    "prompt_template": "check",
                    "goal_objective_template": "Finish host {{ args.host }}",
                    "goal_token_budget": 12000,
                    "enabled": true,
                    "output_schema_ref": null,
                    "created_at": null,
                    "updated_at": null
                }),
            ],
            behaviors_by_id: BTreeMap::from([(
                "default".to_string(),
                json!({
                    "behavior_id": "default",
                    "agent_did": "did:key:z-test",
                    "display_name": "Default",
                    "description": null,
                    "context_id": "default-context",
                    "inference_profile_id": "default-profile",
                    "enabled": true,
                    "created_at": null
                }),
            )]),
            triggers: vec![json!({
                "trigger_id": "host-doc-created",
                "task_id": "host-check",
                "source": {"kind": "event", "event_source_id": "host-created"},
                "enabled": true,
                "concurrency": "parallel",
                "last_attempt_at": null,
                "last_fired_source_doc_id": null,
                "last_status": null,
                "last_error": null,
                "fire_count": 0,
                "created_at": null,
                "updated_at": null
            })],
            schedules_by_id: BTreeMap::new(),
            event_sources_by_id: BTreeMap::from([(
                "host-created".to_string(),
                json!({
                    "event_source_id": "host-created",
                    "source_collection": "Host",
                    "event_kind": "created",
                    "filter": null
                }),
            )]),
        }
    }

    #[test]
    fn task_summary_marks_runnable_and_counts_triggers() {
        let inventory = sample_inventory();
        let summary = inventory.task_summary(&inventory.tasks[1]);

        assert_eq!(
            summary.get("task_id").and_then(Value::as_str),
            Some("host-check")
        );
        assert_eq!(summary.get("runnable").and_then(Value::as_bool), Some(true));
        assert_eq!(
            summary
                .get("goal_objective_template")
                .and_then(Value::as_str),
            Some("Finish host {{ args.host }}")
        );
        assert_eq!(
            summary.get("goal_token_budget").and_then(Value::as_i64),
            Some(12_000)
        );
        assert!(summary
            .get("unavailable_reason")
            .is_some_and(Value::is_null));
        assert_eq!(
            summary.get("trigger_count").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            summary
                .pointer("/behavior/context_id")
                .and_then(Value::as_str),
            Some("default-context")
        );
    }

    #[test]
    fn task_summary_marks_disabled_task_unavailable() {
        let inventory = sample_inventory();
        let summary = inventory.task_summary(&inventory.tasks[0]);

        assert_eq!(
            summary.get("runnable").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            summary.get("unavailable_reason").and_then(Value::as_str),
            Some("task_disabled")
        );
    }

    #[test]
    fn task_inventory_query_filters_task_and_related_rows_for_show() {
        let query = task_inventory_query(Some("host-check"));
        assert!(query.contains("Task(filter:"));
        assert!(query.contains(r#"task_id: { _eq: "host-check" }"#));
        assert!(query.contains("limit: 1"));
        assert!(query.contains("Trigger(filter:"));
        assert!(query.contains("Schedule {"));
        assert!(query.contains("EventSource {"));
    }
}
