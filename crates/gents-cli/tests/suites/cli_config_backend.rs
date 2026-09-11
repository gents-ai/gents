use crate::support::*;

use std::{fs, time::Duration};

use anyhow::{Context, Result};
use serde_json::json;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_backend_discover_models_supports_explicit_probe() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    fs::create_dir_all(&home)?;
    let model = format!("discover-model-{}", Uuid::new_v4().simple());
    let endpoint = MockModelEndpoint::start(&model)?;
    let output = run_cli_json(
        &home,
        &[
            "config",
            "backend",
            "discover-models",
            "--provider-kind",
            "OpenAiCompatible",
            "--endpoint",
            endpoint.endpoint(),
        ],
    )?;
    assert!(output["discovered_models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["model_name"] == model));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_backend_set_accepts_canonical_document_and_supports_discovery() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("home");
    fs::create_dir_all(&home)?;
    let model = format!("backend-model-{}", Uuid::new_v4().simple());
    let endpoint = MockModelEndpoint::start(&model)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(&home, &["--agent-name", "backend-agent"])?;
    let owner = agent_did_from_init(&init)?;
    let mut server = spawn_server(&home, port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &owner, Duration::from_secs(30)).await?;
    let file = tempdir.path().join("backend.json");
    write_json_file(
        &file,
        &json!({
            "agent_did":owner,
            "backend_id":"extra-backend",
            "name":"Extra backend",
            "provider_kind":"OpenAiCompatible",
            "endpoint":endpoint.endpoint(),
            "auth":{"kind":"unauthenticated"}
        }),
    )?;
    assert_eq!(
        run_cli_json(
            &home,
            &[
                "config",
                "backend",
                "set",
                "--graphql",
                &graphql,
                "--file",
                file.to_str().unwrap()
            ],
        )?["backend_id"],
        "extra-backend"
    );
    let discovered = run_cli_json(
        &home,
        &[
            "config",
            "backend",
            "discover-models",
            "--graphql",
            &graphql,
            "--backend-id",
            "extra-backend",
        ],
    )?;
    assert!(discovered["discovered_models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["model_name"] == model));
    drop(server);
    Ok(())
}

#[test]
fn config_backend_discover_models_write_requires_backend_id() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let stderr = run_cli_failure_stderr(
        tempdir.path(),
        &["config", "backend", "discover-models", "--write"],
    )?;
    assert!(stderr.contains("--write requires --backend-id"), "{stderr}");
    Ok(())
}
