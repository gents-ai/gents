use crate::support::*;

use std::{fs, time::Duration};

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_updates_backend_from_canonical_bundle_over_graphql() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    let model = format!("graphql-apply-{}", Uuid::new_v4().simple());
    let endpoint = MockModelEndpoint::start(&model)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home,
        &[
            "--agent-name",
            "graphql-apply",
            "--model-name",
            &model,
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    run_cli_text(
        &home,
        &["config", "export", "--root", root.to_str().unwrap()],
    )?;
    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    let backend_id = config["inference_backends"][0]["backend_id"]
        .as_str()
        .context("exported backend id")?
        .to_string();
    let updated_endpoint = "http://127.0.0.1:9200/v1";
    config["inference_backends"][0]["endpoint"] = Value::String(updated_endpoint.to_string());
    write_json_file(&path, &config)?;

    let mut server = spawn_server(&home, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
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
    assert_eq!(applied["applied"]["inference_backends"], 1);
    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{ InferenceBackend(filter: {{ backend_id: {{ _eq: "{}" }} }}, limit: 1) {{ endpoint }} }}"#,
            escape_graphql_string(&backend_id)
        ),
    )
    .await?;
    assert_eq!(
        first_graphql_row(&response, "InferenceBackend")?["endpoint"],
        updated_endpoint
    );
    Ok(())
}
