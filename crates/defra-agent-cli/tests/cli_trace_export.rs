mod support;
use support::*;

use anyhow::{Context, Result};
use defra_agent::defra_node::{EmbeddedNode, StorageBackend};
use defra_agent::ensure_runtime_schemas;
use rig::completion::message::{AssistantContent, Message, ToolCall, ToolFunction};
use rig::one_or_many::OneOrMany;
use serde_json::{json, Value};

fn workspace_root() -> Result<std::path::PathBuf> {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("unable to resolve workspace root"))
}

fn read_schema_snapshot(relative_path: &str) -> Result<Value> {
    let path = workspace_root()?.join(relative_path);
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str::<Value>(&raw).with_context(|| format!("parsing {}", path.display()))
}

#[tokio::test]
async fn trace_export_emits_amy_style_jsonl_and_classifies_completed_failures() -> Result<()> {
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
        seed_trace_export_rows(&node).await?;
    }

    let output = run_cli_text(
        tempdir.path(),
        &[
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

    assert_eq!(records.len(), 4, "export output:\n{output}");
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

    assert_eq!(
        failed.get("tool_call_id").and_then(Value::as_str),
        Some("call-fail")
    );
    assert_eq!(
        failed.get("tool_status").and_then(Value::as_str),
        Some("completed")
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
    assert_eq!(
        failed.get("inference_profile_id").and_then(Value::as_str),
        Some("amy")
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
    assert_eq!(
        missing_tool
            .get("tool_error")
            .and_then(|value| value.get("available_tools")),
        Some(&json!(["search_posts"]))
    );
    assert_eq!(
        missing_tool
            .get("tool_error")
            .and_then(|value| value.get("requested_tool_name"))
            .and_then(Value::as_str),
        Some("search_post")
    );

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
        Some("error")
    );
    assert_eq!(
        deadline
            .get("request_lifecycle_state")
            .and_then(Value::as_str),
        Some("failed")
    );
    assert_eq!(
        deadline.get("response_status").and_then(Value::as_str),
        Some("error")
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
            &["trace", "project-schema", "--projection", cli_projection],
        )?;
        let json_schema =
            serde_json::from_str::<Value>(&json_schema_output).context("parsing JSON schema")?;
        let expected_json_schema = read_schema_snapshot(&format!(
            "docs/superpowers/contracts/adapter-projections/v1/{snapshot_name}.schema.json"
        ))?;
        assert_eq!(
            json_schema, expected_json_schema,
            "{cli_projection} JSON schema drifted from checked-in snapshot"
        );

        let jsonl_schema_output = run_cli_text(
            tempdir.path(),
            &[
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
        let expected_jsonl_schema = read_schema_snapshot(&format!(
            "docs/superpowers/contracts/adapter-projections/v1/{snapshot_name}.jsonl-record.schema.json"
        ))?;
        assert_eq!(
            jsonl_schema, expected_jsonl_schema,
            "{cli_projection} JSONL schema drifted from checked-in snapshot"
        );

        let training_jsonl_schema_output = run_cli_text(
            tempdir.path(),
            &[
                "trace",
                "project-schema",
                "--projection",
                cli_projection,
                "--format",
                "training-jsonl",
            ],
        )?;
        let training_jsonl_schema = serde_json::from_str::<Value>(&training_jsonl_schema_output)
            .context("parsing training JSONL schema")?;
        let expected_training_jsonl_schema = read_schema_snapshot(&format!(
            "docs/superpowers/contracts/adapter-projections/v1/{snapshot_name}.training-jsonl-record.schema.json"
        ))?;
        assert_eq!(
            training_jsonl_schema, expected_training_jsonl_schema,
            "{cli_projection} training JSONL schema drifted from checked-in snapshot"
        );
    }

    Ok(())
}

#[tokio::test]
async fn trace_timeline_reconstructs_request_events_from_persisted_rows() -> Result<()> {
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
        seed_trace_export_rows(&node).await?;
    }

    let output = run_cli_text(
        tempdir.path(),
        &[
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
            .get("conversation")
            .and_then(|conversation| conversation.get("title"))
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
    assert!(
        events.iter().any(|event| {
            event.get("kind").and_then(Value::as_str) == Some("response")
                && event.get("request_id").and_then(Value::as_str) == Some("req-1")
        }),
        "timeline missing response event: {timeline:#}"
    );

    Ok(())
}

#[tokio::test]
async fn trace_project_exports_first_adapter_shapes_from_persisted_rows() -> Result<()> {
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
        seed_trace_export_rows(&node).await?;
    }

    let home = agent_home.to_str().context("agent home utf8")?;
    let openai = trace_project_json(tempdir.path(), home, "openai-codex", "public")?;
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
    let openai_training_jsonl =
        trace_project_training_jsonl_lines(tempdir.path(), home, "openai-codex", "public")?;
    assert!(
        !openai_training_jsonl.is_empty(),
        "expected openai-codex training JSONL records"
    );
    assert!(openai_training_jsonl.iter().all(|record| {
        record.get("projection_id").and_then(Value::as_str) == Some("openai_codex_run_trace")
            && record.get("source_request_id").and_then(Value::as_str) == Some("req-1")
            && record.get("adapter_record_kind").and_then(Value::as_str)
                == Some("openai_codex_trace_item")
    }));
    assert!(
        openai_training_jsonl.iter().any(|record| {
            record.get("sample_kind").and_then(Value::as_str) == Some("tool_call")
                && record.get("tool_name").and_then(Value::as_str) == Some("bash")
        }),
        "training JSONL should retain tool-call evidence: {openai_training_jsonl:#?}"
    );
    let serialized_openai_training_jsonl = serde_json::to_string(&openai_training_jsonl)?;
    assert!(
        !serialized_openai_training_jsonl.contains("Inspect the repo and show README.md"),
        "public training JSONL adapter projection leaked request content: {openai_training_jsonl:#?}"
    );

    let langgraph = trace_project_json(tempdir.path(), home, "langgraph", "full")?;
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

    let multi_agent = trace_project_json(tempdir.path(), home, "multi-agent", "full")?;
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
    let multi_agent_training_jsonl =
        trace_project_training_jsonl_lines(tempdir.path(), home, "multi-agent", "full")?;
    assert!(
        multi_agent_training_jsonl.iter().any(|record| {
            record.get("sample_kind").and_then(Value::as_str) == Some("delegation")
                && record.get("parent_request_id").and_then(Value::as_str) == Some("req-1")
                && record.get("child_request_id").and_then(Value::as_str) == Some("req-child")
        }),
        "multi-agent training JSONL projection missing delegation sample: {multi_agent_training_jsonl:#?}"
    );

    Ok(())
}

fn trace_project_json(
    cwd: &std::path::Path,
    home: &str,
    projection: &str,
    redaction: &str,
) -> Result<Value> {
    let output = run_cli_text(
        cwd,
        &[
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
            "did:defra-agent:test-viewer",
        ],
    )?;
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
            "did:defra-agent:test-viewer",
        ],
    )?;
    output
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).context("parsing adapter projection JSONL"))
        .collect::<Result<Vec<_>>>()
}

fn trace_project_training_jsonl_lines(
    cwd: &std::path::Path,
    home: &str,
    projection: &str,
    redaction: &str,
) -> Result<Vec<Value>> {
    let output = run_cli_text(
        cwd,
        &[
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
            "training-jsonl",
            "--actor-did",
            "did:defra-agent:test-viewer",
        ],
    )?;
    output
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).context("parsing adapter projection training JSONL")
        })
        .collect::<Result<Vec<_>>>()
}

async fn seed_trace_export_rows(node: &EmbeddedNode) -> Result<()> {
    exec(
        node,
        r#"mutation {
            create_AgentBehavior(input: {
                behavior_id: "amy",
                agent_did: "did:defra-agent:amy",
                display_name: "Amy",
                system_prompt: "baseline",
                backend_id: "studios-cluster",
                model_name: "baa-ai/GLM-5.1-RAM-420GB-MLX",
                tool_selection_id: "default-tools",
                inference_profile_id: "amy",
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
                agent_name: "Amy",
                behavior_id: "amy",
                started: "2026-05-04T12:00:00Z",
                status: "active"
            }) { _docID }
        }"#,
    )
    .await?;
    exec(
        node,
        r#"mutation {
            create_AgentConversation(input: {
                session_id: "session-1",
                agent_name: "Amy",
                agent_did: "did:defra-agent:amy",
                behavior_id: "amy",
                title: "Trace export test",
                title_source: "test",
                preview_text: "Inspect the repo",
                status: "active",
                created_at: "2026-05-04T12:00:00Z",
                updated_at: "2026-05-04T12:00:05Z",
                latest_request_id: "req-1"
            }) { _docID }
        }"#,
    )
    .await?;
    exec(
        node,
        r#"mutation {
            create_AgentRequest(input: {
                request_id: "req-1",
                agent_did: "did:defra-agent:amy",
                behavior_id: "amy",
                session_id: "session-1",
                content: "Inspect the repo and show README.md",
                metadata: "{\"run_id\":\"run-metadata\",\"case_id\":\"case-metadata\"}",
                status: "completed",
                lifecycle_state: "complete",
                backend_id: "studios-cluster",
                failure_reason: "",
                created_at: "2026-05-04T12:00:01Z",
                retry_count: 0
            }) { _docID }
        }"#,
    )
    .await?;
    exec(
        node,
        r#"mutation {
            create_AgentRequest(input: {
                request_id: "req-child",
                agent_did: "did:defra-agent:reviewer",
                behavior_id: "reviewer",
                session_id: "session-1",
                content: "Review the README finding",
                metadata: "",
                status: "completed",
                lifecycle_state: "complete",
                backend_id: "studios-cluster",
                failure_reason: "",
                created_at: "2026-05-04T12:00:04Z",
                retry_count: 0,
                caused_by_parent_request_id: "req-1",
                caused_by_parent_tool_call_id: "call-fail"
            }) { _docID }
        }"#,
    )
    .await?;
    exec(
        node,
        r#"mutation {
            create_AgentResponse(input: {
                response_key: "req-1",
                request_id: "req-1",
                agent_did: "did:defra-agent:amy",
                behavior_id: "amy",
                session_id: "session-1",
                content: "done",
                reasoning: "",
                status: "completed",
                error_message: "",
                token_count: 12,
                progress_seq: 3,
                materialized_message_sequence: 4,
                materialized_at: "2026-05-04T12:00:06Z",
                created_at: "2026-05-04T12:00:01Z",
                completed_at: "2026-05-04T12:00:06Z"
            }) { _docID }
        }"#,
    )
    .await?;

    let success_message =
        assistant_tool_message("call-success", "read", json!({"path":"README.md"}))?;
    let failed_message = assistant_tool_message(
        "call-fail",
        "bash",
        json!({"command":"grep","args":["-P","amy","README.md"]}),
    )?;
    let failed_result = format!(
        "defra_exec: {}\nstdout:\n(empty)\nstderr:\ngrep: invalid option -- P",
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
    let missing_tool_message = assistant_tool_message(
        "call-missing-tool",
        "describe_tool",
        json!({"service_id":"x-data","tool_name":"search_post"}),
    )?;
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "session-1:2",
                    session_id: "session-1",
                    sequence: 2,
                    role: "assistant",
                    content: "{}",
                    timestamp: "2026-05-04T12:00:02Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&success_message)
        ),
    )
    .await?;
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "session-1:3",
                    session_id: "session-1",
                    sequence: 3,
                    role: "assistant",
                    content: "{}",
                    timestamp: "2026-05-04T12:00:03Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&failed_message)
        ),
    )
    .await?;
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "session-1:4",
                    session_id: "session-1",
                    sequence: 4,
                    role: "assistant",
                    content: "{}",
                    timestamp: "2026-05-04T12:00:04Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&missing_tool_message)
        ),
    )
    .await?;
    exec(
        node,
        r#"mutation {
            create_AgentToolCall(input: {
                tool_call_key: "session-1:call-success",
                session_id: "session-1",
                message_sequence: 2,
                tool_name: "read",
                tool_call_id: "call-success",
                args: "{\"path\":\"README.md\"}",
                result: "README contents",
                status: "completed",
                started_at: "2026-05-04T12:00:02Z",
                completed_at: "2026-05-04T12:00:03Z"
            }) { _docID }
        }"#,
    )
    .await?;
    exec(
        node,
        &format!(
            r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "session-1:call-fail",
                session_id: "session-1",
                message_sequence: 3,
                tool_name: "bash",
                tool_call_id: "call-fail",
                args: "{{\"command\":\"grep\",\"args\":[\"-P\",\"amy\",\"README.md\"]}}",
                result: "{}",
                status: "completed",
                started_at: "2026-05-04T12:00:03Z",
                completed_at: "2026-05-04T12:00:04.500Z"
            }}) {{ _docID }}
        }}"#,
            escape_graphql_string(&failed_result)
        ),
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
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "session-1:call-missing-tool",
                    session_id: "session-1",
                    message_sequence: 4,
                    tool_name: "describe_tool",
                    tool_call_id: "call-missing-tool",
                    args: "{{\"service_id\":\"x-data\",\"tool_name\":\"search_post\"}}",
                    result: "{}",
                    status: "completed",
                    started_at: "2026-05-04T12:00:04Z",
                    completed_at: "2026-05-04T12:00:04.250Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&missing_tool_result)
        ),
    )
    .await?;

    exec(
        node,
        r#"mutation {
            create_AgentSession(input: {
                session_id: "session-2",
                agent_name: "Amy",
                behavior_id: "amy",
                started: "2026-05-04T13:00:00Z",
                status: "active"
            }) { _docID }
        }"#,
    )
    .await?;
    exec(
        node,
        r#"mutation {
            create_AgentConversation(input: {
                session_id: "session-2",
                agent_name: "Amy",
                agent_did: "did:defra-agent:amy",
                behavior_id: "amy",
                title: "Trace export deadline test",
                title_source: "test",
                preview_text: "Read a file then deadline",
                status: "active",
                created_at: "2026-05-04T13:00:00Z",
                updated_at: "2026-05-04T13:00:10Z",
                latest_request_id: "req-deadline"
            }) { _docID }
        }"#,
    )
    .await?;
    exec(
        node,
        r#"mutation {
            create_AgentRequest(input: {
                request_id: "req-deadline",
                agent_did: "did:defra-agent:amy",
                behavior_id: "amy",
                session_id: "session-2",
                content: "Read README.md but the request later times out",
                metadata: "",
                status: "error",
                lifecycle_state: "failed",
                backend_id: "studios-cluster",
                failure_reason: "request deadline exceeded while waiting for inference stream item",
                created_at: "2026-05-04T13:00:01Z",
                retry_count: 0
            }) { _docID }
        }"#,
    )
    .await?;
    exec(
        node,
        r#"mutation {
            create_AgentResponse(input: {
                response_key: "req-deadline",
                request_id: "req-deadline",
                agent_did: "did:defra-agent:amy",
                behavior_id: "amy",
                session_id: "session-2",
                content: "",
                reasoning: "",
                status: "error",
                error_message: "request deadline exceeded while waiting for inference stream item",
                token_count: 0,
                progress_seq: 3,
                materialized_message_sequence: 4,
                materialized_at: "2026-05-04T13:00:10Z",
                created_at: "2026-05-04T13:00:01Z",
                completed_at: "2026-05-04T13:00:10Z"
            }) { _docID }
        }"#,
    )
    .await?;
    let deadline_message =
        assistant_tool_message("call-deadline", "read", json!({"path":"README.md"}))?;
    exec(
        node,
        &format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "session-2:2",
                    session_id: "session-2",
                    sequence: 2,
                    role: "assistant",
                    content: "{}",
                    timestamp: "2026-05-04T13:00:02Z"
                }}) {{ _docID }}
            }}"#,
            escape_graphql_string(&deadline_message)
        ),
    )
    .await?;
    exec(
        node,
        r#"mutation {
            create_AgentToolCall(input: {
                tool_call_key: "session-2:call-deadline",
                session_id: "session-2",
                message_sequence: 2,
                tool_name: "read",
                tool_call_id: "call-deadline",
                args: "{\"path\":\"README.md\"}",
                result: "README contents",
                status: "completed",
                started_at: "2026-05-04T13:00:02Z",
                completed_at: "2026-05-04T13:00:03Z"
            }) { _docID }
        }"#,
    )
    .await?;
    Ok(())
}

fn assistant_tool_message(call_id: &str, name: &str, arguments: Value) -> Result<String> {
    serde_json::to_string(&Message::Assistant {
        id: None,
        content: OneOrMany::one(AssistantContent::ToolCall(ToolCall {
            id: call_id.to_string(),
            call_id: Some(call_id.to_string()),
            function: ToolFunction {
                name: name.to_string(),
                arguments,
            },
            signature: None,
            additional_params: None,
        })),
    })
    .context("serializing assistant tool message")
}

async fn exec(node: &EmbeddedNode, query: &str) -> Result<()> {
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!("GraphQL mutation failed: {:?}", response.errors);
    }
    Ok(())
}
