use anyhow::Result;
use serde_json::{json, Map, Value};

use crate::cli::*;
use crate::config_writes::{write_scheduled_task_document, ConfigAccess};
use crate::print_json;
use crate::{
    normalize_optional_rfc3339, require_non_empty, resolve_agent_did, resolve_graphql_endpoint,
    resolve_scheduled_task_behavior_id, resolve_task_prompt,
};

pub(super) async fn scheduled_task_set(args: ScheduledTaskSetArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let access = ConfigAccess::Graphql(graphql.clone());
    let agent_did = resolve_agent_did(args.home.as_deref(), args.agent_did.as_deref())?;
    let task_id = require_non_empty("task_id", &args.task_id)?;
    let name = require_non_empty("name", &args.name)?;
    if args.interval_secs <= 0 {
        anyhow::bail!("--interval-secs must be greater than zero");
    }

    let prompt = resolve_task_prompt(args.prompt.as_deref(), args.prompt_file.as_deref())?;
    let behavior_id =
        resolve_scheduled_task_behavior_id(&graphql, &agent_did, args.behavior_id.as_deref())
            .await?;
    let next_run_at = normalize_optional_rfc3339(args.next_run_at.as_deref())?;
    let mut add_doc = Map::new();
    add_doc.insert("task_id".to_string(), Value::String(task_id.to_string()));
    add_doc.insert("agent_did".to_string(), Value::String(agent_did.clone()));
    add_doc.insert(
        "behavior_id".to_string(),
        Value::String(behavior_id.clone()),
    );
    add_doc.insert("name".to_string(), Value::String(name.to_string()));
    add_doc.insert("prompt".to_string(), Value::String(prompt.clone()));
    add_doc.insert("interval_secs".to_string(), Value::from(args.interval_secs));
    add_doc.insert("enabled".to_string(), Value::Bool(args.enabled));
    if let Some(next_run_at) = next_run_at.as_ref() {
        add_doc.insert(
            "next_run_at".to_string(),
            Value::String(next_run_at.clone()),
        );
    }

    let update_doc = add_doc.clone();

    let add_doc = Value::Object(add_doc);
    let update_doc = Value::Object(update_doc);

    let doc_id = write_scheduled_task_document(&access, task_id, &add_doc, &update_doc).await?;
    let output = json!({
        "doc_id": doc_id,
        "task_id": task_id,
        "agent_did": agent_did,
        "behavior_id": behavior_id,
        "name": name,
        "interval_secs": args.interval_secs,
        "enabled": args.enabled,
        "next_run_at": next_run_at,
    });
    print_json(&output)?;
    Ok(())
}
