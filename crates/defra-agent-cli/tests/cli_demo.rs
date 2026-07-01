//! Integration coverage for `defra-agent demo`: the interactive shell is driven
//! non-interactively (piped stdin) against a test-only mock OpenAI endpoint. The
//! *shipped* demo bundles no mock — these assert node bring-up, streaming chat,
//! the seeded skills, live backend `reconfigure`, resume, and clean teardown.

mod support;
use support::*;

use std::fs;
use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
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
