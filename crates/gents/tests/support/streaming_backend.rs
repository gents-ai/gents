//! Deterministic OpenAI-compatible streaming backend for full-daemon tests.
//!
//! Built on `axum` (a real async HTTP server) rather than a hand-rolled
//! nonblocking `TcpListener` accept loop, so it cannot flake the way the
//! previous server did (its `Err(_) => break` arm killed the listener on any
//! transient accept error under CI load). The SSE byte format, the
//! `StreamScript` pause/release semantics, and the chunk-count accounting are
//! preserved exactly so existing consumers are unchanged.

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use tokio::sync::{oneshot, Notify};

#[derive(Clone, Debug)]
pub struct StreamScript {
    marker: String,
    chunks: Vec<String>,
    pause_after_chunks: bool,
}

impl StreamScript {
    pub fn paused(
        marker: impl Into<String>,
        chunks: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        Self {
            marker: marker.into(),
            chunks: chunks.into_iter().map(ToOwned::to_owned).collect(),
            pause_after_chunks: true,
        }
    }

    pub fn completes(
        marker: impl Into<String>,
        chunks: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        Self {
            marker: marker.into(),
            chunks: chunks.into_iter().map(ToOwned::to_owned).collect(),
            pause_after_chunks: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StreamPlan {
    marker: String,
    responses: Vec<StreamResponse>,
}

impl StreamPlan {
    pub fn new(marker: impl Into<String>, responses: Vec<StreamResponse>) -> Self {
        Self {
            marker: marker.into(),
            responses,
        }
    }

    fn repeat_stream(script: StreamScript) -> Self {
        Self {
            marker: script.marker.clone(),
            responses: vec![StreamResponse::Stream(script)],
        }
    }
}

#[derive(Clone, Debug)]
pub enum StreamResponse {
    Stream(StreamScript),
    HttpStatus { status: u16, body: String },
}

impl StreamResponse {
    pub fn completes(
        marker: impl Into<String>,
        chunks: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        StreamResponse::Stream(StreamScript::completes(marker, chunks))
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        let message = message.into();
        StreamResponse::HttpStatus {
            status: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            body: json!({
                "error": {
                    "message": message,
                    "type": "server_error"
                }
            })
            .to_string(),
        }
    }

    pub fn bad_request(body: impl Into<String>) -> Self {
        StreamResponse::HttpStatus {
            status: StatusCode::BAD_REQUEST.as_u16(),
            body: body.into(),
        }
    }
}

pub struct MockStreamingBackend {
    endpoint: String,
    state: Arc<StreamingState>,
    stop: Arc<AtomicBool>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl MockStreamingBackend {
    pub fn start(model_name: &str, scripts: Vec<StreamScript>) -> anyhow::Result<Self> {
        Self::start_with_plans(
            model_name,
            scripts.into_iter().map(StreamPlan::repeat_stream).collect(),
        )
    }

    pub fn start_with_plans(model_name: &str, plans: Vec<StreamPlan>) -> anyhow::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(StreamingState::new(
            model_name.to_string(),
            plans,
            stop.clone(),
        ));
        let app = Router::new()
            .route("/v1/models", get(handle_models))
            .route("/models", get(handle_models))
            .route("/v1/chat/completions", post(handle_chat))
            .route("/chat/completions", post(handle_chat))
            .fallback(handle_fallback)
            .with_state(state.clone());

        // Bind synchronously, serve on the ambient tokio runtime (consumers are
        // `#[tokio::test]`); `from_std` requires the listener be nonblocking.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let listener = tokio::net::TcpListener::from_std(listener)?;

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await;
        });

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            state,
            stop,
            shutdown: Some(shutdown_tx),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn release(&self, marker: &str) {
        self.state.release(marker);
    }

    pub fn observed_chunks(&self, marker: &str) -> usize {
        self.state.chunk_count(marker)
    }

    pub fn observed_requests(&self, marker: &str) -> usize {
        self.state.request_count(marker)
    }

    pub async fn wait_for_chunks(&self, marker: &str, expected: usize) {
        let observed = self
            .state
            .wait_for_chunk_count(marker, expected, Duration::from_secs(5))
            .await;
        assert!(
            observed >= expected,
            "timed out waiting for {expected} chunk(s) for marker {marker}, observed {observed}"
        );
    }
}

impl Drop for MockStreamingBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.state.notify.notify_waiters();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

struct StreamingState {
    model_name: String,
    plans: Vec<StreamPlan>,
    stop: Arc<AtomicBool>,
    inner: Mutex<StreamingStateInner>,
    notify: Notify,
}

#[derive(Default)]
struct StreamingStateInner {
    chunk_counts: HashMap<String, usize>,
    request_counts: HashMap<String, usize>,
    releases: HashSet<String>,
}

impl StreamingState {
    fn new(model_name: String, plans: Vec<StreamPlan>, stop: Arc<AtomicBool>) -> Self {
        Self {
            model_name,
            plans,
            stop,
            inner: Mutex::new(StreamingStateInner::default()),
            notify: Notify::new(),
        }
    }

    fn next_response(&self, body: &str) -> StreamResponse {
        let Some(plan) = self.plans.iter().find(|plan| body.contains(&plan.marker)) else {
            return StreamResponse::Stream(StreamScript::completes(
                "__default__",
                ["mock streamed response"],
            ));
        };

        let mut inner = self.inner.lock().expect("streaming backend mutex poisoned");
        let count = inner.request_counts.entry(plan.marker.clone()).or_default();
        let response_index = *count;
        *count += 1;
        drop(inner);
        self.notify.notify_waiters();

        plan.responses
            .get(response_index)
            .or_else(|| plan.responses.last())
            .cloned()
            .unwrap_or_else(|| {
                StreamResponse::Stream(StreamScript::completes(
                    "__default__",
                    ["mock streamed response"],
                ))
            })
    }

    fn record_chunk(&self, marker: &str) {
        let mut inner = self.inner.lock().expect("streaming backend mutex poisoned");
        *inner.chunk_counts.entry(marker.to_string()).or_default() += 1;
        drop(inner);
        self.notify.notify_waiters();
    }

    fn chunk_count(&self, marker: &str) -> usize {
        self.inner
            .lock()
            .expect("streaming backend mutex poisoned")
            .chunk_counts
            .get(marker)
            .copied()
            .unwrap_or_default()
    }

    fn request_count(&self, marker: &str) -> usize {
        self.inner
            .lock()
            .expect("streaming backend mutex poisoned")
            .request_counts
            .get(marker)
            .copied()
            .unwrap_or_default()
    }

    async fn wait_for_chunk_count(
        &self,
        marker: &str,
        expected: usize,
        timeout: Duration,
    ) -> usize {
        let started = Instant::now();
        loop {
            let actual = self.chunk_count(marker);
            if actual >= expected || started.elapsed() >= timeout {
                return actual;
            }
            let _ = tokio::time::timeout(Duration::from_millis(25), self.notify.notified()).await;
        }
    }

    fn release(&self, marker: &str) {
        self.inner
            .lock()
            .expect("streaming backend mutex poisoned")
            .releases
            .insert(marker.to_string());
        self.notify.notify_waiters();
    }

    fn is_released(&self, marker: &str) -> bool {
        self.inner
            .lock()
            .expect("streaming backend mutex poisoned")
            .releases
            .contains(marker)
    }

    async fn wait_for_release_or_stop(&self, marker: &str) {
        while !self.stop.load(Ordering::Relaxed) && !self.is_released(marker) {
            let _ = tokio::time::timeout(Duration::from_millis(25), self.notify.notified()).await;
        }
    }
}

fn request_is_streaming(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("stream").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

async fn handle_models(State(state): State<Arc<StreamingState>>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        format!(r#"{{"data":[{{"id":"{}"}}]}}"#, state.model_name),
    )
        .into_response()
}

async fn handle_chat(State(state): State<Arc<StreamingState>>, body: String) -> Response {
    if request_is_streaming(&body) {
        return match state.next_response(&body) {
            StreamResponse::Stream(script) => streaming_response(script, state),
            StreamResponse::HttpStatus { status, body } => (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                [(header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response(),
        };
    }

    let completion = json!({
        "id": "chatcmpl-title",
        "object": "chat.completion",
        "created": 1_710_000_000_u64,
        "model": state.model_name,
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "mock-title",
                "refusal": null,
                "reasoning": null
            }
        }],
        "usage": {
            "prompt_tokens": 4,
            "completion_tokens": 1,
            "total_tokens": 5
        }
    });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        completion.to_string(),
    )
        .into_response()
}

async fn handle_fallback() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":"not found"}"#,
    )
        .into_response()
}

fn streaming_response(script: StreamScript, state: Arc<StreamingState>) -> Response {
    #[derive(Clone, Copy)]
    enum Phase {
        Chunk(usize),
        AwaitRelease,
        Usage,
        Done,
        Finished,
    }

    let init = (Phase::Chunk(0), script, state);
    let stream = futures::stream::unfold(init, |(phase, script, state)| async move {
        let mut phase = phase;
        loop {
            match phase {
                Phase::Chunk(index) => {
                    if state.stop.load(Ordering::Relaxed) {
                        return None;
                    }
                    if index < script.chunks.len() {
                        let event = Event::default().data(chunk_payload(&script.chunks[index]));
                        state.record_chunk(&script.marker);
                        return Some((
                            Ok::<Event, Infallible>(event),
                            (Phase::Chunk(index + 1), script, state),
                        ));
                    }
                    phase = if script.pause_after_chunks {
                        Phase::AwaitRelease
                    } else {
                        Phase::Usage
                    };
                }
                Phase::AwaitRelease => {
                    state.wait_for_release_or_stop(&script.marker).await;
                    if state.stop.load(Ordering::Relaxed) {
                        return None;
                    }
                    phase = Phase::Usage;
                }
                Phase::Usage => {
                    let event = Event::default().data(usage_payload());
                    return Some((Ok(event), (Phase::Done, script, state)));
                }
                Phase::Done => {
                    let event = Event::default().data("[DONE]");
                    return Some((Ok(event), (Phase::Finished, script, state)));
                }
                Phase::Finished => return None,
            }
        }
    });

    Sse::new(stream).into_response()
}

fn chunk_payload(content: &str) -> String {
    json!({
        "choices": [{
            "delta": {
                "content": content,
                "tool_calls": []
            },
            "finish_reason": null
        }],
        "usage": null
    })
    .to_string()
}

fn usage_payload() -> String {
    json!({
        "choices": [],
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 3,
            "total_tokens": 11
        }
    })
    .to_string()
}
