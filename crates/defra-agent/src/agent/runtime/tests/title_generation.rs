use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use super::support::*;

async fn wait_for_request_state(
    node: &defra_node::EmbeddedNode,
    doc_id: &str,
    expected_status: &str,
) {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let query = format!(
            r#"{{
                AgentRequest(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, limit: 1) {{
                    status
                    failure_reason
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "AgentRequest query failed: {:?}",
            response.errors
        );
        let row = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned()
            .expect("AgentRequest row");
        let status = row
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if status == expected_status {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentRequest {} to reach status={}, last row={:?}",
            doc_id,
            expected_status,
            row
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_conversation_title(
    node: &defra_node::EmbeddedNode,
    session_id: &str,
    expected_title: &str,
) {
    let escaped_session_id = escape_graphql_string(session_id);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let query = format!(
            r#"{{
                AgentConversation(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}, limit: 1) {{
                    title
                    title_source
                    status
                }}
            }}"#
        );
        let response = node.execute(&query).await;
        assert!(
            !response.has_errors(),
            "AgentConversation query failed: {:?}",
            response.errors
        );
        let row = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentConversation"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned();
        let title = row
            .as_ref()
            .and_then(|row| row.get("title"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let title_source = row
            .as_ref()
            .and_then(|row| row.get("title_source"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if title == expected_title && title_source == "generated" {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentConversation {} to reach title={}, last row={:?}",
            session_id,
            expected_title,
            row
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn inference_call_rows(node: &defra_node::EmbeddedNode, request_id: &str) -> Vec<Value> {
    let escaped_request_id = escape_graphql_string(request_id);
    let response = node
        .execute(&format!(
            r#"{{
                InferenceCall(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    order: {{ call_seq: ASC }}
                ) {{
                    request_id
                    call_seq
                    call_kind
                    call_state
                    failure_reason
                }}
            }}"#
        ))
        .await;
    assert!(
        !response.has_errors(),
        "InferenceCall query failed: {:?}",
        response.errors
    );
    response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

struct HttpRequestData {
    method: String,
    path: String,
    body: String,
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
    let content_length = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim().eq_ignore_ascii_case("content-length"))
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .next()
        .unwrap_or(0);
    let mut body = buffer[header_end..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut temp)?;
        if read == 0 {
            anyhow::bail!("connection closed before request body");
        }
        body.extend_from_slice(&temp[..read]);
    }
    Ok(HttpRequestData {
        method,
        path,
        body: String::from_utf8_lossy(&body[..content_length]).to_string(),
    })
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

fn mock_completion_sse(text: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{
            "delta": { "content": text },
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
        serde_json::to_string(&chunk_1).expect("serialize streaming completion chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize streaming completion chunk 2"),
    )
}

struct TitleGenerationMockEndpoint {
    endpoint: String,
    port: u16,
    stop: Arc<AtomicBool>,
    chat_requests: Arc<AtomicUsize>,
    title_requests: Arc<AtomicUsize>,
    handle: Option<JoinHandle<()>>,
}

enum TitleResponseMode {
    Success(String),
    Empty,
}

impl TitleGenerationMockEndpoint {
    fn start(model_name: &str, title_text: &str, main_text: &str) -> anyhow::Result<Self> {
        Self::start_with_title_mode(
            model_name,
            TitleResponseMode::Success(title_text.to_string()),
            main_text,
        )
    }

    fn start_with_empty_title(model_name: &str, main_text: &str) -> anyhow::Result<Self> {
        Self::start_with_title_mode(model_name, TitleResponseMode::Empty, main_text)
    }

    fn start_with_title_mode(
        model_name: &str,
        title_mode: TitleResponseMode,
        main_text: &str,
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let chat_requests = Arc::new(AtomicUsize::new(0));
        let title_requests = Arc::new(AtomicUsize::new(0));
        let chat_requests_for_thread = chat_requests.clone();
        let title_requests_for_thread = title_requests.clone();
        let model_name = model_name.to_string();
        let main_text = main_text.to_string();

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

                        let (status, content_type, body) = match request.path.as_str() {
                            "/v1/models" | "/models" if request.method == "GET" => (
                                "200 OK",
                                "application/json",
                                format!(r#"{{"data":[{{"id":"{model_name}"}}]}}"#),
                            ),
                            "/v1/chat/completions" | "/chat/completions"
                                if request.method == "POST" =>
                            {
                                chat_requests_for_thread.fetch_add(1, Ordering::SeqCst);
                                let payload: Value = serde_json::from_str(&request.body)
                                    .unwrap_or_else(|_| Value::Null);
                                let is_stream = payload
                                    .get("stream")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false);
                                let is_title_request = request.body.contains(
                                    "Generate a concise session title for this conversation.",
                                );

                                if is_title_request {
                                    title_requests_for_thread.fetch_add(1, Ordering::SeqCst);
                                    match &title_mode {
                                        TitleResponseMode::Success(title_text) => (
                                            "200 OK",
                                            "application/json",
                                            serde_json::json!({
                                                "id":"chatcmpl-title-test",
                                                "provider":"Mock",
                                                "object":"chat.completion",
                                                "created":1710000000,
                                                "model":model_name,
                                                "choices":[{
                                                    "index":0,
                                                    "finish_reason":"stop",
                                                    "message":{
                                                        "role":"assistant",
                                                        "content":title_text,
                                                        "refusal":null,
                                                        "reasoning":null
                                                    }
                                                }],
                                                "usage":{
                                                    "prompt_tokens":12,
                                                    "completion_tokens":3,
                                                    "total_tokens":15
                                                }
                                            })
                                            .to_string(),
                                        ),
                                        TitleResponseMode::Empty => (
                                            "200 OK",
                                            "application/json",
                                            serde_json::json!({
                                                "id":"chatcmpl-title-test",
                                                "provider":"Mock",
                                                "object":"chat.completion",
                                                "created":1710000000,
                                                "model":model_name,
                                                "choices":[{
                                                    "index":0,
                                                    "finish_reason":"stop",
                                                    "message":{
                                                        "role":"assistant",
                                                        "content":null,
                                                        "refusal":null,
                                                        "reasoning":null
                                                    }
                                                }],
                                                "usage":{
                                                    "prompt_tokens":12,
                                                    "completion_tokens":0,
                                                    "total_tokens":12
                                                }
                                            })
                                            .to_string(),
                                        ),
                                    }
                                } else if is_stream {
                                    (
                                        "200 OK",
                                        "text/event-stream",
                                        mock_completion_sse(&main_text),
                                    )
                                } else {
                                    (
                                        "200 OK",
                                        "application/json",
                                        serde_json::json!({
                                            "id":"chatcmpl-main-test",
                                            "provider":"Mock",
                                            "object":"chat.completion",
                                            "created":1710000000,
                                            "model":model_name,
                                            "choices":[{
                                                "index":0,
                                                "finish_reason":"stop",
                                                "message":{
                                                    "role":"assistant",
                                                    "content":main_text,
                                                    "refusal":null,
                                                    "reasoning":null
                                                }
                                            }],
                                            "usage":{
                                                "prompt_tokens":10,
                                                "completion_tokens":2,
                                                "total_tokens":12
                                            }
                                        })
                                        .to_string(),
                                    )
                                }
                            }
                            _ => (
                                "404 Not Found",
                                "application/json",
                                r#"{"error":"not found"}"#.to_string(),
                            ),
                        };

                        let _ = write_http_response(&mut stream, status, content_type, &body);
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
            chat_requests,
            title_requests,
            handle: Some(handle),
        })
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn chat_request_count(&self) -> usize {
        self.chat_requests.load(Ordering::SeqCst)
    }

    fn title_request_count(&self) -> usize {
        self.title_requests.load(Ordering::SeqCst)
    }
}

impl Drop for TitleGenerationMockEndpoint {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

async fn wait_for_request_counts(
    endpoint: &TitleGenerationMockEndpoint,
    expected_chat_requests: usize,
    expected_title_requests: usize,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let actual_chat = endpoint.chat_request_count();
        let actual_title = endpoint.title_request_count();
        if actual_chat >= expected_chat_requests && actual_title >= expected_title_requests {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for chat/title request counts chat={}/{} title={}/{}",
            actual_chat,
            expected_chat_requests,
            actual_title,
            expected_title_requests
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn first_request_generates_tracked_oneoff_title_without_blocking_completion() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("title-oneoff-success"));
    let mock_endpoint = TitleGenerationMockEndpoint::start(
        "default",
        "architecture-review",
        "The architecture is sound overall.",
    )
    .unwrap();
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-title-oneoff",
        mock_endpoint.endpoint(),
    )
    .await;
    let agent = crate::DefraAgent::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;

    let session_id = "session-title-oneoff";
    let request_id = "req-title-oneoff";
    let request_doc_id = create_agent_request(
        node.as_ref(),
        identity.did(),
        request_id,
        session_id,
        "what do you think about the architecture?",
    )
    .await;

    wait_for_request_counts(&mock_endpoint, 2, 1).await;
    wait_for_request_state(node.as_ref(), &request_doc_id, "completed").await;
    wait_for_conversation_title(node.as_ref(), session_id, "architecture-review").await;

    let rows = inference_call_rows(node.as_ref(), request_id).await;
    assert!(
        rows.iter()
            .any(|row| row["call_kind"] == "oneoff" && row["call_state"] == "completed"),
        "missing completed oneoff inference call: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row["call_kind"] == "inference" && row["call_state"] == "completed"),
        "missing completed main inference call: {rows:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row["call_kind"] == "oneoff" && row["call_state"] == "failed"),
        "title oneoff call should not fail in success test: {rows:?}"
    );

    let _ = shutdown_tx.send(true);
    handle
        .await
        .expect("agent task should join")
        .expect("agent run should return ok");
}

#[tokio::test]
async fn title_oneoff_failure_falls_back_without_blocking_request_completion() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("title-oneoff-fallback"));
    let mock_endpoint = TitleGenerationMockEndpoint::start_with_empty_title(
        "default",
        "The architecture is sound.",
    )
    .unwrap();
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-title-oneoff-fallback",
        mock_endpoint.endpoint(),
    )
    .await;
    let agent = crate::DefraAgent::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        crate::agent::DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;

    let session_id = "session-title-oneoff-fallback";
    let request_id = "req-title-oneoff-fallback";
    let request_doc_id = create_agent_request(
        node.as_ref(),
        identity.did(),
        request_id,
        session_id,
        "what do you think about the architecture?",
    )
    .await;

    wait_for_request_counts(&mock_endpoint, 3, 2).await;
    wait_for_request_state(node.as_ref(), &request_doc_id, "completed").await;
    wait_for_conversation_title(node.as_ref(), session_id, "architecture").await;

    let rows = inference_call_rows(node.as_ref(), request_id).await;
    assert!(
        rows.iter()
            .any(|row| row["call_kind"] == "inference" && row["call_state"] == "completed"),
        "missing completed main inference call: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row["call_kind"] == "oneoff" && row["call_state"] == "failed"),
        "expected failed oneoff title inference call: {rows:?}"
    );

    let _ = shutdown_tx.send(true);
    handle
        .await
        .expect("agent task should join")
        .expect("agent run should return ok");
}
