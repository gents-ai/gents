use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use gents_store::test_support::bounded_process::{BoundedProcess, Exited};
use serde_json::Value;

use super::fs::read_captured_log;
use gents::support::ports;

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

pub fn run_init_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
    let mut command_args = vec!["init", "--store-key-custody", "file"];
    command_args.extend_from_slice(args);
    run_cli_json(home_dir, &command_args)
}

pub fn spawn_server(home_dir: &Path, port: u16) -> Result<ServeProcess> {
    spawn_server_with_env(home_dir, port, &[], &[])
}

/// Spawn a `gents runtime server` and wait for its readiness JSON, failing only
/// when the process exits or writes nothing for the suite's stall ceiling.
/// Never replaces a raced-away port: a caller holding this signature cannot
/// observe a port change, so moving the server would strand it on a dead port.
pub fn spawn_server_with_ready_json(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> Result<(ServeProcess, Value)> {
    let (serve, _port, value) =
        spawn_server_with_ready_json_inner(home_dir, port, extra_args, envs, false)?;
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
) -> Result<(ServeProcess, u16, Value)> {
    spawn_server_with_ready_json_inner(home_dir, port, extra_args, envs, true)
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
) -> Result<(ServeProcess, u16, Value)> {
    let mut port = port;
    let mut attempts_left = if recover_stolen_port {
        MAX_STOLEN_PORT_ATTEMPTS
    } else {
        0
    };
    'attempt: loop {
        let mut progress = (0, 0);
        let mut progressed_at = Instant::now();
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
            .args(extra_args)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        configure_foreground_server_env(&mut command, envs);
        let child = command.spawn().context("spawning gents runtime server")?;
        let mut serve = ServeProcess::with_logs(child, stdout_log, stderr_log);

        loop {
            let stdout_so_far = read_captured_log(serve.stdout_log.as_ref())?;
            if let Some(value) = server_readiness_json(&stdout_so_far) {
                super::graphql::register_served_home(
                    home_dir,
                    &served_home_dir(home_dir, extra_args),
                    envs,
                    port,
                )?;
                return Ok((serve, port, value));
            }
            let output = (
                stdout_so_far.len(),
                read_captured_log(serve.stderr_log.as_ref())?.len(),
            );
            if output != progress {
                progress = output;
                progressed_at = Instant::now();
            }
            let exited = serve
                .child
                .try_wait()
                .context("checking serve child status")?;
            let timed_out = progressed_at.elapsed() >= gents_store::test_support::STALL_CEILING;
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
                    bail!(
                        "server exited before emitting readiness JSON ({status})\nstdout:\n{}\nstderr:\n{}",
                        stdout,
                        stderr
                    );
                }
                bail!(
                    "gents server wrote nothing for {:?} before its readiness JSON\nstdout:\n{}\nstderr:\n{}",
                    gents_store::test_support::STALL_CEILING,
                    stdout,
                    stderr
                );
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

pub(crate) fn server_readiness_json(buffer: &str) -> Option<Value> {
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
        .args(extra_args)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    configure_foreground_server_env(&mut command, envs);
    let child = command.spawn().context("spawning gents runtime server")?;
    super::graphql::register_served_home(
        home_dir,
        &served_home_dir(home_dir, extra_args),
        envs,
        port,
    )?;
    Ok(ServeProcess::with_logs(child, stdout_log, stderr_log))
}

pub(crate) fn served_home_dir(home_dir: &Path, extra_args: &[&str]) -> PathBuf {
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
    home_dir.join(".gents")
}

pub(crate) fn configure_foreground_server_env(command: &mut Command, envs: &[(&str, &str)]) {
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

/// A backstop, not an expectation: it turns a command that would hang until
/// the harness kills the whole test process into one attributed failure.
const CLI_DEADLINE: Duration = Duration::from_secs(600);

/// Init stage boundaries stay visible so a timed-out initialization names the
/// stage it stopped in.
const CLI_LOG: &str = "error,gents_server::commands::init=info";

fn cli_command(home_dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Command {
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", home_dir)
        .env("RUST_LOG", CLI_LOG)
        .args(args);
    for (name, value) in envs {
        command.env(name, value);
    }
    command
}

fn run_bounded(mut command: Command, args: &[&str]) -> Result<Exited> {
    let what = format!("gents {}", args.join(" "));
    let capture = tempfile::tempdir().context("creating a CLI capture directory")?;
    BoundedProcess::spawn(&mut command, capture.path(), "cli", &what)?.exit(CLI_DEADLINE)
}

fn succeeded(exited: Exited, args: &[&str]) -> Result<Vec<u8>> {
    if !exited.status.success() {
        bail!(
            "gents {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&exited.stdout),
            String::from_utf8_lossy(&exited.stderr)
        );
    }
    Ok(exited.stdout)
}

fn failed(exited: Exited, args: &[&str]) -> Result<Exited> {
    if exited.status.success() {
        bail!(
            "expected gents {} to fail\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&exited.stdout),
            String::from_utf8_lossy(&exited.stderr)
        );
    }
    Ok(exited)
}

pub fn run_cli_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
    run_cli_json_with_env(home_dir, args, &[])
}

pub fn run_cli_json_with_env(
    home_dir: &Path,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<Value> {
    let mut command = cli_command(home_dir, args, envs);
    command.current_dir(home_dir);
    let stdout = succeeded(run_bounded(command, args)?, args)?;
    serde_json::from_slice(&stdout)
        .with_context(|| format!("parsing JSON from gents {}", args.join(" ")))
}

pub fn run_cli_text(home_dir: &Path, args: &[&str]) -> Result<String> {
    let stdout = succeeded(run_bounded(cli_command(home_dir, args, &[]), args)?, args)?;
    String::from_utf8(stdout)
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
    let exited = failed(run_bounded(cli_command(home_dir, args, envs), args)?, args)?;
    String::from_utf8(exited.stderr)
        .with_context(|| format!("parsing stderr from gents {}", args.join(" ")))
}

pub fn run_cli_failure_stdout_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
    let mut command = cli_command(home_dir, args, &[]);
    command.current_dir(home_dir);
    let exited = failed(run_bounded(command, args)?, args)?;
    serde_json::from_slice(&exited.stdout).with_context(|| {
        format!(
            "parsing failure JSON from gents {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&exited.stdout),
            String::from_utf8_lossy(&exited.stderr)
        )
    })
}
