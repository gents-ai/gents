use crate::support::*;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provision_initializes_home_binds_manifest_and_diff_exact() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let target_home_env = tempdir.path().join("target-env");
    let target_home = tempdir.path().join("target-home");
    let root = tempdir
        .path()
        .join("infra")
        .join("agents")
        .join("mini-1")
        .join("mini-1-steward");
    fs::create_dir_all(&target_home_env)?;

    let placeholder_did = "did:test:mini-1-steward";
    write_portable_manifest_root(tempdir.path(), &root, placeholder_did)?;

    let report = run_cli_json(
        &target_home_env,
        &[
            "provision",
            "--home",
            target_home.to_str().expect("utf-8 home"),
            "--root",
            root.to_str().expect("utf-8 root"),
            "--bootstrap-file-identity",
        ],
    )?;
    assert_eq!(
        report.get("status").and_then(Value::as_str),
        Some("provisioned")
    );
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        report.pointer("/identity/status").and_then(Value::as_str),
        Some("initialized")
    );
    assert_eq!(
        report.pointer("/apply/ok").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        report.pointer("/diff/ok").and_then(Value::as_bool),
        Some(true)
    );
    let agent_did = agent_did_from_report(&report)?;
    assert_ne!(agent_did, placeholder_did);

    let reexport_root = tempdir.path().join("reexport");
    run_cli_text(
        &target_home_env,
        &[
            "config",
            "export",
            "--root",
            reexport_root.to_str().expect("utf-8 reexport root"),
            "--home",
            target_home.to_str().expect("utf-8 home"),
        ],
    )?;
    assert_manifest_agent_dids(&reexport_root, &agent_did)?;
    assert!(!manifest_contains(&reexport_root, placeholder_did)?);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provision_rejects_initialized_home_without_loadable_identity() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_env = tempdir.path().join("home-env");
    let home = tempdir.path().join("secure-enclave-home");
    let root = tempdir
        .path()
        .join("infra")
        .join("agents")
        .join("mini-2")
        .join("mini-2-steward");
    fs::create_dir_all(&home_env)?;
    fs::create_dir_all(&home)?;

    let placeholder_did = "did:test:mini-2-steward";
    let agent_did = format!("did:key:z{}", Uuid::new_v4().simple());
    write_json_file(
        &home.join("init.json"),
        &serde_json::json!({
            "home": home.to_string_lossy(),
            "agent_name": "mini-2-steward",
            "agent_did": agent_did,
            "key_path": null,
            "tool_ceiling": "Readonly",
            "tool_root": tempdir.path().to_string_lossy()
        }),
    )?;
    write_portable_manifest_root(tempdir.path(), &root, placeholder_did)?;

    let stderr = run_cli_failure_stderr(
        &home_env,
        &[
            "provision",
            "--home",
            home.to_str().expect("utf-8 home"),
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;
    assert!(
        stderr.contains("has no key_path and unsupported identity_backend"),
        "expected fail-closed unloadable-identity error, got:\n{stderr}"
    );
    assert!(
        !home.join("keys").exists(),
        "provision must not mint a file-key identity for a home whose signer it cannot load"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provision_rejects_uninitialized_home_without_bootstrap_flag() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_env = tempdir.path().join("home-env");
    let home = tempdir.path().join("uninitialized-home");
    let root = tempdir
        .path()
        .join("infra")
        .join("agents")
        .join("mini-3")
        .join("mini-3-steward");
    fs::create_dir_all(&home_env)?;

    write_portable_manifest_root(tempdir.path(), &root, "did:test:mini-3-steward")?;

    let stderr = run_cli_failure_stderr(
        &home_env,
        &[
            "provision",
            "--home",
            home.to_str().expect("utf-8 home"),
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;
    assert!(
        stderr.contains("initialized home identity is required"),
        "expected initialized home error, got:\n{stderr}"
    );
    assert!(
        stderr.contains("bootstrap the host identity backend first"),
        "expected bootstrap instruction, got:\n{stderr}"
    );
    assert!(
        !home.join("init.json").exists(),
        "provision must not create file-key init metadata for an enclave manifest"
    );

    Ok(())
}

fn write_portable_manifest_root(
    temp_root: &Path,
    root: &Path,
    placeholder_did: &str,
) -> Result<()> {
    let source_home_env = temp_root.join(format!("source-{}", Uuid::new_v4().simple()));
    fs::create_dir_all(&source_home_env)?;
    let model_name = format!("mock-provision-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("provision-source");

    run_init_json(
        &source_home_env,
        &[
            "--agent-name",
            agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    run_cli_text(
        &source_home_env,
        &[
            "config",
            "export",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;
    rewrite_manifest_agent_dids(root, placeholder_did)?;
    Ok(())
}

fn agent_did_from_report(report: &Value) -> Result<String> {
    let agent_did = report
        .get("agent_did")
        .and_then(Value::as_str)
        .context("provision report missing agent_did")?;
    anyhow::ensure!(
        !agent_did.starts_with("did:test:"),
        "provision returned placeholder DID: {agent_did}"
    );
    Ok(agent_did.to_string())
}
