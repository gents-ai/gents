use super::super::*;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;

use crate::admission::{AdmissionRegistry, BackendAdmissionConfig};

pub(super) fn test_admission_registry(
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

pub(super) struct MockCompletionEndpoint {
    pub(super) endpoint: String,
    pub(super) port: u16,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) handle: Option<JoinHandle<()>>,
}

impl MockCompletionEndpoint {
    pub(super) fn start(model_name: &str, response_text: &str) -> anyhow::Result<Self> {
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

    pub(super) fn endpoint(&self) -> &str {
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

pub(super) struct HttpRequestData {
    pub(super) method: String,
    pub(super) path: String,
}

pub(super) fn read_http_request(stream: &mut TcpStream) -> anyhow::Result<HttpRequestData> {
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

pub(super) fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub(super) fn write_http_response(
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

pub(super) async fn insert_backend(node: &EmbeddedNode, backend_id: &str, endpoint: &str) {
    insert_backend_with_capacity(node, backend_id, endpoint, 1).await;
}

pub(super) async fn insert_backend_with_capacity(
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

pub(super) async fn insert_due_task(
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

pub(super) async fn query_task_row(
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

pub(super) async fn delete_task(node: &EmbeddedNode, doc_id: &str) {
    let mutation = format!(
        r#"mutation {{ delete_ScheduledTask(docID: "{doc_id}") {{ _docID }} }}"#,
        doc_id = escape_graphql_string(doc_id),
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}
