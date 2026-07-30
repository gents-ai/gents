use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::{
    adapter_projection_eval_jsonl_record_schema, adapter_projection_eval_jsonl_records,
    adapter_projection_json_schema, adapter_projection_jsonl_record_schema,
    adapter_projection_jsonl_records, validate_adapter_projection_contract,
    AdapterProjectionEnvelope,
};
use serde_json::Value;

const FIXTURE_ROOT_ENV: &str = "GENTS_ADAPTER_INTEROP_FIXTURES";

#[test]
#[ignore = "external interop: set GENTS_ADAPTER_INTEROP_FIXTURES and pass --ignored"]
fn external_adapter_projection_fixtures_validate_against_contracts() -> Result<()> {
    let root = std::env::var_os(FIXTURE_ROOT_ENV)
        .map(PathBuf::from)
        .map(resolve_fixture_root)
        .context(
            "set GENTS_ADAPTER_INTEROP_FIXTURES to an adapter fixture path and pass --ignored to run external adapter fixture validation",
        )?;
    let files = collect_json_files(&root)?;
    anyhow::ensure!(
        !files.is_empty(),
        "{FIXTURE_ROOT_ENV}={} did not contain JSON fixture files",
        root.display()
    );

    for path in files {
        validate_external_fixture_file(&path)?;
    }
    Ok(())
}

fn resolve_fixture_root(root: PathBuf) -> PathBuf {
    if root.exists() || root.is_absolute() {
        return root;
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(root)
}

fn validate_external_fixture_file(path: &Path) -> Result<()> {
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let value = serde_json::from_str::<Value>(&raw)
        .with_context(|| format!("parsing {} as JSON", path.display()))?;
    let envelope_value = value
        .get("envelope")
        .cloned()
        .unwrap_or_else(|| value.clone());
    let envelope = serde_json::from_value::<AdapterProjectionEnvelope>(envelope_value.clone())
        .with_context(|| {
            format!(
                "deserializing adapter projection envelope in {}",
                path.display()
            )
        })?;
    validate_adapter_projection_contract(&envelope).with_context(|| {
        format!(
            "validating adapter projection contract in {}",
            path.display()
        )
    })?;

    let kind = envelope.output.kind();
    assert_json_schema_valid(
        &adapter_projection_json_schema(kind),
        &envelope_value,
        &format!("{} envelope {}", kind.id(), path.display()),
    )?;

    let jsonl_schema = adapter_projection_jsonl_record_schema(kind);
    let jsonl_records = adapter_projection_jsonl_records(&envelope);
    anyhow::ensure!(
        !jsonl_records.is_empty(),
        "{} produced no adapter JSONL records",
        path.display()
    );
    for record in &jsonl_records {
        assert_json_schema_valid(
            &jsonl_schema,
            &serde_json::to_value(record)?,
            &format!("{} JSONL record {}", path.display(), record.record_id),
        )?;
    }

    let eval_schema = adapter_projection_eval_jsonl_record_schema(kind);
    let eval_records = adapter_projection_eval_jsonl_records(&envelope);
    anyhow::ensure!(
        !eval_records.is_empty(),
        "{} produced no eval JSONL records",
        path.display()
    );
    for record in &eval_records {
        assert_json_schema_valid(
            &eval_schema,
            &serde_json::to_value(record)?,
            &format!("{} eval JSONL record {}", path.display(), record.record_id),
        )?;
    }

    Ok(())
}

fn assert_json_schema_valid(schema: &Value, instance: &Value, label: &str) -> Result<()> {
    let validator =
        jsonschema::validator_for(schema).with_context(|| format!("compiling {label} schema"))?;
    let errors = validator
        .iter_errors(instance)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        errors.is_empty(),
        "{label} failed JSON Schema validation:\n{}",
        errors.join("\n")
    );
    Ok(())
}

fn collect_json_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_json_files_into(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_json_files_into(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_file() {
        if path.extension().and_then(|ext| ext.to_str()) == Some("json")
            && !is_gents_export_file(path)
        {
            files.push(path.to_path_buf());
        }
        return Ok(());
    }
    anyhow::ensure!(
        path.is_dir(),
        "adapter interop fixture path is neither file nor directory: {}",
        path.display()
    );
    for entry in std::fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        let entry = entry?;
        if entry.file_name() == "gents-exports" {
            continue;
        }
        collect_json_files_into(&entry.path(), files)?;
    }
    Ok(())
}

fn is_gents_export_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains(".gents."))
}
