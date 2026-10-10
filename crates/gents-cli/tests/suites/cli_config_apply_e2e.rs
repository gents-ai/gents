use crate::support::*;

use std::{fs, time::Duration};

use anyhow::{Context, Result};
use serde_json::json;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_reconciles_canonical_event_source_and_trigger() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    let model = format!("event-apply-{}", Uuid::new_v4().simple());
    let endpoint = MockModelEndpoint::start(&model)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home,
        &[
            "--node-name",
            "event-apply",
            "--model-name",
            &model,
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    run_cli_text(
        &home,
        &["config", "export", "--root", root.to_str().unwrap()],
    )?;
    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    let agent_id = config["node"]["default_agent_id"]
        .as_str()
        .context("default agent")?
        .to_string();
    config["tasks"] = json!([{
        "task_id": "event-task",
        "agent_id": agent_id,
        "prompt_template": "Observe {{ doc.backend_id }}",
        "enabled": false
    }]);
    config["event_sources"] = json!([{
        "event_source_id": "backend-created",
        "source_collection": "InferenceBackend",
        "event_kind": "created"
    }]);
    config["triggers"] = json!([{
        "trigger_id": "backend-created",
        "task_id": "event-task",
        "source": {"kind": "event", "event_source_id": "backend-created"},
        "concurrency": "serial",
        "enabled": false
    }]);
    write_json_file(&path, &config)?;

    let mut server = spawn_server(&home, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    let applied = run_cli_json(
        &home,
        &[
            "config",
            "apply",
            "--root",
            root.to_str().unwrap(),
            "--graphql",
            &graphql,
        ],
    )?;
    assert_eq!(applied["applied"]["event_sources"], 1);
    assert_eq!(applied["applied"]["triggers"], 1);
    let response = graphql_query(
        &graphql,
        r#"{ Trigger(filter: { trigger_id: { _eq: "backend-created" } }, limit: 1) { trigger_id task_id source } }"#,
    )
    .await?;
    let trigger = first_graphql_row(&response, "Trigger")?;
    assert_eq!(trigger["task_id"], "event-task");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_round_trips_canonical_datastore_surface() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    let exported = tempdir.path().join("exported");
    let init = run_init_json(&home, &["--node-name", "surface-roundtrip"])?;
    let node_did = node_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    run_cli_text(
        &home,
        &["config", "export", "--root", root.to_str().unwrap()],
    )?;
    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    config["datastore_tool_surfaces"] = json!([{
        "surface_id": "backend-read",
        "display_name": "Backend reader",
        "entries": [{
            "tool_name": "read_backend",
            "collection": "InferenceBackend",
            "description": "Read backend records",
            "kind": "query",
            "fields": ["backend_id"]
        }]
    }]);
    write_json_file(&path, &config)?;
    let mut server = spawn_server(&home, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
    run_cli_json(
        &home,
        &[
            "config",
            "apply",
            "--root",
            root.to_str().unwrap(),
            "--graphql",
            &graphql,
        ],
    )?;
    run_cli_text(
        &home,
        &[
            "config",
            "export",
            "--root",
            exported.to_str().unwrap(),
            "--graphql",
            &graphql,
            "--node-did",
            &node_did,
        ],
    )?;
    let round_trip = read_json_file(&exported.join("pack_config.json"))?;
    assert_eq!(
        round_trip["datastore_tool_surfaces"][0]["surface_id"],
        "backend-read"
    );
    Ok(())
}
