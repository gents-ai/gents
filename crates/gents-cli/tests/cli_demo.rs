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

fn run_demo(tmp_home: &Path, args: &[&str], input: &str) -> Result<std::process::Output> {
    run_demo_with_env(tmp_home, args, input, &[])
}

fn run_demo_with_env(
    tmp_home: &Path,
    args: &[&str],
    input: &str,
    env: &[(&str, &Path)],
) -> Result<std::process::Output> {
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", tmp_home)
        .env("RUST_LOG", "error")
        .arg("demo")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().context("spawning gents demo")?;
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
    child.wait_with_output().context("waiting for gents demo")
}

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
            "gents demo exited non-zero\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[test]
fn demo_has_no_legacy_pairing_authority() {
    let fleet = include_str!("../src/commands/demo/fleet.rs");
    let shell = include_str!("../src/commands/demo/shell.rs");
    for forbidden in [
        "p2p\", \"network\", \"grant",
        "p2p\", \"pairings\", \"invite",
        "p2p\", \"pairings\", \"join",
        "DataPlanePairingDesired",
        "SOURCE_OPERATOR",
        "--status-endpoint",
    ] {
        assert!(
            !fleet.contains(forbidden),
            "interactive demo must not retain legacy authority token {forbidden:?}"
        );
    }
    assert!(!shell.contains("\"pair\" =>"));
    assert!(!shell.contains("\"delegate\" =>"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_single_node_chats_lists_skills_and_shuts_down_clean() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("demo-home");

    let reply = format!("demo-reply-{}", Uuid::new_v4().simple());
    let model = format!("demo-model-{}", Uuid::new_v4().simple());
    let mock = MockChatEndpoint::start(&model, &reply)?;
    let port = allocate_port()?;

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

    assert!(stdout.contains("node A: live"));
    assert!(stdout.contains("summarize") && stdout.contains("fleet-guide"));
    assert!(
        stdout.contains(&reply),
        "demo omitted mock reply\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Stopped."));
    assert!(
        wait_port_free(port, Duration::from_secs(15)),
        "demo left an orphaned server listening on port {port}"
    );
    assert!(home.join("init.json").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_desktop_flag_launches_without_importing_status_authority() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("demo-home");
    let desktop_log = tempdir.path().join("desktop.log");
    let desktop_bin = tempdir.path().join("fake-desktop");
    fs::write(
        &desktop_bin,
        r#"#!/bin/sh
if [ "$#" -eq 0 ]; then
  printf 'launch\n' >> "$FAKE_DESKTOP_LOG"
else
  printf 'unexpected:%s\n' "$*" >> "$FAKE_DESKTOP_LOG"
fi
"#,
    )?;
    fs::set_permissions(&desktop_bin, fs::Permissions::from_mode(0o755))?;

    let model = format!("demo-desktop-model-{}", Uuid::new_v4().simple());
    let mock = MockChatEndpoint::start(&model, "unused")?;
    let port = allocate_port()?;
    let output = run_demo_with_env(
        tempdir.path(),
        &[
            "--home",
            home.to_str().unwrap(),
            "--inference-url",
            mock.endpoint(),
            "--model",
            &model,
            "--desktop",
            "--http-port",
            &port.to_string(),
        ],
        "down\n",
        &[
            ("GENTS_DESKTOP_BIN", &desktop_bin),
            ("FAKE_DESKTOP_LOG", &desktop_log),
        ],
    )?;
    let stdout = require_success(&output)?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let launches = loop {
        let launches = fs::read_to_string(&desktop_log).unwrap_or_default();
        if launches.contains("launch") || Instant::now() >= deadline {
            break launches;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(stdout.contains("Desktop app launched"));
    assert!(stdout.contains("request authenticated enrollment"));
    assert_eq!(launches, "launch\n");
    assert!(
        wait_port_free(port, Duration::from_secs(15)),
        "desktop demo left an orphaned server listening on port {port}"
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
        "demo omitted first backend reply\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Reconfigured"));
    assert!(
        stdout.contains(&reply_b),
        "demo omitted second backend reply\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(wait_port_free(port, Duration::from_secs(15)));
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

    let port1 = allocate_port()?;
    let first_args = args_for(&port1.to_string());
    let first_refs: Vec<&str> = first_args.iter().map(String::as_str).collect();
    require_success(&run_demo(tempdir.path(), &first_refs, "down\n")?)?;
    let did_after_first = read_agent_did(&home)?;

    let port2 = allocate_port()?;
    let second_args = args_for(&port2.to_string());
    let second_refs: Vec<&str> = second_args.iter().map(String::as_str).collect();
    let stdout = require_success(&run_demo(tempdir.path(), &second_refs, "status\ndown\n")?)?;

    assert!(stdout.contains("Resuming your demo agent"));
    assert_eq!(did_after_first, read_agent_did(&home)?);
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
