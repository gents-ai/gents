mod support;
use support::*;

use std::fs;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_updates_backend_from_fresh_init_home_over_graphql() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!(
        "mock-apply-graphql-backend-model-{}",
        Uuid::new_v4().simple()
    );
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-apply-graphql-backend-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;

    let exported = run_cli_json(&home_dir, &["config", "export"])?;
    assert!(exported
        .pointer("/inference_backends/0/last_probe")
        .is_none_or(Value::is_null));
    write_manifest_root_from_export(&root, &exported)?;

    let backends_path = root.join("inference-backends.json");
    let mut backends = read_json_file(&backends_path)?;
    let updated_endpoint = "http://127.0.0.1:9200/v1";
    let backend_id = backends[0]
        .get("backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest backend is missing backend_id"))?
        .to_string();
    backends[0]["endpoint"] = Value::String(updated_endpoint.to_string());
    write_json_file(&backends_path, &backends)?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let applied = run_cli_json(
        &home_dir,
        &["config", "apply", "--root", root_str, "--graphql", &graphql],
    )?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(applied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(applied.get("changed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        applied
            .pointer("/applied/inference_backends")
            .and_then(Value::as_u64),
        Some(1)
    );

    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                InferenceBackend(
                    filter: {{ backend_id: {{ _eq: "{}" }} }},
                    limit: 1
                ) {{
                    endpoint
                    probe_status
                    last_probe
                }}
            }}"#,
            escape_graphql_string(&backend_id),
        ),
    )
    .await?;
    let backend_row = first_graphql_row(&response, "InferenceBackend")?;
    assert_eq!(
        backend_row.get("endpoint").and_then(Value::as_str),
        Some(updated_endpoint)
    );
    assert_eq!(
        backend_row.get("probe_status").and_then(Value::as_str),
        Some("healthy")
    );
    assert!(backend_row.get("last_probe").is_none_or(Value::is_null));

    let noop = run_cli_json(
        &home_dir,
        &["config", "apply", "--root", root_str, "--graphql", &graphql],
    )?;
    assert_eq!(noop.get("status").and_then(Value::as_str), Some("noop"));
    assert_eq!(noop.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(noop.get("changed").and_then(Value::as_bool), Some(false));
    assert_eq!(
        noop.pointer("/applied/inference_backends")
            .and_then(Value::as_u64),
        Some(0)
    );

    Ok(())
}
