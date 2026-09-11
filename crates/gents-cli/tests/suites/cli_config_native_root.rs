use crate::support::fs::write_json_file;

use std::process::Command;

use anyhow::Result;
use serde_json::{json, Value};
use tempfile::tempdir;

fn run_validate(root: &std::path::Path) -> Result<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_gents"))
        .args(["config", "validate", "--root"])
        .arg(root)
        .output()?;
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
fn validate_accepts_minimal_canonical_config() -> Result<()> {
    let tmp = tempdir()?;
    write_json_file(
        &tmp.path().join("pack_config.json"),
        &json!({"agent_principal":{"agent_did":"did:key:example"}}),
    )?;
    assert_eq!(run_validate(tmp.path())?["ok"], true);
    Ok(())
}

#[test]
fn validate_requires_pack_config() -> Result<()> {
    let tmp = tempdir()?;
    let report = run_validate(tmp.path())?;
    assert_eq!(report["ok"], false);
    assert!(report["errors"][0]
        .as_str()
        .is_some_and(|error| error.contains("pack_config.json")));
    Ok(())
}

#[test]
fn validate_rejects_missing_canonical_sidecar() -> Result<()> {
    let tmp = tempdir()?;
    write_json_file(
        &tmp.path().join("pack_config.json"),
        &json!({
            "agent_principal":{"agent_did":"did:key:example"},
            "contexts":[{"context_id":"default-context","system_prompt":"./missing.md"}]
        }),
    )?;
    let report = run_validate(tmp.path())?;
    assert_eq!(report["ok"], false);
    assert!(report["errors"][0]
        .as_str()
        .is_some_and(|error| error.contains("sidecar path does not resolve")));
    Ok(())
}

#[test]
fn validate_rejects_unknown_canonical_fields() -> Result<()> {
    let tmp = tempdir()?;
    write_json_file(
        &tmp.path().join("pack_config.json"),
        &json!({
            "agent_principal":{"agent_did":"did:key:example"},
            "unknown_collection":[]
        }),
    )?;
    assert_eq!(run_validate(tmp.path())?["ok"], false);
    Ok(())
}
