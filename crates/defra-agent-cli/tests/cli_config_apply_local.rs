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

    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;

    let backends_dir = root.join("inference-backends");
    let backend_entry = fs::read_dir(&backends_dir)
        .context("reading inference-backends dir after export")?
        .next()
        .ok_or_else(|| anyhow!("no inference-backend subdirs after export"))??;
    let backend_id = backend_entry
        .file_name()
        .to_str()
        .ok_or_else(|| anyhow!("non-utf8 backend dir name"))?
        .to_string();
    let backends_path = root
        .join("inference-backends")
        .join(&backend_id)
        .join("object.json");
    let mut backend = read_json_file(&backends_path)?;
    let updated_endpoint = "http://127.0.0.1:9100/v1";
    backend["endpoint"] = Value::String(updated_endpoint.to_string());
    write_json_file(&backends_path, &backend)?;

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

    let reexport_root = tempdir.path().join("reexport");
    let reexport_root_str = reexport_root
        .to_str()
        .ok_or_else(|| anyhow!("reexport root path is not UTF-8"))?;
    run_cli_text(
        &home_dir,
        &[
            "config",
            "export",
            "--root",
            reexport_root_str,
            "--home",
            explicit_home_str,
        ],
    )?;
    let reexported_backend = read_json_file(
        &reexport_root
            .join("inference-backends")
            .join(&backend_id)
            .join("object.json"),
    )?;
    assert_eq!(
        reexported_backend.get("endpoint").and_then(Value::as_str),
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
