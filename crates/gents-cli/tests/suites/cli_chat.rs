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
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

    let first_stdout = run_cli_text(&home_dir, &["chat", &first_prompt])?;
    assert!(
        first_stdout.contains(&expected_reply),
        "expected first chat turn to contain {expected_reply}, got:\n{first_stdout}"
    );

    let (_request_id, session_id, _agent_id) =
        wait_for_request(&graphql, &node_did, &first_prompt).await?;

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
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

    let output = run_cli_json(
        &home_dir,
        &[
            "chat",
            "--message-file",
            message_path
                .to_str()
                .ok_or_else(|| anyhow!("message path is not utf-8"))?,
            "--output",
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
        output.pointer("/output/kind").and_then(Value::as_str),
        Some("terminal_message")
    );
    assert_eq!(
        output
            .pointer("/output/presentation/body_markdown")
            .and_then(Value::as_str),
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
    // Bound, not dropped: the server must outlive the chat child below.
    let (_serve, port, _readiness) =
        spawn_server_with_ready_json_recovering(&home_dir, port, &[], &[])?;
    let graphql = graphql_url(port);
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;
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
        stdout.contains("[tool] read_file notes.txt -> ok"),
        "expected chat output to contain the short tool completion summary (name, path, outcome), got:\n{stdout}"
    );
    assert!(
        !stdout.contains("gents_fs:") && !stdout.contains("\"path\":\"notes.txt\""),
        "raw tool JSON should not appear without --verbose, got:\n{stdout}"
    );
    assert!(
        stdout.contains(expected_reply),
        "expected chat output to contain final reply {expected_reply}, got:\n{stdout}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_verbose_flag_prints_raw_tool_json() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    fs::write(home_dir.join("notes.txt"), "chat-tool-token\n")?;

    let expected_reply = "chat-tool-token";
    let model_name = format!("mock-verbose-tool-chat-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockOpenAIEndpoint::start(&model_name, expected_reply)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-verbose-tool-chat-{}", Uuid::new_v4().simple());
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
    wait_for_runtime_state_graphql(&home_dir, &graphql, Duration::from_secs(30)).await?;

    let mut child = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .arg("chat")
        .arg("--verbose")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning gents chat --verbose")?;

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
        .context("waiting for gents chat --verbose")?;
    if !output.status.success() {
        bail!(
            "gents chat --verbose failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[tool done] read_file") || stdout.contains("[tool] read_file"),
        "expected --verbose chat output to keep the raw tool markers, got:\n{stdout}"
    );
    assert!(
        stdout.contains("\"path\":\"notes.txt\"") || stdout.contains("\"path\": \"notes.txt\""),
        "expected --verbose chat output to contain raw tool call arguments, got:\n{stdout}"
    );

    Ok(())
}

async fn interactive_input_drains_while_provider_is_held(
    exit_command: bool,
    one_shot: bool,
) -> Result<()> {
    use crate::support::mocks::fake_llm::{ChatAction, FakeLlm};
    use std::sync::Arc;
    fn is_user_text(message: &Value, text: &str) -> bool {
        message["role"] == "user"
            && match &message["content"] {
                Value::String(content) => content == text,
                Value::Array(parts) => parts.len() == 1 && parts[0]["text"] == text,
                _ => false,
            }
    }
    let home = tempfile::tempdir()?;
    let first = "interactive-held-first";
    let second = "interactive-held-second";
    let third = "interactive-held-third";
    let initial_reply = "interactive-initial-response";
    let final_reply = "interactive-final-response";
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let responder_gate = gate.clone();
    let model = "interactive-drain-model";
    let endpoint = FakeLlm::start(
        model,
        None,
        Arc::new(move |request| {
            let exact_user = |text: &str| {
                request["messages"].as_array().is_some_and(|messages| {
                    messages.iter().any(|message| is_user_text(message, text))
                })
            };
            if exact_user(third) {
                ChatAction::Sse(completion_text_sse(final_reply))
            } else if exact_user(first) {
                ChatAction::WaitThenSse(responder_gate.clone(), completion_text_sse(initial_reply))
            } else {
                ChatAction::Sse(completion_text_sse("session title"))
            }
        }),
    )?;
    let init = run_init_json(
        home.path(),
        &[
            "--node-name",
            "interactive-drain",
            "--model-name",
            model,
            "--inference-url",
            endpoint.endpoint(),
        ],
    )?;
    let node = node_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut server = spawn_server(home.path(), port)?;
    wait_for_port(port, &mut server)?;
    wait_for_runtime_ready(&graphql, &node, Duration::from_secs(30)).await?;
    wait_for_runtime_state_graphql(home.path(), &graphql, Duration::from_secs(30)).await?;
    let mut child = Command::new(cli_bin())
        .env("HOME", home.path())
        .env("RUST_LOG", "error")
        .args(["chat", "--poll-secs", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    struct ChildGuard(Option<std::process::Child>);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let mut child = ChildGuard(Some(child));
    let mut stdin = child
        .0
        .as_mut()
        .unwrap()
        .stdin
        .take()
        .context("chat stdin")?;
    writeln!(stdin, "{first}")?;
    stdin.flush()?;
    let result: Result<()> = async {
        tokio::time::timeout(Duration::from_secs(30), async {
            while !endpoint.captured_chat_requests().iter().any(|request| {
                request["messages"].as_array().is_some_and(|messages| messages.iter()
                    .any(|message| is_user_text(message, first)))
            }) { tokio::time::sleep(Duration::from_millis(25)).await; }
        }).await.context("initial main provider request was not held")?;
        let mut one_shot_child = None;
        writeln!(stdin, "{second}")?;
        stdin.flush()?;
        if one_shot {
            let session_query = format!(r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, purpose: {{ _eq: "normal" }} }}) {{ content session_id }} }}"#, escape_graphql_string(&node));
            let session = tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let value = graphql_query(&graphql, &session_query).await?;
                    if let Some(row) = value["data"]["AgentRequest"].as_array().context("session rows")?
                        .iter().find(|row| row["content"] == second) {
                        break Ok::<_, anyhow::Error>(row["session_id"].as_str().context("session ID")?.to_owned());
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }).await.context("second input was not saved")??;
            one_shot_child = Some(ChildGuard(Some(Command::new(cli_bin())
                .env("HOME", home.path()).env("RUST_LOG", "error")
                .args(["chat", "--poll-secs", "1", "--session-id", &session, third])
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?)));
        } else {
            writeln!(stdin, "{third}")?;
        }
        if exit_command { writeln!(stdin, "/exit")?; }
        stdin.flush()?;
        drop(stdin);
        let query = format!(r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, purpose: {{ _eq: "normal" }} }}) {{ _docID request_id content session_id agent_id input lifecycle_state superseded_by_request superseded_by_request_doc_id failure_reason terminal_output }} }}"#, escape_graphql_string(&node));
        let seeded = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let value = graphql_query(&graphql, &query).await?;
                let rows = value["data"]["AgentRequest"].as_array().context("request rows")?;
                if [first, second, third].iter().all(|text| rows.iter().any(|row| row["content"] == *text)) {
                    break Ok::<_, anyhow::Error>(rows.clone());
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }).await.context("interactive stdin blocked behind first response")??;
        assert!(child.0.as_mut().unwrap().try_wait()?.is_none(), "chat exited before held response completed");
        assert_eq!(seeded.len(), 3, "expected exactly three normal submitted inputs");
        let parent = seeded.iter().find(|row| row["content"] == first).unwrap()["_docID"].clone();
        gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(30), async {
            while child.0.as_mut().unwrap().try_wait()?.is_none() { tokio::time::sleep(Duration::from_millis(25)).await; }
            Ok::<_, anyhow::Error>(())
        }).await.context("chat did not drain submitted turns")??;
        let output = child.0.take().unwrap().wait_with_output()?;
        anyhow::ensure!(output.status.success(), "chat failed: {}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(stdout.matches(final_reply).count(), 1, "duplicate/missing final output: {stdout}");
        if let Some(mut single) = one_shot_child {
            tokio::time::timeout(Duration::from_secs(30), async {
                while single.0.as_mut().unwrap().try_wait()?.is_none() {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Ok::<_, anyhow::Error>(())
            }).await.context("one-shot folded chat did not finish")??;
            let output = single.0.take().unwrap().wait_with_output()?;
            anyhow::ensure!(output.status.success(), "one-shot chat failed: {}", String::from_utf8_lossy(&output.stderr));
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert_eq!(stdout.matches(final_reply).count(), 1, "one-shot folded output missing or duplicated: {stdout}");
        }
        let value = graphql_query(&graphql, &query).await?;
        let rows = value["data"]["AgentRequest"].as_array().context("final request rows")?;
        for text in [second, third] {
            let matching = rows.iter().filter(|row| row["content"] == text).collect::<Vec<_>>();
            assert_eq!(matching.len(), 1, "duplicate submitted input {text}");
            assert_eq!(matching[0]["lifecycle_state"], "superseded");
            assert_eq!(matching[0]["superseded_by_request_doc_id"], parent,
                "{text} must be consumed by the original physical request; seeded={}; final={}",
                serde_json::to_string(&seeded)?, serde_json::to_string(rows)?);
            assert_eq!(matching[0]["failure_reason"], "folded into claimed request");
        }
        let captured = endpoint.captured_chat_requests();
        let continuation = captured.iter().find(|request| request["messages"].as_array().is_some_and(|messages|
            messages.iter().any(|message| is_user_text(message, third))))
            .context("provider never received queued inputs")?;
        let messages = continuation["messages"].as_array().unwrap();
        let positions = [first, second, third].map(|text| messages.iter().position(|message|
            is_user_text(message, text)).expect("input missing from provider"));
        assert!(positions[0] < positions[1] && positions[1] < positions[2]);
        for text in [first, second, third] {
            assert_eq!(messages.iter().filter(|message| is_user_text(message, text)).count(), 1);
        }
        Ok(())
    }.await;
    // Release the provider even when an assertion prerequisite failed.
    gate.add_permits(1);
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_chat_accepts_input_during_response_and_drains_on_eof() -> Result<()> {
    interactive_input_drains_while_provider_is_held(false, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactive_chat_accepts_input_during_response_and_drains_on_exit() -> Result<()> {
    interactive_input_drains_while_provider_is_held(true, false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_shot_chat_folded_into_active_turn_returns_its_output_once() -> Result<()> {
    interactive_input_drains_while_provider_is_held(false, true).await
}
