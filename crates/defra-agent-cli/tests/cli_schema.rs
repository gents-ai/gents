mod support;
use support::*;

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;

#[test]
fn schema_apply_registers_sdl_and_additive_patch_idempotently() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let schema_dir = tempdir.path().join("schemas");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&schema_dir)?;

    fs::write(
        schema_dir.join("action_request.graphql"),
        r#"
type ActionRequest {
    task_id: String
}
"#,
    )?;
    fs::write(
        schema_dir.join("action_request.patch.json"),
        r#"[
  {"op":"add","path":"/ActionRequest/Fields/-","value":{"Name":"status","Kind":"String"}}
]"#,
    )?;

    let schema_root = schema_dir.to_str().expect("schema dir is utf-8");
    let first = run_cli_json(&home_dir, &["schema", "apply", schema_root])?;
    assert_eq!(
        first.get("status").and_then(Value::as_str),
        Some("schema_applied")
    );
    assert_eq!(first.get("mode").and_then(Value::as_str), Some("local"));
    assert_eq!(
        first
            .pointer("/schema_files/0/status")
            .and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        first
            .pointer("/patch_files/0/status")
            .and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(
        first
            .pointer("/patch_files/0/applied_fields/0")
            .and_then(Value::as_str),
        Some("status")
    );

    let second = run_cli_json(&home_dir, &["schema", "apply", schema_root])?;
    assert_eq!(
        second
            .pointer("/schema_files/0/status")
            .and_then(Value::as_str),
        Some("already_exists")
    );
    assert_eq!(
        second
            .pointer("/patch_files/0/status")
            .and_then(Value::as_str),
        Some("already_exists")
    );
    assert_eq!(
        second
            .pointer("/patch_files/0/skipped_fields/0")
            .and_then(Value::as_str),
        Some("status")
    );

    Ok(())
}
