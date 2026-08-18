use crate::support::*;

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_uses_runtime_state_for_interactive_turns() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("chat-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-chat-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-chat-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

    let mut child = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .arg("chat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning gents chat")?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("chat child missing stdin"))?;
        stdin
            .write_all(b"Reply with exactly the configured token.\n/exit\n")
            .context("writing interactive chat input")?;
        stdin.flush().context("flushing interactive chat input")?;
    }

    let output = child.wait_with_output().context("waiting for gents chat")?;
    if !output.status.success() {
        bail!(
            "gents chat failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&expected_reply),
        "expected chat output to contain {expected_reply}, got:\n{stdout}"
    );

    let captured_requests = mock_endpoint.captured_chat_requests();
    let chat_request = captured_requests
        .iter()
        .find(|request| {
            request_contains_role_text(request, "user", "Reply with exactly the configured token.")
                && request_system_message(request).is_some_and(|system| {
                    system.contains("read-only operating mode")
                        && system.contains("incident triage")
                })
        })
        .ok_or_else(|| anyhow!("mock endpoint did not capture the user chat request"))?;
    assert_eq!(
        chat_request.get("model").and_then(Value::as_str),
        Some(model_name.as_str())
    );
    assert!(
        request_system_message(chat_request)
            .is_some_and(|system| system.contains("read-only operating mode")
                && system.contains("incident triage")),
        "expected system prompt in request: {}",
        chat_request
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_continues_existing_session_when_session_id_is_provided() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("chat-continue-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-chat-continue-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-chat-continue-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let first_prompt = format!("Remember the token {}.", Uuid::new_v4().simple());
    let second_prompt = "What token did I tell you to remember?";

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

    let first_stdout = run_cli_text(&home_dir, &["chat", &first_prompt])?;
    assert!(
        first_stdout.contains(&expected_reply),
        "expected first chat turn to contain {expected_reply}, got:\n{first_stdout}"
    );

    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&graphql, &agent_did, &first_prompt).await?;

    let second_stdout = run_cli_text(
        &home_dir,
        &["chat", "--session-id", &session_id, second_prompt],
    )?;
    assert!(
        second_stdout.contains(&expected_reply),
        "expected follow-up chat turn to contain {expected_reply}, got:\n{second_stdout}"
    );

    let captured_requests = mock_endpoint.captured_chat_requests();
    let follow_up_request = captured_requests
        .iter()
        .find(|request| request_contains_role_text(request, "user", second_prompt))
        .ok_or_else(|| anyhow!("mock endpoint did not capture the follow-up chat request"))?;
    assert!(
        request_contains_role_text(follow_up_request, "user", &first_prompt),
        "expected follow-up request to include prior user turn: {}",
        follow_up_request
    );
    assert!(
        request_contains_role_text(follow_up_request, "assistant", &expected_reply),
        "expected follow-up request to include prior assistant turn: {}",
        follow_up_request
    );
    assert!(
        request_contains_role_text(follow_up_request, "user", second_prompt),
        "expected follow-up request to include current user turn: {}",
        follow_up_request
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_supports_message_file_json_output_and_output_file() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("chat-json-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-chat-json-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-chat-json-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let message = format!("Reply with exactly {}.", Uuid::new_v4().simple());
    let message_path = tempdir.path().join("chat-message.txt");
    let output_path = tempdir.path().join("chat-output.json");
    fs::write(&message_path, &message)?;

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

    let output = run_cli_json(
        &home_dir,
        &[
            "chat",
            "--message-file",
            message_path
                .to_str()
                .ok_or_else(|| anyhow!("message path is not utf-8"))?,
            "--output-format",
            "json",
            "--output-file",
            output_path
                .to_str()
                .ok_or_else(|| anyhow!("output path is not utf-8"))?,
        ],
    )?;

    let file_output = read_json_file(&output_path)?;
    assert_eq!(output, file_output);
    assert!(
        output.get("request_id").and_then(Value::as_str).is_some(),
        "chat json output should include request_id: {output}"
    );
    assert!(
        output.get("session_id").and_then(Value::as_str).is_some(),
        "chat json output should include session_id: {output}"
    );
    assert_eq!(
        output.pointer("/response/status").and_then(Value::as_str),
        Some("complete")
    );
    assert_eq!(
        output.pointer("/response/content").and_then(Value::as_str),
        Some(expected_reply.as_str())
    );

    let captured_requests = mock_endpoint.captured_chat_requests();
    let chat_request = captured_requests
        .iter()
        .find(|request| request_contains_role_text(request, "user", &message))
        .ok_or_else(|| anyhow!("mock endpoint did not capture the message-file chat request"))?;
    assert!(
        request_contains_role_text(chat_request, "user", &message),
        "expected request to include message file content: {}",
        chat_request
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_buffers_final_response_and_shows_tool_progress() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    fs::write(home_dir.join("notes.txt"), "chat-tool-token\n")?;

    let expected_reply = "chat-tool-token";
    let model_name = format!("mock-tool-chat-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockOpenAIEndpoint::start(&model_name, expected_reply)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-tool-chat-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

    let mut child = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .arg("chat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning gents chat for tool transcript test")?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("chat child missing stdin"))?;
        stdin
            .write_all(b"Read notes.txt and reply with its token.\n/exit\n")
            .context("writing interactive chat input")?;
        stdin.flush().context("flushing interactive chat input")?;
    }

    let output = child
        .wait_with_output()
        .context("waiting for gents chat tool transcript run")?;
    if !output.status.success() {
        bail!(
            "gents chat failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[tool] read_file"),
        "expected chat output to contain tool start, got:\n{stdout}"
    );
    assert!(
        stdout.contains("[tool done] read_file"),
        "expected chat output to contain tool completion, got:\n{stdout}"
    );
    assert!(
        stdout.contains(expected_reply),
        "expected chat output to contain final reply {expected_reply}, got:\n{stdout}"
    );

    Ok(())
}
