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
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let submit = spawn_cli(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--node-did",
            &node_did,
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
        parsed.pointer("/output/kind").and_then(Value::as_str),
        Some("terminal_message")
    );
    assert_eq!(
        parsed
            .pointer("/output/presentation/body_markdown")
            .and_then(Value::as_str),
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
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let submit = spawn_cli(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--node-did",
            &node_did,
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
            .pointer("/output/presentation/body_markdown")
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
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let submitted = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--node-did",
            &node_did,
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
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let submitted = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--node-did",
            &node_did,
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
            .pointer("/output/presentation/body_markdown")
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
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let session_id = format!("show-session-{}", Uuid::new_v4().simple());
    let parent_request_id = format!("show-parent-{}", Uuid::new_v4().simple());
    let child_request_id = format!("show-child-{}", Uuid::new_v4().simple());
    let child_session_id = format!("show-child-session-{}", Uuid::new_v4().simple());
    let tool_call_id = format!("show-tool-{}", Uuid::new_v4().simple());
    let tool_call_key = format!("{session_id}:{tool_call_id}");
    let parent_create = graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{purpose: "normal", 
                    request_id: "{parent_request_id}",
                    node_did: "{node_did}",
                    agent_id: "parent-agent",
                    session_id: "{session_id}",
                    content: "parent request that started a session",
                    lifecycle_state: "processing",
                    backend_id: "studios-cluster",
                    execution_origin: "operatorCli",
                    created_at: "2026-05-20T10:00:00Z",
                    claimed_at: "2026-05-20T10:00:01Z",
                    deadline: "2026-05-20T10:05:00Z",
                    retry_count: 0,
                    request_hop: 0
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
                    node_did: "{node_did}",
                    request_id: "{parent_request_id}",
                    request_doc_id: "{parent_doc_id}",
                    session_id: "{session_id}",
                    message_sequence: 1,
                    tool_name: "agent_new",
                    tool_call_id: "{tool_call_id}",
                    status: "called",
                    lifecycle_state: "running",
                    started_at: "2026-05-20T10:00:02Z",
                    deadline_at: "2026-05-20T10:05:00Z",
                    await_mode: "background"
                }}) {{ _docID }}
            }}"#,
            parent_doc_id = escape_graphql_string(&parent_doc_id),
        ),
    )
    .await?;
    let tool_call_doc_id = doc_id_from_create(&tool_create, "add_AgentToolCall")?;
    let child_create = graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{purpose: "normal", 
                    request_id: "{child_request_id}",
                    node_did: "{node_did}",
                    requester_did: "{node_did}",
                    agent_id: "child-agent",
                    session_id: "{child_session_id}",
                    content: "request caused by the agent_new tool call",
                    lifecycle_state: "processing",
                    created_at: "2026-05-20T10:00:03Z",
                    claimed_at: "2026-05-20T10:00:04Z",
                    retry_count: 0,
                    request_hop: 1,
                    caused_by_parent_request_id: "{parent_request_id}",
                    caused_by_parent_request_doc_id: "{parent_doc_id}",
                    caused_by_parent_tool_call_id: "{tool_call_id}",
                    caused_by_parent_tool_call_doc_id: "{tool_call_doc_id}"
                }}) {{ _docID }}
            }}"#,
            parent_doc_id = escape_graphql_string(&parent_doc_id),
            tool_call_doc_id = escape_graphql_string(&tool_call_doc_id),
        ),
    )
    .await?;
    let child_doc_id = doc_id_from_create(&child_create, "add_AgentRequest")?;
    create_started_session(
        &graphql,
        &identity_from_init(&init)?,
        gents_protocol::session::AgentSession {
            session_id: child_session_id.clone(),
            node_did: node_did.clone(),
            requester_did: Some(node_did.clone()),
            agent_id: "child-agent".to_owned(),
            created_at: "2026-05-20T10:00:03Z".to_owned(),
            closed_at: None,
            archived_at: None,
            archive_revision: 0,
            stopped_at: None,
            title: None,
            tags: Vec::new(),
            provenance: None,
            observation: None,
        },
        gents_protocol::session::SessionReference {
            node_did: node_did.clone(),
            session_id: session_id.clone(),
            requester_did: None,
        },
        &parent_doc_id,
        &child_doc_id,
        &tool_call_doc_id,
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
            .pointer("/tool_calls/0/cancel_cause")
            .and_then(Value::as_str),
        Some("unknown"),
        "request show must render an absent cancel_cause as unknown"
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
            .pointer("/child_requests/0/agent_id")
            .and_then(Value::as_str),
        Some("child-agent")
    );
    assert_eq!(
        json_output
            .pointer("/child_requests/0/session_id")
            .and_then(Value::as_str),
        Some(child_session_id.as_str())
    );
    assert_eq!(
        json_output
            .pointer("/child_requests/0/caused_by_parent_tool_call_id")
            .and_then(Value::as_str),
        Some(tool_call_id.as_str())
    );

    let text_output = run_cli_text(
        &home_dir,
        &["request", "show", "--graphql", &graphql, &parent_request_id],
    )?;
    assert!(text_output.contains("Transition history:"));
    assert!(text_output.contains("Backgrounded tools:"));
    assert!(text_output.contains("await_mode=background"));
    assert!(text_output.contains(&child_request_id));
    assert_eq!(json_output.get("blocked"), Some(&Value::Null));

    let limited_request_id = format!("show-limited-{}", Uuid::new_v4().simple());
    let failure = "provider usage limit reached (resets at 2030-01-01T00:00:00Z): The usage limit has been reached";
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{purpose: "normal",
                    request_id: "{limited_request_id}", node_did: "{node_did}",
                    agent_id: "parent-agent", session_id: "{session_id}",
                    content: "limited", lifecycle_state: "failed",
                    failure_reason: "{failure}", created_at: "2026-05-20T10:01:00Z"
                }}) {{ _docID }}
                create_InferenceCall(input: {{
                    call_id: "{limited_request_id}-call", request_id: "{limited_request_id}",
                    call_seq: 1, backend_id: "studios-cluster", agent_id: "parent-agent",
                    node_did: "{node_did}", call_kind: "inference", attempt: 1,
                    call_state: "failed", failure_reason: "{failure}",
                    queued_at: "2026-05-20T10:01:00Z", started_at: "2026-05-20T10:01:00Z",
                    ended_at: "2026-05-20T10:01:01Z"
                }}) {{ _docID }}
            }}"#
        ),
    )
    .await?;
    let limited = run_cli_json(
        &home_dir,
        &[
            "request",
            "show",
            "--graphql",
            &graphql,
            "--output",
            "json",
            &limited_request_id,
        ],
    )?;
    assert_eq!(
        limited.pointer("/blocked/reason").and_then(Value::as_str),
        Some("usage_limit"),
        "{limited}"
    );
    assert_eq!(
        limited
            .pointer("/blocked/resets_at")
            .and_then(Value::as_str),
        Some("2030-01-01T00:00:00Z")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_message_commands_edit_reorder_and_remove_against_held_provider() -> Result<()> {
    use crate::support::mocks::fake_llm::{ChatAction, FakeLlm};
    use std::process::Child;
    use std::sync::Arc;

    fn is_user_text(message: &Value, text: &str) -> bool {
        message["role"] == "user"
            && match &message["content"] {
                Value::String(content) => content == text,
                Value::Array(parts) => parts.len() == 1 && parts[0]["text"] == text,
                _ => false,
            }
    }
    fn has_user_text(request: &Value, text: &str) -> bool {
        request["messages"]
            .as_array()
            .is_some_and(|messages| messages.iter().any(|message| is_user_text(message, text)))
    }
    struct ChildGuard(Option<Child>);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    fn ids(snapshot: &Value) -> Vec<String> {
        snapshot["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| {
                entry["request_doc_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }
    fn contents(snapshot: &Value) -> Vec<String> {
        snapshot["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| entry["content"].as_str().unwrap_or_default().to_owned())
            .collect()
    }
    fn doc_id_of(snapshot: &Value, content: &str) -> Result<String> {
        snapshot["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| entry["content"] == content)
            .and_then(|entry| entry["request_doc_id"].as_str())
            .map(str::to_owned)
            .with_context(|| format!("{content} is not pending: {snapshot}"))
    }

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("home");
    fs::create_dir_all(&home)?;
    let first = "pending-cli-held-first";
    let alpha = "pending-cli-alpha";
    let alpha_edited = "pending-cli-alpha-edited";
    let beta = "pending-cli-beta";
    let gamma = "pending-cli-gamma";
    let initial_reply = "pending-cli-initial-response";
    let final_reply = "pending-cli-final-response";
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let responder_gate = gate.clone();
    let model = format!("mock-pending-cli-{}", Uuid::new_v4().simple());
    let endpoint = FakeLlm::start(
        &model,
        None,
        Arc::new(move |request| {
            if has_user_text(request, alpha_edited) || has_user_text(request, gamma) {
                ChatAction::Sse(completion_text_sse(final_reply))
            } else if has_user_text(request, first) {
                ChatAction::WaitThenSse(responder_gate.clone(), completion_text_sse(initial_reply))
            } else {
                ChatAction::Sse(completion_text_sse("session title"))
            }
        }),
    )?;
    let init = run_init_json(
        &home,
        &[
            "--node-name",
            &format!("cli-pending-{}", Uuid::new_v4().simple()),
            "--model-name",
            &model,
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    let node = node_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut serve = spawn_server(&home, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node, Duration::from_secs(30)).await?;

    let submitted = run_cli_json(
        &home,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--node-did",
            &node,
            "--content",
            first,
            "--no-wait",
        ],
    )?;
    let session = submitted["session_id"]
        .as_str()
        .context("request submit output missing session_id")?
        .to_owned();
    let scope = [
        "--graphql",
        graphql.as_str(),
        "--node-did",
        node.as_str(),
        "--session-id",
        session.as_str(),
        "--output",
        "json",
    ];
    let pending = || run_cli_json(&home, &[&["request", "pending"][..], &scope[..]].concat());
    let wait_for_pending = |expected: Vec<&'static str>| {
        let pending = &pending;
        async move {
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let snapshot = pending()?;
                    if contents(&snapshot) == expected {
                        break Ok::<_, anyhow::Error>(snapshot);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .with_context(|| format!("pending queue never became {expected:?}"))?
        }
    };

    let mut waiter = ChildGuard(None);
    let result: Result<()> = async {
        tokio::time::timeout(Duration::from_secs(30), async {
            while !endpoint
                .captured_chat_requests()
                .iter()
                .any(|request| has_user_text(request, first))
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .context("first request never reached the held provider")?;

        waiter.0 = Some(spawn_cli(
            &home,
            &[
                "request",
                "submit",
                "--graphql",
                &graphql,
                "--node-did",
                &node,
                "--session-id",
                &session,
                "--content",
                alpha,
                "--poll-secs",
                "1",
            ],
        )?);
        wait_for_pending(vec![alpha]).await?;
        for content in [beta, gamma] {
            run_cli_json(
                &home,
                &[
                    "request",
                    "submit",
                    "--graphql",
                    &graphql,
                    "--node-did",
                    &node,
                    "--session-id",
                    &session,
                    "--content",
                    content,
                    "--no-wait",
                ],
            )?;
        }
        let queued = wait_for_pending(vec![alpha, beta, gamma]).await?;
        assert!(
            queued["entries"]
                .as_array()
                .is_some_and(|entries| entries.iter().all(|entry| entry["editable"] == true)),
            "queued author messages must be editable: {queued}"
        );

        let expected_flags = |snapshot: &Value| {
            ids(snapshot)
                .into_iter()
                .flat_map(|id| ["--expected-request-doc-id".to_owned(), id])
                .collect::<Vec<_>>()
        };
        let mutate = |command: &str, snapshot: &Value, extra: &[&str]| {
            let expected = expected_flags(snapshot);
            let mut args = vec!["request", command];
            args.extend_from_slice(&scope);
            args.extend(expected.iter().map(String::as_str));
            args.extend_from_slice(extra);
            args.into_iter().map(str::to_owned).collect::<Vec<_>>()
        };
        let run = |args: Vec<String>| {
            run_cli_json(&home, &args.iter().map(String::as_str).collect::<Vec<_>>())
        };

        let alpha_id = doc_id_of(&queued, alpha)?;
        let edited = run(mutate(
            "edit-pending",
            &queued,
            &["--request-doc-id", &alpha_id, "--content", alpha_edited],
        ))?;
        assert_eq!(
            edited["request_doc_ids"].as_array().map(Vec::len),
            Some(1),
            "edit must name one replacement: {edited}"
        );
        let after_edit = pending()?;
        assert_eq!(contents(&after_edit), [alpha_edited, beta, gamma]);
        assert!(!ids(&after_edit).contains(&alpha_id));

        let (beta_id, gamma_id) = (
            doc_id_of(&after_edit, beta)?,
            doc_id_of(&after_edit, gamma)?,
        );
        run(mutate(
            "reorder-pending",
            &after_edit,
            &["--request-doc-id", &gamma_id, "--request-doc-id", &beta_id],
        ))?;
        let after_reorder = pending()?;
        assert_eq!(contents(&after_reorder), [alpha_edited, gamma, beta]);

        let removed = run(mutate(
            "remove-pending",
            &after_reorder,
            &["--request-doc-id", &doc_id_of(&after_reorder, beta)?],
        ))?;
        assert_eq!(
            removed["request_doc_ids"].as_array().map(Vec::len),
            Some(0),
            "removal creates no replacement: {removed}"
        );
        let settled = pending()?;
        assert_eq!(contents(&settled), [alpha_edited, gamma]);

        let stale = mutate(
            "edit-pending",
            &after_reorder,
            &[
                "--request-doc-id",
                &doc_id_of(&settled, gamma)?,
                "--content",
                "pending-cli-stale",
            ],
        );
        let stderr =
            run_cli_failure_stderr(&home, &stale.iter().map(String::as_str).collect::<Vec<_>>())?;
        assert!(
            stderr.contains("The message queue changed"),
            "stale snapshot must fail with a queue-changed error: {stderr}"
        );
        let unchanged = pending()?;
        assert_eq!(
            ids(&unchanged),
            ids(&settled),
            "stale edit changed the queue"
        );
        assert_eq!(contents(&unchanged), [alpha_edited, gamma]);

        let child = waiter.0.as_mut().context("waiter child")?;
        assert!(
            child.try_wait()?.is_none(),
            "submit --wait exited before the held provider was released"
        );

        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(60), async {
            while waiter.0.as_mut().unwrap().try_wait()?.is_none() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("submit --wait did not follow its edited message to completion")??;
        let output = waiter.0.take().unwrap().wait_with_output()?;
        anyhow::ensure!(
            output.status.success(),
            "submit --wait failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let waited: Value =
            serde_json::from_slice(&output.stdout).context("parsing submit --wait output")?;
        assert!(
            waited["output"].to_string().contains(final_reply),
            "waiter must return the output that consumed its replacement: {waited}"
        );

        let captured = endpoint.captured_chat_requests();
        let continuation = captured
            .iter()
            .find(|request| has_user_text(request, gamma))
            .context("provider never received the queued replacements")?;
        let messages = continuation["messages"].as_array().unwrap();
        let position = |text: &str| {
            messages
                .iter()
                .position(|message| is_user_text(message, text))
        };
        let (Some(first_at), Some(edited_at), Some(gamma_at)) =
            (position(first), position(alpha_edited), position(gamma))
        else {
            bail!("continuation is missing a delivered message: {continuation}");
        };
        assert!(first_at < edited_at && edited_at < gamma_at);
        for text in [first, alpha_edited, gamma] {
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| is_user_text(message, text))
                    .count(),
                1,
                "{text} delivered more than once"
            );
        }
        for text in [alpha, beta, "pending-cli-stale"] {
            assert!(
                !captured.iter().any(|request| has_user_text(request, text)),
                "replaced or removed message {text} reached the provider"
            );
        }
        assert!(contents(&pending()?).is_empty(), "queue must drain");
        Ok(())
    }
    .await;
    gate.add_permits(1);
    result
}

/// Write `session` as `agent_new` creation does: the immutable starter index
/// at create, then the receipt the node signs for the created document.
async fn create_started_session(
    graphql: &str,
    node: &gents::KeyIdentity,
    session: gents_protocol::session::AgentSession,
    source: gents_protocol::session::SessionReference,
    parent_doc_id: &str,
    opening_doc_id: &str,
    tool_call_doc_id: &str,
) -> Result<()> {
    use gents::NodeIdentity as _;
    use gents_protocol::session::{
        SessionCreationCause, SessionCreationReceipt, SessionCreationStarter, SessionProvenance,
    };

    let mut provenance = SessionProvenance {
        parent_request_doc_id: Some(parent_doc_id.to_owned()),
        opening_request_doc_id: Some(opening_doc_id.to_owned()),
        started_by: Some(source.clone()),
        ..Default::default()
    };
    let mut input = serde_json::to_value(gents_protocol::session::AgentSession {
        provenance: Some(provenance.clone()),
        ..session.clone()
    })?;
    input["started_by_node_did"] = source.node_did.clone().into();
    input["started_by_session_id"] = source.session_id.clone().into();
    let created = graphql_query(
        graphql,
        &format!(
            "mutation {{ create_AgentSession(input: {}) {{ _docID }} }}",
            gents_protocol::graphql::graphql_input_literal(&input)?
        ),
    )
    .await?;
    let session_doc_id = doc_id_from_create(&created, "add_AgentSession")?;
    let mut receipt = SessionCreationReceipt {
        node_did: session.node_did,
        session_id: session.session_id,
        session_doc_id: session_doc_id.clone(),
        requester_did: session.requester_did,
        agent_id: session.agent_id,
        opening_request_doc_id: opening_doc_id.to_owned(),
        cause: SessionCreationCause::AgentNew,
        cause_binding: tool_call_doc_id.to_owned(),
        starter: Some(SessionCreationStarter {
            source,
            parent_request_doc_id: parent_doc_id.to_owned(),
        }),
        signature: Vec::new(),
    };
    receipt.signature = node.sign(&receipt.signing_payload()).await?;
    provenance.creation_receipt = Some(receipt);
    graphql_query(
        graphql,
        &format!(
            r#"mutation {{ update_AgentSession(docID: "{}", input: {}) {{ _docID }} }}"#,
            escape_graphql_string(&session_doc_id),
            gents_protocol::graphql::graphql_input_literal(
                &serde_json::json!({ "provenance": provenance })
            )?
        ),
    )
    .await?;
    Ok(())
}
