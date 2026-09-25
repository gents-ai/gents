use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use super::fs::read_captured_log;
use super::ports;

/// Bound on replacement ports tried after a lost bind race.
const MAX_STOLEN_PORT_ATTEMPTS: u32 = 3;

/// Positive evidence that the child's preflight bind found `addr` already
/// taken. The context string alone is not enough: `serve.rs` attaches it to
/// every bind failure, so descriptor exhaustion or a permission error would
/// otherwise be misread as a stolen port and silently retried. Unix renders
/// `io::ErrorKind::AddrInUse` as this text on both macOS and Linux.
pub fn is_address_in_use(text: &str, addr: &str) -> bool {
    text.contains(&format!("embedded HTTP listener cannot bind to {addr}"))
        && text.contains("Address already in use")
}

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

/// Spawn a `gents server` and wait for its readiness JSON.
///
/// Never replaces a raced-away port: a caller holding this signature cannot
/// observe a port change, so moving the server would strand it on a port
/// nothing is listening on. Use `spawn_server_with_ready_json_recovering`
/// to opt into recovery and take the resulting port back.
pub fn spawn_server_with_ready_json(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(ServeProcess, Value)> {
    // Recovery is off, so the bound port is always the requested one.
    let (serve, _port, value) =
        spawn_server_with_ready_json_inner(home_dir, port, extra_args, envs, false)?;
    Ok((serve, value))
}

/// `spawn_server_with_ready_json`, but if the child's preflight bind finds
/// the requested port already taken and this process reserved that port,
/// release it, allocate another and respawn.
///
/// Returns the port the child actually bound, which may differ from `port`.
/// Derive `graphql_url` and every other address from the returned port,
/// after this call.
pub fn spawn_server_with_ready_json_recovering(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(ServeProcess, u16, Value)> {
    spawn_server_with_ready_json_inner(home_dir, port, extra_args, envs, true)
}

/// Single owner of the readiness wait and the port-replacement policy.
/// `recover_stolen_port` gates replacement by zeroing the attempt budget, so
/// the non-recovering caller keeps reporting the child's own bind failure.
/// The 30s budget spans the whole sequence, replacements included.
fn spawn_server_with_ready_json_inner(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    recover_stolen_port: bool,
) -> Result<(ServeProcess, u16, Value)> {
    let mut port = port;
    let mut attempts_left = if recover_stolen_port {
        MAX_STOLEN_PORT_ATTEMPTS
    } else {
        0
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    'attempt: loop {
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
        configure_foreground_server_env(&mut command, envs);
        let child = command.spawn().context("spawning gents server")?;
        let mut serve = ServeProcess::with_logs(child, stdout_log, stderr_log);

        loop {
            let stdout_so_far = read_captured_log(serve.stdout_log.as_ref())?;
            if let Some(value) = server_readiness_json(&stdout_so_far) {
                return Ok((serve, port, value));
            }
            let exited = serve
                .child
                .try_wait()
                .context("checking serve child status")?;
            let timed_out = Instant::now() >= deadline;
            if exited.is_some() || timed_out {
                let (stdout, stderr) = serve.captured_output()?;
                let addr = format!("127.0.0.1:{port}");
                // A replacement spawned past the deadline cannot reach
                // readiness before the caller gives up, so recovery is only
                // worth attempting while budget remains.
                let recoverable = !timed_out
                    && attempts_left > 0
                    && ports::is_reserved(port)
                    && (is_address_in_use(&stdout, &addr) || is_address_in_use(&stderr, &addr));
                if recoverable {
                    attempts_left -= 1;
                    ports::release(port);
                    port = ports::allocate_port()?;
                    // Reap the losing child before its replacement starts, so
                    // two servers never hold the same home.
                    drop(serve);
                    continue 'attempt;
                }
                if let Some(status) = exited {
                    bail!(
                        "server exited before emitting readiness JSON ({status})\nstdout:\n{}\nstderr:\n{}",
                        stdout,
                        stderr
                    );
                }
                bail!(
                    "timed out waiting for gents server readiness JSON\nstdout:\n{}\nstderr:\n{}",
                    stdout,
                    stderr
                );
            }
            thread::sleep(Duration::from_millis(100));
        }
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
    configure_foreground_server_env(&mut command, envs);
    let child = command.spawn().context("spawning gents server")?;
    Ok(ServeProcess::with_logs(child, stdout_log, stderr_log))
}

/// Foreground test servers must retain ordinary CLI stderr even when the test
/// runner itself was launched from a native service environment. Explicit
/// fixture overrides are applied afterward so native logging remains testable.
pub(crate) fn configure_foreground_server_env(command: &mut Command, envs: &[(&str, &str)]) {
    command.env_remove("GENTS_SYSTEM_LOG");
    for (name, value) in envs {
        command.env(name, value);
    }
}

/// Wait until something accepts TCP on `port`.
///
/// Any listener satisfies this, so a success is not evidence that `serve`
/// owns the port: if an unrelated process holds it, this returns as soon as
/// that process answers. Use `spawn_server_with_ready_json_recovering` when
/// ownership matters -- the child publishes its readiness JSON only after an
/// instance-specific probe confirms the listener is its own.
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
    run_cli_json_with_env(home_dir, args, &[])
}

pub fn run_cli_json_with_env(
    home_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<Value> {
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .current_dir(home_dir)
        .args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    let output = command
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
    run_cli_failure_stderr_with_env(home_dir, args, &[])
}

pub fn run_cli_failure_stderr_with_env(
    home_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<String> {
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    let output = command
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
