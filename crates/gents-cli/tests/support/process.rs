use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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

    /// Run a fallible future and, on failure, attach the server's captured
    /// stdout/stderr to the error chain (the original failure stays as the
    /// cause) so mid-test connection failures are diagnosable (#1041). A
    /// failure to read the capture files is reported in place of the logs
    /// rather than silently rendering as an empty (silent-looking) server.
    pub async fn capturing<T>(
        &self,
        fut: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        match fut.await {
            Ok(value) => Ok(value),
            Err(error) => {
                let (stdout, stderr) = self
                    .captured_output()
                    .unwrap_or_else(|error| (format!("<capture failed: {error}>"), String::new()));
                Err(error.context(format!(
                    "server stdout:\n{stdout}\nserver stderr:\n{stderr}"
                )))
            }
        }
    }
}

pub fn cli_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gents")
}

pub fn desktop_bin() -> Result<PathBuf> {
    let cli_path = Path::new(cli_bin());
    let binary_name = format!("gents-desktop{}", std::env::consts::EXE_SUFFIX);
    let desktop_path = cli_path
        .parent()
        .ok_or_else(|| anyhow!("unable to resolve gents binary directory"))?
        .join(binary_name);

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| anyhow!("unable to resolve workspace root from CARGO_MANIFEST_DIR"))?;
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args(["build", "-p", "gents-desktop", "--bin", "gents-desktop"])
        .status()
        .context("building gents-desktop binary for e2e")?;
    if !status.success() {
        bail!("cargo build -p gents-desktop --bin gents-desktop failed");
    }
    Ok(desktop_path)
}

pub fn run_desktop_init_json(agent_home: &Path, desktop_home: &Path, label: &str) -> Result<Value> {
    let output = Command::new(desktop_bin()?)
        .env("RUST_LOG", "error")
        .arg("init")
        .arg("--agent-home")
        .arg(agent_home)
        .arg("--desktop-home")
        .arg(desktop_home)
        .arg("--label")
        .arg(label)
        .arg("--json")
        .output()
        .context("running gents-desktop init")?;
    if !output.status.success() {
        bail!(
            "gents-desktop init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice(&output.stdout).context("parsing JSON from gents-desktop init")
}

pub fn run_init_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
    let mut command_args = vec!["init"];
    command_args.extend_from_slice(args);
    run_cli_json(home_dir, &command_args)
}

pub fn spawn_server(home_dir: &Path, port: u16) -> Result<ServeProcess> {
    spawn_server_with_env(home_dir, port, &[], &[])
}

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
    let stdout_log = tempfile::NamedTempFile::new().context("creating gents stdout log")?;
    let stderr_log = tempfile::NamedTempFile::new().context("creating gents stderr log")?;
    let stdout = stdout_log.reopen().context("opening gents stdout log")?;
    let stderr = stderr_log.reopen().context("opening gents stderr log")?;
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
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
    let child = command.spawn().context("spawning gents server")?;
    let mut serve = ServeProcess::with_logs(child, stdout_log, stderr_log);

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let stdout_so_far = read_captured_log(serve.stdout_log.as_ref())?;
        if let Some(value) = server_readiness_json(&stdout_so_far) {
            return Ok((serve, value));
        }
        if let Some(status) = serve
            .child
            .try_wait()
            .context("checking serve child status")?
        {
            let (stdout, stderr) = serve.captured_output()?;
            bail!(
                "server exited before emitting readiness JSON ({status})\nstdout:\n{}\nstderr:\n{}",
                stdout,
                stderr
            );
        }
        if Instant::now() >= deadline {
            let (stdout, stderr) = serve.captured_output()?;
            bail!(
                "timed out waiting for gents server readiness JSON\nstdout:\n{}\nstderr:\n{}",
                stdout,
                stderr
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn server_readiness_json(buffer: &str) -> Option<Value> {
    buffer
        .char_indices()
        .filter_map(|(index, ch)| (ch == '{').then_some(index))
        .find_map(|index| {
            let value = serde_json::Deserializer::from_str(&buffer[index..])
                .into_iter::<Value>()
                .next()
                .and_then(|result| result.ok())?;
            (value.get("status").and_then(Value::as_str) == Some("serving")).then_some(value)
        })
}

pub fn spawn_server_with_env(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> Result<ServeProcess> {
    let stdout_log = tempfile::NamedTempFile::new().context("creating gents stdout log")?;
    let stderr_log = tempfile::NamedTempFile::new().context("creating gents stderr log")?;
    let stdout = stdout_log.reopen().context("opening gents stdout log")?;
    let stderr = stderr_log.reopen().context("opening gents stderr log")?;
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
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
    let child = command.spawn().context("spawning gents server")?;
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
                "timed out waiting for gents server on port {port}\nstdout:\n{}\nstderr:\n{}",
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
        .current_dir(home_dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning gents {}", args.join(" ")))
}

pub fn run_cli_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .current_dir(home_dir)
        .args(args)
        .output()
        .with_context(|| format!("running gents {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "gents {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parsing JSON from gents {}", args.join(" ")))
}

pub fn run_cli_text(home_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .args(args)
        .output()
        .with_context(|| format!("running gents {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "gents {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout)
        .with_context(|| format!("parsing stdout from gents {}", args.join(" ")))
}

pub fn run_cli_failure_stderr(home_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .args(args)
        .output()
        .with_context(|| format!("running gents {}", args.join(" ")))?;
    if output.status.success() {
        bail!(
            "expected gents {} to fail\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stderr)
        .with_context(|| format!("parsing stderr from gents {}", args.join(" ")))
}

pub fn run_cli_failure_stdout_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .current_dir(home_dir)
        .args(args)
        .output()
        .with_context(|| format!("running gents {}", args.join(" ")))?;
    if output.status.success() {
        bail!(
            "expected gents {} to fail\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "parsing failure JSON from gents {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
