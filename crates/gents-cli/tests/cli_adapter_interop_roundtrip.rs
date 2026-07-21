mod support;
use support::*;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::defra_node::{EmbeddedNode, StorageBackend};
use gents::{
    adapter_projection_eval_jsonl_record_schema, adapter_projection_json_schema,
    adapter_projection_jsonl_record_schema, ensure_runtime_schemas,
    import_external_adapter_capture_to_timeline_rows, validate_adapter_projection_contract,
    AdapterProjectionEnvelope, AdapterProjectionKind, ExternalAdapterCapture,
    ProjectionRedactionMode, RunTimelineRows, TimelineConversationRow, TimelineMessageRow,
    TimelineRequestRow, TimelineResponseRow, TimelineSessionRow, TimelineToolCallRow,
};
use serde_json::{json, Value};

const FIXTURE_ROOT_ENV: &str = "GENTS_ADAPTER_INTEROP_ROUNDTRIP_FIXTURES";
const LEGACY_FIXTURE_ROOT_ENV: &str = "GENTS_ADAPTER_INTEROP_FIXTURES";
const EXPORT_ROOT_ENV: &str = "GENTS_ADAPTER_INTEROP_EXPORTS";

#[tokio::test]
#[ignore = "external interop: set GENTS_ADAPTER_INTEROP_ROUNDTRIP_FIXTURES and pass --ignored"]
async fn external_adapter_native_captures_roundtrip_through_gents_binary() -> Result<()> {
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

        let tempdir = tempfile::tempdir().context("creating tempdir")?;
        let agent_home = tempdir.path().join("agent-home");
        let data_dir = agent_home.join("data");
        {
            let node = EmbeddedNode::builder()
                .data_path(&data_dir)
                .with_storage_backend(StorageBackend::RocksDb)
                .build()
                .await
                .context("opening embedded node")?;
            ensure_runtime_schemas(&node).await?;
            persist_run_timeline_rows(&node, &import.rows).await?;
        }

        let projection_arg = projection_cli_arg(import.projection);
        let redaction_arg = redaction_cli_arg(&capture);
        let actor_did = import
            .actor_did
            .as_deref()
            .unwrap_or("did:test:external-interop-reader");
        let home = agent_home.to_str().context("agent home utf8")?;
        let json_output = trace_project(
            tempdir.path(),
            home,
            &import.rows.request.request_id,
            projection_arg,
            "json",
            redaction_arg,
            actor_did,
        )?;
        let projection = serde_json::from_str::<Value>(&json_output)
            .with_context(|| format!("parsing JSON projection for {}", path.display()))?;
        validate_cli_exports(&projection, "", "", &path, false)?;
        assert_projection_matches_import(&projection, &capture, &import.rows)
            .with_context(|| format!("validating imported projection for {}", path.display()))?;

        let jsonl_output = trace_project(
            tempdir.path(),
            home,
            &import.rows.request.request_id,
            projection_arg,
            "jsonl",
            redaction_arg,
            actor_did,
        )?;
        anyhow::ensure!(
            !jsonl_output.trim().is_empty(),
            "{} produced empty JSONL export",
            path.display()
        );
        let eval_jsonl_output = trace_project(
            tempdir.path(),
            home,
            &import.rows.request.request_id,
            projection_arg,
            "eval-jsonl",
            redaction_arg,
            actor_did,
        )?;
        anyhow::ensure!(
            !eval_jsonl_output.trim().is_empty(),
            "{} produced empty eval JSONL export",
            path.display()
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
            .with_storage_backend(StorageBackend::RocksDb)
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
            "source_projection_version": "v1"
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
        "AgentConversation",
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

async fn persist_run_timeline_rows(node: &EmbeddedNode, rows: &RunTimelineRows) -> Result<()> {
    if let Some(session) = rows.session.as_ref() {
        create_session(node, session).await?;
    }
    if let Some(conversation) = rows.conversation.as_ref() {
        create_conversation(node, conversation).await?;
    }

    let mut seen_requests = BTreeSet::new();
    for request in &rows.requests {
        if seen_requests.insert(request.request_id.clone()) {
            create_request(node, request).await?;
        }
    }
    if seen_requests.insert(rows.request.request_id.clone()) {
        create_request(node, &rows.request).await?;
    }

    for message in &rows.messages {
        create_message(node, message).await?;
    }
    for tool_call in &rows.tool_calls {
        create_tool_call(node, tool_call).await?;
    }
    for response in &rows.responses {
        create_response(node, response).await?;
    }
    Ok(())
}

async fn create_session(node: &EmbeddedNode, row: &TimelineSessionRow) -> Result<()> {
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{}",
                    {}
                    {}
                    {}
                    {}
                }}) {{ _docID }}
            }}"#,
            esc(&row.session_id),
            string_field("agent_name", row.agent_name.as_deref()),
            string_field("behavior_id", row.behavior_id.as_deref()),
            string_field("started", row.started.as_deref()),
            string_field("status", row.status.as_deref()),
        ),
    )
    .await
}

async fn create_conversation(node: &EmbeddedNode, row: &TimelineConversationRow) -> Result<()> {
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentConversation(input: {{
                    session_id: "{}",
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                }}) {{ _docID }}
            }}"#,
            esc(&row.session_id),
            string_field("agent_name", row.agent_name.as_deref()),
            string_field("agent_did", row.agent_did.as_deref()),
            string_field("behavior_id", row.behavior_id.as_deref()),
            string_field("title", row.title.as_deref()),
            string_field("title_source", row.title_source.as_deref()),
            string_field("preview_text", row.preview_text.as_deref()),
            string_field("status", row.status.as_deref()),
            string_field("created_at", row.created_at.as_deref()),
            string_field("updated_at", row.updated_at.as_deref()),
            string_field("latest_request_id", row.latest_request_id.as_deref()),
        ),
    )
    .await
}

async fn create_request(node: &EmbeddedNode, row: &TimelineRequestRow) -> Result<()> {
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{}",
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                }}) {{ _docID }}
            }}"#,
            esc(&row.request_id),
            string_field("agent_did", row.agent_did.as_deref()),
            string_field("behavior_id", row.behavior_id.as_deref()),
            string_field("session_id", row.session_id.as_deref()),
            string_field("content", row.content.as_deref()),
            string_field("metadata", row.metadata.as_deref()),
            string_field("status", row.status.as_deref()),
            string_field("lifecycle_state", row.lifecycle_state.as_deref()),
            string_field("backend_id", row.backend_id.as_deref()),
            string_field("failure_reason", row.failure_reason.as_deref()),
            string_field("created_at", row.created_at.as_deref()),
            i64_field("retry_count", row.retry_count),
            string_field(
                "caused_by_parent_request_id",
                row.caused_by_parent_request_id.as_deref()
            ),
            string_field(
                "caused_by_parent_tool_call_id",
                row.caused_by_parent_tool_call_id.as_deref()
            ),
        ),
    )
    .await
}

async fn create_message(node: &EmbeddedNode, row: &TimelineMessageRow) -> Result<()> {
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "{}:{}",
                    session_id: "{}",
                    {}
                    sequence: {},
                    role: "{}",
                    content: "{}",
                    {}
                }}) {{ _docID }}
            }}"#,
            esc(&row.session_id),
            row.sequence,
            esc(&row.session_id),
            string_field("request_id", row.request_id.as_deref()),
            row.sequence,
            esc(&row.role),
            esc(&row.content),
            string_field("timestamp", row.timestamp.as_deref()),
        ),
    )
    .await
}

async fn create_tool_call(node: &EmbeddedNode, row: &TimelineToolCallRow) -> Result<()> {
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{}:{}",
                    session_id: "{}",
                    tool_name: "{}",
                    tool_call_id: "{}",
                    args: "{}",
                    result: "{}",
                    status: "{}",
                    {}
                    {}
                    {}
                    {}
                    {}
                }}) {{ _docID }}
            }}"#,
            esc(&row.session_id),
            esc(&row.tool_call_id),
            esc(&row.session_id),
            esc(&row.tool_name),
            esc(&row.tool_call_id),
            esc(&row.args),
            esc(&row.result),
            esc(&row.status),
            string_field("request_id", row.request_id.as_deref()),
            i64_field("message_sequence", row.message_sequence),
            string_field("started_at", row.started_at.as_deref()),
            string_field("completed_at", row.completed_at.as_deref()),
            string_field("child_request_id", row.child_request_id.as_deref()),
        ),
    )
    .await
}

async fn create_response(node: &EmbeddedNode, row: &TimelineResponseRow) -> Result<()> {
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentResponse(input: {{
                    response_key: "{}",
                    request_id: "{}",
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                    {}
                }}) {{ _docID }}
            }}"#,
            esc(&row.request_id),
            esc(&row.request_id),
            string_field("agent_did", row.agent_did.as_deref()),
            string_field("behavior_id", row.behavior_id.as_deref()),
            string_field("session_id", row.session_id.as_deref()),
            string_field("content", row.content.as_deref()),
            string_field("reasoning", row.reasoning.as_deref()),
            string_field("status", row.status.as_deref()),
            string_field("error_message", row.error_message.as_deref()),
            i64_field(
                "materialized_message_sequence",
                row.materialized_message_sequence
            ),
            string_field("created_at", row.created_at.as_deref()),
            string_field("completed_at", row.completed_at.as_deref()),
        ),
    )
    .await
}

fn trace_project(
    cwd: &Path,
    home: &str,
    request_id: &str,
    projection: &str,
    format: &str,
    redaction: &str,
    actor_did: &str,
) -> Result<String> {
    run_cli_text(
        cwd,
        &[
            "trace",
            "project",
            "--home",
            home,
            "--request-id",
            request_id,
            "--projection",
            projection,
            "--format",
            format,
            "--redaction",
            redaction,
            "--actor-did",
            actor_did,
        ],
    )
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

fn projection_cli_arg(projection: AdapterProjectionKind) -> &'static str {
    match projection {
        AdapterProjectionKind::OpenAiCodexRunTrace => "openai-codex",
        AdapterProjectionKind::LangGraphStateHistory => "langgraph",
        AdapterProjectionKind::MultiAgentTask => "multi-agent",
    }
}

fn redaction_cli_arg(capture: &ExternalAdapterCapture) -> &'static str {
    match capture
        .envelope
        .as_ref()
        .map(|envelope| envelope.redaction_mode)
        .unwrap_or(ProjectionRedactionMode::Full)
    {
        ProjectionRedactionMode::Full => "full",
        ProjectionRedactionMode::TrainingSafe => "training-safe",
        ProjectionRedactionMode::Public => "public",
    }
}

fn string_field(name: &str, value: Option<&str>) -> String {
    value
        .map(|value| format!("{name}: \"{}\",", esc(value)))
        .unwrap_or_default()
}

fn i64_field(name: &str, value: Option<i64>) -> String {
    value
        .map(|value| format!("{name}: {value},"))
        .unwrap_or_default()
}

fn esc(value: &str) -> String {
    escape_graphql_string(value)
}

fn value_to_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}
