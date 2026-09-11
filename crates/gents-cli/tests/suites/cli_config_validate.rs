use crate::support::*;

use std::fs;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use uuid::Uuid;

fn canonical_config(agent_did: &str) -> Value {
    json!({
        "agent_principal": {
            "agent_did": agent_did,
            "default_behavior_id": "default",
            "enabled": true
        },
        "agent_behaviors": [{
            "behavior_id": "default",
            "context_id": "default-context",
            "inference_profile_id": "default-profile",
            "enabled": true
        }],
        "contexts": [{
            "context_id": "default-context",
            "system_prompt": "Keep responses short.",
            "tools_id": "default-tools"
        }],
        "tools": [{
            "tools_id": "default-tools",
            "host": {
                "files": {"mode": "ReadOnly"},
                "bash": {"mode": "ReadOnly", "network_mode": "disabled"}
            }
        }],
        "inference_backends": [{
            "backend_id": "default-backend",
            "name": "default-backend",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:8000/v1",
            "auth": {"kind": "unauthenticated"}
        }],
        "inference_profiles": [{
            "profile_id": "default-profile",
            "backend_id": "default-backend",
            "model_name": "mock-model"
        }]
    })
}

fn write_config(root: &std::path::Path, config: &Value) -> Result<()> {
    fs::create_dir_all(root)?;
    write_json_file(&root.join("pack_config.json"), config)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_accepts_canonical_config() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    let agent_did = format!("did:key:z{}", Uuid::new_v4().simple());
    write_config(&root, &canonical_config(&agent_did))?;

    let output = run_cli_json(
        &home,
        &["config", "validate", "--root", root.to_str().unwrap()],
    )?;
    assert_eq!(output["status"], "validated");
    assert_eq!(output["ok"], true);
    assert_eq!(output["agent_did"], agent_did);
    assert_eq!(output["counts"]["contexts"], 1);
    assert_eq!(output["counts"]["tools"], 1);
    assert_eq!(output["counts"]["triggers"], 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_offline_scope_accepts_unresolved_canonical_references() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    let mut config = canonical_config("did:key:broken");
    config["contexts"][0]["tools_id"] = json!("missing-tools");
    write_config(&root, &config)?;

    let output = run_cli_json(
        &home,
        &["config", "validate", "--root", root.to_str().unwrap()],
    )?;
    assert_eq!(output["status"], "validated");
    assert_eq!(output["validation_scope"], "offline_shape");
    assert_eq!(output["errors"], json!([]));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_rejects_retired_flat_tools_fields() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    let mut config = canonical_config("did:key:broken-tools");
    config["tools"][0]["enable_bash"] = json!(true);
    write_config(&root, &config)?;
    let output = run_cli_failure_stdout_json(
        &home,
        &["config", "validate", "--root", root.to_str().unwrap()],
    )?;
    assert_eq!(output["ok"], false);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_bind_home_force_rebinds_local_owner() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let home = tempdir.path().join("home");
    let root = tempdir.path().join("config");
    fs::create_dir_all(&home)?;
    let init = run_init_json(&home, &["--agent-name", "validate-home"])?;
    let home_did = agent_did_from_init(&init)?;
    write_config(&root, &canonical_config("did:key:source"))?;

    let output = run_cli_json(
        &home,
        &[
            "config",
            "validate",
            "--root",
            root.to_str().unwrap(),
            "--bind-agent-did",
            "home",
            "--force-rebind-concrete-did",
        ],
    )?;
    assert_eq!(output["ok"], true);
    assert_eq!(output["agent_did"], home_did);
    Ok(())
}
