use crate::support::*;

use std::{fs, time::Duration};

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_reconciles_running_context_without_restart() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    let model = format!("running-apply-{}", Uuid::new_v4().simple());
    let endpoint = MockModelEndpoint::start(&model)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home,
        &[
            "--agent-name",
            "running-apply",
            "--model-name",
            &model,
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut server = spawn_server(&home, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    run_cli_text(
        &home,
        &[
            "config",
            "export",
            "--root",
            root.to_str().unwrap(),
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
        ],
    )?;
    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    let context_id = config["contexts"][0]["context_id"]
        .as_str()
        .context("context id")?
        .to_string();
    config["contexts"][0]["system_prompt"] = Value::String("updated live prompt".to_string());
    write_json_file(&path, &config)?;
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
    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{ AgentContext(filter: {{ context_id: {{ _eq: "{}" }} }}, limit: 1) {{ system_prompt }} }}"#,
            escape_graphql_string(&context_id)
        ),
    )
    .await?;
    assert_eq!(
        first_graphql_row(&response, "AgentContext")?["system_prompt"],
        "updated live prompt"
    );
    Ok(())
}
