use crate::support::*;

use std::fs;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_submit_waits_for_response_by_default() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-submit-model-{}", Uuid::new_v4().simple());
    let expected_content = format!("wait-ok-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_content)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-submit-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let request_content = format!("CLI wait test {}", Uuid::new_v4());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let submit = spawn_cli(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--content",
            &request_content,
            "--timeout-secs",
            "20",
            "--poll-secs",
            "1",
        ],
    )?;

    let output = submit
        .wait_with_output()
        .context("waiting for request submit child")?;
    if !output.status.success() {
        bail!(
            "request submit failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let parsed: Value =
        serde_json::from_slice(&output.stdout).context("parsing request submit JSON")?;
    assert!(
        parsed
            .get("request_id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "request submit output omitted request_id: {parsed}"
    );
    assert_eq!(
        parsed.pointer("/response/status").and_then(Value::as_str),
        Some("complete")
    );
    assert_eq!(
        parsed.pointer("/response/content").and_then(Value::as_str),
        Some(expected_content.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_submit_supports_content_file_and_output_file() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-submit-file-model-{}", Uuid::new_v4().simple());
    let expected_content = format!("wait-file-ok-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_content)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-submit-file-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let request_content = format!("CLI file request {}", Uuid::new_v4());
    let content_path = tempdir.path().join("request.txt");
    let output_path = tempdir.path().join("request-output.json");
    fs::write(&content_path, &request_content)?;

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let submit = spawn_cli(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--content-file",
            content_path
                .to_str()
                .ok_or_else(|| anyhow!("content path is not utf-8"))?,
            "--output-file",
            output_path
                .to_str()
                .ok_or_else(|| anyhow!("output path is not utf-8"))?,
            "--timeout-secs",
            "20",
            "--poll-secs",
            "1",
        ],
    )?;

    let output = submit
        .wait_with_output()
        .context("waiting for request submit child")?;
    if !output.status.success() {
        bail!(
            "request submit failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout_json: Value =
        serde_json::from_slice(&output.stdout).context("parsing request submit JSON")?;
    let file_json = read_json_file(&output_path)?;
    assert_eq!(stdout_json, file_json);
    assert_eq!(
        stdout_json
            .pointer("/response/content")
            .and_then(Value::as_str),
        Some(expected_content.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_interrupt_waits_until_running_request_is_terminal() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-interrupt-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_hanging(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-interrupt-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let request_content = format!("CLI interrupt request {}", Uuid::new_v4());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let submitted = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--content",
            &request_content,
            "--no-wait",
        ],
    )?;
    let request_id = submitted
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("request submit output missing request_id: {submitted}"))?
        .to_string();

    wait_for_request_lifecycle_state(
        &graphql,
        &request_id,
        &["processing"],
        Duration::from_secs(30),
    )
    .await?;
    let capture_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if mock_endpoint
            .captured_chat_requests()
            .iter()
            .any(|request| request_contains_role_text(request, "user", &request_content))
        {
            break;
        }
        if Instant::now() >= capture_deadline {
            bail!("hanging mock endpoint did not capture the running request");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let interrupted = run_cli_json(
        &home_dir,
        &[
            "request",
            "interrupt",
            "--graphql",
            &graphql,
            "--cause",
            "userCancelled",
            "--wait",
            "--timeout",
            "20s",
            "--output",
            "json",
            &request_id,
        ],
    )?;
    assert_eq!(
        interrupted.get("request_id").and_then(Value::as_str),
        Some(request_id.as_str())
    );
    assert_eq!(
        interrupted.get("cause").and_then(Value::as_str),
        Some("userCancelled")
    );
    assert_eq!(
        interrupted.get("lifecycle_state").and_then(Value::as_str),
        Some("interrupted")
    );
    assert_eq!(
        interrupted.get("terminal").and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        interrupted
            .get("interrupt_landed_at")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "interrupt output should include landing time: {interrupted}"
    );

    let row = wait_for_request_lifecycle_state(
        &graphql,
        &request_id,
        &["interrupted"],
        Duration::from_secs(5),
    )
    .await?;
    assert!(
        row.get("interrupt_requested_at")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "AgentRequest should retain interrupt_requested_at: {row}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_interrupt_does_not_latch_completed_request() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-interrupt-completed-model-{}", Uuid::new_v4().simple());
    let final_text = format!("completed-before-interrupt-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &final_text)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-interrupt-completed-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let request_content = format!("CLI completed interrupt request {}", Uuid::new_v4());

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let submitted = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--content",
            &request_content,
            "--timeout-secs",
            "20",
            "--poll-secs",
            "1",
        ],
    )?;
    let request_id = submitted
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("request submit output missing request_id: {submitted}"))?
        .to_string();
    assert_eq!(
        submitted
            .pointer("/response/content")
            .and_then(Value::as_str),
        Some(final_text.as_str())
    );

    wait_for_request_lifecycle_state(
        &graphql,
        &request_id,
        &["completed"],
        Duration::from_secs(30),
    )
    .await?;

    let interrupted = run_cli_json(
        &home_dir,
        &[
            "request",
            "interrupt",
            "--graphql",
            &graphql,
            "--output",
            "json",
            &request_id,
        ],
    )?;
    assert_eq!(
        interrupted.get("request_id").and_then(Value::as_str),
        Some(request_id.as_str())
    );
    assert_eq!(
        interrupted.get("lifecycle_state").and_then(Value::as_str),
        Some("completed")
    );
    assert_eq!(
        interrupted.get("already_terminal").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        interrupted
            .get("already_interrupted")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert!(
        interrupted
            .get("interrupt_landed_at")
            .is_none_or(Value::is_null),
        "completed request should not report a new interrupt landing time: {interrupted}"
    );
    assert!(
        interrupted
            .get("interrupt_requested_at")
            .is_none_or(Value::is_null),
        "completed request should not latch interrupt_requested_at: {interrupted}"
    );

    let row = wait_for_request_lifecycle_state(
        &graphql,
        &request_id,
        &["completed"],
        Duration::from_secs(5),
    )
    .await?;
    assert!(
        row.get("interrupt_requested_at").is_none_or(Value::is_null),
        "completed AgentRequest should not be mutated by interrupt: {row}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_show_expanded_view_surfaces_background_tools_and_child_lineage() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-show-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-show-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let session_id = format!("show-session-{}", Uuid::new_v4().simple());
    let parent_request_id = format!("show-parent-{}", Uuid::new_v4().simple());
    let child_request_id = format!("show-child-{}", Uuid::new_v4().simple());
    let tool_call_id = format!("show-tool-{}", Uuid::new_v4().simple());
    let tool_call_key = format!("{session_id}:{tool_call_id}");
    let parent_create = graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{parent_request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "parent-behavior",
                    session_id: "{session_id}",
                    content: "parent request with a backgrounded cascade child",
                    status: "processing",
                    lifecycle_state: "processing",
                    backend_id: "studios-cluster",
                    execution_origin: "operatorCli",
                    created_at: "2026-05-20T10:00:00Z",
                    claimed_at: "2026-05-20T10:00:01Z",
                    deadline: "2026-05-20T10:05:00Z",
                    retry_count: 0,
                    subagent_depth: 0
                }}) {{ _docID }}
            }}"#
        ),
    )
    .await?;
    let parent_doc_id = doc_id_from_create(&parent_create, "add_AgentRequest")?;
    let tool_create = graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    agent_did: "{agent_did}",
                    request_id: "{parent_request_id}",
                    request_doc_id: "{parent_doc_id}",
                    session_id: "{session_id}",
                    message_sequence: 1,
                    tool_name: "spawn_subagent",
                    tool_call_id: "{tool_call_id}",
                    args: "{{\"behavior_id\":\"child-behavior\"}}",
                    result: "",
                    status: "called",
                    lifecycle_state: "running",
                    started_at: "2026-05-20T10:00:02Z",
                    deadline_at: "2026-05-20T10:05:00Z",
                    await_mode: "background",
                    cancel_policy: "cascade",
                    child_request_id: "{child_request_id}",
                    spawn_target_did: "{agent_did}"
                }}) {{ _docID }}
            }}"#,
            parent_doc_id = escape_graphql_string(&parent_doc_id),
        ),
    )
    .await?;
    let tool_call_doc_id = doc_id_from_create(&tool_create, "add_AgentToolCall")?;
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{child_request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "child-behavior",
                    session_id: "{session_id}",
                    content: "child request spawned by backgrounded tool",
                    status: "processing",
                    lifecycle_state: "processing",
                    created_at: "2026-05-20T10:00:03Z",
                    claimed_at: "2026-05-20T10:00:04Z",
                    retry_count: 0,
                    subagent_depth: 1,
                    caused_by_parent_request_id: "{parent_request_id}",
                    caused_by_parent_request_doc_id: "{parent_doc_id}",
                    caused_by_parent_tool_call_id: "{tool_call_id}",
                    caused_by_parent_tool_call_doc_id: "{tool_call_doc_id}",
                    caused_by_trigger_kind: "subagent"
                }}) {{ _docID }}
            }}"#,
            parent_doc_id = escape_graphql_string(&parent_doc_id),
            tool_call_doc_id = escape_graphql_string(&tool_call_doc_id),
        ),
    )
    .await?;

    let json_output = run_cli_json(
        &home_dir,
        &[
            "request",
            "show",
            "--graphql",
            &graphql,
            "--output",
            "json",
            &parent_request_id,
        ],
    )?;
    assert_eq!(
        json_output
            .pointer("/request/request_id")
            .and_then(Value::as_str),
        Some(parent_request_id.as_str())
    );
    assert_eq!(
        json_output
            .pointer("/tool_calls/0/await_mode")
            .and_then(Value::as_str),
        Some("background")
    );
    assert_eq!(
        json_output
            .pointer("/tool_calls/0/cancel_policy")
            .and_then(Value::as_str),
        Some("cascade")
    );
    assert_eq!(
        json_output
            .pointer("/tool_calls/0/child_terminal")
            .and_then(Value::as_str),
        Some("unknown"),
        "request show must render child_terminal as unknown when the schema has not landed yet"
    );
    assert_eq!(
        json_output
            .pointer("/tool_calls/0/cancel_cause")
            .and_then(Value::as_str),
        Some("unknown"),
        "request show must render cancel_cause as unknown when the schema has not landed yet"
    );
    assert_eq!(
        json_output
            .pointer("/tool_calls/0/active_tool_call")
            .and_then(Value::as_bool),
        Some(true),
        "running tool call must be linked to liveness.active_tool_calls"
    );
    assert_eq!(
        json_output
            .pointer("/backgrounded_tools/0/tool_call_id")
            .and_then(Value::as_str),
        Some(tool_call_id.as_str())
    );
    assert_eq!(
        json_output
            .pointer("/child_requests/0/request_id")
            .and_then(Value::as_str),
        Some(child_request_id.as_str())
    );
    assert_eq!(
        json_output
            .pointer("/child_requests/0/behavior_id")
            .and_then(Value::as_str),
        Some("child-behavior")
    );

    let text_output = run_cli_text(
        &home_dir,
        &["request", "show", "--graphql", &graphql, &parent_request_id],
    )?;
    assert!(text_output.contains("Transition history:"));
    assert!(text_output.contains("Backgrounded tools:"));
    assert!(text_output.contains("await_mode=background"));
    assert!(text_output.contains("cancel_policy=cascade"));
    assert!(text_output.contains(&child_request_id));

    Ok(())
}
