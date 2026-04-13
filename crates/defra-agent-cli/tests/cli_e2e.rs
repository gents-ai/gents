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
    headers: std::collections::HashMap<String, String>,
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
        Self::start_with_required_bearer(model_name, None)
    }

    fn start_with_required_bearer(model_name: &str, required_bearer: Option<&str>) -> Result<Self> {
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
        Self::start_with_required_bearer(model_name, final_text, None)
    }

    fn start_with_required_bearer(
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

#[test]
fn top_level_help_shows_quickstart_workflow() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = run_cli_text(&home_dir, &["--help"])?;
    assert!(
        output.contains("Quick start:"),
        "expected quick start section in help output, got:\n{output}"
    );
    assert!(
        output.contains("defra-agent init http://HOST:PORT/v1 --model-name MODEL"),
        "expected init example in help output, got:\n{output}"
    );
    assert!(
        output.contains("defra-agent server"),
        "expected server example in help output, got:\n{output}"
    );
    assert!(
        output.contains("defra-agent chat"),
        "expected chat example in help output, got:\n{output}"
    );

    Ok(())
}

#[test]
fn status_without_runtime_suggests_init_and_server() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);

    let stderr = run_cli_failure_stderr(&home_dir, &["status", "--graphql", &graphql])?;
    assert!(
        stderr.contains("defra-agent init <INFERENCE_ENDPOINT> --model-name <MODEL_NAME>"),
        "expected init suggestion in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("defra-agent server"),
        "expected server suggestion in stderr, got:\n{stderr}"
    );
    assert!(
        stderr.contains("defra-agent status"),
        "expected status suggestion in stderr, got:\n{stderr}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduled_task_set_persists_concrete_default_behavior_id() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-task-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-task-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
    let task_id = format!("task-{}", Uuid::new_v4().simple());
    let default_behavior_id = format!("{agent_did}:default");

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
            "task",
            "set",
            "--task-id",
            &task_id,
            "--name",
            "daily-check",
            "--prompt",
            "Check the repo health.",
            "--interval-secs",
            "600",
        ],
    )?;
    assert_eq!(
        output.get("behavior_id").and_then(Value::as_str),
        Some(default_behavior_id.as_str())
    );

    let query = format!(
        r#"{{
            ScheduledTask(filter: {{ task_id: {{ _eq: "{}" }} }}, limit: 1) {{
                task_id
                agent_did
                behavior_id
                name
                prompt
                interval_secs
                enabled
                next_run_at
            }}
        }}"#,
        escape_graphql_string(&task_id),
    );
    let response = graphql_query(&graphql, &query).await?;
    let row = first_graphql_row(&response, "ScheduledTask")?;
    assert_eq!(
        row.get("task_id").and_then(Value::as_str),
        Some(task_id.as_str())
    );
    assert_eq!(
        row.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        row.get("behavior_id").and_then(Value::as_str),
        Some(default_behavior_id.as_str())
    );
    assert_eq!(row.get("name").and_then(Value::as_str), Some("daily-check"));
    assert_eq!(
        row.get("prompt").and_then(Value::as_str),
        Some("Check the repo health.")
    );
    assert_eq!(row.get("interval_secs").and_then(Value::as_i64), Some(600));
    assert_eq!(row.get("enabled").and_then(Value::as_bool), Some(true));
    assert!(row.get("next_run_at").is_some());

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnose_works_from_local_home_without_server() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-diagnose-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-diagnose-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");

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

    let output = run_cli_json(&home_dir, &["diagnose"])?;
    assert_eq!(output.get("status").and_then(Value::as_str), Some("ok"));
    assert_eq!(
        output.get("access_mode").and_then(Value::as_str),
        Some("local")
    );
    assert_eq!(
        output.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        output.get("graphql_reachable").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        output
            .pointer("/checks/default_behavior/ok")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        output
            .pointer("/checks/tool_ceiling/ok")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        output
            .pointer("/checks/backends/0/ok")
            .and_then(Value::as_bool),
        Some(true)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_accepts_normalized_manifest_root() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&root)?;

    let agent_did = format!("did:defra-agent:{}", Uuid::new_v4().simple());
    let default_behavior_id = format!("{agent_did}:default");
    let tool_selection_id = format!("{default_behavior_id}:tools");

    write_json_file(
        &root.join("agent-principal.json"),
        &serde_json::json!({
            "agent_did": agent_did.clone(),
            "display_name": "Default Agent",
            "default_behavior_id": default_behavior_id.clone(),
            "enabled": true
        }),
    )?;
    write_json_file(
        &root.join("agent-behaviors.json"),
        &serde_json::json!([
            {
                "behavior_id": default_behavior_id.clone(),
                "agent_did": agent_did.clone(),
                "display_name": "Default",
                "system_prompt": "Keep responses short.",
                "backend_id": "default-backend",
                "model_name": "mock-model",
                "tool_selection_id": tool_selection_id.clone(),
                "inference_profile_id": null,
                "compaction_strategy": null,
                "compaction_threshold": null,
                "enabled": true
            }
        ]),
    )?;
    write_json_file(
        &root.join("tool-selections.json"),
        &serde_json::json!([
            {
                "selection_id": tool_selection_id.clone(),
                "agent_did": agent_did.clone(),
                "display_name": "Standard",
                "enable_file_tools": true,
                "file_tools_mode": "ReadOnly",
                "enable_bash": true,
                "bash_mode": "ReadOnly",
                "cli_tool_names": [],
                "enable_meta_tools": true,
                "delegate_to": []
            }
        ]),
    )?;
    write_json_file(
        &root.join("inference-backends.json"),
        &serde_json::json!([
            {
                "backend_id": "default-backend",
                "name": "default-backend",
                "endpoint": "http://127.0.0.1:8000/v1",
                "api_key_env_var": "AGENT_DAEMON_API_KEY",
                "max_concurrent": 1,
                "enabled": true,
                "models": ["mock-model"]
            }
        ]),
    )?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "validate",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    assert_eq!(
        output.get("status").and_then(Value::as_str),
        Some("validated")
    );
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        output.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        output
            .pointer("/counts/agent_principal")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/agent_behaviors")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/tool_selections")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/inference_backends")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/inference_profiles")
            .and_then(Value::as_u64),
        Some(0)
    );
    assert_eq!(
        output.get("errors").and_then(Value::as_array).map(Vec::len),
        Some(0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_validate_reports_reference_errors_and_fails_nonzero() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("broken");
    fs::create_dir_all(&home_dir)?;
    fs::create_dir_all(&root)?;

    let agent_did = format!("did:defra-agent:{}", Uuid::new_v4().simple());

    write_json_file(
        &root.join("agent-principal.json"),
        &serde_json::json!({
            "agent_did": agent_did.clone(),
            "display_name": "Broken Agent",
            "default_behavior_id": format!("{agent_did}:default"),
            "enabled": true
        }),
    )?;
    write_json_file(
        &root.join("agent-behaviors.json"),
        &serde_json::json!([
            {
                "behavior_id": "other-behavior",
                "agent_did": agent_did.clone(),
                "display_name": "Other",
                "system_prompt": "Broken config.",
                "backend_id": "missing-backend",
                "model_name": "mock-model",
                "tool_selection_id": "missing-tools",
                "inference_profile_id": "missing-profile",
                "compaction_strategy": null,
                "compaction_threshold": null,
                "enabled": true
            }
        ]),
    )?;
    write_json_file(&root.join("tool-selections.json"), &serde_json::json!([]))?;
    write_json_file(
        &root.join("inference-backends.json"),
        &serde_json::json!([]),
    )?;

    let output = run_cli_failure_stdout_json(
        &home_dir,
        &[
            "config",
            "validate",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    assert_eq!(
        output.get("status").and_then(Value::as_str),
        Some("invalid")
    );
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(false));
    let errors = output
        .get("errors")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("validate output missing errors array: {output}"))?;
    let messages = errors
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        messages.contains("default_behavior_id"),
        "expected default behavior validation error, got:\n{messages}"
    );
    assert!(
        messages.contains("missing backend_id missing-backend"),
        "expected missing backend validation error, got:\n{messages}"
    );
    assert!(
        messages.contains("missing tool_selection_id missing-tools"),
        "expected missing tool selection validation error, got:\n{messages}"
    );
    assert!(
        messages.contains("missing inference_profile_id missing-profile"),
        "expected missing profile validation error, got:\n{messages}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_diff_reports_no_changes_for_matching_live_state() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-diff-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-diff-{}", Uuid::new_v4().simple());

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

    let exported = run_cli_json(&home_dir, &["config", "export"])?;
    write_manifest_root_from_export(&root, &exported)?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "diff",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    assert_eq!(output.get("status").and_then(Value::as_str), Some("diffed"));
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(
        output.get("access_mode").and_then(Value::as_str),
        Some("local")
    );
    assert_eq!(
        output
            .pointer("/counts/agent_principal/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/agent_behaviors/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/tool_selections/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/inference_backends/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/counts/inference_profiles/unchanged")
            .and_then(Value::as_u64),
        Some(0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_diff_reports_updates_for_changed_backend_manifest() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-diff-update-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-diff-update-{}", Uuid::new_v4().simple());

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

    let exported = run_cli_json(&home_dir, &["config", "export"])?;
    write_manifest_root_from_export(&root, &exported)?;

    let backends_path = root.join("inference-backends.json");
    let mut backends = read_json_file(&backends_path)?;
    backends[0]["endpoint"] = Value::String("http://127.0.0.1:9000/v1".to_string());
    write_json_file(&backends_path, &backends)?;

    let backend_id = exported
        .pointer("/inference_backends/0/backend_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("exported bundle missing inference_backends[0].backend_id"))?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "diff",
            "--root",
            root.to_str().expect("utf-8 manifest root"),
        ],
    )?;

    assert_eq!(output.get("status").and_then(Value::as_str), Some("diffed"));
    assert_eq!(output.get("ok").and_then(Value::as_bool), Some(false));
    assert_eq!(
        output
            .pointer("/counts/inference_backends/update")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        output
            .pointer("/collections/inference_backends/update/0")
            .and_then(Value::as_str),
        Some(backend_id)
    );
    assert_eq!(
        output
            .pointer("/counts/agent_behaviors/unchanged")
            .and_then(Value::as_u64),
        Some(1)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_apply_reconciles_running_runtime_without_restart() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    let root = tempdir.path().join("infra").join("agents").join("default");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-apply-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-apply-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");

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

    let exported = run_cli_json(&home_dir, &["config", "export"])?;
    write_manifest_root_from_export(&root, &exported)?;

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let behaviors_path = root.join("agent-behaviors.json");
    let mut behaviors = read_json_file(&behaviors_path)?;
    let updated_prompt = "Keep responses terse. Mention that desired state was applied.";
    behaviors[0]["system_prompt"] = Value::String(updated_prompt.to_string());
    write_json_file(&behaviors_path, &behaviors)?;

    let root_str = root
        .to_str()
        .ok_or_else(|| anyhow!("manifest root path is not UTF-8"))?;
    let applied = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(
        applied.get("status").and_then(Value::as_str),
        Some("applied")
    );
    assert_eq!(applied.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(applied.get("changed").and_then(Value::as_bool), Some(true));
    assert_eq!(
        applied
            .pointer("/applied/agent_behaviors")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        applied
            .pointer("/remaining/agent_behaviors/update")
            .and_then(Value::as_u64),
        Some(0)
    );

    let generation_after_apply =
        wait_for_runtime_quiescence(&graphql, &agent_did, 2, Duration::from_secs(6)).await?;
    let response = graphql_query(
        &graphql,
        &format!(
            r#"{{
                AgentBehavior(
                    filter: {{ agent_did: {{ _eq: "{}" }} }},
                    limit: 1
                ) {{
                    system_prompt
                }}
            }}"#,
            escape_graphql_string(&agent_did),
        ),
    )
    .await?;
    let behavior_row = first_graphql_row(&response, "AgentBehavior")?;
    assert_eq!(
        behavior_row.get("system_prompt").and_then(Value::as_str),
        Some(updated_prompt)
    );

    let noop = run_cli_json(&home_dir, &["config", "apply", "--root", root_str])?;
    assert_eq!(noop.get("status").and_then(Value::as_str), Some("noop"));
    assert_eq!(noop.get("ok").and_then(Value::as_bool), Some(true));
    assert_eq!(noop.get("changed").and_then(Value::as_bool), Some(false));
    assert_eq!(
        noop.pointer("/applied/agent_behaviors")
            .and_then(Value::as_u64),
        Some(0)
    );

    let generation_after_noop = wait_for_runtime_quiescence(
        &graphql,
        &agent_did,
        generation_after_apply,
        Duration::from_secs(3),
    )
    .await?;
    assert_eq!(generation_after_noop, generation_after_apply);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_exposes_prometheus_metrics_endpoint() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-metrics-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-metrics-{}", Uuid::new_v4().simple());
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

    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/metrics"))
        .send()
        .await
        .context("fetching /metrics")?;
    assert!(
        response.status().is_success(),
        "unexpected status: {response:?}"
    );
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("text/plain"),
        "unexpected content-type: {content_type}"
    );
    let body = response.text().await.context("reading /metrics body")?;
    assert!(
        body.contains("# HELP defra_agent_up"),
        "expected defra_agent_up help text in metrics body:\n{body}"
    );
    assert!(
        body.contains(r#"defra_agent_up 1"#),
        "expected defra_agent_up sample in metrics body:\n{body}"
    );
    assert!(
        body.contains(&format!(
            r#"defra_agent_runtime_process_state{{agent_did="{agent_did}",state="ready"}} 1"#
        )),
        "expected ready process-state metric in metrics body:\n{body}"
    );
    assert!(
        body.contains(&format!(
            r#"defra_agent_runtime_active_generation{{agent_did="{agent_did}"}}"#
        )),
        "expected active-generation metric in metrics body:\n{body}"
    );
    assert!(
        body.contains("defra_agent_backend_enabled"),
        "expected backend metrics in metrics body:\n{body}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_export_import_round_trips_offline_and_requires_override() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let source_home = tempdir.path().join("source-home");
    let target_home = tempdir.path().join("target-home");
    fs::create_dir_all(&source_home)?;
    fs::create_dir_all(&target_home)?;

    let model_name = format!("mock-export-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-export-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let export_path = tempdir.path().join("agent-config.json");

    run_init_json(
        &source_home,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;

    let exported = run_cli_json(&source_home, &["config", "export"])?;
    assert_eq!(
        exported.get("format").and_then(Value::as_str),
        Some("defra-agent-config/v1")
    );
    assert_eq!(
        exported.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        exported
            .pointer("/agent_behaviors")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        exported
            .pointer("/tool_selections")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        exported
            .pointer("/inference_backends")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    fs::write(&export_path, serde_json::to_vec_pretty(&exported)?)
        .context("writing config export fixture")?;

    let imported = run_cli_json(
        &target_home,
        &[
            "config",
            "import",
            export_path.to_str().expect("utf-8 export path"),
        ],
    )?;
    assert_eq!(
        imported.get("status").and_then(Value::as_str),
        Some("imported")
    );
    assert_eq!(
        imported
            .pointer("/counts/agent_principal")
            .and_then(Value::as_u64),
        Some(1)
    );
    assert_eq!(
        imported
            .pointer("/counts/agent_behaviors")
            .and_then(Value::as_u64),
        Some(1)
    );

    let reexported = run_cli_json(
        &target_home,
        &["config", "export", "--agent-did", &agent_did],
    )?;
    assert_eq!(
        reexported.get("agent_did").and_then(Value::as_str),
        Some(agent_did.as_str())
    );
    assert_eq!(
        reexported
            .pointer("/agent_principal/default_behavior_id")
            .and_then(Value::as_str),
        exported
            .pointer("/agent_principal/default_behavior_id")
            .and_then(Value::as_str)
    );
    assert_eq!(
        reexported
            .pointer("/agent_behaviors/0/behavior_id")
            .and_then(Value::as_str),
        exported
            .pointer("/agent_behaviors/0/behavior_id")
            .and_then(Value::as_str)
    );

    let stderr = run_cli_failure_stderr(
        &target_home,
        &[
            "config",
            "import",
            export_path.to_str().expect("utf-8 export path"),
        ],
    )?;
    assert!(
        stderr.contains("defra-agent config import --override"),
        "expected override guidance in stderr, got:\n{stderr}"
    );

    let overridden = run_cli_json(
        &target_home,
        &[
            "config",
            "import",
            export_path.to_str().expect("utf-8 export path"),
            "--override",
        ],
    )?;
    assert_eq!(
        overridden.get("override").and_then(Value::as_bool),
        Some(true)
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
async fn request_submit_supports_content_file_and_output_file() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-submit-file-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-submit-file-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let request_agent_did = format!("did:defra-agent:request-file-{}", Uuid::new_v4().simple());
    let request_content = format!("CLI file request {}", Uuid::new_v4());
    let expected_content = format!("wait-file-ok-{}", Uuid::new_v4().simple());
    let content_path = tempdir.path().join("request.txt");
    let output_path = tempdir.path().join("request-output.json");
    fs::write(&content_path, &request_content)?;

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
            "--content-file",
            content_path
                .to_str()
                .ok_or_else(|| anyhow!("content path is not utf-8"))?,
            "--output-file",
            output_path
                .to_str()
                .ok_or_else(|| anyhow!("output path is not utf-8"))?,
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

    let stdout_json: Value =
        serde_json::from_slice(&output.stdout).context("parsing request submit JSON")?;
    let file_json = read_json_file(&output_path)?;
    assert_eq!(stdout_json, file_json);
    assert_eq!(
        stdout_json
            .pointer("/response/content")
            .and_then(Value::as_str),
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
async fn chat_continues_existing_session_when_session_id_is_provided() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("chat-continue-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-chat-continue-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-chat-continue-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
    let first_prompt = format!("Remember the token {}.", Uuid::new_v4().simple());
    let second_prompt = "What token did I tell you to remember?";

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

    let first_stdout = run_cli_text(&home_dir, &["chat", &first_prompt])?;
    assert!(
        first_stdout.contains(&expected_reply),
        "expected first chat turn to contain {expected_reply}, got:\n{first_stdout}"
    );

    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&graphql, &agent_did, &first_prompt).await?;

    let second_stdout = run_cli_text(
        &home_dir,
        &["chat", "--session-id", &session_id, second_prompt],
    )?;
    assert!(
        second_stdout.contains(&expected_reply),
        "expected follow-up chat turn to contain {expected_reply}, got:\n{second_stdout}"
    );

    let captured_requests = mock_endpoint.captured_chat_requests();
    assert_eq!(captured_requests.len(), 2);
    assert!(
        request_contains_role_text(&captured_requests[1], "user", &first_prompt),
        "expected follow-up request to include prior user turn: {}",
        captured_requests[1]
    );
    assert!(
        request_contains_role_text(&captured_requests[1], "assistant", &expected_reply),
        "expected follow-up request to include prior assistant turn: {}",
        captured_requests[1]
    );
    assert!(
        request_contains_role_text(&captured_requests[1], "user", second_prompt),
        "expected follow-up request to include current user turn: {}",
        captured_requests[1]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_supports_message_file_json_output_and_output_file() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("chat-json-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-chat-json-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let port = allocate_port()?;
    let agent_name = format!("cli-chat-json-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);
    let message = format!("Reply with exactly {}.", Uuid::new_v4().simple());
    let message_path = tempdir.path().join("chat-message.txt");
    let output_path = tempdir.path().join("chat-output.json");
    fs::write(&message_path, &message)?;

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
            "chat",
            "--message-file",
            message_path
                .to_str()
                .ok_or_else(|| anyhow!("message path is not utf-8"))?,
            "--output-format",
            "json",
            "--output-file",
            output_path
                .to_str()
                .ok_or_else(|| anyhow!("output path is not utf-8"))?,
        ],
    )?;

    let file_output = read_json_file(&output_path)?;
    assert_eq!(output, file_output);
    assert!(
        output.get("request_id").and_then(Value::as_str).is_some(),
        "chat json output should include request_id: {output}"
    );
    assert!(
        output.get("session_id").and_then(Value::as_str).is_some(),
        "chat json output should include session_id: {output}"
    );
    assert_eq!(
        output.pointer("/response/status").and_then(Value::as_str),
        Some("complete")
    );
    assert_eq!(
        output.pointer("/response/content").and_then(Value::as_str),
        Some(expected_reply.as_str())
    );

    let captured_requests = mock_endpoint.captured_chat_requests();
    assert_eq!(captured_requests.len(), 1);
    assert!(
        request_contains_role_text(&captured_requests[0], "user", &message),
        "expected request to include message file content: {}",
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
        "OpenAiCompatible",
        None,
        None,
        true,
        true,
        false,
        false,
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
        "OpenAiCompatible",
        None,
        None,
        true,
        true,
        false,
        false,
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
async fn init_and_server_use_backend_specific_api_key_env_var() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-auth-model-{}", Uuid::new_v4().simple());
    let expected_reply = "AUTH_BACKEND_OK";
    let mock_endpoint = MockChatEndpoint::start_with_required_bearer(
        &model_name,
        expected_reply,
        Some("backend-key"),
    )?;

    let port = allocate_port()?;
    let agent_name = format!("cli-auth-{}", Uuid::new_v4().simple());
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
            "--api-key-env-var",
            "DEFRA_AGENT_TEST_CLI_BACKEND_KEY",
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        init.pointer("/init/api_key_env_var")
            .and_then(Value::as_str),
        Some("DEFRA_AGENT_TEST_CLI_BACKEND_KEY")
    );

    let mut serve = spawn_server_with_env(
        &home_dir,
        port,
        &[],
        &[("DEFRA_AGENT_TEST_CLI_BACKEND_KEY", "backend-key")],
    )?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenAiCompatible",
        None,
        Some("DEFRA_AGENT_TEST_CLI_BACKEND_KEY"),
        true,
        true,
        false,
        false,
        &model_name,
        &tool_selection_id,
        "ReadOnly",
        "ReadOnly",
        "read-only operating mode",
    )
    .await?;

    let output = run_cli_text(
        &home_dir,
        &[
            "chat",
            "backend auth should flow through the configured env var",
        ],
    )?;
    assert!(
        output.contains(expected_reply),
        "expected chat output to contain {expected_reply}, got:\n{output}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_supports_provider_auth_and_capability_backend_fields() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-openrouter-model-{}", Uuid::new_v4().simple());
    let raw_api_key = "openrouter-raw-key";
    let mock_endpoint = MockChatEndpoint::start_with_required_bearer(
        &model_name,
        "OPENROUTER_BACKEND_OK",
        Some(raw_api_key),
    )?;

    let port = allocate_port()?;
    let agent_name = format!("cli-openrouter-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let backend_id = format!("{agent_name}-backend");
    let graphql = graphql_url(port);
    let tool_selection_id = format!("{agent_did}:default:tools");

    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--provider-kind",
            "OpenRouter",
            "--api-key",
            raw_api_key,
            "--supports-tool-calls",
            "true",
            "--supports-streaming",
            "true",
            "--supports-structured-outputs",
            "true",
            "--supports-json-schema",
            "true",
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    assert_eq!(
        init.pointer("/init/provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );
    assert_eq!(
        init.pointer("/init/api_key").and_then(Value::as_str),
        Some("<redacted>")
    );
    assert_eq!(
        init.pointer("/init/supports_tool_calls")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        init.pointer("/init/supports_structured_outputs")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        init.pointer("/init/supports_json_schema")
            .and_then(Value::as_bool),
        Some(true)
    );

    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    assert_runtime_init_state(
        &graphql,
        &agent_did,
        &backend_id,
        mock_endpoint.endpoint(),
        "OpenRouter",
        Some(raw_api_key),
        None,
        true,
        true,
        true,
        true,
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
        output.get("process_state").and_then(Value::as_str),
        Some("ready")
    );
    assert_eq!(
        output.get("reconcile_phase").and_then(Value::as_str),
        Some("idle")
    );
    assert_eq!(
        output
            .get("runnable_behavior_count")
            .and_then(Value::as_i64),
        Some(1)
    );
    assert_eq!(
        output.get("graphql").and_then(Value::as_str),
        Some(graphql.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_openrouter_preset_applies_hosted_defaults() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let agent_name = format!("cli-openrouter-preset-{}", Uuid::new_v4().simple());
    let model_name = format!("openrouter-model-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--backend-preset",
            "openrouter",
            "--model-name",
            &model_name,
        ],
    )?;

    assert_eq!(
        init.pointer("/init/provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );
    assert_eq!(
        init.pointer("/init/endpoint").and_then(Value::as_str),
        Some("https://openrouter.ai/api/v1")
    );
    assert_eq!(
        init.pointer("/init/api_key_env_var")
            .and_then(Value::as_str),
        Some("OPENROUTER_API_KEY")
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
async fn init_rejects_setting_both_api_key_and_api_key_env_var() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let output = Command::new(cli_bin())
        .env("HOME", &home_dir)
        .env("RUST_LOG", "error")
        .arg("init")
        .arg("--model-name")
        .arg("test-model")
        .arg("--api-key")
        .arg("raw-key")
        .arg("--api-key-env-var")
        .arg("TEST_BACKEND_KEY")
        .arg("http://127.0.0.1:65535/v1")
        .output()
        .context("running defra-agent init with conflicting backend auth flags")?;

    assert!(!output.status.success(), "init should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("provide either --api-key or --api-key-env-var, not both"),
        "expected conflicting auth error, got:\n{stderr}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_backend_discover_models_supports_explicit_preset_probe() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("discover-openrouter-{}", Uuid::new_v4().simple());
    let raw_api_key = "discover-openrouter-key";
    let mock_endpoint =
        MockModelEndpoint::start_with_required_bearer(&model_name, Some(raw_api_key))?;

    let output = run_cli_json(
        &home_dir,
        &[
            "config",
            "backend",
            "discover-models",
            "--backend-preset",
            "openrouter",
            "--endpoint",
            mock_endpoint.endpoint(),
            "--api-key",
            raw_api_key,
        ],
    )?;

    assert_eq!(
        output.get("provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );
    assert_eq!(
        output.get("endpoint").and_then(Value::as_str),
        Some(mock_endpoint.endpoint())
    );
    assert_eq!(
        output
            .pointer("/discovered_models/0")
            .and_then(Value::as_str),
        Some(model_name.as_str())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_backend_set_preset_and_discover_models_from_backend_id() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let bootstrap_model = format!("bootstrap-model-{}", Uuid::new_v4().simple());
    let bootstrap_endpoint = MockModelEndpoint::start(&bootstrap_model)?;
    let discover_model = format!("discover-backend-id-{}", Uuid::new_v4().simple());
    let discover_api_key = "stored-openrouter-key";
    let discover_endpoint =
        MockModelEndpoint::start_with_required_bearer(&discover_model, Some(discover_api_key))?;

    let port = allocate_port()?;
    let agent_name = format!("cli-backend-preset-{}", Uuid::new_v4().simple());
    let agent_did = format!("did:defra-agent:{agent_name}");
    let graphql = graphql_url(port);

    run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &bootstrap_model,
            bootstrap_endpoint.endpoint(),
        ],
    )?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve.child)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let backend_id = format!("{agent_name}-openrouter");
    let upsert = run_cli_json(
        &home_dir,
        &[
            "config",
            "backend",
            "set",
            "--graphql",
            &graphql,
            "--backend-id",
            &backend_id,
            "--name",
            "OpenRouter",
            "--backend-preset",
            "openrouter",
            "--endpoint",
            discover_endpoint.endpoint(),
            "--api-key",
            discover_api_key,
            "--max-concurrent",
            "2",
        ],
    )?;

    assert_eq!(
        upsert.get("backend_preset").and_then(Value::as_str),
        Some("openrouter")
    );
    assert_eq!(
        upsert.get("provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );
    assert_eq!(
        upsert.get("endpoint").and_then(Value::as_str),
        Some(discover_endpoint.endpoint())
    );

    let discover = run_cli_json(
        &home_dir,
        &[
            "config",
            "backend",
            "discover-models",
            "--graphql",
            &graphql,
            "--backend-id",
            &backend_id,
        ],
    )?;

    assert_eq!(
        discover
            .pointer("/discovered_models/0")
            .and_then(Value::as_str),
        Some(discover_model.as_str())
    );
    assert_eq!(
        discover.get("provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );

    let backend_rows = graphql_query(
        &graphql,
        &format!(
            r#"{{
                InferenceBackend(filter: {{ backend_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    backend_id
                    provider_kind
                    endpoint
                    api_key
                    api_key_env_var
                }}
            }}"#,
            escape_graphql_string(&backend_id),
        ),
    )
    .await?;
    let backend = first_graphql_row(&backend_rows, "InferenceBackend")?;
    assert_eq!(
        backend.get("provider_kind").and_then(Value::as_str),
        Some("OpenRouter")
    );
    assert_eq!(
        backend.get("endpoint").and_then(Value::as_str),
        Some(discover_endpoint.endpoint())
    );
    assert_eq!(
        backend.get("api_key").and_then(Value::as_str),
        Some(discover_api_key)
    );
    assert_eq!(backend.get("api_key_env_var").and_then(Value::as_str), None);

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
        "OpenAiCompatible",
        None,
        None,
        true,
        true,
        false,
        false,
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
        "OpenAiCompatible",
        None,
        None,
        true,
        true,
        false,
        false,
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
    spawn_server_with_env(home_dir, port, &[], &[])
}

fn spawn_server_with_env(
    home_dir: &Path,
    port: u16,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> Result<ServeProcess> {
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", home_dir)
        .env("RUST_LOG", "error")
        .current_dir(home_dir)
        .arg("server")
        .arg("--http-port")
        .arg(port.to_string())
        .args(extra_args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (name, value) in envs {
        command.env(name, value);
    }
    let child = command.spawn().context("spawning defra-agent server")?;
    Ok(ServeProcess { child })
}

async fn assert_runtime_init_state(
    graphql: &str,
    agent_did: &str,
    backend_id: &str,
    endpoint: &str,
    expected_provider_kind: &str,
    expected_api_key: Option<&str>,
    expected_api_key_env_var: Option<&str>,
    expected_supports_tool_calls: bool,
    expected_supports_streaming: bool,
    expected_supports_structured_outputs: bool,
    expected_supports_json_schema: bool,
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
                provider_kind
                endpoint
                api_key
                api_key_env_var
                enabled
                supports_tool_calls
                supports_streaming
                supports_structured_outputs
                supports_json_schema
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
    assert_eq!(
        backend.get("provider_kind").and_then(Value::as_str),
        Some(expected_provider_kind)
    );
    assert_eq!(
        backend.get("api_key").and_then(Value::as_str),
        expected_api_key
    );
    assert_eq!(
        backend.get("api_key_env_var").and_then(Value::as_str),
        expected_api_key_env_var
    );
    assert_eq!(backend.get("enabled").and_then(Value::as_bool), Some(true));
    assert_eq!(
        backend.get("supports_tool_calls").and_then(Value::as_bool),
        Some(expected_supports_tool_calls)
    );
    assert_eq!(
        backend.get("supports_streaming").and_then(Value::as_bool),
        Some(expected_supports_streaming)
    );
    assert_eq!(
        backend
            .get("supports_structured_outputs")
            .and_then(Value::as_bool),
        Some(expected_supports_structured_outputs)
    );
    assert_eq!(
        backend.get("supports_json_schema").and_then(Value::as_bool),
        Some(expected_supports_json_schema)
    );
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
        .current_dir(home_dir)
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

fn run_cli_text(home_dir: &Path, args: &[&str]) -> Result<String> {
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

fn run_cli_failure_stderr(home_dir: &Path, args: &[&str]) -> Result<String> {
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

fn run_cli_failure_stdout_json(home_dir: &Path, args: &[&str]) -> Result<Value> {
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

fn write_json_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory {}", parent.display()))?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("writing JSON file {}", path.display()))?;
    Ok(())
}

fn read_json_file(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("reading JSON file {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("decoding JSON file {}", path.display()))
}

fn write_manifest_root_from_export(root: &Path, exported: &Value) -> Result<()> {
    write_json_file(
        &root.join("agent-principal.json"),
        &project_object_fields(
            exported
                .get("agent_principal")
                .ok_or_else(|| anyhow!("exported bundle missing agent_principal"))?,
            &[
                "agent_did",
                "display_name",
                "default_behavior_id",
                "enabled",
            ],
        )?,
    )?;
    write_json_file(
        &root.join("agent-behaviors.json"),
        &project_array_fields(
            exported
                .get("agent_behaviors")
                .ok_or_else(|| anyhow!("exported bundle missing agent_behaviors"))?,
            &[
                "behavior_id",
                "agent_did",
                "display_name",
                "system_prompt",
                "backend_id",
                "model_name",
                "tool_selection_id",
                "inference_profile_id",
                "compaction_strategy",
                "compaction_threshold",
                "enabled",
            ],
        )?,
    )?;
    write_json_file(
        &root.join("tool-selections.json"),
        &project_array_fields(
            exported
                .get("tool_selections")
                .ok_or_else(|| anyhow!("exported bundle missing tool_selections"))?,
            &[
                "selection_id",
                "agent_did",
                "display_name",
                "enable_file_tools",
                "file_tools_mode",
                "enable_bash",
                "bash_mode",
                "cli_tool_names",
                "enable_meta_tools",
                "delegate_to",
            ],
        )?,
    )?;
    write_json_file(
        &root.join("inference-backends.json"),
        &project_array_fields(
            exported
                .get("inference_backends")
                .ok_or_else(|| anyhow!("exported bundle missing inference_backends"))?,
            &[
                "backend_id",
                "name",
                "endpoint",
                "api_key_env_var",
                "max_concurrent",
                "enabled",
                "models",
            ],
        )?,
    )?;

    let inference_profiles = project_array_fields(
        exported
            .get("inference_profiles")
            .ok_or_else(|| anyhow!("exported bundle missing inference_profiles"))?,
        &[
            "profile_id",
            "display_name",
            "context_window",
            "max_output_tokens",
            "max_turns",
            "temperature",
            "stream_batch_ms",
            "deadline_duration_secs",
        ],
    )?;
    if inference_profiles
        .as_array()
        .is_some_and(|rows| !rows.is_empty())
    {
        write_json_file(&root.join("inference-profiles.json"), &inference_profiles)?;
    }

    Ok(())
}

fn project_array_fields(value: &Value, fields: &[&str]) -> Result<Value> {
    let rows = value
        .as_array()
        .ok_or_else(|| anyhow!("expected array while projecting manifest fields: {value}"))?;
    Ok(Value::Array(
        rows.iter()
            .map(|row| project_object_fields(row, fields))
            .collect::<Result<Vec<_>>>()?,
    ))
}

fn project_object_fields(value: &Value, fields: &[&str]) -> Result<Value> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("expected object while projecting manifest fields: {value}"))?;
    let mut projected = serde_json::Map::new();
    for field in fields {
        if let Some(value) = object.get(*field) {
            projected.insert((*field).to_string(), value.clone());
        }
    }
    Ok(Value::Object(projected))
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

fn request_contains_role_text(request: &Value, role: &str, needle: &str) -> bool {
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
