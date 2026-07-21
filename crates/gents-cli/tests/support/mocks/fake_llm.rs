//! One robust axum-based fake OpenAI-compatible endpoint shared by every CLI
//! mock. Replaces the several hand-rolled nonblocking `TcpListener` accept
//! loops (whose `Err(_) => break` arm killed the listener on any transient
//! accept error under CI load). The server runs on its own current-thread
//! tokio runtime in a background thread, so it works whether the consuming
//! test is `#[tokio::test]` or a blocking CLI-subprocess test.
//!
//! Behavior is supplied per construction via a chat `Responder` closure, so the
//! thin `MockChatEndpoint` / `MockModelEndpoint` / `MockOpenAIEndpoint` /
//! spawn-mock wrappers keep their exact public APIs while sharing this one
//! robust server. SSE bodies are written whole (matching the previous mocks),
//! so no incremental streaming is needed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::Value;
use tokio::sync::{oneshot, Notify};

/// What the chat endpoint should do for a given request.
pub enum ChatAction {
    /// Respond immediately with this complete SSE body.
    Sse(String),
    /// Sleep, then respond with this complete SSE body (cut short on shutdown).
    DelayThenSse(Duration, String),
    /// Never respond (the client is expected to time out); returns once the
    /// mock is dropped.
    Hang,
}

/// `Fn(&request_json) -> ChatAction`, invoked per authorized chat request.
pub type Responder = Arc<dyn Fn(&Value) -> ChatAction + Send + Sync>;

struct FakeState {
    model_name: String,
    required_bearer: Option<String>,
    captured: Arc<Mutex<Vec<Value>>>,
    responder: Responder,
    stop: Arc<Notify>,
    stopped: Arc<AtomicBool>,
}

pub struct FakeLlm {
    endpoint: String,
    captured: Arc<Mutex<Vec<Value>>>,
    stop: Arc<Notify>,
    stopped: Arc<AtomicBool>,
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl FakeLlm {
    pub fn start(
        model_name: &str,
        required_bearer: Option<&str>,
        responder: Responder,
    ) -> Result<Self> {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(Notify::new());
        let stopped = Arc::new(AtomicBool::new(false));
        let state = Arc::new(FakeState {
            model_name: model_name.to_string(),
            required_bearer: required_bearer.map(ToOwned::to_owned),
            captured: captured.clone(),
            responder,
            stop: stop.clone(),
            stopped: stopped.clone(),
        });

        let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let join = std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(_) => return,
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
                    Ok(listener) => listener,
                    Err(_) => return,
                };
                let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
                let _ = port_tx.send(port);
                let app = Router::new()
                    .route("/v1/models", get(handle_models))
                    .route("/models", get(handle_models))
                    .route("/v1/chat/completions", post(handle_chat))
                    .route("/chat/completions", post(handle_chat))
                    .fallback(handle_fallback)
                    .with_state(state);
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await;
            });
        });

        let port = port_rx
            .recv()
            .context("mock fake-llm server failed to bind a port")?;

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            captured,
            stop,
            stopped,
            shutdown: Some(shutdown_tx),
            join: Some(join),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn captured_chat_requests(&self) -> Vec<Value> {
        self.captured
            .lock()
            .expect("captured chat request mutex poisoned")
            .clone()
    }
}

impl Drop for FakeLlm {
    fn drop(&mut self) {
        // Release any hanging/delayed handler, stop the server, join the thread.
        self.stopped.store(true, Ordering::Relaxed);
        self.stop.notify_waiters();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn bearer_authorized(state: &FakeState, headers: &HeaderMap) -> bool {
    match &state.required_bearer {
        None => true,
        Some(expected) => {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                == Some(format!("Bearer {expected}").as_str())
        }
    }
}

fn json_response(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn sse_response(body: String) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

fn unauthorized() -> Response {
    json_response(
        StatusCode::UNAUTHORIZED,
        r#"{"error":"unauthorized"}"#.to_string(),
    )
}

async fn handle_models(State(state): State<Arc<FakeState>>, headers: HeaderMap) -> Response {
    if !bearer_authorized(&state, &headers) {
        return unauthorized();
    }
    json_response(
        StatusCode::OK,
        format!(r#"{{"data":[{{"id":"{}"}}]}}"#, state.model_name),
    )
}

async fn handle_chat(
    State(state): State<Arc<FakeState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !bearer_authorized(&state, &headers) {
        return unauthorized();
    }
    let request_json: Value = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"invalid json"}"#.to_string(),
            )
        }
    };
    state
        .captured
        .lock()
        .expect("captured chat request mutex poisoned")
        .push(request_json.clone());

    match (state.responder)(&request_json) {
        ChatAction::Sse(body) => sse_response(body),
        ChatAction::DelayThenSse(delay, body) => {
            // Sleep for `delay`, but wake early on shutdown.
            let _ = tokio::time::timeout(delay, state.stop.notified()).await;
            sse_response(body)
        }
        ChatAction::Hang => {
            // Never respond until the mock is dropped; the client times out.
            while !state.stopped.load(Ordering::Relaxed) {
                let _ =
                    tokio::time::timeout(Duration::from_millis(50), state.stop.notified()).await;
            }
            json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                r#"{"error":"shutting down"}"#.to_string(),
            )
        }
    }
}

async fn handle_fallback() -> Response {
    json_response(
        StatusCode::NOT_FOUND,
        r#"{"error":"not found"}"#.to_string(),
    )
}

/// SSE body for a single assistant tool call (`tool_calls` delta then a
/// `finish_reason: "tool_calls"` terminal chunk and `[DONE]`).
pub fn tool_call_sse(tool_name: &str, arguments: &str) -> String {
    tool_call_sse_with_id("call-read-file", tool_name, arguments)
}

/// SSE body for a single assistant tool call with a caller-selected id.
/// Multi-turn orchestration tests use distinct ids so their provider history
/// matches real model behavior and every tool result has an unambiguous owner.
pub fn tool_call_sse_with_id(tool_call_id: &str, tool_name: &str, arguments: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": tool_call_id,
                    "function": { "name": tool_name, "arguments": "" }
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
                    "function": { "name": null, "arguments": arguments }
                }]
            },
            "finish_reason": null
        }],
        "usage": null
    });
    let chunk_3 = serde_json::json!({
        "choices": [{
            "delta": { "content": null, "tool_calls": [] },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 16, "completion_tokens": 4, "total_tokens": 20 }
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk_1).expect("serialize tool-call chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize tool-call chunk 2"),
        serde_json::to_string(&chunk_3).expect("serialize tool-call chunk 3"),
    )
}

/// SSE body for a single assistant text completion.
pub fn completion_text_sse(text: &str) -> String {
    let chunk_1 = serde_json::json!({
        "choices": [{ "delta": { "content": text }, "finish_reason": null }],
        "usage": null
    });
    let chunk_2 = serde_json::json!({
        "choices": [{
            "delta": { "content": null, "tool_calls": [] },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 24, "completion_tokens": 6, "total_tokens": 30 }
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk_1).expect("serialize completion chunk 1"),
        serde_json::to_string(&chunk_2).expect("serialize completion chunk 2"),
    )
}
