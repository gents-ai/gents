mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_backend_discover_models_supports_explicit_preset_probe() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("discover-openrouter-{}", Uuid::new_v4().simple());
    let raw_api_key = "discover-openrouter-key";
    let mock_endpoint =
        MockModelEndpoint::start_with_required_bearer(&model_name, Some(raw_api_key))?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "backend",
            "discover-models",
            "--backend-preset",
            "openrouter",
            "--endpoint",
            mock_endpoint.endpoint(),
            "--api-key",
            raw_api_key,
        ],
    )?;

    assert_eq!(
        output.get("provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );
    assert_eq!(
        output.get("endpoint").and_then(Value::as_str),
        Some(mock_endpoint.endpoint())
    );
    assert_eq!(
        output
            .pointer("/discovered_models/0")
            .and_then(Value::as_str),
        Some(model_name.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_backend_set_preset_and_discover_models_from_backend_id() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let bootstrap_model = format!("bootstrap-model-{}", Uuid::new_v4().simple());
    let bootstrap_endpoint = MockModelEndpoint::start(&bootstrap_model)?;
    let discover_model = format!("discover-backend-id-{}", Uuid::new_v4().simple());
    let discover_api_key = "stored-openrouter-key";
    let discover_endpoint =
        MockModelEndpoint::start_with_required_bearer(&discover_model, Some(discover_api_key))?;

    let port = allocate_port()?;
    let agent_name = format!("cli-backend-preset-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &bootstrap_model,
            bootstrap_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let backend_id = format!("{agent_name}-openrouter");
    let upsert = run_cli_json(
        &home_dir,
        &[
            "config",
            "backend",
            "set",
            "--graphql",
            &graphql,
            "--backend-id",
            &backend_id,
            "--name",
            "OpenRouter",
            "--backend-preset",
            "openrouter",
            "--endpoint",
            discover_endpoint.endpoint(),
            "--api-key",
            discover_api_key,
            "--max-concurrent",
            "2",
        ],
    )?;

    assert_eq!(
        upsert.get("backend_preset").and_then(Value::as_str),
        Some("openrouter")
    );
    assert_eq!(
        upsert.get("provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );
    assert_eq!(
        upsert.get("endpoint").and_then(Value::as_str),
        Some(discover_endpoint.endpoint())
    );

    let discover = run_cli_json(
        &home_dir,
        &[
            "config",
            "backend",
            "discover-models",
            "--graphql",
            &graphql,
            "--backend-id",
            &backend_id,
        ],
    )?;

    assert_eq!(
        discover
            .pointer("/discovered_models/0")
            .and_then(Value::as_str),
        Some(discover_model.as_str())
    );
    assert_eq!(
        discover.get("provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );

    let backend_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                InferenceBackend(filter: {{ backend_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    backend_id
                    provider_kind
                    endpoint
                    api_key
                    api_key_env_var
                }}
            }}"#,
            escape_graphql_string(&backend_id),
        ),
    )
    .await?;
    let backend = first_graphql_row(&backend_rows, "InferenceBackend")?;
    assert_eq!(
        backend.get("provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );
    assert_eq!(
        backend.get("endpoint").and_then(Value::as_str),
        Some(discover_endpoint.endpoint())
    );
    assert_eq!(
        backend.get("api_key").and_then(Value::as_str),
        Some(discover_api_key)
    );
    assert_eq!(backend.get("api_key_env_var").and_then(Value::as_str), None);

    Ok(())
}
