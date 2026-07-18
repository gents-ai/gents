//! Integration coverage for `defra-agent demo`: the interactive shell is driven
//! non-interactively (piped stdin) against a test-only mock OpenAI endpoint. The
//! *shipped* demo bundles no mock — these assert node bring-up, streaming chat,
//! the seeded skills, live backend `reconfigure`, resume, and clean teardown.

mod support;
use support::*;

use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use defra_agent::defra_node::{EmbeddedNode, StorageBackend};
use serde_json::{json, Value};
use uuid::Uuid;

/// Drive `defra-agent demo` to completion with `input` fed to its shell.
fn run_demo(tmp_home: &Path, args: &[&str], input: &str) -> Result<std::process::Output> {
    let mut child = Command::new(cli_bin())
        .env("HOME", tmp_home)
        .env("RUST_LOG", "error")
        .arg("demo")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning defra-agent demo")?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("demo child missing stdin"))?;
        stdin
            .write_all(input.as_bytes())
            .context("writing demo shell input")?;
        stdin.flush().context("flushing demo shell input")?;
    }
    child
        .wait_with_output()
        .context("waiting for defra-agent demo")
}

/// True once nothing accepts connections on `127.0.0.1:port` (evidence the demo
/// tore its server subprocess down and left no orphan).
fn wait_port_free(port: u16, timeout: Duration) -> bool {
    let addr = format!("127.0.0.1:{port}").parse().expect("loopback addr");
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn allocate_demo_base_port() -> Result<u16> {
    loop {
        let port = allocate_port()?;
        let Some(worker_port) = port.checked_add(1) else {
            continue;
        };
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", worker_port)) {
            drop(listener);
            return Ok(port);
        }
    }
}

fn require_success(output: &std::process::Output) -> Result<String> {
    if !output.status.success() {
        bail!(
            "defra-agent demo exited non-zero\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_single_node_chats_lists_skills_and_shuts_down_clean() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("demo-home");

    let reply = format!("demo-reply-{}", Uuid::new_v4().simple());
    let model = format!("demo-model-{}", Uuid::new_v4().simple());
    let mock = MockChatEndpoint::start(&model, &reply)?;
    let port = allocate_port()?;

    // status (node A live) → skill (list) → chat one turn → /back → down.
    let input = "status\nskill\nchat\nSay the configured token.\n/back\ndown\n";
    let output = run_demo(
        tempdir.path(),
        &[
            "--home",
            home.to_str().unwrap(),
            "--inference-url",
            mock.endpoint(),
            "--model",
            &model,
            "--http-port",
            &port.to_string(),
        ],
        input,
    )?;
    let stdout = require_success(&output)?;

    assert!(
        stdout.contains("node A: live"),
        "expected `status` to show node A live, got:\n{stdout}"
    );
    assert!(
        stdout.contains("summarize") && stdout.contains("fleet-guide"),
        "expected the seeded demo skills to be listed, got:\n{stdout}"
    );
    assert!(
        stdout.contains(&reply),
        "expected the streamed chat reply {reply}, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Stopped."),
        "expected a clean shutdown message, got:\n{stdout}"
    );
    assert!(
        wait_port_free(port, Duration::from_secs(15)),
        "demo left an orphaned server listening on port {port}"
    );

    // The persistent agent was written for a later resume.
    assert!(
        home.join("init.json").exists(),
        "expected the demo to persist init.json under its home"
    );
    Ok(())
}

fn request_tool_result_values(request: &Value) -> Vec<Value> {
    request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        .filter_map(|message| match message.get("content") {
            Some(Value::String(content)) => Some(content.clone()),
            Some(Value::Array(parts)) => Some(
                parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .filter_map(|content| serde_json::from_str(&content).ok())
        .collect()
}

fn list_entries_are_materialized(value: &Value, expected: usize) -> bool {
    value
        .get("entries")
        .and_then(Value::as_array)
        .is_some_and(|entries| {
            entries.len() == expected
                && entries.iter().all(|entry| {
                    entry
                        .get("child_session_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| !id.is_empty())
                        && matches!(
                            entry.get("status").and_then(Value::as_str),
                            Some("running" | "completed")
                        )
                        && entry.get("diagnostic").is_none_or(Value::is_null)
                })
        })
}

/// #734/#735 regressions: the shipped `pair` -> `delegate` path must carry
/// background spawn bridges to node B, expose both materialized live children
/// through list/read on the parent, and let the worker claim and complete them.
/// This uses the real two-process demo and document-driven pairing/config;
/// only inference is hermetic. It intentionally stays in the default CLI suite
/// as the process-level regression fence; event-driven child waits keep its
/// expected runtime to roughly one worker hold plus fleet startup/teardown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_pair_delegate_materializes_remote_worker() -> Result<()> {
    use support::mocks::fake_llm::{ChatAction, FakeLlm};

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("demo-home");
    let model = format!("demo-delegate-model-{}", Uuid::new_v4().simple());
    let parent_prompt = "Run the hermetic demo delegation check.";
    let worker_prompt_a = "Hold and return the first hermetic remote-worker proof token.";
    let worker_prompt_b = "Hold and return the second hermetic remote-worker proof token.";
    let parent_reply = format!("demo-parent-finished-{}", Uuid::new_v4().simple());
    let worker_reply_a = format!("demo-worker-a-finished-{}", Uuid::new_v4().simple());
    let worker_reply_b = format!("demo-worker-b-finished-{}", Uuid::new_v4().simple());
    let list_poll_started_at = Arc::new(std::sync::Mutex::new(None::<Instant>));

    let mock = FakeLlm::start(&model, None, {
        let parent_reply = parent_reply.clone();
        let worker_reply_a = worker_reply_a.clone();
        let worker_reply_b = worker_reply_b.clone();
        let list_poll_started_at = Arc::clone(&list_poll_started_at);
        Arc::new(move |request: &Value| {
            if request_contains_role_text(request, "user", worker_prompt_a) {
                return ChatAction::DelayThenSse(
                    Duration::from_secs(15),
                    completion_text_sse(&worker_reply_a),
                );
            }
            if request_contains_role_text(request, "user", worker_prompt_b) {
                return ChatAction::DelayThenSse(
                    Duration::from_secs(15),
                    completion_text_sse(&worker_reply_b),
                );
            }
            if request_contains_role_text(request, "user", parent_prompt) {
                let results = request_tool_result_values(request);
                let spawn_results = results
                    .iter()
                    .filter(|result| {
                        result.get("await_mode").and_then(Value::as_str) == Some("background")
                            && result.get("child_request_id").is_some()
                    })
                    .collect::<Vec<_>>();
                let list_results = results
                    .iter()
                    .filter(|result| result.get("entries").is_some())
                    .collect::<Vec<_>>();
                let read_results = results
                    .iter()
                    .filter(|result| result.get("transcript").is_some())
                    .collect::<Vec<_>>();

                if spawn_results.is_empty() {
                    let args = json!({
                        "name": "worker",
                        "prompt": worker_prompt_a,
                        "await_mode": "background"
                    })
                    .to_string();
                    return ChatAction::Sse(tool_call_sse_with_id(
                        "demo-spawn-worker-a",
                        "spawn_subagent",
                        &args,
                    ));
                }
                if spawn_results.len() == 1 {
                    let args = json!({
                        "name": "worker",
                        "prompt": worker_prompt_b,
                        "await_mode": "background"
                    })
                    .to_string();
                    return ChatAction::Sse(tool_call_sse_with_id(
                        "demo-spawn-worker-b",
                        "spawn_subagent",
                        &args,
                    ));
                }

                let latest_materialized_list = list_results
                    .iter()
                    .rev()
                    .find(|result| list_entries_are_materialized(result, 2));
                if latest_materialized_list.is_none() {
                    let elapsed = {
                        let mut started_at = list_poll_started_at
                            .lock()
                            .expect("demo list poll clock poisoned");
                        let started_at = started_at.get_or_insert_with(Instant::now);
                        started_at.elapsed()
                    };
                    if elapsed >= Duration::from_secs(30) {
                        return ChatAction::Sse(completion_text_sse(
                            "demo-live-child-visibility-failed",
                        ));
                    }
                    let delay = if list_results.is_empty() {
                        Duration::from_secs(3)
                    } else {
                        Duration::from_millis(500)
                    };
                    return ChatAction::DelayThenSse(
                        delay,
                        tool_call_sse_with_id(
                            &format!("demo-list-live-{}", list_results.len()),
                            "list_subagents",
                            r#"{"status":"all","limit":10}"#,
                        ),
                    );
                }

                if read_results.is_empty() {
                    let non_read_results = spawn_results.len() + list_results.len();
                    if results.len() > non_read_results {
                        return ChatAction::Sse(completion_text_sse("demo-live-child-read-failed"));
                    }
                    let child_request_id = spawn_results[0]
                        .get("child_request_id")
                        .and_then(Value::as_str)
                        .expect("background spawn receipt has child_request_id");
                    let args = json!({
                        "child_request_id": child_request_id,
                        "include_user_messages": true
                    })
                    .to_string();
                    return ChatAction::Sse(tool_call_sse_with_id(
                        "demo-read-live-worker-a",
                        "read_subagent",
                        &args,
                    ));
                }

                let wait_results = results
                    .iter()
                    .filter(|result| {
                        result.get("await_mode").and_then(Value::as_str) != Some("background")
                            && result.get("entries").is_none()
                            && result.get("transcript").is_none()
                            && result.get("ok").is_some()
                            && result.get("child_request_id").is_some()
                    })
                    .collect::<Vec<_>>();
                if wait_results.iter().any(|result| {
                    result.get("ok").and_then(Value::as_bool) == Some(false)
                        && result.get("retryable").and_then(Value::as_bool) != Some(true)
                }) {
                    return ChatAction::Sse(completion_text_sse("demo-child-wait-failed"));
                }
                let completed_wait_ids = wait_results
                    .iter()
                    .filter(|result| {
                        result.get("ok").and_then(Value::as_bool) == Some(true)
                            && result.get("status").and_then(Value::as_str) == Some("completed")
                    })
                    .filter_map(|result| result.get("child_request_id").and_then(Value::as_str))
                    .collect::<std::collections::HashSet<_>>();
                if let Some(child_request_id) = spawn_results
                    .iter()
                    .filter_map(|result| result.get("child_request_id").and_then(Value::as_str))
                    .find(|child_request_id| !completed_wait_ids.contains(child_request_id))
                {
                    if wait_results.len() >= 12 {
                        return ChatAction::Sse(completion_text_sse("demo-child-wait-exhausted"));
                    }
                    let args = json!({ "child_request_id": child_request_id }).to_string();
                    return ChatAction::Sse(tool_call_sse_with_id(
                        &format!("demo-wait-worker-{}", wait_results.len()),
                        "wait_subagent",
                        &args,
                    ));
                }

                return ChatAction::Sse(completion_text_sse(&parent_reply));
            }
            ChatAction::Sse(completion_text_sse("demo fallback"))
        })
    })?;
    let port = allocate_demo_base_port()?;
    let worker_port = port + 1;

    let input = format!("pair\ndelegate\nchat\n{parent_prompt}\n/back\ndown\n");
    let output = run_demo(
        tempdir.path(),
        &[
            "--home",
            home.to_str().unwrap(),
            "--inference-url",
            mock.endpoint(),
            "--model",
            &model,
            "--http-port",
            &port.to_string(),
        ],
        &input,
    )?;
    let stdout = require_success(&output)?;
    assert!(
        stdout.contains("paired. Node B (worker) is live"),
        "demo pair did not converge:\n{stdout}"
    );
    assert!(
        stdout.contains("cross-node delegation enabled"),
        "demo delegate did not configure the fleet:\n{stdout}"
    );
    assert!(
        stdout.contains(&parent_reply),
        "parent did not finish after live background-child inspection:\n{stdout}"
    );
    assert!(
        wait_port_free(port, Duration::from_secs(15))
            && wait_port_free(worker_port, Duration::from_secs(15)),
        "demo left a paired server process listening"
    );

    let parent_requests = mock
        .captured_chat_requests()
        .into_iter()
        .filter(|request| request_contains_role_text(request, "user", parent_prompt))
        .collect::<Vec<_>>();
    let inspected_request = parent_requests
        .iter()
        .rev()
        .find(|request| {
            request_tool_result_values(request)
                .iter()
                .any(|result| result.get("transcript").is_some())
        })
        .context("parent never received a read_subagent result")?;
    let inspection_results = request_tool_result_values(inspected_request);
    let spawn_ids = inspection_results
        .iter()
        .filter(|result| {
            result.get("await_mode").and_then(Value::as_str) == Some("background")
                && result.get("child_request_id").is_some()
        })
        .filter_map(|result| result.get("child_request_id").and_then(Value::as_str))
        .collect::<std::collections::HashSet<_>>();
    anyhow::ensure!(
        spawn_ids.len() == 2,
        "parent did not receive two distinct background spawn receipts: {inspection_results:?}"
    );
    let materialized_list = inspection_results
        .iter()
        .filter(|result| list_entries_are_materialized(result, 2))
        .last()
        .context("list_subagents never exposed both materialized children")?;
    let listed_ids = materialized_list["entries"]
        .as_array()
        .expect("validated entries")
        .iter()
        .filter_map(|entry| entry.get("child_request_id").and_then(Value::as_str))
        .collect::<std::collections::HashSet<_>>();
    anyhow::ensure!(
        listed_ids == spawn_ids,
        "list_subagents returned the wrong child set: listed={listed_ids:?} spawned={spawn_ids:?}"
    );
    let read_result = inspection_results
        .iter()
        .find(|result| result.get("transcript").is_some())
        .context("missing read_subagent result")?;
    anyhow::ensure!(
        read_result
            .get("terminal")
            .and_then(Value::as_bool)
            .is_some()
            && read_result.get("diagnostic").is_none_or(Value::is_null)
            && read_result
                .get("transcript")
                .and_then(Value::as_str)
                .is_some_and(|transcript| transcript.contains(worker_prompt_a)),
        "read_subagent did not return the child's materialized transcript: {read_result}"
    );
    let inspected_child_request_id = read_result
        .get("child_request_id")
        .and_then(Value::as_str)
        .context("read_subagent result omitted child_request_id")?;
    anyhow::ensure!(
        spawn_ids.contains(inspected_child_request_id),
        "read_subagent inspected an unowned child: {read_result}"
    );

    // The user-visible failure in #734 was `no_peer_claimed_spawn`. Prove the
    // opposite from both durable stores. First recover the exact parent request
    // and spawn tool-call lineage recorded on node A for each bridge.
    let coordinator_data_dir = home.join("data");
    let node_a = EmbeddedNode::builder()
        .data_path(&coordinator_data_dir)
        .with_storage_backend(StorageBackend::RocksDb)
        .build()
        .await
        .with_context(|| {
            format!(
                "opening coordinator store at {}",
                coordinator_data_dir.display()
            )
        })?;
    let response = node_a
        .execute(
            r#"{
                AgentToolCall(filter: { tool_name: { _eq: "spawn_subagent" } }) {
                    request_id
                    tool_call_id
                    child_request_id
                }
            }"#,
        )
        .await;
    anyhow::ensure!(
        !response.has_errors(),
        "querying coordinator spawn bridges failed: {:?}",
        response.errors
    );
    let bridge_rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(Value::as_array)
        .context("coordinator spawn bridge query omitted AgentToolCall rows")?;
    let mut expected_lineage = std::collections::HashMap::new();
    for bridge in bridge_rows {
        let Some(child_request_id) = bridge.get("child_request_id").and_then(Value::as_str) else {
            continue;
        };
        if !spawn_ids.contains(child_request_id) {
            continue;
        }
        let parent_request_id = bridge
            .get("request_id")
            .and_then(Value::as_str)
            .context("spawn bridge omitted request_id")?;
        let parent_tool_call_id = bridge
            .get("tool_call_id")
            .and_then(Value::as_str)
            .context("spawn bridge omitted tool_call_id")?;
        anyhow::ensure!(
            expected_lineage
                .insert(
                    child_request_id.to_string(),
                    (
                        parent_request_id.to_string(),
                        parent_tool_call_id.to_string()
                    ),
                )
                .is_none(),
            "coordinator persisted duplicate spawn bridges for child {child_request_id}"
        );
    }
    anyhow::ensure!(
        expected_lineage.len() == spawn_ids.len(),
        "coordinator did not persist an exact bridge for every spawned child: expected={spawn_ids:?} lineage={expected_lineage:?}"
    );
    drop(node_a);

    // Every spawned child must materialize and complete on node B with lineage
    // exactly matching its node-A bridge—not merely non-empty parent fields.
    let data_dir = home.join("node-b").join("data");
    let node_b = EmbeddedNode::builder()
        .data_path(&data_dir)
        .with_storage_backend(StorageBackend::RocksDb)
        .build()
        .await
        .with_context(|| format!("opening worker store at {}", data_dir.display()))?;
    for child_request_id in &spawn_ids {
        let escaped_child_request_id = escape_graphql_string(child_request_id);
        let response = node_b
            .execute(&format!(
                r#"{{
                    AgentRequest(
                        filter: {{ request_id: {{ _eq: "{escaped_child_request_id}" }} }},
                        order: {{ created_at: DESC }},
                        limit: 1
                    ) {{
                        request_id
                        agent_did
                        status
                        lifecycle_state
                        caused_by_parent_request_id
                        caused_by_parent_tool_call_id
                    }}
                }}"#
            ))
            .await;
        anyhow::ensure!(
            !response.has_errors(),
            "querying worker child {child_request_id} failed: {:?}",
            response.errors
        );
        let child = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .with_context(|| {
                format!("paired worker never materialized remote child {child_request_id}")
            })?;
        let (expected_parent_request_id, expected_parent_tool_call_id) = expected_lineage
            .get(*child_request_id)
            .context("spawned child had no coordinator lineage witness")?;
        anyhow::ensure!(
            child.get("lifecycle_state").and_then(Value::as_str) == Some("completed"),
            "remote worker child {child_request_id} did not complete: {child}"
        );
        anyhow::ensure!(
            child
                .get("caused_by_parent_request_id")
                .and_then(Value::as_str)
                == Some(expected_parent_request_id.as_str())
                && child
                    .get("caused_by_parent_tool_call_id")
                    .and_then(Value::as_str)
                    == Some(expected_parent_tool_call_id.as_str()),
            "remote worker child {child_request_id} has the wrong parent lineage: expected request={expected_parent_request_id} tool={expected_parent_tool_call_id} child={child}"
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_reconfigure_swaps_the_backend_live() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("demo-home");

    let reply_a = format!("from-backend-a-{}", Uuid::new_v4().simple());
    let model_a = format!("model-a-{}", Uuid::new_v4().simple());
    let mock_a = MockChatEndpoint::start(&model_a, &reply_a)?;

    let reply_b = format!("from-backend-b-{}", Uuid::new_v4().simple());
    let model_b = format!("model-b-{}", Uuid::new_v4().simple());
    let mock_b = MockChatEndpoint::start(&model_b, &reply_b)?;

    let port = allocate_port()?;

    // Chat on backend A, reconfigure to backend B via the custom-URL picker path
    // (choice 3 → URL → model), then chat again and confirm the swap took hold.
    let input = format!(
        "chat\nhello A\n/back\nreconfigure\n3\n{}\n{}\nchat\nhello B\n/back\ndown\n",
        mock_b.endpoint(),
        model_b,
    );
    let output = run_demo(
        tempdir.path(),
        &[
            "--home",
            home.to_str().unwrap(),
            "--inference-url",
            mock_a.endpoint(),
            "--model",
            &model_a,
            "--http-port",
            &port.to_string(),
        ],
        &input,
    )?;
    let stdout = require_success(&output)?;

    assert!(
        stdout.contains(&reply_a),
        "expected a reply from backend A before reconfigure, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Reconfigured"),
        "expected the reconfigure confirmation, got:\n{stdout}"
    );
    assert!(
        stdout.contains(&reply_b),
        "expected a reply from backend B after reconfigure, got:\n{stdout}"
    );
    assert!(
        wait_port_free(port, Duration::from_secs(15)),
        "demo left an orphaned server listening on port {port}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_resume_reuses_the_saved_agent() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("demo-home");

    let reply = format!("resume-reply-{}", Uuid::new_v4().simple());
    let model = format!("resume-model-{}", Uuid::new_v4().simple());
    let mock = MockChatEndpoint::start(&model, &reply)?;

    let args_for = |port: &str| {
        vec![
            "--home".to_string(),
            home.to_str().unwrap().to_string(),
            "--inference-url".to_string(),
            mock.endpoint().to_string(),
            "--model".to_string(),
            model.clone(),
            "--http-port".to_string(),
            port.to_string(),
        ]
    };

    // First run: set up and persist, then exit.
    let port1 = allocate_port()?;
    let first_args = args_for(&port1.to_string());
    let first_refs: Vec<&str> = first_args.iter().map(String::as_str).collect();
    let first = run_demo(tempdir.path(), &first_refs, "down\n")?;
    require_success(&first)?;
    let did_after_first = read_agent_did(&home)?;

    // Second run: same home reuses the saved agent rather than re-initializing.
    let port2 = allocate_port()?;
    let second_args = args_for(&port2.to_string());
    let second_refs: Vec<&str> = second_args.iter().map(String::as_str).collect();
    let second = run_demo(tempdir.path(), &second_refs, "status\ndown\n")?;
    let stdout = require_success(&second)?;

    assert!(
        stdout.contains("Resuming your demo agent"),
        "expected the second run to resume the saved agent, got:\n{stdout}"
    );
    let did_after_second = read_agent_did(&home)?;
    assert_eq!(
        did_after_first, did_after_second,
        "resume must keep the same agent DID"
    );
    Ok(())
}

fn read_agent_did(home: &Path) -> Result<String> {
    let raw = fs::read_to_string(home.join("init.json"))
        .with_context(|| format!("reading {}", home.join("init.json").display()))?;
    let json: serde_json::Value = serde_json::from_str(&raw)?;
    json.get("agent_did")
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("init.json missing agent_did"))
}
