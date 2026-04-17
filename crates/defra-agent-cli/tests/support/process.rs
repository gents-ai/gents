use std::io::{BufRead, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use super::fs::read_captured_log;

pub struct ServeProcess {
    pub child: Child,
    pub stdout_log: Option<tempfile::NamedTempFile>,
    pub stderr_log: Option<tempfile::NamedTempFile>,
}

pub struct MockModelEndpoint {
    pub endpoint: String,
    pub port: u16,
    pub stop: Arc<AtomicBool>,
    pub handle: Option<JoinHandle<()>>,
}

pub struct MockOpenAIEndpoint {
    pub endpoint: String,
    pub port: u16,
    pub stop: Arc<AtomicBool>,
    pub captured_chat_requests: Arc<Mutex<Vec<Value>>>,
    pub handle: Option<JoinHandle<()>>,
}

pub struct MockChatEndpoint {
    pub endpoint: String,
    pub port: u16,
    pub stop: Arc<AtomicBool>,
    pub captured_chat_requests: Arc<Mutex<Vec<Value>>>,
    pub handle: Option<JoinHandle<()>>,
}

pub struct HttpRequestData {
    pub method: String,
    pub path: String,
    pub headers: std::collections::HashMap<String, String>,
    pub body: Vec<u8>,
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

impl MockModelEndpoint {
    pub fn start(model_name: &str) -> Result<Self> {
        Self::start_with_required_bearer(model_name, None)
    }

    pub fn start_with_required_bearer(model_name: &str, required_bearer: Option<&str>) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding mock model port")?;
        listener
            .set_nonblocking(true)
            .context("marking mock model listener nonblocking")?;
        let port = listener
            .local_addr()
            .context("reading mock model port")?
            .port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        let required_bearer = required_bearer.map(ToOwned::to_owned);
        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = match read_http_request(&mut stream) {
                            Ok(request) => request,
                            Err(_) => {
                                let _ = stream.shutdown(Shutdown::Both);
                                continue;
                            }
                        };

                        let authorized = required_bearer.as_ref().is_none_or(|expected| {
                            request
                                .headers
                                .get("authorization")
                                .is_some_and(|value| value == &format!("Bearer {expected}"))
                        });
                        let (status, body) = if request.method == "GET"
                            && (request.path == "/v1/models" || request.path == "/models")
                        {
                            if authorized {
                                ("200 OK", format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#))
                            } else {
                                (
                                    "401 Unauthorized",
                                    r#"{"error":"unauthorized"}"#.to_string(),
                                )
                            }
                        } else {
                            ("404 Not Found", r#"{"error":"not found"}"#.to_string())
                        };
                        let _ = write_http_response(&mut stream, status, "application/json", &body);
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            stop,
            handle: Some(handle),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for MockModelEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl MockOpenAIEndpoint {
    pub fn start(model_name: &str, final_token: &str) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding mock OpenAI port")?;
        listener
            .set_nonblocking(true)
            .context("marking mock OpenAI listener nonblocking")?;
        let port = listener
            .local_addr()
            .context("reading mock OpenAI port")?
            .port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        let final_token = final_token.to_string();
        let captured_chat_requests = Arc::new(Mutex::new(Vec::new()));
        let captured_chat_requests_for_thread = captured_chat_requests.clone();

        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = match read_http_request(&mut stream) {
                            Ok(request) => request,
                            Err(_) => {
                                let _ = stream.shutdown(Shutdown::Both);
                                continue;
                            }
                        };

                        match (request.method.as_str(), request.path.as_str()) {
                            ("GET", "/v1/models") => {
                                let body = format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#);
                                let _ = write_http_response(
                                    &mut stream,
                                    "200 OK",
                                    "application/json",
                                    &body,
                                );
                            }
                            ("POST", "/v1/chat/completions") => {
                                let request_json: Value =
                                    match serde_json::from_slice(&request.body) {
                                        Ok(value) => value,
                                        Err(_) => {
                                            let _ = write_http_response(
                                                &mut stream,
                                                "400 Bad Request",
                                                "application/json",
                                                r#"{"error":"invalid json"}"#,
                                            );
                                            let _ = stream.shutdown(Shutdown::Both);
                                            continue;
                                        }
                                    };
                                captured_chat_requests_for_thread
                                    .lock()
                                    .expect("captured chat request mutex poisoned")
                                    .push(request_json.clone());

                                let sse_body = if request_has_tool_result_message(&request_json) {
                                    completion_text_sse(&final_token)
                                } else {
                                    tool_call_sse("read_file", r#"{"path":"notes.txt"}"#)
                                };
                                let _ = write_http_response(
                                    &mut stream,
                                    "200 OK",
                                    "text/event-stream",
                                    &sse_body,
                                );
                            }
                            _ => {
                                let _ = write_http_response(
                                    &mut stream,
                                    "404 Not Found",
                                    "application/json",
                                    r#"{"error":"not found"}"#,
                                );
                            }
                        }

                        let _ = stream.flush();
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            stop,
            captured_chat_requests,
            handle: Some(handle),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn captured_chat_requests(&self) -> Vec<Value> {
        self.captured_chat_requests
            .lock()
            .expect("captured chat request mutex poisoned")
            .clone()
    }
}

impl Drop for MockOpenAIEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl MockChatEndpoint {
    pub fn start(model_name: &str, final_text: &str) -> Result<Self> {
        Self::start_with_required_bearer(model_name, final_text, None)
    }

    pub fn start_with_required_bearer(
        model_name: &str,
        final_text: &str,
        required_bearer: Option<&str>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding mock chat port")?;
        listener
            .set_nonblocking(true)
            .context("marking mock chat listener nonblocking")?;
        let port = listener
            .local_addr()
            .context("reading mock chat port")?
            .port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        let final_text = final_text.to_string();
        let required_bearer = required_bearer.map(ToOwned::to_owned);
        let captured_chat_requests = Arc::new(Mutex::new(Vec::new()));
        let captured_chat_requests_for_thread = captured_chat_requests.clone();

        let handle = thread::spawn(move || {
            while !stop_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request = match read_http_request(&mut stream) {
                            Ok(request) => request,
                            Err(_) => {
                                let _ = stream.shutdown(Shutdown::Both);
                                continue;
                            }
                        };

                        let authorized = required_bearer.as_ref().is_none_or(|expected| {
                            request
                                .headers
                                .get("authorization")
                                .is_some_and(|value| value == &format!("Bearer {expected}"))
                        });

                        match (request.method.as_str(), request.path.as_str()) {
                            ("GET", "/v1/models") => {
                                let (status, body) = if authorized {
                                    ("200 OK", format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#))
                                } else {
                                    (
                                        "401 Unauthorized",
                                        r#"{"error":"unauthorized"}"#.to_string(),
                                    )
                                };
                                let _ = write_http_response(
                                    &mut stream,
                                    status,
                                    "application/json",
                                    &body,
                                );
                            }
                            ("POST", "/v1/chat/completions") => {
                                if !authorized {
                                    let _ = write_http_response(
                                        &mut stream,
                                        "401 Unauthorized",
                                        "application/json",
                                        r#"{"error":"unauthorized"}"#,
                                    );
                                    let _ = stream.shutdown(Shutdown::Both);
                                    continue;
                                }
                                let request_json: Value =
                                    match serde_json::from_slice(&request.body) {
                                        Ok(value) => value,
                                        Err(_) => {
                                            let _ = write_http_response(
                                                &mut stream,
                                                "400 Bad Request",
                                                "application/json",
                                                r#"{"error":"invalid json"}"#,
                                            );
                                            let _ = stream.shutdown(Shutdown::Both);
                                            continue;
                                        }
                                    };
                                captured_chat_requests_for_thread
                                    .lock()
                                    .expect("captured chat request mutex poisoned")
                                    .push(request_json);

                                let _ = write_http_response(
                                    &mut stream,
                                    "200 OK",
                                    "text/event-stream",
                                    &completion_text_sse(&final_text),
                                );
                            }
                            _ => {
                                let _ = write_http_response(
                                    &mut stream,
                                    "404 Not Found",
                                    "application/json",
                                    r#"{"error":"not found"}"#,
                                );
                            }
                        }

                        let _ = stream.flush();
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            port,
            stop,
            captured_chat_requests,
            handle: Some(handle),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn captured_chat_requests(&self) -> Vec<Value> {
        self.captured_chat_requests
            .lock()
            .expect("captured chat request mutex poisoned")
            .clone()
    }
}

impl Drop for MockChatEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
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
        .current_dir(home_dir)
        .arg("server")
        .arg("--http-port")
        .arg(port.to_string())
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
        .current_dir(home_dir)
        .arg("server")
        .arg("--http-port")
        .arg(port.to_string())
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

pub fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequestData> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut header_end = None;
    let mut content_length = 0_usize;

    loop {
        let read = stream.read(&mut chunk).context("reading mock request")?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);

        if header_end.is_none() {
            if let Some(offset) = find_header_end(&buffer) {
                let end = offset + 4;
                let headers = String::from_utf8_lossy(&buffer[..end]);
                header_end = Some(end);
                content_length = parse_content_length(&headers).unwrap_or(0);
                if buffer.len() >= end + content_length {
                    break;
                }
            }
        } else if buffer.len() >= header_end.expect("header_end should be set") + content_length {
            break;
        }
    }

    let header_end = header_end.ok_or_else(|| anyhow!("missing request headers"))?;
    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let request_line = header_text
        .lines()
        .next()
        .ok_or_else(|| anyhow!("missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP method"))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| anyhow!("missing HTTP path"))?
        .to_string();
    let headers = header_text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    let body_end = header_end + content_length;
    let body = if buffer.len() >= body_end {
        buffer[header_end..body_end].to_vec()
    } else {
        Vec::new()
    };

    Ok(HttpRequestData {
        method,
        path,
        headers,
        body,
    })
}

pub fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

pub fn parse_content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    })
}

pub fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .context("writing mock response")?;
    Ok(())
}

pub fn tool_call_sse(tool_name: &str, arguments: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call-read-file",
                    "function": {
                        "name": tool_name,
                        "arguments": ""
                    }
                }]
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_2 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": null,
                    "function": {
                        "name": null,
                        "arguments": arguments
                    }
                }]
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_3 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": null,
                "tool_calls": []
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 16,
            "completion_tokens": 4,
            "total_tokens": 20
        }
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk_1).expect("serialize tool-call chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize tool-call chunk 2"),
        serde_json::to_string(&chunk_3).expect("serialize tool-call chunk 3"),
    )
}

pub fn completion_text_sse(text: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": text
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_2 = serde_json::json!({
        "choices": [{
            "delta": {
                "content": null,
                "tool_calls": []
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 24,
            "completion_tokens": 6,
            "total_tokens": 30
        }
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk_1).expect("serialize completion chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize completion chunk 2"),
    )
}

pub fn request_has_tool_result_message(request: &Value) -> bool {
    request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages
                .iter()
                .any(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        })
}

pub fn request_tool_names(request: &Value) -> Vec<String> {
    request
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    tool.get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub fn request_system_message(request: &Value) -> Option<&str> {
    request
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().find_map(|message| {
                if message.get("role").and_then(Value::as_str) != Some("system") {
                    return None;
                }
                match message.get("content") {
                    Some(Value::String(content)) => Some(content.as_str()),
                    Some(Value::Array(parts)) => parts
                        .iter()
                        .find_map(|part| part.get("text").and_then(Value::as_str)),
                    _ => None,
                }
            })
        })
}

pub fn request_tool_result_text(request: &Value) -> Option<String> {
    request
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages.iter().find_map(|message| {
                if message.get("role").and_then(Value::as_str) != Some("tool") {
                    return None;
                }
                match message.get("content") {
                    Some(Value::String(content)) => Some(content.to_string()),
                    Some(Value::Array(parts)) => {
                        let text = parts
                            .iter()
                            .filter_map(|part| part.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n");
                        Some(text)
                    }
                    _ => None,
                }
            })
        })
}

pub fn request_contains_role_text(request: &Value, role: &str, needle: &str) -> bool {
    request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                if message.get("role").and_then(Value::as_str) != Some(role) {
                    return false;
                }
                match message.get("content") {
                    Some(Value::String(content)) => content.contains(needle),
                    Some(Value::Array(parts)) => parts.iter().any(|part| {
                        part.get("text")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.contains(needle))
                    }),
                    _ => false,
                }
            })
        })
}
