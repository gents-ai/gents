use crate::support::*;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents_observe::observe::adapter_projection::{
    adapter_projection_eval_jsonl_records, adapter_projection_jsonl_records,
    build_external_adapter_projection,
};
use gents_observe::observe::external_adapter_capture::ExternalAdapterView;
use gents_observe::{
    observe::adapter_projection::adapter_projection_eval_jsonl_record_schema,
    observe::adapter_projection::adapter_projection_json_schema,
    observe::adapter_projection::adapter_projection_jsonl_record_schema,
    observe::adapter_projection::validate_adapter_projection_contract,
    observe::adapter_projection::AdapterProjectionEnvelope,
    observe::adapter_projection::ProjectionContext,
    observe::adapter_projection::ProjectionRedactionMode,
    observe::external_adapter_capture::import_external_adapter_capture_to_derived_view,
    observe::external_adapter_capture::ExternalAdapterCapture,
};
use serde_json::Value;

const FIXTURE_ROOT_ENV: &str = "GENTS_ADAPTER_INTEROP_ROUNDTRIP_FIXTURES";
const LEGACY_FIXTURE_ROOT_ENV: &str = "GENTS_ADAPTER_INTEROP_FIXTURES";
const EXPORT_ROOT_ENV: &str = "GENTS_ADAPTER_INTEROP_EXPORTS";

#[tokio::test]
#[ignore = "external interop: set GENTS_ADAPTER_INTEROP_ROUNDTRIP_FIXTURES and pass --ignored"]
async fn external_adapter_native_captures_project_to_export_formats() -> Result<()> {
    let Some(root) = fixture_root() else {
        eprintln!(
            "{FIXTURE_ROOT_ENV} or {LEGACY_FIXTURE_ROOT_ENV} is not set; skipping external adapter roundtrip"
        );
        return Ok(());
    };
    let root = resolve_fixture_root(root);
    let files = collect_json_files(&root)?;
    anyhow::ensure!(
        !files.is_empty(),
        "{}={} did not contain JSON fixture files",
        FIXTURE_ROOT_ENV,
        root.display()
    );

    let export_root = std::env::var_os(EXPORT_ROOT_ENV)
        .map(PathBuf::from)
        .map(resolve_fixture_root);
    if let Some(export_root) = export_root.as_ref() {
        std::fs::create_dir_all(export_root)
            .with_context(|| format!("creating {}", export_root.display()))?;
    }

    let mut imported_count = 0usize;
    for path in files {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let capture = serde_json::from_str::<ExternalAdapterCapture>(&raw)
            .with_context(|| format!("parsing {} as external adapter capture", path.display()))?;
        if capture.mapping.is_none() {
            eprintln!(
                "skipping {}: capture has no Gents import mapping",
                path.display()
            );
            continue;
        }
        let import = match import_external_adapter_capture_to_derived_view(&capture) {
            Ok(import) => import,
            Err(error)
                if error
                    .to_string()
                    .contains("external adapter import for projection") =>
            {
                eprintln!("skipping {}: {error}", path.display());
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("importing {}", path.display()))
            }
        };
        imported_count += 1;

        let context = ProjectionContext {
            actor_did: import.actor_did.clone(),
            redaction_mode: capture
                .envelope
                .as_ref()
                .map(|envelope| envelope.redaction_mode)
                .unwrap_or(ProjectionRedactionMode::Full),
        };
        let envelope = build_external_adapter_projection(&capture, &context)?;
        let json_output = serde_json::to_string_pretty(&envelope)?;
        let projection = serde_json::to_value(&envelope)?;
        assert_projection_matches_import(&projection, &capture, &import.view)
            .with_context(|| format!("validating imported projection for {}", path.display()))?;
        let jsonl_output = serialize_jsonl(adapter_projection_jsonl_records(&envelope))?;
        let eval_jsonl_output = serialize_jsonl(adapter_projection_eval_jsonl_records(&envelope))?;
        anyhow::ensure!(!jsonl_output.trim().is_empty(), "empty JSONL export");
        anyhow::ensure!(
            !eval_jsonl_output.trim().is_empty(),
            "empty eval JSONL export"
        );
        validate_cli_exports(&projection, &jsonl_output, &eval_jsonl_output, &path, true)?;

        if let Some(export_root) = export_root.as_ref() {
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("external-adapter-capture");
            std::fs::write(export_root.join(format!("{stem}.gents.json")), json_output)
                .with_context(|| format!("writing JSON export for {}", path.display()))?;
            std::fs::write(
                export_root.join(format!("{stem}.gents.jsonl")),
                jsonl_output,
            )
            .with_context(|| format!("writing JSONL export for {}", path.display()))?;
            std::fs::write(
                export_root.join(format!("{stem}.gents.eval-jsonl")),
                eval_jsonl_output,
            )
            .with_context(|| format!("writing eval JSONL export for {}", path.display()))?;
        }
    }

    anyhow::ensure!(
        imported_count > 0,
        "no external adapter captures with supported Gents import mappings were found in {}",
        root.display()
    );
    Ok(())
}

fn fixture_root() -> Option<PathBuf> {
    std::env::var_os(FIXTURE_ROOT_ENV)
        .or_else(|| std::env::var_os(LEGACY_FIXTURE_ROOT_ENV))
        .map(PathBuf::from)
}

fn resolve_fixture_root(root: PathBuf) -> PathBuf {
    if root.exists() || root.is_absolute() {
        return root;
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(root)
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

fn serialize_jsonl(records: Vec<impl serde::Serialize>) -> Result<String> {
    let mut output = String::new();
    for record in records {
        output.push_str(&serde_json::to_string(&record)?);
        output.push('\n');
    }
    Ok(output)
}

fn validate_cli_exports(
    projection: &Value,
    jsonl_output: &str,
    eval_jsonl_output: &str,
    path: &Path,
    validate_records: bool,
) -> Result<()> {
    let envelope = serde_json::from_value::<AdapterProjectionEnvelope>(projection.clone())
        .with_context(|| format!("deserializing JSON projection for {}", path.display()))?;
    validate_adapter_projection_contract(&envelope)
        .with_context(|| format!("validating JSON projection contract for {}", path.display()))?;
    let kind = envelope.output.kind();
    assert_json_schema_valid(
        &adapter_projection_json_schema(kind),
        projection,
        &format!("{} JSON projection", path.display()),
    )?;

    if !validate_records {
        return Ok(());
    }

    let jsonl_schema = adapter_projection_jsonl_record_schema(kind);
    let jsonl_records = parse_jsonl(jsonl_output, &format!("{} JSONL", path.display()))?;
    anyhow::ensure!(
        !jsonl_records.is_empty(),
        "{} produced no JSONL records",
        path.display()
    );
    for record in &jsonl_records {
        assert_json_schema_valid(
            &jsonl_schema,
            record,
            &format!("{} JSONL record", path.display()),
        )?;
    }

    let eval_schema = adapter_projection_eval_jsonl_record_schema(kind);
    let eval_records = parse_jsonl(eval_jsonl_output, &format!("{} eval JSONL", path.display()))?;
    anyhow::ensure!(
        !eval_records.is_empty(),
        "{} produced no eval JSONL records",
        path.display()
    );
    for record in &eval_records {
        assert_json_schema_valid(
            &eval_schema,
            record,
            &format!("{} eval JSONL record", path.display()),
        )?;
    }
    Ok(())
}

fn assert_projection_matches_import(
    projection: &Value,
    capture: &ExternalAdapterCapture,
    view: &ExternalAdapterView,
) -> Result<()> {
    assert_eq!(
        projection.get("source_request_id").and_then(Value::as_str),
        Some(view.request.request_id.as_str())
    );
    assert_eq!(
        projection
            .pointer("/output/adapter")
            .and_then(Value::as_str),
        Some(
            projection
                .get("projection_id")
                .and_then(Value::as_str)
                .context("projection_id")?
        )
    );
    if projection
        .pointer("/output/adapter")
        .and_then(Value::as_str)
        == Some("multi_agent_task")
    {
        assert_multi_agent_projection_matches_import(projection, capture)?;
    }
    Ok(())
}

fn assert_multi_agent_projection_matches_import(
    projection: &Value,
    capture: &ExternalAdapterCapture,
) -> Result<()> {
    let mapping = capture.mapping.as_ref().context("capture mapping")?;
    let participants = projection
        .pointer("/output/projection/participants")
        .and_then(Value::as_array)
        .context("participants")?;
    for expected in &mapping.participants {
        let role = expected.role.as_str();
        anyhow::ensure!(
            participants
                .iter()
                .any(|participant| participant.get("role").and_then(Value::as_str) == Some(role)),
            "projection missing participant role {role}: {projection:#}"
        );
    }

    let serialized_projection = serde_json::to_string(projection)?;
    for message in native_message_contents(capture) {
        anyhow::ensure!(
            serialized_projection.contains(&message),
            "projection missing native message content {message:?}: {projection:#}"
        );
    }

    let delegations = projection
        .pointer("/output/projection/delegations")
        .and_then(Value::as_array)
        .context("delegations")?;
    for expected in &mapping.delegations {
        anyhow::ensure!(
            delegations.iter().any(|delegation| {
                delegation.get("parent_request_id").and_then(Value::as_str)
                    == Some(expected.parent_request_id.as_str())
                    && delegation.get("child_request_id").and_then(Value::as_str)
                        == Some(expected.child_request_id.as_str())
            }),
            "projection missing delegation {} -> {}: {projection:#}",
            expected.parent_request_id,
            expected.child_request_id
        );
    }
    Ok(())
}

fn native_message_contents(capture: &ExternalAdapterCapture) -> Vec<String> {
    match capture.source.system.as_str() {
        "autogen-agentchat" => capture
            .native
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|message| message.get("content").map(value_to_text))
            .collect(),
        _ => Vec::new(),
    }
}

fn value_to_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}
