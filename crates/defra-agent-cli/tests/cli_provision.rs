mod support;
use support::*;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provision_initializes_home_binds_manifest_writes_identity_and_diff_exact() -> Result<()> {
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

    let placeholder_did = "did:defra-agent:mini-1-steward";
    write_portable_manifest_root(tempdir.path(), &root, placeholder_did)?;

    let report = run_cli_json(
        &target_home_env,
        &[
            "provision",
            "--home",
            target_home.to_str().expect("utf-8 home"),
            "--root",
            root.to_str().expect("utf-8 root"),
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

    let identity_binding = read_json_file(&root.join("identity.json"))?;
    assert_eq!(
        identity_binding
            .get("identity_status")
            .and_then(Value::as_str),
        Some("provisioned")
    );
    assert_eq!(
        identity_binding.get("did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        identity_binding.get("key_backend").and_then(Value::as_str),
        Some("macos-secure-enclave")
    );
    assert_eq!(
        identity_binding
            .get("secure_enclave_label")
            .and_then(Value::as_str),
        Some("amygdala/agents/mini-1/mini-1-steward")
    );
    assert!(identity_binding.get("identity_backend").is_none());

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
async fn provision_accepts_initialized_home_did_without_file_key_path() -> Result<()> {
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

    let placeholder_did = "did:defra-agent:mini-2-steward";
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

    let report = run_cli_json(
        &home_env,
        &[
            "provision",
            "--home",
            home.to_str().expect("utf-8 home"),
            "--root",
            root.to_str().expect("utf-8 root"),
        ],
    )?;
    assert_eq!(
        report.pointer("/identity/status").and_then(Value::as_str),
        Some("existing")
    );
    assert_eq!(
        report.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        report.pointer("/apply/ok").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        report.pointer("/diff/ok").and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        !home.join("keys").exists(),
        "provision should not create a file-key identity when init.json already contains a real DID without key_path"
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
    let host_name = root
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .unwrap_or("host");

    run_init_json(
        &source_home_env,
        &[
            "--agent-name",
            agent_name,
            "--model-name",
            &model_name,
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
    write_json_file(
        &root.join("identity.json"),
        &serde_json::json!({
            "identity_status": "unprovisioned",
            "did": null,
            "key_backend": "macos-secure-enclave",
            "secure_enclave_label": format!("amygdala/agents/{}/{}", host_name, agent_name)
        }),
    )?;
    Ok(())
}

fn agent_did_from_report(report: &Value) -> Result<String> {
    let agent_did = report
        .get("agent_did")
        .and_then(Value::as_str)
        .context("provision report missing agent_did")?;
    anyhow::ensure!(
        !agent_did.starts_with("did:defra-agent:"),
        "provision returned placeholder DID: {agent_did}"
    );
    Ok(agent_did.to_string())
}
