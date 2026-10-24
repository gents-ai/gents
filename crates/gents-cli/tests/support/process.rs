use crate::support::TestResult;
use gents_protocol::error_chain::ErrorChain;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::fs::read_captured_log;
use super::ports;

const MAX_STOLEN_PORT_ATTEMPTS: u32 = 3;

/// Positive evidence that the child's preflight bind found `addr` taken; the
/// context string alone would misread descriptor exhaustion or permission
/// errors as a stolen port. `io::ErrorKind::AddrInUse` renders as this text
/// on macOS and Linux.
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

    pub fn captured_output(&self) -> TestResult<(String, String)> {
        Ok((
            read_captured_log(self.stdout_log.as_ref())?,
            read_captured_log(self.stderr_log.as_ref())?,
        ))
    }

    pub async fn capturing<T>(
        &self,
        fut: impl std::future::Future<Output = TestResult<T>>,
    ) -> TestResult<T> {
        match fut.await {
            Ok(value) => Ok(value),
            Err(error) => {
                let (stdout, stderr) = self
                    .captured_output()
                    .unwrap_or_else(|error| (format!("<capture failed: {error}>"), String::new()));
                Err(format!(
                    "server stdout:\n{stdout}\nserver stderr:\n{stderr}: {}",
                    ErrorChain(&*error)
                )
                .into())
            }
        }
    }
}

pub fn cli_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gents")
}

pub fn desktop_bin() -> TestResult<PathBuf> {
    let cli_path = Path::new(cli_bin());
    let binary_name = format!("gents-desktop{}", std::env::consts::EXE_SUFFIX);
    let desktop_path = cli_path
        .parent()
        .ok_or_else(|| "unable to resolve gents binary directory".to_string())?
        .join(binary_name);

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "unable to resolve workspace root from CARGO_MANIFEST_DIR".to_string())?;
    let status = Command::new("cargo")
        .current_dir(workspace_root)
        .args(["build", "-p", "gents-desktop", "--bin", "gents-desktop"])
        .status()
        .map_err(|error| {
            format!(
                "building gents-desktop binary for e2e: {}",
                ErrorChain(&error)
            )
        })?;
    if !status.success() {
        return Err("cargo build -p gents-desktop --bin gents-desktop failed"
            .to_string()
            .into());
    }
    Ok(desktop_path)
}

pub fn run_desktop_init_json(
    node_home: &Path,
    desktop_home: &Path,
    label: &str,
) -> TestResult<Value> {
    let output = Command::new(desktop_bin()?)
        .env("RUST_LOG", "error")
        .arg("init")
        .arg("--node-home")
        .arg(node_home)
        .arg("--desktop-home")
        .arg(desktop_home)
        .arg("--label")
        .arg(label)
        .arg("--json")
        .output()
        .map_err(|error| format!("running gents-desktop init: {}", ErrorChain(&error)))?;
    if !output.status.success() {
        return Err(format!(
            "gents-desktop init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "parsing JSON from gents-desktop init: {}",
            ErrorChain(&error)
        )
        .into()
    })
}

pub fn run_init_json(home_dir: &Path, args: &[&str]) -> TestResult<Value> {
    let mut command_args = vec!["init", "--store-key-custody", "file"];
    command_args.extend_from_slice(args);
    run_cli_json(home_dir, &command_args)
}

pub fn spawn_server(home_dir: &Path, port: u16) -> TestResult<ServeProcess> {
    spawn_server_with_env(home_dir, port, &[], &[])
}

fn codex_shim_opt_out(extra_args: &[&str]) -> &'static [&'static str] {
    if extra_args.iter().any(|arg| arg.starts_with("--codex-shim")) {
        &[]
    } else {
        &["--no-codex-shim"]
    }
}

/// Spawn a `gents server` and wait for its readiness JSON. Never replaces a
/// raced-away port: a caller holding this signature cannot observe a port
/// change, so moving the server would strand it on a dead port.
pub fn spawn_server_with_ready_json(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> TestResult<(ServeProcess, Value)> {
    let (serve, _port, value) =
        spawn_server_with_ready_json_inner(home_dir, port, extra_args, envs, false, None)?;
    Ok((serve, value))
}

/// The fixture owns cleanup of these logs, including when startup fails before
/// a `ServeProcess` can be returned.
pub fn spawn_server_with_ready_json_in(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    log_dir: &Path,
) -> TestResult<(ServeProcess, Value)> {
    let (serve, _port, value) =
        spawn_server_with_ready_json_inner(home_dir, port, extra_args, envs, false, Some(log_dir))?;
    Ok((serve, value))
}

/// `spawn_server_with_ready_json`, but if the child's preflight bind finds
/// the requested port taken and this process reserved it, release it,
/// allocate another and respawn. Derive addresses from the returned port.
pub fn spawn_server_with_ready_json_recovering(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> TestResult<(ServeProcess, u16, Value)> {
    spawn_server_with_ready_json_inner(home_dir, port, extra_args, envs, true, None)
}

#[expect(
    clippy::disallowed_methods,
    reason = "synchronous harness polling a std child process and its log files"
)]
fn spawn_server_with_ready_json_inner(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
    recover_stolen_port: bool,
    log_dir: Option<&Path>,
) -> TestResult<(ServeProcess, u16, Value)> {
    let mut port = port;
    let mut attempts_left = if recover_stolen_port {
        MAX_STOLEN_PORT_ATTEMPTS
    } else {
        0
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    'attempt: loop {
        let stdout_log = server_log(log_dir, "stdout")?;
        let stderr_log = server_log(log_dir, "stderr")?;
        let stdout = stdout_log
            .reopen()
            .map_err(|error| format!("opening gents stdout log: {}", ErrorChain(&error)))?;
        let stderr = stderr_log
            .reopen()
            .map_err(|error| format!("opening gents stderr log: {}", ErrorChain(&error)))?;
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
        let child = command
            .spawn()
            .map_err(|error| format!("spawning gents server: {}", ErrorChain(&error)))?;
        let mut serve = ServeProcess::with_logs(child, stdout_log, stderr_log);

        loop {
            let stdout_so_far = read_captured_log(serve.stdout_log.as_ref())?;
            if let Some(value) = server_readiness_json(&stdout_so_far) {
                super::graphql::register_served_home(
                    &served_home_dir(home_dir, extra_args, envs),
                    port,
                )?;
                return Ok((serve, port, value));
            }
            let exited = serve
                .child
                .try_wait()
                .map_err(|error| format!("checking serve child status: {}", ErrorChain(&error)))?;
            let timed_out = Instant::now() >= deadline;
            if exited.is_some() || timed_out {
                let (stdout, stderr) = serve.captured_output()?;
                let addr = format!("127.0.0.1:{port}");
                let recoverable = !timed_out
                    && attempts_left > 0
                    && ports::is_reserved(port)
                    && (is_address_in_use(&stdout, &addr) || is_address_in_use(&stderr, &addr));
                if recoverable {
                    attempts_left -= 1;
                    ports::release(port);
                    port = ports::allocate_port()?;
                    drop(serve);
                    continue 'attempt;
                }
                if let Some(status) = exited {
                    return Err(format!(
                        "server exited before emitting readiness JSON ({status})\nstdout:\n{}\nstderr:\n{}",
                        stdout,
                        stderr
                    ).into());
                }
                return Err(format!(
                    "timed out waiting for gents server readiness JSON\nstdout:\n{}\nstderr:\n{}",
                    stdout, stderr
                )
                .into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

pub(crate) fn server_log(
    directory: Option<&Path>,
    stream: &str,
) -> TestResult<tempfile::NamedTempFile> {
    let mut log = match directory {
        Some(directory) => tempfile::Builder::new()
            .prefix(&format!("server.{stream}."))
            .suffix(".log")
            .tempfile_in(directory),
        None => tempfile::NamedTempFile::new(),
    }
    .map_err(|error| format!("creating gents {stream} log: {}", ErrorChain(&error)))?;
    log.disable_cleanup(directory.is_some());
    Ok(log)
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
) -> TestResult<ServeProcess> {
    let stdout_log = tempfile::NamedTempFile::new()
        .map_err(|error| format!("creating gents stdout log: {}", ErrorChain(&error)))?;
    let stderr_log = tempfile::NamedTempFile::new()
        .map_err(|error| format!("creating gents stderr log: {}", ErrorChain(&error)))?;
    let stdout = stdout_log
        .reopen()
        .map_err(|error| format!("opening gents stdout log: {}", ErrorChain(&error)))?;
    let stderr = stderr_log
        .reopen()
        .map_err(|error| format!("opening gents stderr log: {}", ErrorChain(&error)))?;
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
    let child = command
        .spawn()
        .map_err(|error| format!("spawning gents server: {}", ErrorChain(&error)))?;
    super::graphql::register_served_home(&served_home_dir(home_dir, extra_args, envs), port)?;
    Ok(ServeProcess::with_logs(child, stdout_log, stderr_log))
}

fn served_home_dir(home_dir: &Path, extra_args: &[&str], envs: &[(&str, &str)]) -> PathBuf {
    if let Some(index) = extra_args.iter().position(|arg| *arg == "--home") {
        if let Some(home) = extra_args.get(index + 1) {
            return PathBuf::from(home);
        }
    }
    if let Some(home) = extra_args
        .iter()
        .find_map(|arg| arg.strip_prefix("--home="))
    {
        return PathBuf::from(home);
    }
    if let Some((_, home)) = envs.iter().find(|(name, _)| *name == "GENTS_HOME") {
        return PathBuf::from(home);
    }
    home_dir.join(".gents")
}

/// Foreground test servers must retain ordinary CLI stderr even when the test
/// runner was launched from a native service environment; explicit fixture
/// overrides are applied afterward so native logging remains testable.
pub(crate) fn configure_foreground_server_env(command: &mut Command, envs: &[(&str, &str)]) {
    command.env_remove("GENTS_SYSTEM_LOG");
    for (name, value) in envs {
        command.env(name, value);
    }
}

/// Wait until something accepts TCP on `port`; an unrelated process holding
/// the port also satisfies this, so a success is not evidence `serve` owns
/// it.
#[expect(
    clippy::disallowed_methods,
    reason = "synchronous harness polling a std child process and its port"
)]
pub fn wait_for_port(port: u16, serve: &mut ServeProcess) -> TestResult<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        if let Some(status) = serve
            .child
            .try_wait()
            .map_err(|error| format!("checking serve child status: {}", ErrorChain(&error)))?
        {
            let (stdout, stderr) = serve.captured_output()?;
            return Err(format!(
                "serve exited before becoming ready: {status}\nstdout:\n{}\nstderr:\n{}",
                stdout, stderr
            )
            .into());
        }
        if Instant::now() >= deadline {
            let (stdout, stderr) = serve.captured_output()?;
            return Err(format!(
                "timed out waiting for gents server on port {port}\nstdout:\n{}\nstderr:\n{}",
                stdout, stderr
            )
            .into());
        }
        thread::sleep(Duration::from_millis(200));
    }
}

pub fn spawn_cli(home_dir: &Path, args: &[&str]) -> TestResult<Child> {
    Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .current_dir(home_dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!("spawning gents {}: {}", args.join(" "), ErrorChain(&error)).into()
        })
}

pub fn run_cli_json(home_dir: &Path, args: &[&str]) -> TestResult<Value> {
    run_cli_json_with_env(home_dir, args, &[])
}

pub fn run_cli_json_with_env(
    home_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> TestResult<Value> {
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
        .map_err(|error| format!("running gents {}: {}", args.join(" "), ErrorChain(&error)))?;
    if !output.status.success() {
        return Err(format!(
            "gents {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "parsing JSON from gents {}: {}",
            args.join(" "),
            ErrorChain(&error)
        )
        .into()
    })
}

pub fn run_cli_text(home_dir: &Path, args: &[&str]) -> TestResult<String> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .args(args)
        .output()
        .map_err(|error| format!("running gents {}: {}", args.join(" "), ErrorChain(&error)))?;
    if !output.status.success() {
        return Err(format!(
            "gents {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    String::from_utf8(output.stdout).map_err(|error| {
        format!(
            "parsing stdout from gents {}: {}",
            args.join(" "),
            ErrorChain(&error)
        )
        .into()
    })
}

pub fn run_cli_failure_stderr(home_dir: &Path, args: &[&str]) -> TestResult<String> {
    run_cli_failure_stderr_with_env(home_dir, args, &[])
}

pub fn run_cli_failure_stderr_with_env(
    home_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> TestResult<String> {
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
        .map_err(|error| format!("running gents {}: {}", args.join(" "), ErrorChain(&error)))?;
    if output.status.success() {
        return Err(format!(
            "expected gents {} to fail\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    String::from_utf8(output.stderr).map_err(|error| {
        format!(
            "parsing stderr from gents {}: {}",
            args.join(" "),
            ErrorChain(&error)
        )
        .into()
    })
}

pub fn run_cli_failure_stdout_json(home_dir: &Path, args: &[&str]) -> TestResult<Value> {
    let output = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .current_dir(home_dir)
        .args(args)
        .output()
        .map_err(|error| format!("running gents {}: {}", args.join(" "), ErrorChain(&error)))?;
    if output.status.success() {
        return Err(format!(
            "expected gents {} to fail\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "parsing failure JSON from gents {}\nstdout:\n{}\nstderr:\n{}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
            ErrorChain(&error)
        )
        .into()
    })
}
