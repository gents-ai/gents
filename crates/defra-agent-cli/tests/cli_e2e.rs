use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use uuid::Uuid;

const DEFAULT_MODEL_ENDPOINT: &str = "http://100.73.235.38:8000/v1";

struct ServeProcess {
    child: Child,
}

struct MockModelEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

struct MockOpenAIEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    captured_chat_requests: Arc<Mutex<Vec<Value>>>,
    handle: Option<JoinHandle<()>>,
}

struct MockChatEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    captured_chat_requests: Arc<Mutex<Vec<Value>>>,
    handle: Option<JoinHandle<()>>,
}

struct HttpRequestData {
    method: String,
    path: String,
    body: Vec<u8>,
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl MockModelEndpoint {
    fn start(model_name: &str) -> Result<Self> {
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

                        let (status, body) = if request.method == "GET"
                            && (request.path == "/v1/models" || request.path == "/models")
                        {
                            ("200 OK", format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#))
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

    fn endpoint(&self) -> &str {
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
    fn start(model_name: &str, final_token: &str) -> Result<Self> {
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

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn captured_chat_requests(&self) -> Vec<Value> {
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
    fn start(model_name: &str, final_text: &str) -> Result<Self> {
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

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn captured_chat_requests(&self) -> Vec<Value> {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_selection_upsert_defaults_enabled_modes_to_readonly() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-config-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-config-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
    let selection_id = format!("{agent_name}:tools");

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "tools",
            "set",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--selection-id",
            &selection_id,
            "--enable-file-tools",
            "--enable-bash",
        ],
    )?;
    let doc_id = output
        .get("doc_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("tool-selection output missing doc_id: {output}"))?;
    assert_eq!(
        output.get("file_tools_mode").and_then(Value::as_str),
        Some("ReadOnly")
    );
    assert_eq!(
        output.get("bash_mode").and_then(Value::as_str),
        Some("ReadOnly")
    );

    let query = format!(
        r#"{{
            ToolSelection(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                selection_id
                enable_file_tools
                file_tools_mode
                enable_bash
                bash_mode
            }}
        }}"#,
        escape_graphql_string(doc_id),
    );
    let response = graphql_query(&graphql, &query).await?;
    let row = first_graphql_row(&response, "ToolSelection")?;
    assert_eq!(
        row.get("selection_id").and_then(Value::as_str),
        Some(selection_id.as_str())
    );
    assert_eq!(
        row.get("enable_file_tools").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        row.get("file_tools_mode").and_then(Value::as_str),
        Some("ReadOnly")
    );
    assert_eq!(row.get("enable_bash").and_then(Value::as_bool), Some(true));
    assert_eq!(
        row.get("bash_mode").and_then(Value::as_str),
        Some("ReadOnly")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_submit_waits_for_response_by_default() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-submit-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-submit-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let request_agent_did = format!("did:defra-agent:request-wait-{}", Uuid::new_v4().simple());
    let request_content = format!("CLI wait test {}", Uuid::new_v4());
    let expected_content = format!("wait-ok-{}", Uuid::new_v4().simple());

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(
        &graphql,
        &format!("did:defra-agent:{agent_name}"),
        Duration::from_secs(30),
    )
    .await?;

    let submit = spawn_cli(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &request_agent_did,
            "--content",
            &request_content,
            "--timeout-secs",
            "20",
            "--poll-secs",
            "1",
        ],
    )?;

    let (request_id, session_id, behavior_id) =
        wait_for_request(&graphql, &request_agent_did, &request_content).await?;
    insert_terminal_response(
        &graphql,
        &request_id,
        &request_agent_did,
        &behavior_id,
        &session_id,
        &expected_content,
    )
    .await?;

    let output = submit
        .wait_with_output()
        .context("waiting for request submit child")?;
    if !output.status.success() {
        bail!(
            "request submit failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let parsed: Value =
        serde_json::from_slice(&output.stdout).context("parsing request submit JSON")?;
    assert_eq!(
        parsed.get("request_id").and_then(Value::as_str),
        Some(request_id.as_str())
    );
    assert_eq!(
        parsed.pointer("/response/status").and_then(Value::as_str),
        Some("complete")
    );
    assert_eq!(
        parsed.pointer("/response/content").and_then(Value::as_str),
        Some(expected_content.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_uses_runtime_state_for_interactive_turns() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("chat-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-chat-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-chat-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let mut child = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .arg("chat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning defra-agent chat")?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("chat child missing stdin"))?;
        stdin
            .write_all(b"Reply with exactly the configured token.\n/exit\n")
            .context("writing interactive chat input")?;
        stdin.flush().context("flushing interactive chat input")?;
    }

    let output = child
        .wait_with_output()
        .context("waiting for defra-agent chat")?;
    if !output.status.success() {
        bail!(
            "defra-agent chat failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&expected_reply),
        "expected chat output to contain {expected_reply}, got:\n{stdout}"
    );

    let captured_requests = mock_endpoint.captured_chat_requests();
    assert_eq!(captured_requests.len(), 1);
    assert_eq!(
        captured_requests[0].get("model").and_then(Value::as_str),
        Some(model_name.as_str())
    );
    assert!(
        request_system_message(&captured_requests[0])
            .is_some_and(|system| system.contains("read-only operating mode")
                && system.contains("incident triage")),
        "expected system prompt in request: {}",
        captured_requests[0]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_buffers_final_response_and_shows_tool_progress() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    fs::write(home_dir.join("notes.txt"), "chat-tool-token\n")?;

    let expected_reply = "chat-tool-token";
    let model_name = format!("mock-tool-chat-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockOpenAIEndpoint::start(&model_name, expected_reply)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-tool-chat-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let mut child = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .arg("chat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning defra-agent chat for tool transcript test")?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("chat child missing stdin"))?;
        stdin
            .write_all(b"Read notes.txt and reply with its token.\n/exit\n")
            .context("writing interactive chat input")?;
        stdin.flush().context("flushing interactive chat input")?;
    }

    let output = child
        .wait_with_output()
        .context("waiting for defra-agent chat tool transcript run")?;
    if !output.status.success() {
        bail!(
            "defra-agent chat failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[tool] read_file"),
        "expected chat output to contain tool start, got:\n{stdout}"
    );
    assert!(
        stdout.contains("[tool done] read_file"),
        "expected chat output to contain tool completion, got:\n{stdout}"
    );
    assert!(
        stdout.contains(expected_reply),
        "expected chat output to contain final reply {expected_reply}, got:\n{stdout}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_bootstraps_backend_default_behavior_and_tool_selection_idempotently() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-init-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let backend_id = format!("{agent_name}-backend");
    let graphql = graphql_url(port);
    let tool_selection_id = format!("{agent_did}:default:tools");

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        init.get("status").and_then(Value::as_str),
        Some("initialized")
    );
    assert_eq!(
        init.pointer("/init/tool_ceiling").and_then(Value::as_str),
        Some("Readonly")
    );

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        &model_name,
        &tool_selection_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    drop(serve);

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        &model_name,
        &tool_selection_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    let backend_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                InferenceBackend(filter: {{ backend_id: {{ _eq: "{}" }} }}) {{
                    backend_id
                }}
            }}"#,
            escape_graphql_string(&backend_id),
        ),
    )
    .await?;
    assert_eq!(
        backend_rows
            .pointer("/data/InferenceBackend")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    let behavior_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(filter: {{ agent_did: {{ _eq: "{}" }} }}) {{
                    behavior_id
                }}
            }}"#,
            escape_graphql_string(&agent_did),
        ),
    )
    .await?;
    assert_eq!(
        behavior_rows
            .pointer("/data/AgentBehavior")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    let selection_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}) {{
                    selection_id
                }}
            }}"#,
            escape_graphql_string(&tool_selection_id),
        ),
    )
    .await?;
    assert_eq!(
        selection_rows
            .pointer("/data/ToolSelection")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reads_local_runtime_context_by_default() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-status-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-status-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let output = run_cli_json(&home_dir, &["status"])?;
    assert_eq!(
        output.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        output
            .pointer("/runtime/process_state")
            .and_then(Value::as_str),
        Some("ready")
    );
    assert_eq!(
        output.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_requires_model_name() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .arg("init")
        .arg("http://127.0.0.1:65535/v1")
        .output()
        .context("running defra-agent init without model name")?;

    assert!(!output.status.success(), "init should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--model-name <MODEL_NAME>"),
        "expected clap missing-argument error, got:\n{stderr}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_dangerously_overwrite_replaces_existing_home() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("overwrite-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-overwrite-{}", Uuid::new_v4().simple());

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;

    let runtime_home = home_dir.join(".defra-agent");
    let stale_path = runtime_home.join("stale.txt");
    fs::write(&stale_path, "stale").context("writing stale file into runtime home")?;
    assert!(stale_path.exists(), "expected stale file to exist");

    run_init_json(
        &home_dir,
        &[
            "--dangerously-overwrite",
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;

    assert!(
        !stale_path.exists(),
        "dangerously overwrite should remove stale files in the runtime home"
    );
    assert!(
        runtime_home.join("init.json").exists(),
        "init config should be recreated after dangerously overwrite"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_accepts_explicit_backend_and_model_together() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("explicit-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-explicit-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
    let backend_id = format!("{agent_name}-custom-backend");
    let tool_selection_id = format!("{agent_did}:default:tools");

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--backend-id",
            &backend_id,
            mock_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        &model_name,
        &tool_selection_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_with_write_tools_bootstraps_write_defaults() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("write-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-write-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
    let backend_id = format!("{agent_name}-backend");
    let tool_selection_id = format!("{agent_did}:default:tools");

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--write-tools",
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        init.pointer("/init/tool_ceiling").and_then(Value::as_str),
        Some("Readwrite")
    );

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        &model_name,
        &tool_selection_id,
        "ReadWrite",
        "Unrestricted",
        "write-capable local tools",
    )
    .await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconciled_runtime_sends_generation_two_tools_and_completes_tool_loop() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let token = format!("E2E_TOKEN_{}", Uuid::new_v4().simple());
    fs::write(home_dir.join("notes.txt"), format!("{token}\n"))?;

    let system_prompt = tempdir.path().join("system_prompt.txt");
    fs::write(
        &system_prompt,
        "When the user asks you to read a local file, call read_file and respond with only the token from that file.",
    )?;

    let model_name = format!("mock-tool-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockOpenAIEndpoint::start(&model_name, &token)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-tool-loop-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();
    let selection_id = init
        .pointer("/init/tool_selection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing tool_selection_id: {init}"))?
        .to_string();
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    let behavior = run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "set",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--display-name",
            "Default",
            "--system-prompt-file",
            system_prompt
                .to_str()
                .context("system prompt path is not UTF-8")?,
            "--backend-id",
            &backend_id,
            "--model-name",
            &model_name,
            "--tool-selection-id",
            &selection_id,
        ],
    )?;
    let behavior_doc_id = behavior
        .get("doc_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("behavior output missing doc_id: {behavior}"))?;
    let selection_doc_id = doc_id_for_selection(&graphql, &selection_id).await?;
    let config_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                    behavior_id
                    tool_selection_id
                    backend_id
                }}
                ToolSelection(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                    selection_id
                    enable_file_tools
                    file_tools_mode
                }}
            }}"#,
            escape_graphql_string(behavior_doc_id),
            escape_graphql_string(&selection_doc_id),
        ),
    )
    .await?;
    let behavior_row = first_graphql_row(&config_rows, "AgentBehavior")?;
    assert_eq!(
        behavior_row
            .get("tool_selection_id")
            .and_then(Value::as_str),
        Some(selection_id.as_str())
    );
    assert_eq!(
        behavior_row.get("backend_id").and_then(Value::as_str),
        Some(backend_id.as_str())
    );
    let selection_row = first_graphql_row(&config_rows, "ToolSelection")?;
    assert_eq!(
        selection_row
            .get("enable_file_tools")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        selection_row.get("file_tools_mode").and_then(Value::as_str),
        Some("ReadOnly")
    );
    wait_for_runtime_quiescence(&graphql, &agent_did, 2, Duration::from_secs(6)).await?;

    let prompt =
        "Use the read_file tool to read notes.txt. Reply with only the token from that file.";
    let result = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--content",
            prompt,
            "--timeout-secs",
            "60",
            "--poll-secs",
            "1",
        ],
    )?;
    let response = result
        .pointer("/response/content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("request submit result did not include response content: {result}")
        })?;
    assert_eq!(response, token);

    let captured_requests = mock_endpoint.captured_chat_requests();
    assert!(
        captured_requests.len() >= 2,
        "expected at least two chat completion requests, got {}",
        captured_requests.len()
    );

    let initial_request = captured_requests
        .iter()
        .find(|request| !request_has_tool_result_message(request))
        .ok_or_else(|| anyhow!("missing initial chat completion request"))?;
    let tool_result_request = captured_requests
        .iter()
        .find(|request| request_has_tool_result_message(request))
        .ok_or_else(|| anyhow!("missing follow-up chat completion request with tool result"))?;

    assert_eq!(
        initial_request.get("model").and_then(Value::as_str),
        Some(model_name.as_str())
    );
    let initial_tool_names = request_tool_names(initial_request);
    assert!(
        initial_tool_names.contains(&"read_file".to_string()),
        "expected initial request to include read_file, got tools {:?} in request {initial_request}",
        initial_tool_names
    );
    assert!(
        initial_tool_names.contains(&"list_files".to_string()),
        "expected initial request to include list_files, got tools {:?} in request {initial_request}",
        initial_tool_names
    );
    assert!(
        request_system_message(initial_request)
            .is_some_and(|system| system.contains("You have access to these tools")
                && system.contains("read_file")),
        "expected initial system message to advertise direct tools: {initial_request}"
    );

    let followup_tool_names = request_tool_names(tool_result_request);
    assert!(followup_tool_names.contains(&"read_file".to_string()));
    assert!(
        request_tool_result_text(tool_result_request)
            .is_some_and(|content| content.contains(&token)),
        "expected follow-up request to include persisted tool result with token {token}: {tool_result_request}"
    );

    let (_request_id, session_id, behavior_id) =
        wait_for_request(&graphql, &agent_did, prompt).await?;
    assert!(
        !behavior_id.is_empty(),
        "request should be pinned to a behavior"
    );

    let tool_call = wait_for_tool_call(&graphql, &session_id, "read_file").await?;
    assert_eq!(
        tool_call.get("status").and_then(Value::as_str),
        Some("completed")
    );
    assert!(
        tool_call
            .get("result")
            .and_then(Value::as_str)
            .is_some_and(|result| result.contains(&token)),
        "expected persisted tool result to contain token {token}: {tool_call}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a reachable external OpenAI-compatible endpoint"]
async fn cli_flow_runs_real_tool_loop_against_live_endpoint() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let token = format!("E2E_TOKEN_{}", Uuid::new_v4().simple());
    fs::write(home_dir.join("notes.txt"), format!("{token}\n"))?;

    let system_prompt = tempdir.path().join("system_prompt.txt");
    fs::write(
        &system_prompt,
        "When the user asks you to read a local file, use the available file tools instead of guessing. If they ask for a token from a file, respond with only that token.",
    )?;

    let port = allocate_port()?;
    let agent_name = format!("cli-live-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
    let model_endpoint = std::env::var("DEFRA_AGENT_CLI_E2E_MODEL_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let model_name = std::env::var("DEFRA_AGENT_CLI_E2E_MODEL_NAME")
        .context("set DEFRA_AGENT_CLI_E2E_MODEL_NAME for the live CLI e2e test")?;
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            &model_endpoint,
        ],
    )?;
    let backend_id = init
        .pointer("/init/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing backend_id: {init}"))?
        .to_string();
    let selection_id = init
        .pointer("/init/tool_selection_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("init output missing tool_selection_id: {init}"))?
        .to_string();
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;
    run_cli_json(
        &home_dir,
        &[
            "config",
            "behavior",
            "set",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--display-name",
            "Default",
            "--system-prompt-file",
            system_prompt
                .to_str()
                .context("system prompt path is not UTF-8")?,
            "--backend-id",
            &backend_id,
            "--model-name",
            &model_name,
            "--tool-selection-id",
            &selection_id,
        ],
    )?;
    wait_for_runtime_quiescence(&graphql, &agent_did, 2, Duration::from_secs(6)).await?;

    let prompt =
        "Use the read_file tool to read notes.txt. Reply with only the token from that file.";
    let result = run_cli_json(
        &home_dir,
        &[
            "request",
            "submit",
            "--graphql",
            &graphql,
            "--agent-did",
            &agent_did,
            "--content",
            prompt,
            "--timeout-secs",
            "180",
            "--poll-secs",
            "1",
        ],
    )?;
    let response = result
        .pointer("/response/content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow!("request submit result did not include response content: {result}")
        })?;
    assert!(
        response.contains(&token),
        "expected response to contain token {token}, got {response}"
    );

    Ok(())
}

fn cli_bin() -> &'static str {
    env!("CARGO_BIN_EXE_defra-agent")
}

fn allocate_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding ephemeral port")?;
    let port = listener
        .local_addr()
        .context("reading ephemeral port")?
        .port();
    drop(listener);
    Ok(port)
}

fn graphql_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/api/v0/graphql")
}

fn run_init_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
    let mut command_args = vec!["init"];
    command_args.extend_from_slice(args);
    run_cli_json(home_dir, &command_args)
}

fn spawn_server(home_dir: &Path, port: u16) -> Result<ServeProcess> {
    spawn_server_with_args(home_dir, port, &[])
}

fn spawn_server_with_args(home_dir: &Path, port: u16, extra_args: &[&str]) -> Result<ServeProcess> {
    let child = Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .arg("server")
        .arg("--http-port")
        .arg(port.to_string())
        .args(extra_args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawning defra-agent server")?;
    Ok(ServeProcess { child })
}

async fn assert_runtime_init_state(
    graphql: &str,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
    model_name: &str,
    tool_selection_id: &str,
    expected_file_tools_mode: &str,
    expected_bash_mode: &str,
    expected_prompt_snippet: &str,
) -> Result<()> {
    let query = format!(
        r#"{{
            AgentPrincipal(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 1) {{
                agent_did
                default_behavior_id
                enabled
            }}
            AgentBehavior(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 1) {{
                behavior_id
                backend_id
                model_name
                tool_selection_id
                system_prompt
                enabled
            }}
            InferenceBackend(filter: {{ backend_id: {{ _eq: "{}" }} }}, limit: 1) {{
                backend_id
                endpoint
                enabled
                probe_status
                models
            }}
            ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}, limit: 1) {{
                selection_id
                enable_file_tools
                file_tools_mode
                enable_bash
                bash_mode
                enable_meta_tools
            }}
        }}"#,
        escape_graphql_string(agent_did),
        escape_graphql_string(agent_did),
        escape_graphql_string(backend_id),
        escape_graphql_string(tool_selection_id),
    );
    let response = graphql_query(graphql, &query).await?;
    let principal = first_graphql_row(&response, "AgentPrincipal")?;
    let behavior = first_graphql_row(&response, "AgentBehavior")?;
    let backend = first_graphql_row(&response, "InferenceBackend")?;
    let tool_selection = first_graphql_row(&response, "ToolSelection")?;

    let default_behavior_id = format!("{agent_did}:default");
    assert_eq!(
        principal.get("agent_did").and_then(Value::as_str),
        Some(agent_did)
    );
    assert_eq!(
        principal.get("default_behavior_id").and_then(Value::as_str),
        Some(default_behavior_id.as_str())
    );
    assert_eq!(
        principal.get("enabled").and_then(Value::as_bool),
        Some(true)
    );

    assert_eq!(
        behavior.get("behavior_id").and_then(Value::as_str),
        Some(default_behavior_id.as_str())
    );
    assert_eq!(
        behavior.get("backend_id").and_then(Value::as_str),
        Some(backend_id)
    );
    assert_eq!(
        behavior.get("model_name").and_then(Value::as_str),
        Some(model_name)
    );
    assert_eq!(
        behavior.get("tool_selection_id").and_then(Value::as_str),
        Some(tool_selection_id)
    );
    assert!(
        behavior
            .get("system_prompt")
            .and_then(Value::as_str)
            .is_some_and(|prompt| prompt.contains(expected_prompt_snippet)),
        "expected system_prompt to contain {expected_prompt_snippet}: {behavior}"
    );
    assert_eq!(behavior.get("enabled").and_then(Value::as_bool), Some(true));

    assert_eq!(
        backend.get("backend_id").and_then(Value::as_str),
        Some(backend_id)
    );
    assert_eq!(
        backend.get("endpoint").and_then(Value::as_str),
        Some(endpoint)
    );
    assert_eq!(backend.get("enabled").and_then(Value::as_bool), Some(true));
    assert_eq!(
        backend.get("probe_status").and_then(Value::as_str),
        Some("healthy")
    );
    assert_eq!(
        backend.pointer("/models/0").and_then(Value::as_str),
        Some(model_name)
    );
    assert_eq!(
        tool_selection.get("selection_id").and_then(Value::as_str),
        Some(tool_selection_id)
    );
    assert_eq!(
        tool_selection
            .get("enable_file_tools")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        tool_selection
            .get("file_tools_mode")
            .and_then(Value::as_str),
        Some(expected_file_tools_mode)
    );
    assert_eq!(
        tool_selection.get("enable_bash").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        tool_selection.get("bash_mode").and_then(Value::as_str),
        Some(expected_bash_mode)
    );
    assert_eq!(
        tool_selection
            .get("enable_meta_tools")
            .and_then(Value::as_bool),
        Some(true)
    );

    Ok(())
}

async fn doc_id_for_selection(graphql: &str, selection_id: &str) -> Result<String> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                ToolSelection(filter: {{ selection_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                }}
            }}"#,
            escape_graphql_string(selection_id),
        ),
    )
    .await?;
    first_graphql_row(&response, "ToolSelection")?
        .get("_docID")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("ToolSelection row missing _docID for {selection_id}"))
}

fn wait_for_port(port: u16, child: &mut Child) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        if let Some(status) = child.try_wait().context("checking serve child status")? {
            bail!("serve exited before becoming ready: {status}");
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for defra-agent server on port {port}");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn spawn_cli(home_dir: &Path, args: &[&str]) -> Result<Child> {
    Command::new(cli_bin())
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning defra-agent {}", args.join(" ")))
}

fn run_cli_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
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

    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parsing JSON from defra-agent {}", args.join(" ")))
}

async fn wait_for_request(
    graphql: &str,
    agent_did: &str,
    content: &str,
) -> Result<(String, String, String)> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let query = format!(
            r#"{{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{}" }},
                        content: {{ _eq: "{}" }}
                    }},
                    order: {{ created_at: DESC }},
                    limit: 1
                ) {{
                    request_id
                    session_id
                    behavior_id
                }}
            }}"#,
            escape_graphql_string(agent_did),
            escape_graphql_string(content),
        );
        let response = graphql_query(graphql, &query).await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
            let request_id = row
                .get("request_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing request_id: {row}"))?;
            let session_id = row
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("AgentRequest row missing session_id: {row}"))?;
            let behavior_id = row
                .get("behavior_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Ok((
                request_id.to_string(),
                session_id.to_string(),
                behavior_id.to_string(),
            ));
        }

        if Instant::now() >= deadline {
            bail!("timed out waiting for AgentRequest for {agent_did}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn insert_terminal_response(
    graphql: &str,
    request_id: &str,
    agent_did: &str,
    behavior_id: &str,
    session_id: &str,
    content: &str,
) -> Result<()> {
    let response_key = format!("response-{request_id}");
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentResponse(input: {{
                response_key: "{response_key}",
                request_id: "{request_id}",
                agent_did: "{agent_did}",
                behavior_id: "{behavior_id}",
                session_id: "{session_id}",
                content: "{content}",
                status: "complete",
                token_count: 0,
                progress_seq: 0,
                created_at: "{now}",
                completed_at: "{now}"
            }}) {{ _docID }}
        }}"#,
        response_key = escape_graphql_string(&response_key),
        request_id = escape_graphql_string(request_id),
        agent_did = escape_graphql_string(agent_did),
        behavior_id = escape_graphql_string(behavior_id),
        session_id = escape_graphql_string(session_id),
        content = escape_graphql_string(content),
        now = escape_graphql_string(&now),
    );
    graphql_query(graphql, &mutation).await?;
    Ok(())
}

async fn graphql_query(graphql: &str, query: &str) -> Result<Value> {
    let response = reqwest::Client::new()
        .post(graphql)
        .json(&serde_json::json!({ "query": query }))
        .send()
        .await
        .with_context(|| format!("posting GraphQL to {graphql}"))?;
    let value: Value = response.json().await.context("decoding GraphQL response")?;
    if let Some(errors) = value.get("errors") {
        bail!("graphql returned errors: {errors}");
    }
    Ok(value)
}

fn first_graphql_row<'a>(response: &'a Value, field: &str) -> Result<&'a Value> {
    response
        .pointer(&format!("/data/{field}"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| anyhow!("missing {field} row in GraphQL response: {response}"))
}

async fn wait_for_runtime_ready(graphql: &str, agent_did: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentRuntime(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 1) {{
                        process_state
                    }}
                }}"#,
                escape_graphql_string(agent_did),
            ),
        )
        .await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRuntime") {
            if row.get("process_state").and_then(Value::as_str) == Some("ready") {
                return Ok(());
            }
        }

        if Instant::now() >= deadline {
            bail!("timed out waiting for AgentRuntime ready state for {agent_did}");
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_runtime_quiescence(
    graphql: &str,
    agent_did: &str,
    minimum_generation: i64,
    quiet_period: Duration,
) -> Result<i64> {
    let deadline = Instant::now() + Duration::from_secs(45);
    let runtime_doc_id = wait_for_runtime_doc_id(graphql, agent_did).await?;
    let mut last_generation = None;
    let mut last_change_at = None;
    let mut last_runtime_row = None::<Value>;

    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentRuntime(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{
                        reconcile_phase
                        active_generation
                        router_generation
                        last_reconcile_result
                    }}
                }}"#,
                escape_graphql_string(&runtime_doc_id),
            ),
        )
        .await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRuntime") {
            last_runtime_row = Some(row.clone());
            let generation = row
                .get("active_generation")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let router_generation = row
                .get("router_generation")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let phase = row
                .get("reconcile_phase")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let result = row
                .get("last_reconcile_result")
                .and_then(Value::as_str)
                .unwrap_or_default();

            if generation >= minimum_generation
                && router_generation >= minimum_generation
                && phase == "idle"
                && matches!(result, "startup" | "applied" | "noop")
            {
                let now = Instant::now();
                match last_generation {
                    Some(previous) if previous == generation => {
                        if last_change_at.is_some_and(|changed_at| {
                            now.duration_since(changed_at) >= quiet_period
                        }) {
                            return Ok(generation);
                        }
                    }
                    _ => {
                        last_generation = Some(generation);
                        last_change_at = Some(now);
                    }
                }
            } else {
                last_generation = None;
                last_change_at = None;
            }
        }

        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for AgentRuntime quiescence at generation >= {minimum_generation} for {agent_did}; last_runtime_row={}",
                last_runtime_row
                    .map(|row| row.to_string())
                    .unwrap_or_else(|| "null".to_string())
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_runtime_doc_id(graphql: &str, agent_did: &str) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentRuntime(filter: {{ agent_did: {{ _eq: "{}" }} }}, limit: 1) {{
                        _docID
                    }}
                }}"#,
                escape_graphql_string(agent_did),
            ),
        )
        .await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRuntime") {
            if let Some(doc_id) = row.get("_docID").and_then(Value::as_str) {
                return Ok(doc_id.to_string());
            }
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for AgentRuntime _docID for {agent_did}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn escape_graphql_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

async fn wait_for_tool_call(graphql: &str, session_id: &str, tool_name: &str) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentToolCall(
                        filter: {{
                            session_id: {{ _eq: "{}" }},
                            tool_name: {{ _eq: "{}" }}
                        }},
                        order: {{ started_at: DESC }},
                        limit: 1
                    ) {{
                        tool_name
                        args
                        result
                        status
                    }}
                }}"#,
                escape_graphql_string(session_id),
                escape_graphql_string(tool_name),
            ),
        )
        .await?;
        if let Ok(row) = first_graphql_row(&response, "AgentToolCall") {
            let status = row
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if status == "completed" {
                return Ok(row.clone());
            }
        }

        if Instant::now() >= deadline {
            bail!("timed out waiting for AgentToolCall {tool_name} in session {session_id}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequestData> {
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
    let body_end = header_end + content_length;
    let body = if buffer.len() >= body_end {
        buffer[header_end..body_end].to_vec()
    } else {
        Vec::new()
    };

    Ok(HttpRequestData { method, path, body })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            value.trim().parse().ok()
        } else {
            None
        }
    })
}

fn write_http_response(
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

fn tool_call_sse(tool_name: &str, arguments: &str) -> String {
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

fn completion_text_sse(text: &str) -> String {
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

fn request_has_tool_result_message(request: &Value) -> bool {
    request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages
                .iter()
                .any(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        })
}

fn request_tool_names(request: &Value) -> Vec<String> {
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

fn request_system_message(request: &Value) -> Option<&str> {
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

fn request_tool_result_text(request: &Value) -> Option<String> {
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
