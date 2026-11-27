use crate::support::*;

use std::sync::Arc;

use anyhow::{Context, Result};
use gents_inference::rendered_request::RenderedCompletionRequest;
use gents_store::schema::ensure_runtime_schemas;
use serde_json::{json, Value};

fn adapter_schema_snapshot(snapshot_name: &str, suffix: &str) -> Result<Value> {
    read_workspace_json(&format!(
        "crates/gents/tests/fixtures/adapter_projections/contracts/{snapshot_name}.{suffix}.json"
    ))
}

fn assert_projection_json_matches_schema(snapshot_name: &str, projection: &Value) -> Result<()> {
    assert_json_schema_valid(
        &adapter_schema_snapshot(snapshot_name, "schema")?,
        projection,
        &format!("{snapshot_name} JSON projection"),
    )
}

fn assert_projection_records_match_schema(
    snapshot_name: &str,
    suffix: &str,
    records: &[Value],
) -> Result<()> {
    let schema = adapter_schema_snapshot(snapshot_name, suffix)?;
    anyhow::ensure!(
        !records.is_empty(),
        "{snapshot_name} {suffix} should produce records"
    );
    for (index, record) in records.iter().enumerate() {
        assert_json_schema_valid(
            &schema,
            record,
            &format!("{snapshot_name} {suffix} record {index}"),
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn trace_export_emits_amy_style_jsonl_and_classifies_completed_failures() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let agent_home = tempdir.path().join("agent-home");

    {
        let node = initialized_trace_node(tempdir.path(), &agent_home).await?;
        ensure_runtime_schemas(node.node()).await?;
        seed_trace_export_rows(&node).await?;
        node.shutdown().await?;
    }

    let output = run_cli_text(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "export",
            "--home",
            agent_home.to_str().context("agent home utf8")?,
            "--run-id",
            "run-cli",
            "--case-id",
            "case-cli",
            "--limit",
            "10",
        ],
    )?;
    let mut records = output
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).context("parsing JSONL record"))
        .collect::<Result<Vec<_>>>()?;
    records.sort_by_key(|record| {
        record
            .get("tool_call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    });

    assert_eq!(records.len(), 5, "export output:\n{output}");
    let find_record = |tool_call_id: &str| {
        records
            .iter()
            .find(|record| record.get("tool_call_id").and_then(Value::as_str) == Some(tool_call_id))
            .unwrap_or_else(|| panic!("missing record {tool_call_id}; output:\n{output}"))
    };
    let deadline = find_record("call-deadline");
    let failed = find_record("call-fail");
    let missing_tool = find_record("call-missing-tool");
    let succeeded = find_record("call-success");
    let timed_out = find_record("call-timed-out");

    assert_eq!(
        failed.get("tool_call_id").and_then(Value::as_str),
        Some("call-fail")
    );
    assert_eq!(
        failed.get("tool_status").and_then(Value::as_str),
        Some("failed")
    );
    assert_eq!(
        failed.get("tool_result_ok").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        failed.get("tool_failure_class").and_then(Value::as_str),
        Some("toolReturnedError")
    );
    assert_eq!(
        failed
            .get("tool_error")
            .and_then(|value| value.get("retryable"))
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        failed.get("failure_class").and_then(Value::as_str),
        Some("toolReturnedError")
    );
    assert!(failed
        .get("request_failure_class")
        .is_some_and(Value::is_null));
    assert_eq!(
        failed.get("request_id").and_then(Value::as_str),
        Some("req-1")
    );
    assert_eq!(
        failed.get("backend_id").and_then(Value::as_str),
        Some("studios-cluster")
    );
    assert_eq!(
        failed.get("model_name").and_then(Value::as_str),
        Some("baa-ai/GLM-5.1-RAM-420GB-MLX")
    );
    assert!(
        failed
            .get("inference_profile_id")
            .is_some_and(Value::is_null),
        "historical inference profile is not recorded by the capture"
    );
    assert_eq!(
        failed
            .get("raw_tool_call_json")
            .and_then(|value| value.get("function"))
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str),
        Some("bash")
    );
    assert_eq!(failed.get("latency_ms").and_then(Value::as_i64), Some(1500));
    let native_output = failed
        .get("native_tool_output")
        .unwrap_or_else(|| panic!("missing native_tool_output in {failed:#}"));
    assert_eq!(
        native_output.get("ok").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        native_output.get("status").and_then(Value::as_str),
        Some("exit_nonzero")
    );
    assert_eq!(
        native_output.get("command").and_then(Value::as_str),
        Some("grep -P amy README.md")
    );
    assert_eq!(
        native_output.get("exit_code").and_then(Value::as_i64),
        Some(2)
    );
    assert_eq!(
        native_output.get("execution_mode").and_then(Value::as_str),
        Some("read_only")
    );
    assert_eq!(
        native_output.get("sandbox").and_then(Value::as_str),
        Some("policy_read_only")
    );

    assert_eq!(
        missing_tool.get("tool_result_ok").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        missing_tool
            .get("tool_failure_class")
            .and_then(Value::as_str),
        Some("serviceUnavailable")
    );
    assert!(missing_tool
        .get("tool_error")
        .and_then(|value| value.get("available_tools"))
        .is_none());
    assert!(missing_tool
        .get("tool_error")
        .and_then(|value| value.get("requested_tool_name"))
        .is_none());

    assert_eq!(
        succeeded.get("tool_call_id").and_then(Value::as_str),
        Some("call-success")
    );
    assert_eq!(
        succeeded.get("tool_status").and_then(Value::as_str),
        Some("completed")
    );
    assert_eq!(
        succeeded.get("tool_result_ok").and_then(Value::as_bool),
        Some(true)
    );
    assert!(succeeded
        .get("tool_failure_class")
        .is_some_and(Value::is_null));
    assert!(succeeded.get("failure_class").is_some_and(Value::is_null));
    assert_eq!(
        succeeded.get("run_id").and_then(Value::as_str),
        Some("run-cli")
    );
    assert_eq!(
        succeeded.get("case_id").and_then(Value::as_str),
        Some("case-cli")
    );
    assert_eq!(
        succeeded.get("prompt").and_then(Value::as_str),
        Some("Inspect the repo and show README.md")
    );

    assert_eq!(
        deadline.get("tool_call_id").and_then(Value::as_str),
        Some("call-deadline")
    );
    assert_eq!(
        deadline.get("tool_result_ok").and_then(Value::as_bool),
        Some(true)
    );
    assert!(deadline
        .get("tool_failure_class")
        .is_some_and(Value::is_null));
    assert_eq!(
        deadline
            .get("request_failure_class")
            .and_then(Value::as_str),
        Some("external")
    );
    assert_eq!(
        deadline.get("request_status").and_then(Value::as_str),
        Some("failed")
    );
    assert!(
        deadline.get("request_lifecycle_state").is_none(),
        "Amy trace records must expose request state only as request_status: {deadline:#}"
    );
    assert_eq!(
        deadline.get("response_status").and_then(Value::as_str),
        Some("failed")
    );

    assert_eq!(
        timed_out.get("tool_status").and_then(Value::as_str),
        Some("timedOut")
    );
    assert_eq!(
        timed_out.get("tool_result_ok").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        timed_out.get("tool_failure_class").and_then(Value::as_str),
        Some("external")
    );
    assert_eq!(
        timed_out.get("failure_class").and_then(Value::as_str),
        Some("external")
    );
    assert_eq!(
        timed_out
            .get("tool_error")
            .and_then(|value| value.get("retryable"))
            .and_then(Value::as_bool),
        Some(true),
        "external failures are retryable: {timed_out}"
    );
    assert_eq!(
        timed_out.get("latency_ms").and_then(Value::as_i64),
        Some(1500)
    );

    Ok(())
}

#[test]
fn trace_project_schema_prints_adapter_contracts_without_runtime() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let schema_cases = [
        ("openai-codex", "openai_codex_run_trace"),
        ("langgraph", "langgraph_state_history"),
        ("multi-agent", "multi_agent_task"),
    ];

    for (cli_projection, snapshot_name) in schema_cases {
        let json_schema_output = run_cli_text(
            tempdir.path(),
            &[
                "utils",
                "debug",
                "trace",
                "project-schema",
                "--projection",
                cli_projection,
            ],
        )?;
        let json_schema =
            serde_json::from_str::<Value>(&json_schema_output).context("parsing JSON schema")?;
        let expected_json_schema = read_workspace_json(&format!(
            "crates/gents/tests/fixtures/adapter_projections/contracts/{snapshot_name}.schema.json"
        ))?;
        assert_eq!(
            json_schema, expected_json_schema,
            "{cli_projection} JSON schema drifted from checked-in snapshot"
        );

        let jsonl_schema_output = run_cli_text(
            tempdir.path(),
            &[
                "utils",
                "debug",
                "trace",
                "project-schema",
                "--projection",
                cli_projection,
                "--format",
                "jsonl",
            ],
        )?;
        let jsonl_schema =
            serde_json::from_str::<Value>(&jsonl_schema_output).context("parsing JSONL schema")?;
        let expected_jsonl_schema = read_workspace_json(&format!(
            "crates/gents/tests/fixtures/adapter_projections/contracts/{snapshot_name}.jsonl-record.schema.json"
        ))?;
        assert_eq!(
            jsonl_schema, expected_jsonl_schema,
            "{cli_projection} JSONL schema drifted from checked-in snapshot"
        );

        let eval_jsonl_schema_output = run_cli_text(
            tempdir.path(),
            &[
                "utils",
                "debug",
                "trace",
                "project-schema",
                "--projection",
                cli_projection,
                "--format",
                "eval-jsonl",
            ],
        )?;
        let eval_jsonl_schema = serde_json::from_str::<Value>(&eval_jsonl_schema_output)
            .context("parsing eval JSONL schema")?;
        let expected_eval_jsonl_schema = read_workspace_json(&format!(
            "crates/gents/tests/fixtures/adapter_projections/contracts/{snapshot_name}.eval-jsonl-record.schema.json"
        ))?;
        assert_eq!(
            eval_jsonl_schema, expected_eval_jsonl_schema,
            "{cli_projection} eval JSONL schema drifted from checked-in snapshot"
        );
    }

    let atif_native_schema_output = run_cli_text(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "project-schema",
            "--projection",
            "atif",
            "--format",
            "native-json",
        ],
    )?;
    let atif_native_schema = serde_json::from_str::<Value>(&atif_native_schema_output)
        .context("parsing ATIF native JSON schema")?;
    assert_eq!(
        atif_native_schema
            .pointer("/properties/schema_version/const")
            .and_then(Value::as_str),
        Some("ATIF-v1.7")
    );
    assert_eq!(
        atif_native_schema.get("$id").and_then(Value::as_str),
        Some(
            "https://schemas.defra.ai/gents/adapter-projection/atif_trajectory/v1-native.schema.json"
        )
    );

    Ok(())
}

#[tokio::test]
async fn trace_timeline_reconstructs_request_events_from_persisted_rows() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let agent_home = tempdir.path().join("agent-home");

    {
        let node = initialized_trace_node(tempdir.path(), &agent_home).await?;
        ensure_runtime_schemas(node.node()).await?;
        seed_trace_export_rows(&node).await?;
        node.shutdown().await?;
    }

    let output = run_cli_text(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "timeline",
            "--home",
            agent_home.to_str().context("agent home utf8")?,
            "--request-id",
            "req-1",
        ],
    )?;
    let timeline = serde_json::from_str::<Value>(&output).context("parsing timeline JSON")?;

    assert_eq!(
        timeline.get("request_id").and_then(Value::as_str),
        Some("req-1")
    );
    assert_eq!(
        timeline.get("session_id").and_then(Value::as_str),
        Some("session-1")
    );
    assert_eq!(
        timeline
            .pointer("/session/title/text")
            .and_then(Value::as_str),
        Some("Trace export test")
    );
    let events = timeline
        .get("events")
        .and_then(Value::as_array)
        .context("timeline events array")?;
    assert!(
        events
            .iter()
            .any(|event| event.get("kind").and_then(Value::as_str) == Some("request")),
        "timeline missing request event: {timeline:#}"
    );
    assert!(
        events.iter().any(|event| {
            event.get("kind").and_then(Value::as_str) == Some("message")
                && event.get("sequence").and_then(Value::as_i64) == Some(2)
        }),
        "timeline missing assistant message event: {timeline:#}"
    );
    let failed_tool = events
        .iter()
        .find(|event| {
            event.get("kind").and_then(Value::as_str) == Some("tool_call")
                && event.get("tool_call_id").and_then(Value::as_str) == Some("call-fail")
        })
        .unwrap_or_else(|| panic!("missing call-fail tool event: {timeline:#}"));
    assert_eq!(
        failed_tool.get("request_id").and_then(Value::as_str),
        Some("req-1")
    );
    assert_eq!(
        failed_tool.get("tool_name").and_then(Value::as_str),
        Some("bash")
    );
    let final_message = events
        .iter()
        .find(|event| {
            event.get("kind").and_then(Value::as_str) == Some("message")
                && event.get("request_id").and_then(Value::as_str) == Some("req-1")
                && event.pointer("/header/sequence").and_then(Value::as_u64) == Some(6)
        })
        .unwrap_or_else(|| panic!("timeline missing canonical final message: {timeline:#}"));
    assert!(final_message["doc_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    let native: gents_protocol::message::Message =
        serde_json::from_value(final_message["message"].clone())?;
    assert!(
        matches!(native, gents_protocol::message::Message::Assistant { content, .. }
        if content.iter().any(|block| matches!(block,
            gents_protocol::message::AssistantContent::Text(text) if text.text == "done")))
    );

    Ok(())
}

async fn seed_rendered_request_rows(node: &UserSeededHome) -> Result<()> {
    let request_doc_id = exec_doc_id(
        node,
        r#"mutation {
            create_AgentRequest(input: {purpose: "normal", 
                request_id: "req-cap",
                node_did: "did:test:amy",
                agent_did: "amy",
                session_id: "session-cap",
                content: "capture me",

                lifecycle_state: "completed",
                backend_id: "studios-cluster",
                failure_reason: "",
                created_at: "2026-08-07T12:00:00Z",
                retry_count: 0
            }) { _docID }
        }"#,
        "AgentRequest",
    )
    .await?;
    let response = node
        .execute(&format!(
            r#"query {{ _commits(docID: "{}") {{ cid fieldName }} }}"#,
            gents_store::graphql::escape_graphql_string(&request_doc_id)
        ))
        .await;
    if response.has_errors() {
        anyhow::bail!(
            "querying seeded request commits failed: {:?}",
            response.errors
        );
    }
    let request_commit_cid = response
        .data
        .as_ref()
        .and_then(|data| data.get("_commits"))
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("fieldName").and_then(Value::as_str) == Some("_C"))
        })
        .and_then(|row| row.get("cid"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("seeded AgentRequest has no composite commit cid")?;
    let escaped_request_doc_id = gents_store::graphql::escape_graphql_string(&request_doc_id);
    let escaped_request_commit_cid =
        gents_store::graphql::escape_graphql_string(&request_commit_cid);
    for (suffix, attempt, call_id, call_seq, created_at) in [
        ("a0", 0, "call-cap-1", 1, "2026-08-07T12:00:02Z"),
        ("a1", 1, "call-cap-2", 2, "2026-08-07T12:00:04Z"),
    ] {
        let provenance = json!({
            "manifest_version": 3,
            "status": "captured_only",
            "status_reason": "seeded",
            "capture_seam": "transport_body",
            "capture_scope": "inference.1",
            "admission": { "call_id": call_id, "call_seq": call_seq },
            "assembly_trace": {
                "trace_version": 4,
                "build_path": "budgeted",
                "effective_message_count": 0,
                "effective_messages": null,
                "assistant_message_ids": [],
                "threaded_tool_results": [],
                "reduction_keys": [],
                "context_accounting": null
            }
        })
        .to_string();
        let request_json = json!({
            "model": "test-model",
            "messages": [{ "role": "user", "content": "capture me" }]
        })
        .to_string();
        exec(
            node,
            &format!(
                r#"mutation {{
                    create_RenderedRequest(input: {{
                        capture_key: "rendered:v1:seeded-{suffix}",
                        request_doc_id: "{escaped_request_doc_id}",
                        request_commit_cid: "{escaped_request_commit_cid}",
                        request_id: "req-cap",
                        session_id: "session-cap",
                        node_did: "did:test:amy",
                        requester_did: "",
                        agent_did: "amy",
                        capture_scope: "inference.1",
                        turn_index: 0,
                        attempt: {attempt},
                        capture_version: 1,
                        model_name: "test-model",
                        source: "openai_chat_completions",
                        request_json: "{request_json}",
                        provenance_json: "{provenance}",
                        created_at: "{created_at}"
                    }}) {{ _docID }}
                }}"#,
                request_json = gents_store::graphql::escape_graphql_string(&request_json),
                provenance = gents_store::graphql::escape_graphql_string(&provenance),
                escaped_request_doc_id = escaped_request_doc_id,
                escaped_request_commit_cid = escaped_request_commit_cid,
            ),
        )
        .await?;
    }
    Ok(())
}

async fn seed_v2_rendered_request_rows(node: &UserSeededHome) -> Result<()> {
    let mut first_capture_key = String::new();
    for turn in 0..2usize {
        let capture_scope = "inference.1".to_string();
        let assembly_trace =
            gents_inference::rendered_request::AssemblyTrace::from_effective_messages(
                gents_inference::rendered_request::AssemblyBuildPath::Budgeted,
                Vec::new(),
            );
        let mut payload = serde_json::to_value(&assembly_trace)?;
        payload["threaded_tool_results"] = json!([{
            "tool_call_id": "native-result",
            "native_tool_output": {"ok": false, "exit_code": 7},
            "padding": "p".repeat(4096 + turn * 512)
        }]);
        let rendered = RenderedCompletionRequest {
            capture_key: gents_inference::rendered_request::capture_key(
                "did:test:amy",
                "session-cap-v2",
                "",
                &capture_scope,
                turn,
                0,
            )?,
            capture_version: gents_inference::rendered_request::CAPTURE_VERSION,
            request_doc_id: String::new(),
            request_commit_cid: String::new(),
            request_id: "req-cap-v2".into(),
            capture_scope: capture_scope.clone(),
            turn_index: turn,
            attempt: 0,
            node_did: "did:test:amy".into(),
            requester_did: String::new(),
            agent_did: "amy".into(),
            session_id: "session-cap-v2".into(),
            model_name: "test-model".into(),
            source: gents_inference::rendered_request::RenderedRequestSource::OpenAiChatCompletions,
            request_json: json!({
                "model":"test-model",
                "messages":[{"role":"user","content":"x".repeat(8192 + turn * 512)}]
            }),
            messages_json: json!([]),
            tools_json: json!([]),
            tool_choice_json: Value::Null,
            sampling_json: Value::Null,
            provenance_json: serde_json::to_value(
                gents_inference::rendered_request::ProvenanceManifest::captured_only(
                    capture_scope,
                    None,
                    None,
                    assembly_trace.clone(),
                ),
            )?,
            provenance_payload_json: payload,
            assembly_trace,
        };
        if turn == 0 {
            first_capture_key = rendered.capture_key.clone();
        }
        let canonical_provenance = gents_inference::rendered_request::canonical_json_string(
            &rendered.provenance_payload_json,
        )?;
        if turn == 0 {
            let container = json!({
                "gents_capture_json": 1,
                "request_body": {"gents_lossless_json": 1, "kind": "full", "value": rendered.request_json},
                "provenance_payload": {
                    "gents_lossless_json": 1, "kind": "full", "value": rendered.provenance_payload_json
                }
            });
            exec(
                node,
                &format!(
                    r#"mutation {{ create_RenderedRequest(input: {{
                        capture_key: "{capture_key}", request_doc_id: "", request_commit_cid: "",
                        request_id: "{request_id}", session_id: "{session_id}",
                        node_did: "did:test:amy", requester_did: "", agent_did: "amy",
                        capture_scope: "inference.1", turn_index: {turn}, attempt: 0,
                        capture_version: 2, model_name: "test-model", source: "openai_chat_completions",
                        request_json: "{request_json}", provenance_json: "{provenance}",
                        created_at: "2026-10-07T00:00:0{turn}Z"
                    }}) {{ _docID }} }}"#,
                    capture_key = gents_store::graphql::escape_graphql_string(&rendered.capture_key),
                    request_id = gents_store::graphql::escape_graphql_string(&rendered.request_id),
                    session_id = gents_store::graphql::escape_graphql_string(&rendered.session_id),
                    request_json = gents_store::graphql::escape_graphql_string(&container.to_string()),
                    provenance = gents_store::graphql::escape_graphql_string(&canonical_provenance),
                ),
            )
            .await?;
            continue;
        }
        let first_row = node
            .execute(&format!(
                r#"{{RenderedRequest(filter:{{capture_key:{{_eq:"{}"}}}},limit:1){{_docID}}}}"#,
                gents_store::graphql::escape_graphql_string(&first_capture_key)
            ))
            .await;
        anyhow::ensure!(!first_row.has_errors(), "{:?}", first_row.errors);
        let first_doc_id = first_row.data.expect("data")["RenderedRequest"][0]["_docID"]
            .as_str()
            .context("first v2 row doc id")?
            .to_owned();
        let base_cid = node
            .execute(&format!(
                r#"{{_commits(docID:"{}"){{cid fieldName}}}}"#,
                gents_store::graphql::escape_graphql_string(&first_doc_id)
            ))
            .await;
        anyhow::ensure!(!base_cid.has_errors(), "{:?}", base_cid.errors);
        let base_cid = base_cid.data.expect("data")["_commits"]
            .as_array()
            .context("commits")?
            .iter()
            .find(|commit| commit["fieldName"].as_str() == Some("request_json"))
            .and_then(|commit| commit["cid"].as_str())
            .context("request_json field commit")?
            .to_owned();
        let witness = json!({
            "doc_id": first_doc_id,
            "field_commit_cid": base_cid,
            "depth": 0,
            "node_did": rendered.node_did,
            "requester_did": rendered.requester_did,
            "session_id": rendered.session_id,
            "source": "openai_chat_completions",
            "capture_scope": rendered.capture_scope,
        });
        let container = json!({
            "gents_capture_json": 1,
            "request_body": {
                "gents_lossless_json": 1,
                "kind": "object_delta",
                "base": witness,
                "changed": {"messages": {"kind": "full", "value": rendered.request_json["messages"]}},
                "removed": []
            },
            "provenance_payload": {
                "gents_lossless_json": 1,
                "kind": "object_delta",
                "base": witness,
                "changed": {
                    "threaded_tool_results": {
                        "kind": "full",
                        "value": rendered.provenance_payload_json["threaded_tool_results"]
                    }
                },
                "removed": []
            }
        });
        exec(
            node,
            &format!(
                r#"mutation {{ create_RenderedRequest(input: {{
                    capture_key: "{capture_key}", request_doc_id: "", request_commit_cid: "",
                    request_id: "{request_id}", session_id: "{session_id}",
                    node_did: "did:test:amy", requester_did: "", agent_did: "amy",
                    capture_scope: "inference.1", turn_index: {turn}, attempt: 0,
                    capture_version: 2, model_name: "test-model", source: "openai_chat_completions",
                    request_json: "{request_json}", provenance_json: "{provenance}",
                    created_at: "2026-10-07T00:00:0{turn}Z"
                }}) {{ _docID }} }}"#,
                capture_key = gents_store::graphql::escape_graphql_string(&rendered.capture_key),
                request_id = gents_store::graphql::escape_graphql_string(&rendered.request_id),
                session_id = gents_store::graphql::escape_graphql_string(&rendered.session_id),
                request_json = gents_store::graphql::escape_graphql_string(&container.to_string()),
                provenance = gents_store::graphql::escape_graphql_string(&canonical_provenance),
            ),
        )
        .await?;
    }
    Ok(())
}

/// Captures a manifest request as the runtime's sink does: a managed Agent
/// signs it into its own session, which the home's User then reads.
async fn seed_manifest_rendered_request_row(node: &UserSeededHome) -> Result<()> {
    let session_id = "session-cap-manifest";
    let agent = node
        .managed_agent_session("Trace capture agent", session_id)
        .await?;
    let (node_did, agent_did) = (node.node_did().to_owned(), agent.did().to_owned());
    let capture_scope = "inference.1".to_string();
    let assembly_trace = gents_inference::rendered_request::AssemblyTrace::from_effective_messages(
        gents_inference::rendered_request::AssemblyBuildPath::Budgeted,
        Vec::new(),
    );
    let mut provenance_payload = serde_json::to_value(&assembly_trace)?;
    provenance_payload["threaded_tool_results"] = json!([{
        "tool_call_id": "native-result",
        "native_tool_output": {"ok": true, "exit_code": 3},
        "padding": "q".repeat(6 * 1024)
    }]);
    let sink = gents_inference::rendered_request::DefraRenderedRequestSink::new_managed(
        Arc::clone(node.node()),
        agent.as_ref(),
    )?;
    sink.capture(RenderedCompletionRequest {
        capture_key: gents_inference::rendered_request::capture_key(
            &node_did,
            session_id,
            &agent_did,
            &capture_scope,
            0,
            0,
        )?,
        capture_version: gents_inference::rendered_request::CAPTURE_VERSION,
        request_doc_id: String::new(),
        request_commit_cid: String::new(),
        request_id: "req-cap-manifest".into(),
        capture_scope: capture_scope.clone(),
        turn_index: 0,
        attempt: 0,
        node_did: node_did.clone(),
        requester_did: agent_did.clone(),
        agent_did: agent_did.clone(),
        session_id: session_id.into(),
        model_name: "test-model".into(),
        source: gents_inference::rendered_request::RenderedRequestSource::OpenAiChatCompletions,
        request_json: json!({
            "model": "test-model",
            "messages": [{"role": "user", "content": "manifest capture"}]
        }),
        messages_json: json!([]),
        tools_json: json!([]),
        tool_choice_json: Value::Null,
        sampling_json: Value::Null,
        provenance_json: serde_json::to_value(
            gents_inference::rendered_request::ProvenanceManifest::captured_only(
                capture_scope,
                None,
                None,
                assembly_trace.clone(),
            ),
        )?,
        provenance_payload_json: provenance_payload,
        assembly_trace,
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn trace_capture_fetches_metadata_with_field_commit_cid() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let agent_home = tempdir.path().join("agent-home");

    {
        let node = initialized_trace_node(tempdir.path(), &agent_home).await?;
        ensure_runtime_schemas(node.node()).await?;
        seed_rendered_request_rows(&node).await?;
        seed_v2_rendered_request_rows(&node).await?;
        seed_manifest_rendered_request_row(&node).await?;
        node.shutdown().await?;
    }
    let home = agent_home.to_str().context("agent home utf8")?;

    let output = run_cli_text(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "capture",
            "--home",
            home,
            "--request-id",
            "req-cap",
            "--list",
        ],
    )?;
    let listing = serde_json::from_str::<Value>(&output).context("parsing capture list")?;
    let captures = listing
        .get("captures")
        .and_then(Value::as_array)
        .context("captures array")?;
    assert_eq!(captures.len(), 2, "{listing:#}");
    assert_eq!(
        captures[0].get("attempt").and_then(Value::as_i64),
        Some(0),
        "identity order: attempt 0 first"
    );
    assert!(
        captures
            .iter()
            .all(|capture| capture.get("request_json").is_none()),
        "list output must not carry bodies: {listing:#}"
    );
    assert_eq!(
        captures[0].get("call_seq").and_then(Value::as_i64),
        Some(1),
        "admission join surfaces: {listing:#}"
    );

    let output = run_cli_text(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "capture",
            "--home",
            home,
            "--request-id",
            "req-cap",
            "--scope",
            "inference.1",
            "--turn",
            "0",
            "--attempt",
            "0",
        ],
    )?;
    let capture = serde_json::from_str::<Value>(&output).context("parsing single capture")?;
    assert_eq!(
        capture.get("capture_key").and_then(Value::as_str),
        Some("rendered:v1:seeded-a0")
    );
    assert_eq!(
        capture.get("provenance_status").and_then(Value::as_str),
        Some("captured_only")
    );
    let cid = capture
        .pointer("/request_json_commit/cid")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected field-commit cid: {capture:#}"));
    assert!(!cid.is_empty());
    assert!(
        capture.get("request_json").is_none(),
        "default output must not carry the body: {capture:#}"
    );

    let output = run_cli_text(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "capture",
            "--home",
            home,
            "--capture-key",
            "rendered:v1:seeded-a1",
            "--include-body",
        ],
    )?;
    let capture = serde_json::from_str::<Value>(&output).context("parsing bodied capture")?;
    let body = capture
        .get("request_json")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected request_json with --include-body: {capture:#}"));
    assert!(body.contains("capture me"));
    assert!(capture.get("provenance_json").is_some());
    let provenance_payload = capture
        .get("provenance_payload_json")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("decoded provenance payload must be JSON text: {capture:#}"));
    let provenance_payload: Value =
        serde_json::from_str(provenance_payload).context("parsing decoded provenance payload")?;
    assert_eq!(provenance_payload["trace_version"], 4);

    let output = run_cli_text(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "capture",
            "--home",
            home,
            "--request-id",
            "req-cap-v2",
            "--include-body",
            "--list",
        ],
    )?;
    let listing: Value = serde_json::from_str(&output)?;
    let v2 = listing["captures"].as_array().context("v2 captures")?;
    assert_eq!(v2.len(), 2);
    for capture in v2 {
        assert!(capture["request_json"].is_string(), "{capture:#}");
        let payload = capture["provenance_payload_json"]
            .as_str()
            .unwrap_or_else(|| panic!("v2 provenance payload must be JSON text: {capture:#}"));
        let payload: Value = serde_json::from_str(payload)?;
        assert_eq!(
            payload["threaded_tool_results"][0]["native_tool_output"]["exit_code"],
            7
        );
    }

    let output = run_cli_text(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "capture",
            "--home",
            home,
            "--request-id",
            "req-cap-manifest",
            "--include-body",
        ],
    )?;
    let capture = serde_json::from_str::<Value>(&output)?;
    let body = capture["request_json"].as_str().context("manifest body")?;
    assert!(body.contains("manifest capture"), "{body}");
    let payload = capture["provenance_payload_json"]
        .as_str()
        .context("payload")?;
    let payload: Value = serde_json::from_str(payload)?;
    assert_eq!(
        payload["threaded_tool_results"][0]["native_tool_output"]["exit_code"],
        3
    );

    let stderr = run_cli_failure_stderr(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "capture",
            "--home",
            home,
            "--request-id",
            "req-cap",
        ],
    )?;
    assert!(
        stderr.contains("narrow with --scope/--turn/--attempt"),
        "{stderr}"
    );

    let output = run_cli_text(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "timeline",
            "--home",
            home,
            "--request-id",
            "req-cap",
        ],
    )?;
    let timeline = serde_json::from_str::<Value>(&output).context("parsing timeline")?;
    let rendered_events = timeline
        .get("events")
        .and_then(Value::as_array)
        .context("timeline events")?
        .iter()
        .filter(|event| event.get("kind").and_then(Value::as_str) == Some("rendered_request"))
        .collect::<Vec<_>>();
    assert_eq!(rendered_events.len(), 2, "{timeline:#}");
    assert_eq!(
        rendered_events[0]
            .get("provenance_status")
            .and_then(Value::as_str),
        Some("captured_only")
    );
    assert!(
        rendered_events
            .iter()
            .all(|event| event.get("request_json").is_none()
                && event.get("provenance_json").is_none()),
        "timeline events must stay metadata-only: {timeline:#}"
    );

    Ok(())
}

#[tokio::test]
async fn trace_project_exports_first_adapter_shapes_from_persisted_rows() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let agent_home = tempdir.path().join("agent-home");

    {
        let node = initialized_trace_node(tempdir.path(), &agent_home).await?;
        ensure_runtime_schemas(node.node()).await?;
        seed_trace_export_rows(&node).await?;
        node.shutdown().await?;
    }

    let home = agent_home.to_str().context("agent home utf8")?;
    let atif = trace_project_json_with_extra_args(
        tempdir.path(),
        home,
        "atif",
        "full",
        &["--format", "native-json"],
    )?;
    assert_eq!(
        atif.get("schema_version").and_then(Value::as_str),
        Some("ATIF-v1.7")
    );
    assert!(atif.get("projection_id").is_none());
    assert!(
        atif.get("steps")
            .and_then(Value::as_array)
            .is_some_and(|steps| !steps.is_empty()),
        "ATIF projection should contain trajectory steps: {atif:#}"
    );
    assert!(
        atif.pointer("/agent/name")
            .and_then(Value::as_str)
            .is_some(),
        "ATIF projection should identify the Gents agent: {atif:#}"
    );
    let openai = trace_project_json(tempdir.path(), home, "openai-codex", "public")?;
    assert_projection_json_matches_schema("openai_codex_run_trace", &openai)?;
    assert_eq!(
        openai.get("projection_id").and_then(Value::as_str),
        Some("openai_codex_run_trace")
    );
    assert_eq!(
        openai.get("redaction_mode").and_then(Value::as_str),
        Some("public")
    );
    assert_eq!(
        openai.pointer("/output/adapter").and_then(Value::as_str),
        Some("openai_codex_run_trace")
    );
    let serialized_openai = serde_json::to_string(&openai)?;
    assert!(
        !serialized_openai.contains("Inspect the repo and show README.md"),
        "public adapter projection leaked request content: {openai:#}"
    );
    assert!(
        serialized_openai.contains("[redacted]"),
        "public adapter projection should show redaction markers: {openai:#}"
    );
    let openai_jsonl = trace_project_jsonl_lines(tempdir.path(), home, "openai-codex", "public")?;
    assert_projection_records_match_schema(
        "openai_codex_run_trace",
        "jsonl-record.schema",
        &openai_jsonl,
    )?;
    assert!(
        !openai_jsonl.is_empty(),
        "expected openai-codex JSONL projection records"
    );
    assert!(openai_jsonl.iter().all(|record| {
        record.get("projection_id").and_then(Value::as_str) == Some("openai_codex_run_trace")
            && record.get("source_request_id").and_then(Value::as_str) == Some("req-1")
            && record.get("record_kind").and_then(Value::as_str) == Some("openai_codex_trace_item")
    }));
    let serialized_openai_jsonl = serde_json::to_string(&openai_jsonl)?;
    assert!(
        !serialized_openai_jsonl.contains("Inspect the repo and show README.md"),
        "public JSONL adapter projection leaked request content: {openai_jsonl:#?}"
    );
    let openai_eval_jsonl =
        trace_project_eval_jsonl_lines(tempdir.path(), home, "openai-codex", "public")?;
    assert_projection_records_match_schema(
        "openai_codex_run_trace",
        "eval-jsonl-record.schema",
        &openai_eval_jsonl,
    )?;
    assert!(
        !openai_eval_jsonl.is_empty(),
        "expected openai-codex eval JSONL records"
    );
    assert!(openai_eval_jsonl.iter().all(|record| {
        record.get("projection_id").and_then(Value::as_str) == Some("openai_codex_run_trace")
            && record.get("source_request_id").and_then(Value::as_str) == Some("req-1")
            && record.get("adapter_record_kind").and_then(Value::as_str)
                == Some("openai_codex_trace_item")
    }));
    assert!(
        openai_eval_jsonl.iter().any(|record| {
            record.get("sample_kind").and_then(Value::as_str) == Some("tool_call")
                && record.get("tool_name").and_then(Value::as_str) == Some("bash")
        }),
        "eval JSONL should retain tool-call evidence: {openai_eval_jsonl:#?}"
    );
    let serialized_openai_eval_jsonl = serde_json::to_string(&openai_eval_jsonl)?;
    assert!(
        !serialized_openai_eval_jsonl.contains("Inspect the repo and show README.md"),
        "public eval JSONL adapter projection leaked request content: {openai_eval_jsonl:#?}"
    );
    for projection in ["atif", "openai-codex", "langgraph", "multi-agent"] {
        let training_safe = trace_project_json(tempdir.path(), home, projection, "training-safe")?;
        let serialized_training_safe = serde_json::to_string(&training_safe)?;
        assert_eq!(
            training_safe.get("redaction_mode").and_then(Value::as_str),
            Some("training_safe"),
            "{projection} should record training-safe redaction mode"
        );
        assert!(
            serialized_training_safe.contains("[training_safe_redacted]"),
            "{projection} training-safe projection should include training-safe redaction markers: {training_safe:#}"
        );
        for sensitive_text in [
            "Inspect the repo and show README.md",
            "reviewer private child message",
        ] {
            assert!(
                !serialized_training_safe.contains(sensitive_text),
                "{projection} training-safe projection leaked sensitive text {sensitive_text:?}: {training_safe:#}"
            );
        }
    }
    let openai_full = trace_project_json(tempdir.path(), home, "openai-codex", "full")?;
    assert_projection_json_matches_schema("openai_codex_run_trace", &openai_full)?;
    let openai_full_serialized = serde_json::to_string(&openai_full)?;
    {
        let expected_child_text = "reviewer private child message";
        assert!(
            openai_full_serialized.contains(expected_child_text),
            "unscoped full projection should include child-agent content {expected_child_text:?}: {openai_full:#}"
        );
    }
    let scoped_openai = trace_project_json_with_extra_args(
        tempdir.path(),
        home,
        "openai-codex",
        "full",
        &["--scope-node-did", "did:test:amy"],
    )?;
    let scoped_openai_serialized = serde_json::to_string(&scoped_openai)?;
    assert!(
        scoped_openai_serialized.contains("req-child"),
        "scoped projection should retain child delegation metadata: {scoped_openai:#}"
    );
    assert!(
        !scoped_openai_serialized.contains("reviewer private child message"),
        "scoped projection leaked child-agent message content: {scoped_openai:#}"
    );
    let denied_scope = run_cli_failure_stderr(
        tempdir.path(),
        &[
            "utils",
            "debug",
            "trace",
            "project",
            "--home",
            home,
            "--request-id",
            "req-1",
            "--projection",
            "openai-codex",
            "--scope-node-did",
            "did:test:reviewer",
        ],
    )?;
    assert!(
        denied_scope.contains("projection scope denied request req-1"),
        "expected projection scope denial, got:\n{denied_scope}"
    );

    let langgraph = trace_project_json(tempdir.path(), home, "langgraph", "full")?;
    assert_projection_json_matches_schema("langgraph_state_history", &langgraph)?;
    assert_eq!(
        langgraph.get("projection_id").and_then(Value::as_str),
        Some("langgraph_state_history")
    );
    assert_eq!(
        langgraph.pointer("/output/adapter").and_then(Value::as_str),
        Some("langgraph_state_history")
    );
    let edges = langgraph
        .pointer("/output/projection/edges")
        .and_then(Value::as_array)
        .context("langgraph edges")?;
    assert!(
        edges.iter().any(|edge| {
            edge.get("kind").and_then(Value::as_str) == Some("child_request")
                && edge.get("to").and_then(Value::as_str) == Some("request:req-child")
        }),
        "langgraph projection missing child request edge: {langgraph:#}"
    );
    let langgraph_jsonl = trace_project_jsonl_lines(tempdir.path(), home, "langgraph", "full")?;
    assert_projection_records_match_schema(
        "langgraph_state_history",
        "jsonl-record.schema",
        &langgraph_jsonl,
    )?;
    assert!(
        langgraph_jsonl.iter().any(|record| {
            record.get("record_kind").and_then(Value::as_str) == Some("langgraph_edge")
                && record.pointer("/value/kind").and_then(Value::as_str) == Some("child_request")
                && record.pointer("/value/to").and_then(Value::as_str) == Some("request:req-child")
        }),
        "langgraph JSONL projection missing child request edge record: {langgraph_jsonl:#?}"
    );
    let langgraph_eval_jsonl =
        trace_project_eval_jsonl_lines(tempdir.path(), home, "langgraph", "full")?;
    assert_projection_records_match_schema(
        "langgraph_state_history",
        "eval-jsonl-record.schema",
        &langgraph_eval_jsonl,
    )?;
    assert!(
        langgraph_eval_jsonl.iter().any(|record| {
            record.get("sample_kind").and_then(Value::as_str) == Some("task")
                && record.get("tool_name").and_then(Value::as_str) == Some("bash")
        }),
        "langgraph eval JSONL projection missing task sample: {langgraph_eval_jsonl:#?}"
    );
    assert!(
        langgraph_eval_jsonl.iter().any(|record| {
            record.get("sample_kind").and_then(Value::as_str) == Some("state_transition")
                && record.pointer("/metadata/kind").and_then(Value::as_str) == Some("child_request")
        }),
        "langgraph eval JSONL projection missing child transition sample: {langgraph_eval_jsonl:#?}"
    );

    let multi_agent = trace_project_json(tempdir.path(), home, "multi-agent", "full")?;
    assert_projection_json_matches_schema("multi_agent_task", &multi_agent)?;
    assert_eq!(
        multi_agent.get("projection_id").and_then(Value::as_str),
        Some("multi_agent_task")
    );
    assert_eq!(
        multi_agent
            .pointer("/output/adapter")
            .and_then(Value::as_str),
        Some("multi_agent_task")
    );
    let delegations = multi_agent
        .pointer("/output/projection/delegations")
        .and_then(Value::as_array)
        .context("multi-agent delegations")?;
    assert!(
        delegations.iter().any(|delegation| {
            delegation.get("parent_request_id").and_then(Value::as_str) == Some("req-1")
                && delegation.get("child_request_id").and_then(Value::as_str) == Some("req-child")
        }),
        "multi-agent projection missing child delegation: {multi_agent:#}"
    );
    let multi_agent_jsonl = trace_project_jsonl_lines(tempdir.path(), home, "multi-agent", "full")?;
    assert_projection_records_match_schema(
        "multi_agent_task",
        "jsonl-record.schema",
        &multi_agent_jsonl,
    )?;
    assert!(
        multi_agent_jsonl.iter().any(|record| {
            record.get("record_kind").and_then(Value::as_str) == Some("multi_agent_delegation")
                && record
                    .pointer("/value/parent_request_id")
                    .and_then(Value::as_str)
                    == Some("req-1")
                && record
                    .pointer("/value/child_request_id")
                    .and_then(Value::as_str)
                    == Some("req-child")
        }),
        "multi-agent JSONL projection missing delegation record: {multi_agent_jsonl:#?}"
    );
    let multi_agent_eval_jsonl =
        trace_project_eval_jsonl_lines(tempdir.path(), home, "multi-agent", "full")?;
    assert_projection_records_match_schema(
        "multi_agent_task",
        "eval-jsonl-record.schema",
        &multi_agent_eval_jsonl,
    )?;
    assert!(
        multi_agent_eval_jsonl.iter().any(|record| {
            record.get("sample_kind").and_then(Value::as_str) == Some("delegation")
                && record.get("parent_request_id").and_then(Value::as_str) == Some("req-1")
                && record.get("child_request_id").and_then(Value::as_str) == Some("req-child")
        }),
        "multi-agent eval JSONL projection missing delegation sample: {multi_agent_eval_jsonl:#?}"
    );

    Ok(())
}

fn trace_project_json(
    cwd: &std::path::Path,
    home: &str,
    projection: &str,
    redaction: &str,
) -> Result<Value> {
    trace_project_json_with_extra_args(cwd, home, projection, redaction, &[])
}

fn trace_project_json_with_extra_args(
    cwd: &std::path::Path,
    home: &str,
    projection: &str,
    redaction: &str,
    extra_args: &[&str],
) -> Result<Value> {
    let mut args = vec![
        "utils",
        "debug",
        "trace",
        "project",
        "--home",
        home,
        "--request-id",
        "req-1",
        "--projection",
        projection,
        "--redaction",
        redaction,
        "--actor-did",
        "did:test:test-viewer",
    ];
    args.extend_from_slice(extra_args);
    let output = run_cli_text(cwd, &args)?;
    serde_json::from_str::<Value>(&output).context("parsing adapter projection JSON")
}

fn trace_project_jsonl_lines(
    cwd: &std::path::Path,
    home: &str,
    projection: &str,
    redaction: &str,
) -> Result<Vec<Value>> {
    let output = run_cli_text(
        cwd,
        &[
            "utils",
            "debug",
            "trace",
            "project",
            "--home",
            home,
            "--request-id",
            "req-1",
            "--projection",
            projection,
            "--redaction",
            redaction,
            "--format",
            "jsonl",
            "--actor-did",
            "did:test:test-viewer",
        ],
    )?;
    output
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).context("parsing adapter projection JSONL"))
        .collect::<Result<Vec<_>>>()
}

fn trace_project_eval_jsonl_lines(
    cwd: &std::path::Path,
    home: &str,
    projection: &str,
    redaction: &str,
) -> Result<Vec<Value>> {
    let output = run_cli_text(
        cwd,
        &[
            "utils",
            "debug",
            "trace",
            "project",
            "--home",
            home,
            "--request-id",
            "req-1",
            "--projection",
            projection,
            "--redaction",
            redaction,
            "--format",
            "eval-jsonl",
            "--actor-did",
            "did:test:test-viewer",
        ],
    )?;
    output
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).context("parsing adapter projection eval JSONL")
        })
        .collect::<Result<Vec<_>>>()
}

async fn initialized_trace_node(
    cwd: &std::path::Path,
    agent_home: &std::path::Path,
) -> Result<UserSeededHome> {
    UserSeededHome::open(cwd, agent_home, "trace-test").await
}

async fn exec(node: &UserSeededHome, query: &str) -> Result<()> {
    user_exec(node, query).await
}

async fn exec_doc_id(node: &UserSeededHome, query: &str, collection: &str) -> Result<String> {
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!("GraphQL mutation failed: {:?}", response.errors);
    }
    let create_key = format!("create_{collection}");
    let add_key = format!("add_{collection}");
    let value = response
        .data
        .as_ref()
        .and_then(|data| data.get(&create_key).or_else(|| data.get(&add_key)))
        .with_context(|| format!("{collection} mutation response missing create/add field"))?;
    value
        .get("_docID")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .as_array()
                .and_then(|rows| rows.first())
                .and_then(|row| row.get("_docID"))
                .and_then(Value::as_str)
        })
        .map(ToOwned::to_owned)
        .with_context(|| format!("{collection} mutation response missing _docID"))
}

async fn exec_variables_doc_id(
    node: &UserSeededHome,
    query: &str,
    variables: Value,
    collection: &str,
) -> Result<String> {
    let response = node.execute_with_variables(query, variables).await;
    anyhow::ensure!(!response.has_errors(), "{:?}", response.errors);
    gents_store::graphql::created_doc_id(&json!({"data": response.data}), collection)
        .with_context(|| format!("{collection} mutation omitted document"))
}

struct TraceRequest<'a> {
    request_doc_id: &'a str,
    request_id: &'a str,
    session_id: &'a str,
    node_did: &'a str,
}

struct ToolCallSeed<'a> {
    sequence: u32,
    tool_call_id: &'a str,
    tool_name: &'a str,
    arguments: Value,
    lifecycle_state: &'a str,
    result: Option<&'a str>,
    failure_class: Option<&'a str>,
}

async fn seed_canonical_trace_tool(
    node: &UserSeededHome,
    request: &TraceRequest<'_>,
    call: ToolCallSeed<'_>,
) -> Result<String> {
    let TraceRequest {
        request_doc_id,
        request_id,
        session_id,
        node_did,
    } = *request;
    let ToolCallSeed {
        sequence,
        tool_call_id,
        tool_name,
        arguments,
        lifecycle_state,
        result,
        failure_class,
    } = call;
    use gents_protocol::output::rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, ToolResultPart, TranscriptMessage,
    };
    use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

    let arguments = arguments.to_string();
    let tool_doc_id = exec_doc_id(
        node,
        &format!(r#"mutation {{ create_AgentToolCall(input: {{tool_call_key: "{}:{}", node_did: "{}", request_id: "{}", request_doc_id: "{}", session_id: "{}", message_sequence: {}, tool_name: "{}", tool_call_id: "{}", lifecycle_state: "{}", tool_failure_class: {}, started_at: "2026-05-04T12:00:04.500Z", completed_at: "2026-05-04T12:00:06Z"}}) {{_docID}} }}"#,
            escape_graphql_string(request_doc_id), escape_graphql_string(tool_call_id), escape_graphql_string(node_did), escape_graphql_string(request_id), escape_graphql_string(request_doc_id), escape_graphql_string(session_id), sequence, escape_graphql_string(tool_name), escape_graphql_string(tool_call_id), escape_graphql_string(lifecycle_state), failure_class.map(|v| format!("\"{}\"", escape_graphql_string(v))).unwrap_or_else(|| "null".into())),
        "AgentToolCall",
    ).await?;
    let generation = format!("trace:{request_doc_id}");
    let argument_bytes = arguments.len();
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: None,
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: sequence.into(),
            },
            turn_index: sequence,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: argument_bytes as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolArguments {
                    id: tool_call_id.into(),
                    call_id: Some(tool_call_id.into()),
                    name: tool_name.into(),
                },
            }),
        }],
        payload: arguments,
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![argument_bytes as u64],
        }),
        created_at: "2026-05-04T12:00:02Z".into(),
    };
    let close_doc_id = exec_variables_doc_id(
        node,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        output_segment_create_variables(&segment)?,
        "AgentOutputSegment",
    )
    .await?;
    let header = TranscriptMessage {
        native_request_id: None,
        message_key: format!("trace:{session_id}:{sequence}"),
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: None,
        request_doc_id: Some(request_doc_id.into()),
        publication: MessagePublication::RequestExecution {
            execution_generation: generation,
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::Assistant,
        native_id: None,
        blocks: vec![MessageBlock::ToolCall {
            tool_call_doc_id: tool_doc_id.clone(),
            id: tool_call_id.into(),
            call_id: Some(tool_call_id.into()),
            name: tool_name.into(),
            arguments: PayloadRef {
                close_doc_id,
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }],
        created_at: "2026-05-04T12:00:02Z".into(),
    };
    exec_variables_doc_id(
        node,
        CREATE_AGENT_MESSAGE_MUTATION,
        transcript_message_create_variables(&header)?,
        "AgentMessage",
    )
    .await?;
    if let Some(result) = result {
        let output = OutputSegment {
            node_did: node_did.into(),
            requester_did: None,
            session_id: session_id.into(),
            request_doc_id: request_doc_id.into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: tool_doc_id.clone(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: tool_doc_id.clone(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: result.len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::ToolOutput,
                }),
            }],
            payload: result.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![result.len() as u64],
            }),
            created_at: "2026-05-04T12:00:06Z".into(),
        };
        let close_doc_id = exec_variables_doc_id(
            node,
            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
            output_segment_create_variables(&output)?,
            "AgentOutputSegment",
        )
        .await?;
        let delivery = TranscriptMessage {
            native_request_id: None,
            message_key: format!("trace-delivery:{tool_doc_id}"),
            session_id: session_id.into(),
            node_did: node_did.into(),
            requester_did: None,
            request_doc_id: Some(request_doc_id.into()),
            publication: MessagePublication::ToolDelivery {
                tool_call_doc_id: tool_doc_id.clone(),
            },
            outcome: OutputOutcome::Complete,
            sequence: 100 + sequence,
            role: MessageRole::User,
            native_id: None,
            blocks: vec![MessageBlock::ToolResult {
                tool_call_doc_id: tool_doc_id.clone(),
                id: tool_call_id.into(),
                call_id: Some(tool_call_id.into()),
                parts: vec![ToolResultPart::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id,
                            stream: 0,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                }],
            }],
            created_at: "2026-05-04T12:00:06Z".into(),
        };
        exec_variables_doc_id(
            node,
            CREATE_AGENT_MESSAGE_MUTATION,
            transcript_message_create_variables(&delivery)?,
            "AgentMessage",
        )
        .await?;
    }
    Ok(tool_doc_id)
}

async fn seed_canonical_terminal_text(
    node: &UserSeededHome,
    request_doc_id: &str,
    session_id: &str,
    node_did: &str,
    sequence: u32,
    text: &str,
) -> Result<()> {
    use gents_protocol::output::rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
        OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TerminalOutput, TranscriptMessage,
    };
    let generation = format!("trace:{request_doc_id}");
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: None,
        session_id: session_id.into(),
        request_doc_id: request_doc_id.into(),
        source: OutputSource::Authored {
            key: format!("trace-terminal:{request_doc_id}"),
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: text.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: text.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![text.len() as u64],
        }),
        created_at: "2026-05-04T12:00:07Z".into(),
    };
    let close_doc_id = exec_variables_doc_id(
        node,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        output_segment_create_variables(&segment)?,
        "AgentOutputSegment",
    )
    .await?;
    let header = TranscriptMessage {
        native_request_id: None,
        message_key: format!("trace-terminal:{request_doc_id}"),
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: None,
        request_doc_id: Some(request_doc_id.into()),
        publication: MessagePublication::RequestExecution {
            execution_generation: generation,
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::Assistant,
        native_id: None,
        blocks: vec![MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id,
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        }],
        created_at: "2026-05-04T12:00:07Z".into(),
    };
    let message_doc_id = exec_variables_doc_id(
        node,
        CREATE_AGENT_MESSAGE_MUTATION,
        transcript_message_create_variables(&header)?,
        "AgentMessage",
    )
    .await?;
    let terminal = serde_json::to_value(TerminalOutput::Message { message_doc_id })?;
    let response = node.execute_with_variables(r#"mutation($id: String!, $terminal: JSON!) { update_AgentRequest(filter: {_docID: {_eq: $id}}, input: {terminal_output: $terminal}) {_docID} }"#, json!({"id": request_doc_id, "terminal": terminal})).await;
    anyhow::ensure!(!response.has_errors(), "{:?}", response.errors);
    Ok(())
}

async fn seed_trace_export_rows(node: &UserSeededHome) -> Result<()> {
    exec(
        node,
        r#"mutation {
            create_Agent(input: {
                agent_did: "amy",
                node_did: "did:test:amy",
                display_name: "Amy",
                enabled: true,
                created_at: "2026-05-04T12:00:00Z"
            }) { _docID }
        }"#,
    )
    .await?;
    exec(
        node,
        r#"mutation {
            create_AgentSession(input: {
                session_id: "session-1",
                node_did: "did:test:amy",
                agent_did: "amy",
                title: { text: "Trace export test", source: "user" },
                created_at: "2026-05-04T12:00:00Z",
                observation: { last_activity_at: "2026-05-04T12:00:05Z", preview: "Inspect the repo" }
            }) { _docID }
        }"#,
    )
    .await?;
    let root_request_doc_id = exec_doc_id(
        node,
        r#"mutation {
            create_AgentRequest(input: {purpose: "normal", 
                request_id: "req-1",
                node_did: "did:test:amy",
                agent_did: "amy",
                session_id: "session-1",
                content: "Inspect the repo and show README.md",
                lifecycle_state: "completed",
                backend_id: "studios-cluster",
                failure_reason: "",
                created_at: "2026-05-04T12:00:01Z",
                retry_count: 0
            }) { _docID }
        }"#,
        "AgentRequest",
    )
    .await?;
    let observed = gents_store::graphql::escape_graphql_string(&root_request_doc_id);
    let commit_response = node
        .execute(&format!(
            r#"{{ _commits(docID: "{observed}") {{cid fieldName}} }}"#
        ))
        .await;
    anyhow::ensure!(
        !commit_response.has_errors(),
        "{:?}",
        commit_response.errors
    );
    let commit = commit_response
        .data
        .as_ref()
        .and_then(|data| data["_commits"].as_array())
        .and_then(|rows| rows.iter().find(|row| row["fieldName"] == "_C"))
        .and_then(|row| row["cid"].as_str())
        .context("trace root has no composite commit")?;
    let commit = escape_graphql_string(commit);
    let rendered_body = escape_graphql_string(&json!({"model":"baa-ai/GLM-5.1-RAM-420GB-MLX", "messages":[{"role":"user","content":"Inspect the repo and show README.md"}]}).to_string());
    exec(node, &format!(r#"mutation {{
        create_InferenceCall(input: {{call_id: "trace-inference", request_id: "req-1", request_doc_id: "{observed}", node_did: "did:test:amy", agent_did: "amy", backend_id: "studios-cluster", call_seq: 0, attempt: 0, call_kind: "primary", call_state: "completed", queued_at: "2026-05-04T12:00:02Z"}}) {{_docID}}
        create_RenderedRequest(input: {{capture_key: "trace-model", request_commit_cid: "{commit}", request_json: "{rendered_body}", request_id: "req-1", request_doc_id: "{observed}", session_id: "session-1", node_did: "did:test:amy", requester_did: "", agent_did: "amy", model_name: "baa-ai/GLM-5.1-RAM-420GB-MLX", capture_scope: "inference.0", turn_index: 0, attempt: 0, capture_version: 1, source: "openai_chat_completions", created_at: "2026-05-04T12:00:02Z"}}) {{_docID}}
    }}"#)).await?;
    let failed_result = format!(
        "gents_exec: {}\nstdout:\n(empty)\nstderr:\ngrep: invalid option -- P",
        json!({
            "ok": false,
            "status": "exit_nonzero",
            "command": "grep -P amy README.md",
            "argv": ["grep", "-P", "amy", "README.md"],
            "cwd": "/repo",
            "exit_code": 2,
            "timed_out": false,
            "duration_ms": 1500,
            "timeout_ms": 10000,
            "execution_mode": "read_only",
            "network_mode": "inherit",
            "sandbox": "policy_read_only",
            "stdout_truncation": {
                "returned_bytes": 0,
                "total_bytes": 0,
                "max_bytes": 16000,
                "truncated": false
            },
            "stderr_truncation": {
                "returned_bytes": 24,
                "total_bytes": 24,
                "max_bytes": 16000,
                "truncated": false
            }
        })
    );
    seed_canonical_trace_tool(
        node,
        &TraceRequest {
            request_doc_id: &root_request_doc_id,
            request_id: "req-1",
            session_id: "session-1",
            node_did: "did:test:amy",
        },
        ToolCallSeed {
            sequence: 2,
            tool_call_id: "call-success",
            tool_name: "read",
            arguments: json!({"path":"README.md"}),
            lifecycle_state: "completed",
            result: Some("README contents"),
            failure_class: None,
        },
    )
    .await?;
    let parent_tool_call_doc_id = seed_canonical_trace_tool(
        node,
        &TraceRequest {
            request_doc_id: &root_request_doc_id,
            request_id: "req-1",
            session_id: "session-1",
            node_did: "did:test:amy",
        },
        ToolCallSeed {
            sequence: 3,
            tool_call_id: "call-fail",
            tool_name: "bash",
            arguments: json!({"command":"grep","args":["-P","amy","README.md"]}),
            lifecycle_state: "failed",
            result: Some(&failed_result),
            failure_class: Some("toolReturnedError"),
        },
    )
    .await?;

    exec(
        node,
        r#"mutation {
            create_AgentSession(input: {
                session_id: "session-child",
                node_did: "did:test:reviewer",
                agent_did: "reviewer",
                title: { text: "Child trace export test", source: "user" },
                created_at: "2026-05-04T12:00:04Z",
                observation: { last_activity_at: "2026-05-04T12:00:07Z", preview: "Review the README finding" }
            }) { _docID }
        }"#,
    )
    .await?;
    let child_request_doc_id = exec_doc_id(
        node,
        &format!(
            r#"mutation {{
            create_AgentRequest(input: {{purpose: "normal", 
                request_id: "req-child",
                node_did: "did:test:reviewer",
                agent_did: "reviewer",
                session_id: "session-child",
                content: "Review the README finding",
                lifecycle_state: "completed",
                backend_id: "studios-cluster",
                failure_reason: "",
                created_at: "2026-05-04T12:00:04Z",
                retry_count: 0,
                caused_by_parent_request_id: "req-1",
                caused_by_parent_request_doc_id: "{}",
                caused_by_parent_tool_call_id: "call-fail",
                caused_by_parent_tool_call_doc_id: "{}"
            }}) {{ _docID }}
        }}"#,
            escape_graphql_string(&root_request_doc_id),
            escape_graphql_string(&parent_tool_call_doc_id)
        ),
        "AgentRequest",
    )
    .await?;
    seed_canonical_terminal_text(
        node,
        &child_request_doc_id,
        "session-child",
        "did:test:reviewer",
        1,
        "reviewer private child message",
    )
    .await?;

    let missing_tool_result = json!({
        "ok": false,
        "failure_class": "tool_not_found",
        "path": "/tool_name",
        "message": "tool 'search_post' was not found on service 'x-data'; available tools: search_posts",
        "retryable": true,
        "service_id": "x-data",
        "tool_name": "search_post",
        "requested_tool_name": "search_post",
        "available_tools": ["search_posts"]
    })
    .to_string();
    seed_canonical_trace_tool(
        node,
        &TraceRequest {
            request_doc_id: &root_request_doc_id,
            request_id: "req-1",
            session_id: "session-1",
            node_did: "did:test:amy",
        },
        ToolCallSeed {
            sequence: 4,
            tool_call_id: "call-missing-tool",
            tool_name: "describe_tool",
            arguments: json!({"service_id":"x-data","tool_name":"search_post"}),
            lifecycle_state: "failed",
            result: Some(&missing_tool_result),
            failure_class: Some("serviceUnavailable"),
        },
    )
    .await?;
    seed_canonical_trace_tool(
        node,
        &TraceRequest {
            request_doc_id: &root_request_doc_id,
            request_id: "req-1",
            session_id: "session-1",
            node_did: "did:test:amy",
        },
        ToolCallSeed {
            sequence: 5,
            tool_call_id: "call-timed-out",
            tool_name: "bash",
            arguments: json!({"command":"sleep 120"}),
            lifecycle_state: "timedOut",
            result: Some(""),
            failure_class: None,
        },
    )
    .await?;
    seed_canonical_terminal_text(
        node,
        &root_request_doc_id,
        "session-1",
        "did:test:amy",
        6,
        "done",
    )
    .await?;

    exec(
        node,
        r#"mutation {
            create_AgentSession(input: {
                session_id: "session-2",
                node_did: "did:test:amy",
                agent_did: "amy",
                title: { text: "Trace export deadline test", source: "user" },
                created_at: "2026-05-04T13:00:00Z",
                observation: { last_activity_at: "2026-05-04T13:00:10Z", preview: "Read a file then deadline" }
            }) { _docID }
        }"#,
    )
    .await?;
    let deadline_request_doc_id = exec_doc_id(
        node,
        r#"mutation {
            create_AgentRequest(input: {purpose: "normal", 
                request_id: "req-deadline",
                node_did: "did:test:amy",
                agent_did: "amy",
                session_id: "session-2",
                content: "Read README.md but the request later times out",
                lifecycle_state: "failed",
                backend_id: "studios-cluster",
                failure_reason: "request deadline exceeded while waiting for inference stream item",
                created_at: "2026-05-04T13:00:01Z",
                retry_count: 0
            }) { _docID }
        }"#,
        "AgentRequest",
    )
    .await?;
    seed_canonical_trace_tool(
        node,
        &TraceRequest {
            request_doc_id: &deadline_request_doc_id,
            request_id: "req-deadline",
            session_id: "session-2",
            node_did: "did:test:amy",
        },
        ToolCallSeed {
            sequence: 2,
            tool_call_id: "call-deadline",
            tool_name: "read",
            arguments: json!({"path":"README.md"}),
            lifecycle_state: "completed",
            result: Some("README contents"),
            failure_class: None,
        },
    )
    .await?;
    for (session, agent, request, doc, state, activity, preview) in [
        (
            "session-1",
            "did:test:amy",
            "req-1",
            root_request_doc_id.as_str(),
            "completed",
            "2026-05-04T12:00:05Z",
            "Inspect the repo",
        ),
        (
            "session-child",
            "did:test:reviewer",
            "req-child",
            child_request_doc_id.as_str(),
            "completed",
            "2026-05-04T12:00:07Z",
            "Review the README finding",
        ),
        (
            "session-2",
            "did:test:amy",
            "req-deadline",
            deadline_request_doc_id.as_str(),
            "failed",
            "2026-05-04T13:00:10Z",
            "Read a file then deadline",
        ),
    ] {
        let mut patch = json!({"observation": {
            "last_activity_at": activity, "preview": preview,
            "latest_request": {"request_doc_id": doc, "request_id": request, "lifecycle_state": state}
        }});
        if session == "session-child" {
            patch["provenance"] = json!({"parent_request_doc_id": root_request_doc_id});
        }
        let input = gents_protocol::graphql::graphql_input_literal(&patch)?;
        let session = escape_graphql_string(session);
        let agent = escape_graphql_string(agent);
        exec(
            node,
            &format!(
                r#"mutation {{ update_AgentSession(
            filter: {{
                session_id: {{ _eq: "{session}" }},
                node_did: {{ _eq: "{agent}" }},
                requester_did: {{ _eq: null }}
            }}, input: {input}
        ) {{ _docID }} }}"#
            ),
        )
        .await?;
    }
    Ok(())
}
