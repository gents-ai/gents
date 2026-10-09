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
            "--node-name",
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
    run_init_json(&home, &["--node-name", "local-prune"])?;
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
        "task_id": "temporary-task",
        "agent_id": agent_id,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_refuses_a_count_field_no_count_can_come_back_through() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    run_init_json(&home, &["--node-name", "event-source-count"])?;
    run_cli_text(
        &home,
        &["config", "export", "--root", root.to_str().unwrap()],
    )?;
    let path = root.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    config["event_sources"] = json!([{
        "event_source_id": "backend-created",
        "source_collection": "InferenceBackend",
        "event_kind": "created",
        "correlation_field": "backend_id",
        "group": {"expected_count": {"source_field": "enabled"}},
    }]);
    write_json_file(&path, &config)?;
    let stderr = run_cli_failure_stderr(
        &home,
        &["config", "apply", "--root", root.to_str().unwrap()],
    )?;
    assert!(
        stderr.contains("cannot carry the count"),
        "the publishing transaction must refuse the count field: {stderr}"
    );
    assert!(stderr.contains("Boolean"), "{stderr}");

    config["event_sources"][0]["group"] =
        json!({"expected_count": {"source_field": "max_queue_depth"}});
    write_json_file(&path, &config)?;
    let applied = run_cli_json(
        &home,
        &["config", "apply", "--root", root.to_str().unwrap()],
    )?;
    assert_eq!(applied["applied"]["event_sources"], 1);
    Ok(())
}
