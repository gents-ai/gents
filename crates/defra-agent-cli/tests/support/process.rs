use std::io::BufRead;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use super::fs::read_captured_log;

pub struct ServeProcess {
    pub child: Child,
    pub stdout_log: Option<tempfile::NamedTempFile>,
    pub stderr_log: Option<tempfile::NamedTempFile>,
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl ServeProcess {
    pub fn new(child: Child) -> Self {
        Self {
            child,
            stdout_log: None,
            stderr_log: None,
        }
    }

    pub fn with_logs(
        child: Child,
        stdout_log: tempfile::NamedTempFile,
        stderr_log: tempfile::NamedTempFile,
    ) -> Self {
        Self {
            child,
            stdout_log: Some(stdout_log),
            stderr_log: Some(stderr_log),
        }
    }

    pub fn captured_output(&self) -> Result<(String, String)> {
        Ok((
            read_captured_log(self.stdout_log.as_ref())?,
            read_captured_log(self.stderr_log.as_ref())?,
        ))
    }
}

pub fn cli_bin() -> &'static str {
    env!("CARGO_BIN_EXE_defra-agent")
}

pub fn desktop_bin() -> Result<PathBuf> {
    let cli_path = Path::new(cli_bin());
    let binary_name = format!("defra-agent-desktop{}", std::env::consts::EXE_SUFFIX);
    let desktop_path = cli_path
        .parent()
        .ok_or_else(|| anyhow!("unable to resolve defra-agent binary directory"))?
        .join(binary_name);

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| anyhow!("unable to resolve workspace root from CARGO_MANIFEST_DIR"))?;
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args([
            "build",
            "-p",
            "defra-agent-desktop",
            "--bin",
            "defra-agent-desktop",
        ])
        .status()
        .context("building defra-agent-desktop binary for demo e2e")?;
    if !status.success() {
        bail!("cargo build -p defra-agent-desktop --bin defra-agent-desktop failed");
    }
    Ok(desktop_path)
}

pub fn run_desktop_init_json(agent_home: &Path, desktop_home: &Path, label: &str) -> Result<Value> {
    let output = Command::new(desktop_bin()?)
        .env("RUST_LOG", "error")
        // CLI mocks (and their assertions) speak Chat Completions; force that
        // wire API so the Responses-API default doesn't break them.
        .env("DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS", "1")
        .arg("init")
        .arg("--agent-home")
        .arg(agent_home)
        .arg("--desktop-home")
        .arg(desktop_home)
        .arg("--label")
        .arg(label)
        .arg("--json")
        .output()
        .context("running defra-agent-desktop init")?;
    if !output.status.success() {
        bail!(
            "defra-agent-desktop init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice(&output.stdout).context("parsing JSON from defra-agent-desktop init")
}

pub fn run_init_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
    let mut command_args = vec!["init"];
    command_args.extend_from_slice(args);
    run_cli_json(home_dir, &command_args)
}

pub fn spawn_server(home_dir: &Path, port: u16) -> Result<ServeProcess> {
    spawn_server_with_env(home_dir, port, &[], &[])
}

/// The Codex shim is on by default and binds a fixed port, which parallel
/// test servers would fight over. Disable it unless the test configures the
/// shim explicitly.
fn codex_shim_opt_out(extra_args: &[&str]) -> &'static [&'static str] {
    if extra_args.iter().any(|arg| arg.starts_with("--codex-shim")) {
        &[]
    } else {
        &["--no-codex-shim"]
    }
}

pub fn spawn_server_with_ready_json(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(ServeProcess, Value)> {
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        // CLI mocks (and their assertions) speak Chat Completions; force that
        // wire API so the Responses-API default doesn't break them.
        .env("DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS", "1")
        .current_dir(home_dir)
        .arg("server")
        .arg("--http-port")
        .arg(port.to_string())
        .args(codex_shim_opt_out(extra_args))
        .args(extra_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in envs {
        command.env(name, value);
    }
    let mut child = command.spawn().context("spawning defra-agent server")?;
    let stdout = child
        .stdout
        .take()
        .context("capturing defra-agent server stdout")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stdout);
        let mut buffer = String::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Err((
                        anyhow!("server stdout closed before readiness JSON was emitted"),
                        buffer,
                    )));
                    break;
                }
                Ok(_) => {
                    buffer.push_str(&line);
                    if let Ok(value) = serde_json::from_str::<Value>(&buffer) {
                        let _ = tx.send(Ok(value));
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err((anyhow!("reading server stdout: {error}"), buffer)));
                    break;
                }
            }
        }
    });

    let readiness = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(value)) => value,
        Ok(Err((error, captured_stdout))) => {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .context("waiting for failed defra-agent server process")?;
            return Err(anyhow!(
                "{error}\nstdout:\n{}\nstderr:\n{}",
                captured_stdout,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Err(_) => {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .context("waiting for timed out defra-agent server process")?;
            bail!(
                "timed out waiting for defra-agent server readiness JSON\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    };

    Ok((ServeProcess::new(child), readiness))
}

pub fn spawn_server_with_env(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> Result<ServeProcess> {
    let stdout_log = tempfile::NamedTempFile::new().context("creating defra-agent stdout log")?;
    let stderr_log = tempfile::NamedTempFile::new().context("creating defra-agent stderr log")?;
    let stdout = stdout_log
        .reopen()
        .context("opening defra-agent stdout log")?;
    let stderr = stderr_log
        .reopen()
        .context("opening defra-agent stderr log")?;
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        // CLI mocks (and their assertions) speak Chat Completions; force that
        // wire API so the Responses-API default doesn't break them.
        .env("DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS", "1")
        .current_dir(home_dir)
        .arg("server")
        .arg("--http-port")
        .arg(port.to_string())
        .args(codex_shim_opt_out(extra_args))
        .args(extra_args)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (name, value) in envs {
        command.env(name, value);
    }
    let child = command.spawn().context("spawning defra-agent server")?;
    Ok(ServeProcess::with_logs(child, stdout_log, stderr_log))
}

pub fn wait_for_port(port: u16, serve: &mut ServeProcess) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        if let Some(status) = serve
            .child
            .try_wait()
            .context("checking serve child status")?
        {
            let (stdout, stderr) = serve.captured_output()?;
            bail!(
                "serve exited before becoming ready: {status}\nstdout:\n{}\nstderr:\n{}",
                stdout,
                stderr
            );
        }
        if Instant::now() >= deadline {
            let (stdout, stderr) = serve.captured_output()?;
            bail!(
                "timed out waiting for defra-agent server on port {port}\nstdout:\n{}\nstderr:\n{}",
                stdout,
                stderr
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

pub fn spawn_cli(home_dir: &Path, args: &[&str]) -> Result<Child> {
    Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        // CLI mocks (and their assertions) speak Chat Completions; force that
        // wire API so the Responses-API default doesn't break them.
        .env("DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS", "1")
        .current_dir(home_dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning defra-agent {}", args.join(" ")))
}

pub fn run_cli_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        // CLI mocks (and their assertions) speak Chat Completions; force that
        // wire API so the Responses-API default doesn't break them.
        .env("DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS", "1")
        .current_dir(home_dir)
        .args(args)
        .output()
        .with_context(|| format!("running defra-agent {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "defra-agent {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parsing JSON from defra-agent {}", args.join(" ")))
}

pub fn run_cli_text(home_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        // CLI mocks (and their assertions) speak Chat Completions; force that
        // wire API so the Responses-API default doesn't break them.
        .env("DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS", "1")
        .args(args)
        .output()
        .with_context(|| format!("running defra-agent {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "defra-agent {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout)
        .with_context(|| format!("parsing stdout from defra-agent {}", args.join(" ")))
}

pub fn run_cli_failure_stderr(home_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        // CLI mocks (and their assertions) speak Chat Completions; force that
        // wire API so the Responses-API default doesn't break them.
        .env("DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS", "1")
        .args(args)
        .output()
        .with_context(|| format!("running defra-agent {}", args.join(" ")))?;
    if output.status.success() {
        bail!(
            "expected defra-agent {} to fail\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stderr)
        .with_context(|| format!("parsing stderr from defra-agent {}", args.join(" ")))
}

pub fn run_cli_failure_stdout_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        // CLI mocks (and their assertions) speak Chat Completions; force that
        // wire API so the Responses-API default doesn't break them.
        .env("DEFRA_AGENT_OPENAI_CHAT_COMPLETIONS", "1")
        .current_dir(home_dir)
        .args(args)
        .output()
        .with_context(|| format!("running defra-agent {}", args.join(" ")))?;
    if output.status.success() {
        bail!(
            "expected defra-agent {} to fail\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "parsing failure JSON from defra-agent {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
