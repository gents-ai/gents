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

/// #734 regression: the shipped `pair` -> `delegate` path must carry the
/// coordinator's background spawn bridge to node B, where the worker claims
/// and completes it. This uses the real two-process demo and its document-
/// driven pairing/config commands; only inference is hermetic.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_pair_delegate_materializes_remote_worker() -> Result<()> {
    use support::mocks::fake_llm::{ChatAction, FakeLlm};

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("demo-home");
    let model = format!("demo-delegate-model-{}", Uuid::new_v4().simple());
    let parent_prompt = "Run the hermetic demo delegation check.";
    let worker_prompt = "Return the hermetic remote-worker proof token.";
    let parent_reply = format!("demo-parent-finished-{}", Uuid::new_v4().simple());
    let worker_reply = format!("demo-worker-finished-{}", Uuid::new_v4().simple());

    let mock = FakeLlm::start(&model, None, {
        let parent_reply = parent_reply.clone();
        let worker_reply = worker_reply.clone();
        Arc::new(move |request: &Value| {
            if request_contains_role_text(request, "user", worker_prompt) {
                return ChatAction::Sse(completion_text_sse(&worker_reply));
            }
            if request_contains_role_text(request, "user", parent_prompt) {
                if request_has_tool_result_message(request) {
                    // Keep both demo daemons alive while the background bridge
                    // replicates, is claimed, and the immediate worker answer
                    // persists on node B.
                    return ChatAction::DelayThenSse(
                        Duration::from_secs(20),
                        completion_text_sse(&parent_reply),
                    );
                }
                let args = json!({
                    "name": "worker",
                    "prompt": worker_prompt,
                    "await_mode": "background"
                })
                .to_string();
                return ChatAction::Sse(tool_call_sse("spawn_subagent", &args));
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
        "parent did not finish after the background spawn:\n{stdout}"
    );
    assert!(
        wait_port_free(port, Duration::from_secs(15))
            && wait_port_free(worker_port, Duration::from_secs(15)),
        "demo left a paired server process listening"
    );

    // The user-visible failure in #734 was `no_peer_claimed_spawn`. Prove the
    // opposite from node B's durable store: the targeted child materialized
    // there and reached the terminal completed state.
    let data_dir = home.join("node-b").join("data");
    let node_b = EmbeddedNode::builder()
        .data_path(&data_dir)
        .with_storage_backend(StorageBackend::RocksDb)
        .build()
        .await
        .with_context(|| format!("opening worker store at {}", data_dir.display()))?;
    let escaped_prompt = escape_graphql_string(worker_prompt);
    let response = node_b
        .execute(&format!(
            r#"{{
                AgentRequest(
                    filter: {{ content: {{ _eq: "{escaped_prompt}" }} }},
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
        "querying worker child failed: {:?}",
        response.errors
    );
    let child = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .context("paired worker never materialized the remote child")?;
    anyhow::ensure!(
        child.get("lifecycle_state").and_then(Value::as_str) == Some("completed"),
        "remote worker child did not complete: {child}"
    );
    anyhow::ensure!(
        child
            .get("caused_by_parent_request_id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
            && child
                .get("caused_by_parent_tool_call_id")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
        "remote worker child lost its parent lineage: {child}"
    );

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
