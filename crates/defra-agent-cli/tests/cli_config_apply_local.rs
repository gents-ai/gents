mod support;
use support::*;

use std::fs;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_updates_backend_from_fresh_init_home_locally() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-apply-local-backend-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-apply-local-backend-{}", Uuid::new_v4().simple());

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
    let updated_endpoint = "http://127.0.0.1:9100/v1";
    backends[0]["endpoint"] = Value::String(updated_endpoint.to_string());
    write_json_file(&backends_path, &backends)?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let explicit_home = home_dir.join(".defra-agent");
    let explicit_home_str = explicit_home
        .to_str()
        .ok_or_else(|| anyhow!("explicit home path is not UTF-8"))?;
    let applied = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
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

    let reexported = run_cli_json(
        &home_dir,
        &["config", "export", "--home", explicit_home_str],
    )?;
    assert_eq!(
        reexported
            .pointer("/inference_backends/0/endpoint")
            .and_then(Value::as_str),
        Some(updated_endpoint)
    );

    let noop = run_cli_json(
        &home_dir,
        &[
            "config",
            "apply",
            "--root",
            root_str,
            "--home",
            explicit_home_str,
        ],
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
