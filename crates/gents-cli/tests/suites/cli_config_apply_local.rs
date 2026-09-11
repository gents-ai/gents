use crate::support::*;

use std::fs;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_updates_backend_from_canonical_bundle_locally() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    let model = format!("local-apply-{}", Uuid::new_v4().simple());
    let endpoint = MockModelEndpoint::start(&model)?;
    run_init_json(
        &home,
        &[
            "--agent-name",
            "local-apply",
            "--model-name",
            &model,
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    run_cli_text(
        &home,
        &["config", "export", "--root", root.to_str().unwrap()],
    )?;
    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    config["inference_backends"][0]["endpoint"] =
        Value::String("http://127.0.0.1:9201/v1".to_string());
    write_json_file(&path, &config)?;
    let applied = run_cli_json(
        &home,
        &["config", "apply", "--root", root.to_str().unwrap()],
    )?;
    assert_eq!(applied["applied"]["inference_backends"], 1);
    let diff = run_cli_json(&home, &["config", "diff", "--root", root.to_str().unwrap()])?;
    assert_eq!(diff["status"], "diffed");
    assert_eq!(diff["counts"]["inference_backends"]["unchanged"], 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_prunes_live_only_task_from_canonical_bundle() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    run_init_json(&home, &["--agent-name", "local-prune"])?;
    run_cli_text(
        &home,
        &["config", "export", "--root", root.to_str().unwrap()],
    )?;
    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    let behavior_id = config["agent_principal"]["default_behavior_id"]
        .as_str()
        .context("default behavior")?
        .to_string();
    config["tasks"] = json!([{
        "task_id": "temporary-task",
        "behavior_id": behavior_id,
        "prompt_template": "temporary"
    }]);
    write_json_file(&path, &config)?;
    run_cli_json(
        &home,
        &["config", "apply", "--root", root.to_str().unwrap()],
    )?;
    config["tasks"] = json!([]);
    write_json_file(&path, &config)?;
    let pruned = run_cli_json(
        &home,
        &[
            "config",
            "apply",
            "--root",
            root.to_str().unwrap(),
            "--prune",
        ],
    )?;
    assert_eq!(pruned["pruned"]["tasks"], 1);
    Ok(())
}
