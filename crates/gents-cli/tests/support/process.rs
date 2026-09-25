use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use super::fs::read_captured_log;
use super::ports;

/// Bound retries for lost-port-race recovery (`wait_for_port_recovering`,
/// `spawn_server_with_ready_json`) -- never a blind "try the same thing
/// again": every retry changes the port, and only after positive evidence
/// (see `is_port_conflict_diagnostic`) that the previous one was raced away.
const MAX_STOLEN_PORT_ATTEMPTS: u32 = 3;

/// The gap between `ports::allocate_port`'s advisory reservation and a
/// spawned `gents server`'s own bind is a real TOCTOU that cannot be closed
/// from this side: the bind happens inside the pinned DefraDB dependency
/// (`defra_node::EmbeddedNode::build`, vendored `crates/defra-node/src/lib.rs`
/// around line 1232), which exposes neither a pre-bound-listener hook nor a
/// way to read back the address it eventually binds. `serve.rs`'s own
/// preflight check fails closed with this diagnostic when something else
/// already owns the target address; treat it as positive evidence a
/// reservation was raced away, never as a generic startup failure to paper
/// over. Deliberately narrower than also matching the later
/// "did not become ready" timeout in the same file, which can fire from
/// ordinary host slowness with no port theft involved.
fn is_port_conflict_diagnostic(text: &str) -> bool {
    text.contains("embedded HTTP listener cannot bind to")
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
/// Never recovers from a lost port race: a caller holding only this
/// signature cannot observe a port change, so moving the server to a fresh
/// port would strand it waiting on a port nothing is listening on, replacing
/// the child's precise bind diagnostic with an opaque timeout. Callers that
/// want recovery must take the port back explicitly via
/// `spawn_server_with_ready_json_recovering`.
pub fn spawn_server_with_ready_json(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(ServeProcess, Value)> {
    // Recovery is off, so the returned port is always the requested one.
    let (serve, _port, value) =
        spawn_server_with_ready_json_inner(home_dir, port, extra_args, envs, false)?;
    Ok((serve, value))
}

/// `spawn_server_with_ready_json` plus recovery from a lost port race: when
/// the child fails its preflight bind on a port this process reserved (see
/// `is_port_conflict_diagnostic`), release that reservation, allocate a fresh
/// port and respawn, at most `MAX_STOLEN_PORT_ATTEMPTS` times.
///
/// Returns the port the server actually bound, which may differ from `port`.
/// A recovered port invalidates every value derived from the requested one:
/// derive `graphql_url` and any other address from the returned port, after
/// this call, never from the port passed in. Fixtures that build those URLs
/// before spawning are why adoption is a deliberate per-site move rather than
/// a mechanical sweep -- a blanket destructure that kept using the requested
/// port would compile while leaving recovery silently half-wired.
pub fn spawn_server_with_ready_json_recovering(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(ServeProcess, u16, Value)> {
    spawn_server_with_ready_json_inner(home_dir, port, extra_args, envs, true)
}

/// Single implementation behind both readiness-JSON spawners, so the
/// recovery policy exists in exactly one place. `recover_stolen_port` gates
/// it by starting the attempt budget at zero, which keeps the non-recovering
/// caller's failures byte-identical to what it reported before recovery
/// existed.
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

        let deadline = Instant::now() + Duration::from_secs(30);
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
                let recoverable = attempts_left > 0
                    && ports::is_reserved(port)
                    && (is_port_conflict_diagnostic(&stdout)
                        || is_port_conflict_diagnostic(&stderr));
                if recoverable {
                    attempts_left -= 1;
                    ports::release(port);
                    port = ports::allocate_port()?;
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

/// Like `wait_for_port`, but when the wait fails specifically because an
/// unrelated process won the race for an `allocate_port()`-reserved port
/// (see `is_port_conflict_diagnostic`), release the stolen reservation,
/// allocate a fresh port, and respawn with `respawn` instead of failing the
/// caller's test on a known-recoverable race. A port the caller bound itself
/// (never registered with `ports::allocate_port`) is never recovered, so
/// fixtures that deliberately occupy a port to exercise bind-conflict
/// handling keep observing that failure unchanged.
///
/// Returns the port the server is actually listening on, which may differ
/// from `port`. A recovered port invalidates every value derived from the
/// requested one: derive `graphql_url` and any other address from the
/// returned port, after this call, never from the port passed in.
pub fn wait_for_port_recovering(
    mut port: u16,
    serve: &mut ServeProcess,
    respawn: impl Fn(u16) -> Result<ServeProcess>,
) -> Result<u16> {
    let mut attempts_left = MAX_STOLEN_PORT_ATTEMPTS;
    loop {
        match wait_for_port(port, serve) {
            Ok(()) => return Ok(port),
            Err(error) => {
                let (stdout, stderr) = serve.captured_output()?;
                let recoverable = attempts_left > 0
                    && ports::is_reserved(port)
                    && (is_port_conflict_diagnostic(&stdout)
                        || is_port_conflict_diagnostic(&stderr));
                if !recoverable {
                    return Err(error);
                }
                attempts_left -= 1;
                ports::release(port);
                port = ports::allocate_port()?;
                *serve = respawn(port)?;
            }
        }
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
