use super::*;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::admission::{AdmissionRegistry, BackendAdmissionConfig, CallKind};
use crate::compaction::CompactionStrategy;
use crate::config::{
    BehaviorConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS,
    DEFAULT_STREAM_BATCH_MS,
};
use crate::ensure_runtime_schemas;
use crate::identity::SimpleIdentity;
use crate::tool_surface::{BehaviorToolConfig, ToolRuntimeContext};
use crate::BackendProviderKind;

fn test_admission_registry(
    node: Arc<EmbeddedNode>,
    backend_id: &str,
    max_concurrent: usize,
) -> AdmissionRegistry {
    let registry = AdmissionRegistry::new(node);
    registry.reconcile(
        1,
        &std::collections::HashMap::from([(
            backend_id.to_string(),
            BackendAdmissionConfig {
                backend_id: backend_id.to_string(),
                max_concurrent,
                max_queue_depth: 100,
                enabled: true,
                probe_status: "healthy".to_string(),
                config_fingerprint: format!("{backend_id}:{max_concurrent}:100"),
            },
        )]),
    );
    registry
}

#[test]
fn scheduled_task_parses_from_json() {
    let json = serde_json::json!({
        "_docID": "abc123",
        "task_id": "seed-fleet-health",
        "name": "fleet-health-daily",
        "behavior_id": "amy-general",
        "prompt": "Check fleet health",
        "interval_secs": 86400,
        "enabled": true,
        "next_run_at": null,
        "run_count": 0,
    });

    let task = ScheduledTask {
        doc_id: json["_docID"].as_str().unwrap_or_default().to_string(),
        task_id: json["task_id"].as_str().unwrap_or_default().to_string(),
        name: json["name"].as_str().unwrap_or_default().to_string(),
        behavior_id: json["behavior_id"].as_str().unwrap_or_default().to_string(),
        prompt: json["prompt"].as_str().unwrap_or_default().to_string(),
        interval_secs: json["interval_secs"].as_i64().unwrap_or(3600),
        enabled: json["enabled"].as_bool().unwrap_or(false),
        next_run_at: json["next_run_at"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc)),
        run_count: json["run_count"].as_i64().unwrap_or(0),
    };

    assert_eq!(task.doc_id, "abc123");
    assert_eq!(task.name, "fleet-health-daily");
    assert_eq!(task.interval_secs, 86400);
    assert!(task.next_run_at.is_none());
    assert_eq!(task.run_count, 0);
}

#[test]
fn runtime_context_format() {
    let now = "2026-04-02T14:30:00Z";
    let host = "studio-1";
    let name = "fleet-health-daily";
    let run_count = 5;

    let context = format!(
        "Current time: {}\nHost: {}\nTask: {} (run #{})\n\n",
        now,
        host,
        name,
        run_count + 1,
    );

    assert!(context.contains("Current time: 2026-04-02T14:30:00Z"));
    assert!(context.contains("Host: studio-1"));
    assert!(context.contains("Task: fleet-health-daily (run #6)"));
}

#[test]
fn scheduled_task_from_value_parses() {
    let json = serde_json::json!({
        "_docID": "doc1",
        "task_id": "task-1",
        "name": "test-task",
        "behavior_id": "general",
        "prompt": "Do something",
        "interval_secs": 3600,
        "enabled": true,
        "next_run_at": null,
        "run_count": 3,
    });

    let task = ScheduledTask::from_value(&json).expect("should parse");
    assert_eq!(task.doc_id, "doc1");
    assert_eq!(task.task_id, "task-1");
    assert_eq!(task.name, "test-task");
    assert_eq!(task.behavior_id, "general");
    assert_eq!(task.prompt, "Do something");
    assert_eq!(task.interval_secs, 3600);
    assert!(task.enabled);
    assert!(task.next_run_at.is_none());
    assert_eq!(task.run_count, 3);
}

#[test]
fn scheduled_task_from_value_rejects_missing_behavior_id() {
    let json = serde_json::json!({
        "_docID": "doc2",
        "task_id": "task-2",
        "name": "timed-task",
        "prompt": "Run check",
        "interval_secs": 600,
        "enabled": true,
        "next_run_at": "2026-04-02T14:30:00Z",
        "run_count": 1,
    });

    let error = ScheduledTask::from_value(&json).expect_err("should reject");
    assert!(error.to_string().contains("behavior_id"));
}

#[test]
fn scheduled_task_from_value_rejects_invalid_timestamp() {
    let json = serde_json::json!({
        "_docID": "doc3",
        "task_id": "task-3",
        "name": "bad-timestamp",
        "behavior_id": "code",
        "prompt": "Run check",
        "interval_secs": 600,
        "enabled": true,
        "next_run_at": "not-a-time",
        "run_count": 1,
    });

    let error = ScheduledTask::from_value(&json).expect_err("should reject");
    assert!(error.to_string().contains("next_run_at"));
}

#[test]
fn scheduled_task_from_value_with_timestamp() {
    let json = serde_json::json!({
        "_docID": "doc4",
        "task_id": "task-4",
        "name": "timed-task",
        "behavior_id": "general",
        "prompt": "Run check",
        "interval_secs": 600,
        "enabled": true,
        "next_run_at": "2026-04-02T14:30:00Z",
        "run_count": 1,
    });

    let task = ScheduledTask::from_value(&json).expect("should parse");
    assert!(task.next_run_at.is_some());
    let next = task.next_run_at.unwrap();
    assert_eq!(
        next.to_rfc3339_opts(SecondsFormat::Secs, true),
        "2026-04-02T14:30:00Z"
    );
}

#[test]
fn is_due_when_never_run() {
    let task = ScheduledTask {
        doc_id: "d".into(),
        task_id: "t".into(),
        name: "n".into(),
        behavior_id: "p".into(),
        prompt: "x".into(),
        interval_secs: 3600,
        enabled: true,
        next_run_at: None,
        run_count: 0,
    };
    assert!(task.is_due());
}

#[test]
fn is_due_when_past() {
    let past = Utc::now() - chrono::Duration::seconds(10);
    let task = ScheduledTask {
        doc_id: "d".into(),
        task_id: "t".into(),
        name: "n".into(),
        behavior_id: "p".into(),
        prompt: "x".into(),
        interval_secs: 3600,
        enabled: true,
        next_run_at: Some(past),
        run_count: 1,
    };
    assert!(task.is_due());
}

#[test]
fn not_due_when_future() {
    let future = Utc::now() + chrono::Duration::seconds(3600);
    let task = ScheduledTask {
        doc_id: "d".into(),
        task_id: "t".into(),
        name: "n".into(),
        behavior_id: "p".into(),
        prompt: "x".into(),
        interval_secs: 3600,
        enabled: true,
        next_run_at: Some(future),
        run_count: 1,
    };
    assert!(!task.is_due());
}

#[test]
fn task_timeout_is_fifteen_minutes() {
    assert_eq!(TASK_TIMEOUT_SECS, 900);
}

#[test]
fn not_due_when_disabled() {
    let task = ScheduledTask {
        doc_id: "d".into(),
        task_id: "t".into(),
        name: "n".into(),
        behavior_id: "p".into(),
        prompt: "x".into(),
        interval_secs: 3600,
        enabled: false,
        next_run_at: None,
        run_count: 0,
    };
    assert!(!task.is_due());
}

#[test]
fn missing_scheduled_task_collection_is_treated_as_empty() {
    assert!(is_missing_scheduled_task_collection_error(
        "query ScheduledTask failed: [QueryResponseError { message: \"collection not found: Cannot query collection 'ScheduledTask': collection not found\", path: None, locations: None }]"
    ));
    assert!(!is_missing_scheduled_task_collection_error(
        "query ScheduledTask failed: some other datastore error"
    ));
}

struct MockCompletionEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockCompletionEndpoint {
    fn start(model_name: &str, response_text: &str) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let model_name = model_name.to_string();
        let response_text = response_text.to_string();
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
                            ("POST", "/v1/chat/completions") => {
                                let body = serde_json::json!({
                                    "id": "chatcmpl-test",
                                    "object": "chat.completion",
                                    "created": 1_744_082_400u64,
                                    "model": model_name,
                                    "choices": [{
                                        "index": 0,
                                        "message": {
                                            "role": "assistant",
                                            "content": response_text,
                                        },
                                        "finish_reason": "stop",
                                        "logprobs": null,
                                    }],
                                    "usage": {
                                        "prompt_tokens": 10,
                                        "total_tokens": 15
                                    }
                                })
                                .to_string();
                                let _ = write_http_response(
                                    &mut stream,
                                    "200 OK",
                                    "application/json",
                                    &body,
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

impl Drop for MockCompletionEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct HttpRequestData {
    method: String,
    path: String,
}

fn read_http_request(stream: &mut TcpStream) -> anyhow::Result<HttpRequestData> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut buffer = Vec::new();
    let mut temp = [0u8; 1024];
    let header_end = loop {
        let read = stream.read(&mut temp)?;
        if read == 0 {
            anyhow::bail!("connection closed before headers");
        }
        buffer.extend_from_slice(&temp[..read]);
        if let Some(index) = find_subslice(&buffer, b"\r\n\r\n") {
            break index + 4;
        }
    };
    let header_text = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = header_text.split("\r\n").filter(|line| !line.is_empty());
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request method"))?
        .to_string();
    let path = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing request path"))?
        .to_string();

    Ok(HttpRequestData { method, path })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> anyhow::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

async fn insert_backend(node: &EmbeddedNode, backend_id: &str, endpoint: &str) {
    insert_backend_with_capacity(node, backend_id, endpoint, 1).await;
}

async fn insert_backend_with_capacity(
    node: &EmbeddedNode,
    backend_id: &str,
    endpoint: &str,
    max_concurrent: i64,
) {
    let mutation = format!(
        r#"mutation {{
            create_InferenceBackend(input: {{
                backend_id: "{backend_id}",
                name: "scheduler-backend",
                endpoint: "{endpoint}",
                max_concurrent: {max_concurrent},
                max_queue_depth: 100,
                enabled: true,
                models: ["scheduled-model"],
                probe_status: "healthy"
            }}) {{ _docID }}
        }}"#,
        backend_id = escape_graphql_string(backend_id),
        endpoint = escape_graphql_string(endpoint),
    );
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);
}

#[tokio::test]
async fn scheduled_execution_succeeds_without_external_ops_service() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let mock_endpoint = MockCompletionEndpoint::start("scheduled-model", "scheduled-ok").unwrap();
    insert_backend(node.as_ref(), "backend-1", mock_endpoint.endpoint()).await;

    let identity = Arc::new(SimpleIdentity::new(
        "scheduled-test",
        dir.path().join("identity.key"),
        None,
    ));
    let behavior = BehaviorConfig {
        name: "did:defra-agent:scheduled-test:default".to_string(),
        identity,
        backend_id: Some("backend-1".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: mock_endpoint.endpoint().to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: "scheduled-model".to_string(),
        context_window: DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: DEFAULT_MAX_TURNS,
        system_prompt: "You are a scheduler test agent.".to_string(),
        tools: BehaviorToolConfig::meta_only(),
        compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
        sampling: crate::config::SamplingConfig::default(),
    };

    let tool_surface = behavior.tools.resolve(node.as_ref()).await.unwrap();
    let tool_runtime = ToolRuntimeContext::oneshot(node.clone());
    let task = ScheduledTask {
        doc_id: "task-doc".to_string(),
        task_id: "task-1".to_string(),
        name: "nightly-check".to_string(),
        behavior_id: behavior.name.clone(),
        prompt: "Say scheduled-ok".to_string(),
        interval_secs: 60,
        enabled: true,
        next_run_at: None,
        run_count: 0,
    };

    super::execution::execute_task_standalone(
        &task,
        &behavior,
        &tool_surface,
        &tool_runtime,
        &node,
        test_admission_registry(node.clone(), "backend-1", 1),
        CancellationToken::new(),
    )
    .await
    .expect("scheduled execution should not depend on external ops service");
}

async fn insert_due_task(
    node: &EmbeddedNode,
    task_id: &str,
    behavior_id: &str,
    prompt: &str,
) -> String {
    let mutation = format!(
        r#"mutation {{
            create_ScheduledTask(input: {{
                task_id: "{task_id}",
                agent_did: "did:defra-agent:scheduled-test",
                behavior_id: "{behavior_id}",
                name: "blocked-task",
                prompt: "{prompt}",
                interval_secs: 60,
                enabled: true,
                next_run_at: "2026-04-10T00:00:00Z",
                run_count: 0
            }}) {{ _docID }}
        }}"#,
        task_id = escape_graphql_string(task_id),
        behavior_id = escape_graphql_string(behavior_id),
        prompt = escape_graphql_string(prompt),
    );
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);
    let data = resp.data.clone();
    ["create_ScheduledTask", "add_ScheduledTask"]
        .iter()
        .find_map(|field| {
            data.as_ref()
                .and_then(|data| data.get(*field))
                .and_then(|value| {
                    value
                        .get("_docID")
                        .and_then(serde_json::Value::as_str)
                        .or_else(|| {
                            value
                                .as_array()
                                .and_then(|rows| rows.first())
                                .and_then(|row| row.get("_docID"))
                                .and_then(serde_json::Value::as_str)
                        })
                })
        })
        .unwrap_or_else(|| panic!("ScheduledTask create should return _docID: {:?}", data))
        .to_string()
}

async fn query_task_row(
    node: &EmbeddedNode,
    task_id: &str,
    show_deleted: bool,
) -> Option<serde_json::Value> {
    let show_deleted_arg = if show_deleted {
        "showDeleted: true, "
    } else {
        ""
    };
    let query = format!(
        r#"query {{
            ScheduledTask(
                {show_deleted_arg}filter: {{ task_id: {{ _eq: "{task_id}" }} }},
                limit: 4
            ) {{
                _docID
                _deleted
                task_id
                name
                behavior_id
                prompt
                interval_secs
                enabled
                next_run_at
                last_run_at
                last_status
                last_error
                run_count
            }}
        }}"#,
        show_deleted_arg = show_deleted_arg,
        task_id = escape_graphql_string(task_id),
    );
    let response = node.execute(&query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response
        .data
        .as_ref()
        .and_then(|data| data.get("ScheduledTask"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
}

async fn delete_task(node: &EmbeddedNode, doc_id: &str) {
    let mutation = format!(
        r#"mutation {{ delete_ScheduledTask(docID: "{doc_id}") {{ _docID }} }}"#,
        doc_id = escape_graphql_string(doc_id),
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

#[tokio::test]
async fn scheduled_execution_updates_live_task_runtime_fields() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let mock_endpoint = MockCompletionEndpoint::start("scheduled-model", "scheduled-ok").unwrap();
    insert_backend(node.as_ref(), "backend-runtime", mock_endpoint.endpoint()).await;

    let identity = Arc::new(SimpleIdentity::new(
        "scheduled-test",
        dir.path().join("identity.key"),
        None,
    ));
    let behavior = BehaviorConfig {
        name: "did:defra-agent:scheduled-test:default".to_string(),
        identity,
        backend_id: Some("backend-runtime".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: mock_endpoint.endpoint().to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: "scheduled-model".to_string(),
        context_window: DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: DEFAULT_MAX_TURNS,
        system_prompt: "You are a scheduler test agent.".to_string(),
        tools: BehaviorToolConfig::meta_only(),
        compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
        sampling: crate::config::SamplingConfig::default(),
    };

    let tool_surface = behavior.tools.resolve(node.as_ref()).await.unwrap();
    let tool_runtime = ToolRuntimeContext::oneshot(node.clone());
    insert_due_task(
        node.as_ref(),
        "task-runtime-state",
        &behavior.name,
        "Say scheduled-ok",
    )
    .await;
    let task = ScheduledTask::from_value(
        &query_task_row(node.as_ref(), "task-runtime-state", false)
            .await
            .expect("task should exist"),
    )
    .expect("task row should parse");

    super::execution::execute_task_standalone(
        &task,
        &behavior,
        &tool_surface,
        &tool_runtime,
        &node,
        test_admission_registry(node.clone(), "backend-runtime", 1),
        CancellationToken::new(),
    )
    .await
    .expect("scheduled execution should succeed");

    let updated = query_task_row(node.as_ref(), "task-runtime-state", false)
        .await
        .expect("updated task should exist");
    assert_eq!(
        updated
            .get("last_status")
            .and_then(serde_json::Value::as_str),
        Some("success")
    );
    assert_eq!(
        updated
            .get("last_error")
            .and_then(serde_json::Value::as_str),
        Some("")
    );
    assert_eq!(
        updated.get("run_count").and_then(serde_json::Value::as_i64),
        Some(1)
    );
    let last_run_at = updated
        .get("last_run_at")
        .and_then(serde_json::Value::as_str)
        .expect("last_run_at should be set");
    let next_run_at = updated
        .get("next_run_at")
        .and_then(serde_json::Value::as_str)
        .expect("next_run_at should be set");
    let last_run = chrono::DateTime::parse_from_rfc3339(last_run_at).unwrap();
    let next_run = chrono::DateTime::parse_from_rfc3339(next_run_at).unwrap();
    assert!(next_run > last_run);
}

#[tokio::test]
async fn stale_runtime_bookkeeping_is_skipped_after_task_delete() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let doc_id = insert_due_task(
        node.as_ref(),
        "task-stale-delete",
        "did:defra-agent:scheduled-test:default",
        "Say scheduled-ok",
    )
    .await;
    let task = ScheduledTask::from_value(
        &query_task_row(node.as_ref(), "task-stale-delete", false)
            .await
            .expect("task should exist"),
    )
    .expect("task row should parse");

    delete_task(node.as_ref(), &doc_id).await;
    super::update_task_runtime_state(&node, &task, "success", None)
        .await
        .expect("deleted task bookkeeping should be skipped cleanly");

    assert!(
        query_task_row(node.as_ref(), "task-stale-delete", false)
            .await
            .is_none(),
        "deleted task should not reappear in live queries"
    );
    let deleted = query_task_row(node.as_ref(), "task-stale-delete", true)
        .await
        .expect("showDeleted should return tombstone");
    assert_eq!(
        deleted.get("_deleted").and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert!(deleted.get("last_status").is_none() || deleted.get("last_status").unwrap().is_null());
    assert_eq!(
        deleted.get("run_count").and_then(serde_json::Value::as_i64),
        Some(0)
    );
}

#[tokio::test]
async fn scheduler_tick_shutdown_is_prompt_while_task_waits_for_backend_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(node.as_ref()).await.unwrap();

    let mock_endpoint = MockCompletionEndpoint::start("scheduled-model", "scheduled-ok").unwrap();
    insert_backend_with_capacity(
        node.as_ref(),
        "backend-blocked",
        mock_endpoint.endpoint(),
        1,
    )
    .await;

    let identity = Arc::new(SimpleIdentity::new(
        "scheduled-test",
        dir.path().join("identity.key"),
        None,
    ));
    let behavior = Arc::new(BehaviorConfig {
        name: "did:defra-agent:scheduled-test:default".to_string(),
        identity,
        backend_id: Some("backend-blocked".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: mock_endpoint.endpoint().to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: "scheduled-model".to_string(),
        context_window: DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: DEFAULT_MAX_TURNS,
        system_prompt: "You are a scheduler test agent.".to_string(),
        tools: BehaviorToolConfig::meta_only(),
        compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
        deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
        sampling: crate::config::SamplingConfig::default(),
    });
    let tool_surface = Arc::new(behavior.tools.resolve(node.as_ref()).await.unwrap());
    insert_due_task(
        node.as_ref(),
        "task-blocked",
        &behavior.name,
        "Say scheduled-ok",
    )
    .await;

    let active_snapshot = Arc::new(crate::runtime_snapshot::ActiveRuntimeSnapshot {
        generation: 1,
        default_behavior_id: behavior.name.clone(),
        behaviors: std::collections::HashMap::from([(behavior.name.clone(), behavior.clone())]),
        tool_surfaces: std::collections::HashMap::from([(behavior.name.clone(), tool_surface)]),
        backend_admission_configs: std::collections::HashMap::from([(
            "backend-blocked".to_string(),
            BackendAdmissionConfig {
                backend_id: "backend-blocked".to_string(),
                max_concurrent: 1,
                max_queue_depth: 100,
                enabled: true,
                probe_status: "healthy".to_string(),
                config_fingerprint: "backend-blocked:1:100".to_string(),
            },
        )]),
        unavailable_behaviors: std::collections::HashMap::new(),
        dispatchers: std::collections::HashMap::new(),
    });
    let registry = test_admission_registry(node.clone(), "backend-blocked", 1);
    let _held_permit = registry
        .acquire_for_test(
            "req-held-scheduler-capacity",
            "backend-blocked",
            &behavior.name,
            behavior.did(),
            CallKind::Scheduled,
        )
        .await
        .expect("test permit should acquire backend capacity");
    let (_tx, rx) = watch::channel(active_snapshot);
    let mut scheduler = Scheduler::new(
        node.clone(),
        rx,
        ToolRuntimeContext::oneshot(node.clone()),
        registry,
    );
    let cancel = CancellationToken::new();
    let cancel_for_tick = cancel.clone();

    let tick = tokio::spawn(async move { scheduler.tick(cancel_for_tick).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();

    tokio::time::timeout(Duration::from_secs(2), tick)
        .await
        .expect("scheduler tick should not wait for backend deadline")
        .expect("tick task should join")
        .expect("scheduler tick should return ok");
}
