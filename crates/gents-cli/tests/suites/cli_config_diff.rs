use crate::support::*;

use std::fs;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_diff_reports_no_changes_for_matching_live_state() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-diff-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-diff-{}", Uuid::new_v4().simple());

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;

    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "diff",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    assert_eq!(output.get("status").and_then(Value::as_str), Some("diffed"));
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        output.get("access_mode").and_then(Value::as_str),
        Some("local")
    );
    assert_eq!(
        output
            .pointer("/counts/agent_principal/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/agent_behaviors/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/tools/unchanged")
            .and_then(Value::as_u64),
        Some(2)
    );
    assert_eq!(
        output
            .pointer("/counts/inference_backends/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/inference_profiles/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_diff_reports_updates_for_changed_backend_manifest() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-diff-update-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-diff-update-{}", Uuid::new_v4().simple());

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;

    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;

    let config_path = root.join("pack_config.json");
    let mut config = read_json_file(&config_path)?;
    let backend_id = config["inference_backends"][0]["backend_id"]
        .as_str()
        .context("exported backend id")?
        .to_string();
    config["inference_backends"][0]["endpoint"] =
        Value::String("http://127.0.0.1:9000/v1".to_string());
    write_json_file(&config_path, &config)?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "diff",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    assert_eq!(output.get("status").and_then(Value::as_str), Some("diffed"));
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        output
            .pointer("/counts/inference_backends/update")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/collections/inference_backends/update/0")
            .and_then(Value::as_str),
        Some(backend_id.as_str())
    );
    assert_eq!(
        output
            .pointer("/counts/agent_behaviors/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );

    Ok(())
}
