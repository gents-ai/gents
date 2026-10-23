use rapidhash::{RapidHashMap, RapidHashSet};
use std::collections::VecDeque;
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
pub enum StreamChunk {
    Text(String),
    Reasoning(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
}

impl StreamChunk {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    pub fn reasoning(reasoning: impl Into<String>) -> Self {
        Self::Reasoning(reasoning.into())
    }

    pub fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct StreamScript {
    marker: String,
    chunks: Vec<StreamChunk>,
    pause_before_chunks: bool,
    pause_after_chunks: bool,
}

impl StreamScript {
    pub fn paused(
        marker: impl Into<String>,
        chunks: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        Self {
            marker: marker.into(),
            chunks: chunks.into_iter().map(StreamChunk::text).collect(),
            pause_before_chunks: false,
            pause_after_chunks: true,
        }
    }

    pub fn completes(
        marker: impl Into<String>,
        chunks: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        Self {
            marker: marker.into(),
            chunks: chunks.into_iter().map(StreamChunk::text).collect(),
            pause_before_chunks: false,
            pause_after_chunks: false,
        }
    }

    pub fn streams(marker: impl Into<String>, chunks: Vec<StreamChunk>) -> Self {
        Self {
            marker: marker.into(),
            chunks,
            pause_before_chunks: false,
            pause_after_chunks: false,
        }
    }

    pub fn paused_before(marker: impl Into<String>, chunks: Vec<StreamChunk>) -> Self {
        Self {
            marker: marker.into(),
            chunks,
            pause_before_chunks: true,
            pause_after_chunks: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct StreamPlan {
    marker: String,
    responses: Vec<StreamResponse>,
    selector: PlanSelector,
}

#[derive(Clone, Copy, Debug)]
enum PlanSelector {
    WholeBody,
    CurrentAuthoredUser,
}

impl StreamPlan {
    pub fn new(marker: impl Into<String>, responses: Vec<StreamResponse>) -> Self {
        Self {
            marker: marker.into(),
            responses,
            selector: PlanSelector::WholeBody,
        }
    }

    pub fn current_authored_user(
        marker: impl Into<String>,
        responses: Vec<StreamResponse>,
    ) -> Self {
        Self {
            marker: marker.into(),
            responses,
            selector: PlanSelector::CurrentAuthoredUser,
        }
    }

    pub fn with_current_authored_user(mut self) -> Self {
        self.selector = PlanSelector::CurrentAuthoredUser;
        self
    }

    fn repeat_stream(script: StreamScript) -> Self {
        Self {
            marker: script.marker.clone(),
            responses: vec![StreamResponse::Stream(script)],
            selector: PlanSelector::WholeBody,
        }
    }
}

#[derive(Clone, Debug)]
pub enum StreamResponse {
    Stream(StreamScript),
    HoldHeaders(StreamScript),
    HttpStatus { status: u16, body: String },
}

impl StreamResponse {
    pub fn completes(
        marker: impl Into<String>,
        chunks: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        StreamResponse::Stream(StreamScript::completes(marker, chunks))
    }

    pub fn streams(marker: impl Into<String>, chunks: Vec<StreamChunk>) -> Self {
        StreamResponse::Stream(StreamScript::streams(marker, chunks))
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

    pub fn enable_dynamic_followups(&self, marker: &str) {
        let mut inner = self
            .state
            .inner
            .lock()
            .expect("streaming backend mutex poisoned");
        inner.dynamic_followups.insert(marker.to_string());
        drop(inner);
        self.state.notify.notify_waiters();
    }

    pub fn enqueue_response(&self, marker: &str, response: StreamResponse) {
        let mut inner = self
            .state
            .inner
            .lock()
            .expect("streaming backend mutex poisoned");
        inner
            .dynamic_responses
            .entry(marker.to_string())
            .or_default()
            .push_back(response);
        drop(inner);
        self.state.notify.notify_waiters();
    }

    pub fn observed_completion_bodies(&self) -> Vec<serde_json::Value> {
        self.state
            .inner
            .lock()
            .expect("streaming backend mutex poisoned")
            .completion_bodies
            .clone()
    }

    pub fn observed_completion_requests(&self) -> usize {
        self.observed_completion_bodies().len()
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
    chunk_counts: RapidHashMap<String, usize>,
    request_counts: RapidHashMap<String, usize>,
    completion_bodies: Vec<serde_json::Value>,
    releases: RapidHashSet<String>,
    dynamic_followups: RapidHashSet<String>,
    dynamic_responses: RapidHashMap<String, VecDeque<StreamResponse>>,
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

    fn record_completion_body(&self, body: &str) {
        let value = serde_json::from_str::<serde_json::Value>(body)
            .unwrap_or_else(|_| json!({ "__unparsed__": body }));
        self.inner
            .lock()
            .expect("streaming backend mutex poisoned")
            .completion_bodies
            .push(value);
        self.notify.notify_waiters();
    }

    async fn next_response(&self, body: &str) -> StreamResponse {
        if request_is_session_title(body) {
            return StreamResponse::Stream(StreamScript::completes(
                "__session_title__",
                ["mock-title"],
            ));
        }
        let matching_plans = match matching_plans(&self.plans, body) {
            Ok(plans) => plans,
            Err(diagnostic) => return StreamResponse::bad_request(diagnostic),
        };
        let Some(plan) = matching_plans.into_iter().next() else {
            let dynamic_markers = {
                let inner = self.inner.lock().expect("streaming backend mutex poisoned");
                match matching_dynamic_markers(&inner.dynamic_followups, body) {
                    Ok(markers) => markers,
                    Err(diagnostic) => return StreamResponse::bad_request(diagnostic),
                }
            };
            if let Some(marker) = dynamic_markers.into_iter().next() {
                self.reserve_response_index(&marker);
                self.notify.notify_waiters();
                loop {
                    if let Some(response) = self
                        .inner
                        .lock()
                        .expect("streaming backend mutex poisoned")
                        .dynamic_responses
                        .get_mut(&marker)
                        .and_then(VecDeque::pop_front)
                    {
                        return response;
                    }
                    if self.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = tokio::time::timeout(Duration::from_millis(25), self.notify.notified())
                        .await;
                }
            }
            return StreamResponse::Stream(StreamScript::completes(
                "__default__",
                ["mock streamed response"],
            ));
        };

        let response_index = self.reserve_response_index(&plan.marker);
        self.notify.notify_waiters();

        if response_index > 0 {
            loop {
                let dynamic = {
                    let mut inner = self.inner.lock().expect("streaming backend mutex poisoned");
                    if !inner.dynamic_followups.contains(&plan.marker) {
                        None
                    } else if let Some(response) = inner
                        .dynamic_responses
                        .get_mut(&plan.marker)
                        .and_then(VecDeque::pop_front)
                    {
                        return response;
                    } else {
                        Some(())
                    }
                };
                if dynamic.is_none() || self.stop.load(Ordering::Relaxed) {
                    break;
                }
                let _ =
                    tokio::time::timeout(Duration::from_millis(25), self.notify.notified()).await;
            }
        }

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

    fn reserve_response_index(&self, marker: &str) -> usize {
        let mut inner = self.inner.lock().expect("streaming backend mutex poisoned");
        let count = inner.request_counts.entry(marker.to_string()).or_default();
        let response_index = *count;
        *count += 1;
        response_index
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

fn request_is_session_title(body: &str) -> bool {
    let Ok(body) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    body.get("tools").is_none()
        && body
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .and_then(|messages| messages.first())
            .is_some_and(|message| {
                message.get("role").and_then(serde_json::Value::as_str) == Some("system")
                    && message.get("content").is_some_and(|content| {
                        content
                            .to_string()
                            .contains("Generate concise conversation titles.")
                    })
            })
}

fn current_authored_user_input(body: &str) -> Result<String, String> {
    let body = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|error| format!("invalid mock provider request JSON: {error}"))?;
    body.get("messages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "mock provider request is missing messages array".to_string())?
        .iter()
        .rev()
        .filter(|message| message.get("role").and_then(serde_json::Value::as_str) == Some("user"))
        .find_map(|message| match message.get("content")? {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Array(blocks) => {
                let text = blocks
                    .iter()
                    .filter(|block| {
                        block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                    })
                    .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        })
        .ok_or_else(|| "mock provider request has no authored user input".to_string())
}

fn matching_plans<'a>(plans: &'a [StreamPlan], body: &str) -> Result<Vec<&'a StreamPlan>, String> {
    let current_input = current_authored_user_input(body);
    let current_matches = plans
        .iter()
        .filter(|plan| matches!(plan.selector, PlanSelector::CurrentAuthoredUser))
        .filter(|plan| {
            current_input
                .as_ref()
                .is_ok_and(|input| input.contains(&plan.marker))
        })
        .collect::<Vec<_>>();
    if current_matches.is_empty()
        && plans
            .iter()
            .any(|plan| matches!(plan.selector, PlanSelector::CurrentAuthoredUser))
    {
        current_input?;
    }
    if current_matches.len() > 1 {
        return Err(format!(
            "ambiguous mock provider plan for current user input: {:?}",
            current_matches
                .iter()
                .map(|plan| plan.marker.as_str())
                .collect::<Vec<_>>()
        ));
    }
    if !current_matches.is_empty() {
        return Ok(current_matches);
    }
    Ok(plans
        .iter()
        .find(|plan| {
            matches!(plan.selector, PlanSelector::WholeBody) && body.contains(&plan.marker)
        })
        .into_iter()
        .collect())
}

fn matching_dynamic_markers(
    markers: &RapidHashSet<String>,
    body: &str,
) -> Result<Vec<String>, String> {
    let current_input = current_authored_user_input(body)?;
    let matched = markers
        .iter()
        .filter(|marker| current_input.contains(marker.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if matched.len() > 1 {
        return Err(format!(
            "ambiguous dynamic mock provider plan for current user input: {matched:?}"
        ));
    }
    Ok(matched)
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
    state.record_completion_body(&body);
    if request_is_streaming(&body) {
        return match state.next_response(&body).await {
            StreamResponse::Stream(script) => streaming_response(script, state),
            StreamResponse::HoldHeaders(script) => {
                state.wait_for_release_or_stop(&script.marker).await;
                streaming_response(script, state)
            }
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
        AwaitInitialRelease,
        Chunk(usize),
        AwaitRelease,
        Usage,
        Done,
        Finished,
    }

    let initial_phase = if script.pause_before_chunks {
        Phase::AwaitInitialRelease
    } else {
        Phase::Chunk(0)
    };
    let init = (initial_phase, script, state);
    let stream = futures::stream::unfold(init, |(phase, script, state)| async move {
        let mut phase = phase;
        loop {
            match phase {
                Phase::AwaitInitialRelease => {
                    state.wait_for_release_or_stop(&script.marker).await;
                    if state.stop.load(Ordering::Relaxed) {
                        return None;
                    }
                    phase = Phase::Chunk(0);
                }
                Phase::Chunk(index) => {
                    if state.stop.load(Ordering::Relaxed) {
                        return None;
                    }
                    if index < script.chunks.len() {
                        let event =
                            Event::default().data(chunk_payload(index, &script.chunks[index]));
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

fn chunk_payload(index: usize, chunk: &StreamChunk) -> String {
    match chunk {
        StreamChunk::Text(content) => json!({
            "choices": [{
                "delta": {
                    "content": content,
                    "tool_calls": []
                },
                "finish_reason": null
            }],
            "usage": null
        })
        .to_string(),
        StreamChunk::Reasoning(reasoning) => json!({
            "choices": [{
                "delta": {
                    "content": null,
                    "reasoning_content": reasoning,
                    "tool_calls": []
                },
                "finish_reason": null
            }],
            "usage": null
        })
        .to_string(),
        StreamChunk::ToolCall {
            id,
            name,
            arguments,
        } => json!({
            "choices": [{
                "delta": {
                    "content": null,
                    "tool_calls": [{
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments
                        }
                    }]
                },
                "finish_reason": null
            }],
            "usage": null
        })
        .to_string(),
    }
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
