mod support;
use support::*;

use anyhow::{Context, Result};
use gents::defra_node::{EmbeddedNode, StorageBackend};
use gents::ensure_runtime_schemas;
use serde_json::Value;

#[tokio::test]
async fn background_list_json_filters_and_lists_backgrounded_tool_calls() -> Result<()> {
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
        seed_background_tool_calls(&node).await?;
    }

    let agent_home = agent_home.to_str().context("agent home utf8")?;
    let output = run_cli_json(
        tempdir.path(),
        &[
            "background",
            "list",
            "--home",
            agent_home,
            "--request",
            "req-background",
            "--state",
            "running",
            "--age-gt",
            "1d",
            "--output",
            "json",
        ],
    )?;
    assert_eq!(output.get("count").and_then(Value::as_u64), Some(1));
    let items = output
        .get("items")
        .and_then(Value::as_array)
        .context("background list output must include items")?;
    let row = items.first().context("expected one background row")?;
    assert_eq!(
        row.get("tool_call_id").and_then(Value::as_str),
        Some("call-background-running")
    );
    assert_eq!(
        row.get("parent_request_id").and_then(Value::as_str),
        Some("req-background")
    );
    assert_eq!(row.get("state").and_then(Value::as_str), Some("running"));
    assert_eq!(
        row.get("await_mode").and_then(Value::as_str),
        Some("background")
    );
    assert!(
        row.get("age_ms")
            .and_then(Value::as_i64)
            .is_some_and(|age| age > 0),
        "expected positive age_ms in row: {row}"
    );

    let unfiltered_state_output = run_cli_json(
        tempdir.path(),
        &[
            "background",
            "list",
            "--home",
            agent_home,
            "--request",
            "req-background",
            "--age-gt",
            "1d",
            "--output",
            "json",
        ],
    )?;
    assert_eq!(
        unfiltered_state_output.get("count").and_then(Value::as_u64),
        Some(3),
        "without --state the fixture should include all three background rows for req-background"
    );

    let status_fallback_output = run_cli_json(
        tempdir.path(),
        &[
            "background",
            "list",
            "--home",
            agent_home,
            "--request",
            "req-background",
            "--state",
            "called",
            "--output",
            "json",
        ],
    )?;
    assert_eq!(
        status_fallback_output.get("count").and_then(Value::as_u64),
        Some(1),
        "--state should match the displayed status fallback when lifecycle_state is empty"
    );
    let fallback_items = status_fallback_output
        .get("items")
        .and_then(Value::as_array)
        .context("status fallback output must include items")?;
    assert_eq!(
        fallback_items
            .first()
            .and_then(|row| row.get("tool_call_id"))
            .and_then(Value::as_str),
        Some("call-background-status-only")
    );

    let table = run_cli_text(
        tempdir.path(),
        &[
            "background",
            "list",
            "--home",
            agent_home,
            "--request",
            "req-background",
        ],
    )?;
    assert!(
        table.contains("TOOL_CALL_ID") && table.contains("PARENT_REQUEST"),
        "table output should include default columns:\n{table}"
    );
    assert!(
        table.contains("call-background-running")
            && table.contains("call-background-completed")
            && table.contains("call-background-status-only")
            && !table.contains("call-foreground-running"),
        "table output should list only backgrounded calls for req-background:\n{table}"
    );

    Ok(())
}

async fn seed_background_tool_calls(node: &EmbeddedNode) -> Result<()> {
    exec(
        node,
        r#"mutation {
            create_AgentRequest(input: {
                request_id: "req-background",
                agent_did: "did:test:test",
                behavior_id: "default",
                session_id: "session-background",
                content: "background list fixture",
                status: "processing",
                lifecycle_state: "processing",
                backend_id: "test-backend",
                created_at: "2024-01-01T11:00:00Z",
                claimed_at: "2024-01-01T11:00:01Z",
                deadline: "2024-01-01T11:30:00Z",
                retry_count: 0
            }) { _docID }
            create_AgentRequest(input: {
                request_id: "req-other",
                agent_did: "did:test:test",
                behavior_id: "default",
                session_id: "session-other",
                content: "background list other fixture",
                status: "processing",
                lifecycle_state: "processing",
                backend_id: "test-backend",
                created_at: "2024-01-01T12:00:00Z",
                claimed_at: "2024-01-01T12:00:01Z",
                deadline: "2024-01-01T12:30:00Z",
                retry_count: 0
            }) { _docID }
        }"#,
    )
    .await?;

    exec(
        node,
        r#"mutation {
            create_AgentToolCall(input: {
                tool_call_key: "session-background:call-background-running",
                request_id: "req-background",
                session_id: "session-background",
                message_sequence: 1,
                tool_name: "bash",
                tool_call_id: "call-background-running",
                args: "{}",
                result: "",
                status: "called",
                lifecycle_state: "running",
                started_at: "2024-01-01T11:00:05Z",
                await_mode: "background"
            }) { _docID }
            create_AgentToolCall(input: {
                tool_call_key: "session-background:call-background-completed",
                request_id: "req-background",
                session_id: "session-background",
                message_sequence: 2,
                tool_name: "read_file",
                tool_call_id: "call-background-completed",
                args: "{}",
                result: "done",
                status: "completed",
                lifecycle_state: "completed",
                started_at: "2024-01-01T11:00:10Z",
                completed_at: "2024-01-01T11:00:11Z",
                await_mode: "background"
            }) { _docID }
            create_AgentToolCall(input: {
                tool_call_key: "session-background:call-foreground-running",
                request_id: "req-background",
                session_id: "session-background",
                message_sequence: 3,
                tool_name: "bash",
                tool_call_id: "call-foreground-running",
                args: "{}",
                result: "",
                status: "called",
                lifecycle_state: "running",
                started_at: "2024-01-01T11:00:15Z",
                await_mode: "foreground"
            }) { _docID }
            create_AgentToolCall(input: {
                tool_call_key: "session-background:call-background-status-only",
                request_id: "req-background",
                session_id: "session-background",
                message_sequence: 4,
                tool_name: "bash",
                tool_call_id: "call-background-status-only",
                args: "{}",
                result: "",
                status: "called",
                started_at: "2024-01-01T11:00:20Z",
                await_mode: "background"
            }) { _docID }
            create_AgentToolCall(input: {
                tool_call_key: "session-other:call-background-other",
                request_id: "req-other",
                session_id: "session-other",
                message_sequence: 1,
                tool_name: "bash",
                tool_call_id: "call-background-other",
                args: "{}",
                result: "",
                status: "called",
                lifecycle_state: "running",
                started_at: "2024-01-01T12:00:05Z",
                await_mode: "background"
            }) { _docID }
        }"#,
    )
    .await
}

async fn exec(node: &EmbeddedNode, query: &str) -> Result<()> {
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!("GraphQL mutation failed: {:?}", response.errors);
    }
    Ok(())
}
