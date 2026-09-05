use crate::support::fs::write_json_file;

use std::fs;
use std::process::Command;

use anyhow::Result;
use serde_json::Value;
use tempfile::tempdir;

fn gents() -> Command {
    Command::new(env!("CARGO_BIN_EXE_gents"))
}

fn run_validate(root: &std::path::Path) -> Result<Value> {
    let output = gents()
        .args(["config", "validate", "--root"])
        .arg(root)
        .output()?;
    let stdout = String::from_utf8(output.stdout)?;
    Ok(serde_json::from_str(&stdout)?)
}

fn write_principal_with_behavior(root: &std::path::Path) -> Result<()> {
    let agent_did = "did:key:example";
    let default_behavior_id = "default";

    write_json_file(
        &root.join("agent_principal.json"),
        &serde_json::json!({
            "agent_did": agent_did,
            "default_behavior_id": default_behavior_id,
            "enabled": true
        }),
    )?;

    let dir = root
        .join("agent_behaviors")
        .join(default_behavior_id.replace('-', "_"));
    fs::create_dir_all(&dir)?;
    write_json_file(
        &dir.join("object.json"),
        &serde_json::json!({
            "behavior_id": default_behavior_id,
            "agent_did": agent_did,
            "enabled": true
        }),
    )?;

    Ok(())
}

#[test]
fn validate_accepts_minimal_per_doc_root() -> Result<()> {
    let tmp = tempdir()?;
    write_principal_with_behavior(tmp.path())?;
    let report = run_validate(tmp.path())?;
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(true));
    Ok(())
}

#[test]
fn validate_rejects_handle_mismatch() -> Result<()> {
    let tmp = tempdir()?;
    write_principal_with_behavior(tmp.path())?;
    let dir = tmp.path().join("agent_behaviors").join("on-disk");
    fs::create_dir_all(&dir)?;
    write_json_file(
        &dir.join("object.json"),
        &serde_json::json!({
            "behavior_id": "inside-json",
            "agent_did": "did:key:example",
            "enabled": true,
        }),
    )?;
    let report = run_validate(tmp.path())?;
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(false));
    let joined = report
        .get("errors")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("does not match behavior_id"),
        "got: {joined}"
    );
    Ok(())
}

#[test]
fn validate_rejects_missing_sidecar() -> Result<()> {
    let tmp = tempdir()?;
    let agent_did = "did:key:example";
    let default_behavior_id = "default";
    write_json_file(
        &tmp.path().join("agent_principal.json"),
        &serde_json::json!({
            "agent_did": agent_did,
            "default_behavior_id": default_behavior_id,
            "enabled": true
        }),
    )?;
    let dir = tmp
        .path()
        .join("agent_behaviors")
        .join(default_behavior_id.replace('-', "_"));
    fs::create_dir_all(&dir)?;
    write_json_file(
        &dir.join("object.json"),
        &serde_json::json!({
            "behavior_id": default_behavior_id,
            "agent_did": agent_did,
            "system_prompt": "./system_prompt.md",
            "enabled": true,
        }),
    )?;
    let report = run_validate(tmp.path())?;
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(false));
    let joined = report
        .get("errors")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("sidecar path does not resolve"),
        "got: {joined}"
    );
    Ok(())
}

#[test]
fn validate_accepts_stray_readme_in_doc_dir() -> Result<()> {
    let tmp = tempdir()?;
    write_principal_with_behavior(tmp.path())?;
    let dir = tmp.path().join("agent_behaviors").join("default");
    fs::write(dir.join("README.md"), "notes")?;
    let report = run_validate(tmp.path())?;
    assert_eq!(report.get("ok").and_then(Value::as_bool), Some(true));
    Ok(())
}
