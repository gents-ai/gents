use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_read_commands_list_and_show_trigger_schedule_and_mcp() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-config-read-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-config-read-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    run_cli_text(
        &home_dir,
        &["config", "export", "--root", &root.to_string_lossy()],
    )?;
    let principal = read_json_file(&root.join("agent-principal.json"))?;
    let behavior_id = principal
        .get("default_behavior_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing default_behavior_id after export"))?
        .to_string();

    let task_id = format!("{agent_name}-task");
    let schedule_id = format!("{agent_name}-schedule");
    let trigger_id = format!("{agent_name}-trigger");
    let service_id = format!("{agent_name}-mcp");

    write_json_file(
        &root.join("tasks").join(&task_id).join("object.json"),
        &serde_json::json!({
            "task_id": task_id.clone(),
            "name": "Read Commands Task",
            "description": "Seeds config read command coverage.",
            "behavior_id": behavior_id.clone(),
            "prompt_template": "Report config read status.",
            "enabled": false,
        }),
    )?;
    write_json_file(
        &root
            .join("schedules")
            .join(&schedule_id)
            .join("object.json"),
        &serde_json::json!({
            "schedule_id": schedule_id.clone(),
            "task_id": task_id.clone(),
            "interval_secs": 3600,
            "enabled": false,
            "concurrency": "serial",
        }),
    )?;
    write_json_file(
        &root
            .join("event_triggers")
            .join(&trigger_id)
            .join("object.json"),
        &serde_json::json!({
            "trigger_id": trigger_id.clone(),
            "task_id": task_id.clone(),
            "source_collection": "InferenceBackend",
            "event_kind": "created",
            "enabled": false,
            "concurrency": "serial",
        }),
    )?;
    write_json_file(
        &root
            .join("tool-services")
            .join(&service_id)
            .join("object.json"),
        &serde_json::json!({
            "service_id": service_id.clone(),
            "display_name": "Read Commands MCP",
            "description": "Seeded MCP service.",
            "hostname": "localhost",
            "tailscale_ip": "",
            "lan_ip": "",
            "mcp_port": 3030,
            "mcp_path": "/mcp",
            "send_agent_did": true,
        }),
    )?;

    let apply = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            &root.to_string_lossy(),
            "--graphql",
            &graphql,
        ],
    )?;
    assert_eq!(apply.get("ok").and_then(Value::as_bool), Some(true));

    assert_list_show(
        &home_dir,
        &graphql,
        &["config", "trigger"],
        "EventTrigger",
        "trigger_id",
        &trigger_id,
    )?;
    assert_list_show(
        &home_dir,
        &graphql,
        &["config", "schedule"],
        "Schedule",
        "schedule_id",
        &schedule_id,
    )?;
    assert_list_show(
        &home_dir,
        &graphql,
        &["config", "mcp"],
        "ToolServiceRegistry",
        "service_id",
        &service_id,
    )?;

    Ok(())
}

fn assert_list_show(
    home_dir: &std::path::Path,
    graphql: &str,
    command_prefix: &[&str],
    collection: &str,
    unique_field: &str,
    id: &str,
) -> Result<()> {
    let mut list_args = command_prefix.to_vec();
    list_args.extend(["list", "--graphql", graphql, "--output", "json"]);
    let list = run_cli_json(home_dir, &list_args)?;
    let contains_id = list
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .any(|row| row.get(unique_field).and_then(Value::as_str) == Some(id))
        })
        .unwrap_or(false);
    assert!(
        contains_id,
        "{collection} list did not contain {id}: {list}"
    );

    let mut show_args = command_prefix.to_vec();
    show_args.extend(["show", "--graphql", graphql, id]);
    let show = run_cli_json(home_dir, &show_args)?;
    assert_eq!(show.get(unique_field).and_then(Value::as_str), Some(id));

    Ok(())
}
