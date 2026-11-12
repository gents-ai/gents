use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::{json, Value};

use crate::cli::{TaskCommand, TaskListArgs, TaskShowArgs};
use crate::commands::config::task_run::{config_task_run, resolve_task_id_for};
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access};

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

pub(crate) struct TaskInventory {
    pub(crate) tasks: Vec<Value>,
    agents_by_id: BTreeMap<(String, String), Value>,
    pub(crate) triggers: Vec<Value>,
    pub(crate) schedules_by_id: BTreeMap<String, Value>,
    pub(crate) event_sources_by_id: BTreeMap<String, Value>,
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
        let agent_did = string_field(task, "agent_did");
        let agent = self.agent_for(task);
        let triggers = self.triggers_for_task(&task_id);
        let (runnable, unavailable_reason) = runnable_status(task, agent);

        json!({
            "task_id": task_id,
            "display_name": task.get("display_name").cloned().unwrap_or(Value::Null),
            "description": task.get("description").cloned().unwrap_or(Value::Null),
            "agent_did": agent_did,
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
        let agent = self.agent_for(task);
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

    fn agent_for(&self, task: &Value) -> Option<&Value> {
        let key = string_field(task, "node_did").zip(string_field(task, "agent_did"))?;
        self.agents_by_id.get(&key)
    }

    fn triggers_for_task(&self, task_id: &str) -> Vec<Value> {
        self.triggers
            .iter()
            .filter(|row| string_field(row, "task_id").as_deref() == Some(task_id))
            .map(|row| self.enrich_trigger(row))
            .collect::<Vec<_>>()
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

pub(crate) async fn load_task_inventory(
    access: &ConfigAccess,
    task_id_filter: Option<&str>,
) -> Result<TaskInventory> {
    let node_did = crate::commands::config::binding::resolve_target_node_did(
        None,
        Some(crate::cli::ManifestNodeDidBindingArg::Live),
        None,
        None,
        Some(access),
    )
    .await?;
    let inventory = access
        .transact_readonly("task.inventory", |txn| {
            let node_did = &node_did;
            Box::pin(async move {
                gents_documents::views::tasks::inventory(txn, node_did, false, task_id_filter)
                    .await
                    .map_err(anyhow::Error::from)
            })
        })
        .await?;
    let tasks = inventory
        .tasks
        .iter()
        .map(|row| enabled_document(&row.task, row.task.enabled))
        .collect::<Result<Vec<_>>>()?;
    let agents_by_id = inventory
        .agents
        .iter()
        .map(|row| {
            Ok((
                (row.agent.node_did.clone(), row.agent.agent_did.clone()),
                enabled_document(&row.agent, row.agent.enabled)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let triggers = inventory
        .triggers
        .iter()
        .map(|row| enabled_document(row, row.trigger.enabled))
        .collect::<Result<Vec<_>>>()?;
    let schedules_by_id = inventory
        .schedules
        .iter()
        .map(|row| {
            Ok((
                row.schedule.schedule_id.clone(),
                serde_json::to_value(&row.schedule)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    let event_sources_by_id = inventory
        .event_sources
        .iter()
        .map(|row| {
            Ok((
                row.event_source.event_source_id.clone(),
                serde_json::to_value(&row.event_source)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(TaskInventory {
        tasks,
        agents_by_id,
        triggers,
        schedules_by_id,
        event_sources_by_id,
    })
}

fn enabled_document<T>(document: &T, enabled: bool) -> Result<Value>
where
    T: serde::Serialize,
{
    let mut value = serde_json::to_value(document)?;
    value["enabled"] = json!(enabled);
    Ok(value)
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

fn agent_summary(agent: &Value) -> Option<Value> {
    let agent_did = string_field(agent, "agent_did")?;
    Some(json!({
        "agent_did": agent_did,
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
    if string_field(task, "agent_did").is_none() {
        return (false, Some("missing_agent_did"));
    }
    let Some(agent) = agent else {
        return (false, Some("agent_missing"));
    };
    if !bool_field(agent, "enabled").unwrap_or(false) {
        return (false, Some("agent_disabled"));
    }
    (true, None)
}
