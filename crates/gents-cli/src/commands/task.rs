use std::collections::BTreeMap;

use anyhow::Result;
use gents::graphql::escape_graphql_string;
use serde_json::{json, Value};

use crate::cli::{TaskCommand, TaskListArgs, TaskShowArgs};
use crate::commands::config::task_run::{config_task_run, resolve_task_id_for};
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access};

#[derive(Debug, thiserror::Error)]
pub(crate) enum TaskCommandError {
    #[error("no Task with task_id = {task_id}")]
    NotFound { task_id: String },
    #[error("{context} failed: {errors}")]
    GraphqlErrors { context: String, errors: String },
    #[error(transparent)]
    Config(#[from] gents::config_client::ConfigClientError),
}

const TASK_FIELDS: &str = "task_id node_did display_name description agent_id prompt_template emit_outcome goal_objective_template goal_token_budget hooks enabled output_schema_ref created_at updated_at tags";
const BEHAVIOR_FIELDS: &str = "agent_id node_did display_name description context_id inference_profile_id enabled tags created_at updated_at";
const TRIGGER_FIELDS: &str = "trigger_id node_did display_name description task_id source session_id_template enabled concurrency next_run_at last_attempt_at last_fired_source_doc_id last_status last_error fire_count created_at updated_at tags";
const SCHEDULE_FIELDS: &str =
    "schedule_id node_did display_name cadence created_at updated_at tags";
const EVENT_SOURCE_FIELDS: &str = "event_source_id node_did display_name source_collection event_kind filter correlation_field group workspace_authority created_at updated_at tags";

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
        return Err(TaskCommandError::NotFound { task_id }.into());
    };
    print_json(&inventory.task_detail(task))?;
    Ok(())
}

struct TaskInventory {
    tasks: Vec<Value>,
    agents_by_id: BTreeMap<String, Value>,
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
        let agent_id = string_field(task, "agent_id");
        let agent = agent_id.as_deref().and_then(|id| self.agents_by_id.get(id));
        let triggers = self.triggers_for_task(&task_id);
        let (runnable, unavailable_reason) = runnable_status(task, agent);

        json!({
            "task_id": task_id,
            "display_name": task.get("display_name").cloned().unwrap_or(Value::Null),
            "description": task.get("description").cloned().unwrap_or(Value::Null),
            "agent_id": agent_id,
            "goal_objective_template": task.get("goal_objective_template").cloned().unwrap_or(Value::Null),
            "goal_token_budget": task.get("goal_token_budget").cloned().unwrap_or(Value::Null),
            "enabled": bool_field(task, "enabled").unwrap_or(false),
            "runnable": runnable,
            "unavailable_reason": unavailable_reason,
            "agent": agent.and_then(agent_summary).unwrap_or(Value::Null),
            "trigger_count": triggers.len(),
            "trigger_ids": triggers.iter().filter_map(|row| string_field(row, "trigger_id")).collect::<Vec<_>>(),
        })
    }

    fn task_detail(&self, task: &Value) -> Value {
        let task_id = string_field(task, "task_id").unwrap_or_default();
        let agent = string_field(task, "agent_id")
            .as_deref()
            .and_then(|id| self.agents_by_id.get(id));
        let triggers = self.triggers_for_task(&task_id);
        let (runnable, unavailable_reason) = runnable_status(task, agent);

        json!({
            "task": task,
            "agent": agent.cloned().unwrap_or(Value::Null),
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
) -> Result<TaskInventory, TaskCommandError> {
    let query = task_inventory_query(task_id_filter);
    let response = access.execute(&query).await?;
    ensure_no_graphql_errors(&response, "query task inventory")?;

    let mut tasks = rows(&response, "Task");
    sort_rows_by_string_field(&mut tasks, "task_id");

    let agents_by_id = rows(&response, "Agent")
        .into_iter()
        .filter_map(|row| string_field(&row, "agent_id").map(|id| (id, row)))
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
        agents_by_id,
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
            Agent {{
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

fn ensure_no_graphql_errors(response: &Value, context: &str) -> Result<(), TaskCommandError> {
    if let Some(errors) = response
        .get("errors")
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    {
        return Err(TaskCommandError::GraphqlErrors {
            context: context.to_owned(),
            errors: format!("{errors:?}"),
        });
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

fn agent_summary(agent: &Value) -> Option<Value> {
    let agent_id = string_field(agent, "agent_id")?;
    Some(json!({
        "agent_id": agent_id,
        "node_did": agent.get("node_did").cloned().unwrap_or(Value::Null),
        "display_name": agent.get("display_name").cloned().unwrap_or(Value::Null),
        "enabled": bool_field(agent, "enabled").unwrap_or(false),
        "context_id": agent.get("context_id").cloned().unwrap_or(Value::Null),
        "inference_profile_id": agent.get("inference_profile_id").cloned().unwrap_or(Value::Null),
    }))
}

fn runnable_status(task: &Value, agent: Option<&Value>) -> (bool, Option<&'static str>) {
    if !bool_field(task, "enabled").unwrap_or(false) {
        return (false, Some("task_disabled"));
    }
    if string_field(task, "agent_id").is_none() {
        return (false, Some("missing_agent_id"));
    }
    let Some(agent) = agent else {
        return (false, Some("agent_missing"));
    };
    if !bool_field(agent, "enabled").unwrap_or(false) {
        return (false, Some("agent_disabled"));
    }
    (true, None)
}
