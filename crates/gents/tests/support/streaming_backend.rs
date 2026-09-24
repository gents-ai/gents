//! Deterministic OpenAI-compatible streaming backend for full-daemon tests.
//!
//! Built on `axum` (a real async HTTP server) rather than a hand-rolled
//! nonblocking `TcpListener` accept loop, so it cannot flake the way the
//! previous server did (its `Err(_) => break` arm killed the listener on any
//! transient accept error under CI load). The SSE byte format, the
//! `StreamScript` pause/release semantics, and the chunk-count accounting are
//! preserved exactly so existing consumers are unchanged.

use std::collections::{HashMap, HashSet, VecDeque};
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

/// One streamed delta: assistant text, or a tool call the model is asking for.
///
/// Tool calls exist here because a multi-turn request is *only* reachable
/// through them — the owned loop continues to a second turn exactly when a turn
/// produced tool calls. A backend that can only stream text can never exercise
/// turn 1, which is where per-turn compaction, threaded tool results, and the
/// `turn_index` half of the rendered-capture key live.
#[derive(Clone, Debug)]
pub enum StreamChunk {
    Text(String),
    ToolCall {
        id: String,
        name: String,
        /// The `arguments` string exactly as an OpenAI-compatible provider
        /// streams it: a JSON *string* whose contents are the argument object.
        arguments: String,
    },
}

impl StreamChunk {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
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

    /// A script over explicit chunks, so a caller can stream tool calls.
    pub fn streams(marker: impl Into<String>, chunks: Vec<StreamChunk>) -> Self {
        Self {
            marker: marker.into(),
            chunks,
            pause_before_chunks: false,
            pause_after_chunks: false,
        }
    }

    /// Hold a matched provider request before publishing its first chunk.
    /// Callers can mutate external state only after runtime startup and then
    /// release the exact provider turn with [`MockStreamingBackend::release`].
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
    /// Withhold response headers until the script's marker is released.
    HoldHeaders(StreamScript),
    HttpStatus {
        status: u16,
        body: String,
    },
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

    /// Make provider responses after the first accepted turn caller-driven.
    ///
    /// This is useful when a later tool call needs an identifier returned by
    /// the first one (for example a durable background-process handle). The
    /// request handler waits until [`Self::enqueue_response`] supplies that
    /// response instead of replaying the plan's final static response.
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

    /// Every completion body this backend was posted, matched plan or not.
    ///
    /// Marker-scoped counts cannot answer "was anything sent at all?", which is
    /// the assertion a fail-closed capture test needs: an uncaptured send that
    /// matches no plan would still be a provider call that escaped.
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
    chunk_counts: HashMap<String, usize>,
    request_counts: HashMap<String, usize>,
    /// Every completion body posted to this backend, in arrival order.
    completion_bodies: Vec<serde_json::Value>,
    releases: HashSet<String>,
    dynamic_followups: HashSet<String>,
    dynamic_responses: HashMap<String, VecDeque<StreamResponse>>,
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
        // Session-title inference is auxiliary to the authored agent turn. It
        // may contain the same latest user text, but has no tool surface and
        // must never consume that turn's scripted tool response.
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
    body.get("max_tokens").and_then(serde_json::Value::as_u64) == Some(24)
        && body.get("tools").is_none()
        && body
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .and_then(|messages| messages.first())
            .is_some_and(|message| {
                message.get("role").and_then(serde_json::Value::as_str) == Some("system")
                    && message
                        .get("content")
                        .is_some_and(|content| content.to_string().contains(
                            "Generate concise conversation titles. Return only a lowercase hyphenated 3-5 word title.",
                        ))
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

fn matching_dynamic_markers(markers: &HashSet<String>, body: &str) -> Result<Vec<String>, String> {
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

/// `index` is the tool-call slot an OpenAI-compatible provider assigns. Using
/// the chunk position keeps two tool calls in one turn from colliding on slot 0,
/// which rig's accumulator would otherwise have to disambiguate by id and name.
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

#[cfg(test)]
mod routing_tests {
    use super::*;

    fn plan(marker: &str) -> StreamPlan {
        StreamPlan::current_authored_user(marker, Vec::new())
    }

    #[test]
    fn current_user_selector_ignores_child_prompt_in_parent_tool_arguments() {
        let body = json!({"messages": [
            {"role": "user", "content": [{"type": "text", "text": "parent prompt"}]},
            {"role": "assistant", "content": [{"type": "toolcall", "arguments": {"prompt": "child prompt"}}]}
        ]}).to_string();
        let plans = [plan("parent prompt"), plan("child prompt")];
        let matched = matching_plans(&plans, &body).unwrap();
        assert_eq!(matched[0].marker, "parent prompt");
    }

    #[test]
    fn current_user_selector_uses_child_authored_input_not_parent_history() {
        let body = json!({"messages": [
            {"role": "user", "content": [{"type": "text", "text": "parent prompt"}]},
            {"role": "user", "content": [{"type": "toolresult", "content": [{"type": "text", "text": "spawned child prompt"}]}]},
            {"role": "user", "content": [{"type": "text", "text": "child prompt"}]}
        ]}).to_string();
        let plans = [plan("parent prompt"), plan("child prompt")];
        let matched = matching_plans(&plans, &body).unwrap();
        assert_eq!(matched[0].marker, "child prompt");
    }

    #[test]
    fn current_user_selector_rejects_ambiguous_markers() {
        let body = json!({"messages": [
            {"role": "user", "content": [{"type": "text", "text": "alpha beta"}]}
        ]})
        .to_string();
        let plans = [plan("alpha"), plan("beta")];
        let error = matching_plans(&plans, &body).unwrap_err();
        assert!(error.contains("ambiguous mock provider plan"));
    }

    #[test]
    fn whole_body_selector_preserves_first_match_for_intentional_overlap() {
        let body = json!({"messages": [
            {"role": "system", "content": "structured-output schema"},
            {"role": "user", "content": "history marker"}
        ]})
        .to_string();
        let plans = [
            StreamPlan::new("structured-output schema", Vec::new()),
            StreamPlan::new("history marker", Vec::new()),
        ];
        let matched = matching_plans(&plans, &body).unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].marker, "structured-output schema");
    }

    #[test]
    fn dynamic_followup_selects_new_authored_marker_not_prior_tool_arguments() {
        let body = json!({"messages": [
            {"role": "user", "content": [{"type": "text", "text": "initial request"}]},
            {"role": "assistant", "content": [{"type": "toolcall", "arguments": {"prompt": "old dynamic marker"}}]},
            {"role": "user", "content": [{"type": "text", "text": "new dynamic marker"}]}
        ]}).to_string();
        let markers = HashSet::from([
            "old dynamic marker".to_string(),
            "new dynamic marker".to_string(),
        ]);
        assert_eq!(
            matching_dynamic_markers(&markers, &body).unwrap(),
            vec!["new dynamic marker"]
        );
    }

    #[test]
    fn dynamic_followup_rejects_ambiguous_new_authored_markers() {
        let body = json!({"messages": [
            {"role": "user", "content": [{"type": "text", "text": "new alpha beta request"}]}
        ]})
        .to_string();
        let markers = HashSet::from(["alpha".to_string(), "beta".to_string()]);
        let error = matching_dynamic_markers(&markers, &body).unwrap_err();
        assert!(error.contains("ambiguous dynamic mock provider plan"));
    }

    #[tokio::test]
    async fn session_title_request_does_not_consume_authored_turn_plan() {
        let marker = "nested child prompt";
        let state = StreamingState::new(
            "model".into(),
            vec![StreamPlan::current_authored_user(
                marker,
                vec![StreamResponse::completes(marker, ["authored turn"])],
            )],
            Arc::new(AtomicBool::new(false)),
        );
        let title = json!({
            "max_tokens": 24,
            "messages": [
                {"role":"system","content":[{"type":"text","text":"Generate concise conversation titles. Return only a lowercase hyphenated 3-5 word title. Never call tools. Never explain."}]},
                {"role":"user","content":format!("First user request:\n{marker}")}
            ],
            "stream": true
        }).to_string();
        let _ = state.next_response(&title).await;
        assert_eq!(state.request_count(marker), 0);

        let authored = json!({
            "max_tokens": 32768,
            "messages": [{"role":"user","content":marker}],
            "tools": [],
            "stream": true
        })
        .to_string();
        let _ = state.next_response(&authored).await;
        assert_eq!(state.request_count(marker), 1);
    }
}
