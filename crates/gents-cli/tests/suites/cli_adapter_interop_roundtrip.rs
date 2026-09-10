use crate::support::*;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::adapter_projection::{
    adapter_projection_eval_jsonl_records, adapter_projection_jsonl_records,
    build_external_adapter_projection,
};
use gents::defra_node::{EmbeddedNode, StorageBackend};
use gents::{
    adapter_projection_eval_jsonl_record_schema, adapter_projection_json_schema,
    adapter_projection_jsonl_record_schema, ensure_runtime_schemas,
    import_external_adapter_capture_to_timeline_rows, validate_adapter_projection_contract,
    AdapterProjectionEnvelope, AdapterProjectionKind, ExternalAdapterCapture, ProjectionContext,
    ProjectionRedactionMode, RunTimelineRows,
};
use serde_json::{json, Value};

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
        let import = match import_external_adapter_capture_to_timeline_rows(&capture) {
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

        // External framework state stays in the original capture. Persisted Gents
        // requests deliberately do not carry a parallel framework metadata model.
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
        assert_projection_matches_import(&projection, &capture, &import.rows)
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

#[test]
fn external_projection_preserves_mapped_children_without_forging_native_provenance() -> Result<()> {
    let capture: ExternalAdapterCapture =
        serde_json::from_value(valid_multi_agent_capture_value())?;
    let imported = import_external_adapter_capture_to_timeline_rows(&capture)?;
    assert!(imported.rows.requests.iter().all(
        |request| request.doc_id.is_none() && request.caused_by_parent_request_doc_id.is_none()
    ));
    let envelope = build_external_adapter_projection(&capture, &ProjectionContext::default())?;
    validate_adapter_projection_contract(&envelope)?;
    let projection = serde_json::to_value(&envelope)?;
    assert_projection_matches_import(&projection, &capture, &imported.rows)?;
    assert_eq!(
        projection
            .pointer("/output/projection/messages")
            .and_then(Value::as_array)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        projection.pointer("/output/projection/delegations/0/parent_tool_call_id"),
        Some(&json!("tool-delegate"))
    );
    for mode in [
        ProjectionRedactionMode::TrainingSafe,
        ProjectionRedactionMode::Public,
    ] {
        let envelope = build_external_adapter_projection(
            &capture,
            &ProjectionContext {
                actor_did: None,
                redaction_mode: mode,
            },
        )?;
        validate_adapter_projection_contract(&envelope)?;
        let rendered = serde_json::to_string(&envelope.output)?;
        assert!(!rendered.contains("Research complete."));
        assert!(!rendered.contains("did:test:researcher"));
    }
    Ok(())
}

#[test]
fn external_langgraph_projection_preserves_history_graph_tasks_and_redacts_values() -> Result<()> {
    let mut value = langgraph_capture_without_history();
    value["native"]["graph"] = json!({
        "nodes": ["provider_model", "research_subgraph"],
        "edges": [{"from":"langgraph:start","to":"langgraph:node:provider_model","kind":"flow"}],
        "subgraphs": {"research_subgraph": {"nodes": ["search"]}}
    });
    value["native"]["history"] = json!([
        {"config":{"configurable":{"checkpoint_id":"checkpoint-current"}},
         "values":{"topic":"private-topic", "status":"completed", "child_request_id":"req-child"}},
        {"values":{"topic":"prior-topic"}}
    ]);
    let capture: ExternalAdapterCapture = serde_json::from_value(value)?;
    let envelope = build_external_adapter_projection(&capture, &ProjectionContext::default())?;
    validate_adapter_projection_contract(&envelope)?;
    let gents::adapter_projection::AdapterProjection::LangGraphStateHistory(projection) =
        &envelope.output
    else {
        panic!("expected LangGraph projection")
    };
    assert_eq!(projection.checkpoint_id, "checkpoint-current");
    assert_eq!(projection.values["history_checkpoint_count"], json!(2));
    assert_eq!(projection.values["topic"], json!("private-topic"));
    assert!(projection
        .nodes
        .iter()
        .any(|node| node.id == "langgraph:subgraph:research:search"));
    assert_eq!(projection.edges.len(), 1);
    assert!(projection.tasks.iter().any(|task| task.name == "search"));
    let public = build_external_adapter_projection(
        &capture,
        &ProjectionContext {
            actor_did: None,
            redaction_mode: ProjectionRedactionMode::Public,
        },
    )?;
    validate_adapter_projection_contract(&public)?;
    assert!(!serde_json::to_string(&public)?.contains("private-topic"));
    Ok(())
}

#[test]
fn malformed_external_capture_json_is_rejected_before_import() {
    let error = serde_json::from_str::<ExternalAdapterCapture>(
        r#"{"source":{"system":"autogen-agentchat"},"mapping":"#,
    )
    .unwrap_err();

    assert!(
        error.is_eof(),
        "expected truncated JSON to fail during capture parsing, got {error}"
    );
}

#[tokio::test]
async fn negative_external_capture_imports_reject_bad_mappings_without_partial_rows() -> Result<()>
{
    let cases = [
        (
            "missing participants",
            capture_with_mutation(|capture| {
                capture["mapping"]["participants"] = json!([]);
            }),
            "must include at least one participant",
        ),
        (
            "delegation references absent child request",
            capture_with_mutation(|capture| {
                capture["mapping"]["delegations"][0]["child_request_id"] = json!("req-missing");
            }),
            "child_request_id \"req-missing\" does not reference a declared child participant",
        ),
        (
            "tool result references absent child request",
            capture_with_mutation(|capture| {
                capture["mapping"]["tool_events"][0]["child_request_id"] = json!("req-missing");
            }),
            "child_request_id \"req-missing\" does not reference a declared child participant",
        ),
        (
            "unknown multi-agent framework",
            capture_with_mutation(|capture| {
                capture["source"]["system"] = json!("unknown-agent-framework");
            }),
            "is not supported for mapped import",
        ),
        (
            "wrong envelope projection",
            capture_with_mutation(|capture| {
                capture["envelope"] = langgraph_envelope_value();
            }),
            "envelope projection langgraph_state_history does not match mapping projection multi_agent_task",
        ),
        (
            "langgraph capture without history",
            langgraph_capture_without_history(),
            "requires non-empty native.history",
        ),
    ];

    for (name, value, expected_error) in cases {
        let tempdir = tempfile::tempdir().context("creating tempdir")?;
        let data_dir = tempdir.path().join("data");
        let node = EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_storage_backend(StorageBackend::Regolith)
            .build()
            .await
            .with_context(|| format!("opening embedded node for {name}"))?;
        ensure_runtime_schemas(&node)
            .await
            .with_context(|| format!("creating runtime schemas for {name}"))?;

        let capture = serde_json::from_value::<ExternalAdapterCapture>(value)
            .with_context(|| format!("parsing negative capture case {name}"))?;
        let error = import_external_adapter_capture_to_timeline_rows(&capture)
            .expect_err("negative capture import unexpectedly succeeded");
        let error_text = format!("{error:#}");
        assert!(
            error_text.contains(expected_error),
            "{name} produced unexpected error:\nexpected substring: {expected_error}\nactual: {error_text}"
        );
        assert_no_timeline_rows(&node)
            .await
            .with_context(|| format!("checking rejected import left no rows for {name}"))?;
    }
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

fn valid_multi_agent_capture_value() -> Value {
    json!({
        "source": {
            "system": "autogen-agentchat",
            "package": "autogen-agentchat",
            "package_version": "0.7.0",
            "generator": "negative-case-test"
        },
        "native": {
            "messages": [
                {
                    "source": "planner",
                    "content": "Plan the work."
                },
                {
                    "source": "researcher",
                    "content": "Research complete."
                }
            ]
        },
        "mapping": {
            "projection": "multi_agent_task",
            "scenario_id": "negative-case-test",
            "request_id": "req-root",
            "session_id": "session-negative",
            "participants": [
                {
                    "role": "planner",
                    "agent_did": "did:test:planner"
                },
                {
                    "role": "researcher",
                    "agent_did": "did:test:researcher",
                    "request_id": "req-child"
                }
            ],
            "delegations": [
                {
                    "parent_request_id": "req-root",
                    "child_request_id": "req-child",
                    "parent_tool_call_id": "tool-delegate",
                    "tool_name": "delegate_to_researcher",
                    "status": "completed"
                }
            ],
            "tool_events": [
                {
                    "id": "tool-delegate",
                    "request_id": "req-root",
                    "tool_name": "delegate_to_researcher",
                    "status": "completed",
                    "child_request_id": "req-child"
                }
            ]
        }
    })
}

fn capture_with_mutation(mut mutate: impl FnMut(&mut Value)) -> Value {
    let mut capture = valid_multi_agent_capture_value();
    mutate(&mut capture);
    capture
}

fn langgraph_capture_without_history() -> Value {
    json!({
        "source": {
            "system": "langgraph",
            "package": "langgraph",
            "package_version": "0.2.0"
        },
        "native": {
            "thread_id": "thread-missing-history"
        },
        "mapping": {
            "projection": "langgraph_state_history",
            "scenario_id": "missing-history",
            "request_id": "req-langgraph"
        }
    })
}

fn langgraph_envelope_value() -> Value {
    json!({
        "projection_id": "langgraph_state_history",
        "projection_version": "v1",
        "source_request_id": "req-root",
        "redaction_mode": "full",
        "provenance": {
            "runtime": "gents",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "source_version_status": "current_state_captured_only"
        },
        "output": {
            "adapter": "langgraph_state_history",
            "projection": {
                "checkpoint_id": "checkpoint-negative",
                "root_request_id": "req-root",
                "values": {
                    "request_id": "req-root"
                },
                "nodes": [
                    {
                        "id": "langgraph:start",
                        "kind": "start"
                    }
                ],
                "edges": [],
                "tasks": []
            }
        }
    })
}

async fn assert_no_timeline_rows(node: &EmbeddedNode) -> Result<()> {
    for collection in [
        "AgentSession",
        "AgentRequest",
        "AgentMessage",
        "AgentToolCall",
        "AgentResponse",
    ] {
        let response = node
            .execute(&format!("{{ {collection} {{ _docID }} }}"))
            .await;
        if response.has_errors() {
            anyhow::bail!(
                "GraphQL query for {collection} failed: {:?}",
                response.errors
            );
        }
        let row_count = response
            .data
            .as_ref()
            .and_then(|data| data.get(collection))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        anyhow::ensure!(
            row_count == 0,
            "rejected import left {row_count} row(s) in {collection}"
        );
    }
    Ok(())
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
    rows: &RunTimelineRows,
) -> Result<()> {
    assert_eq!(
        projection.get("source_request_id").and_then(Value::as_str),
        Some(rows.request.request_id.as_str())
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
