use rapidhash::{RapidHashMap, RapidHashSet};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::StreamExt as _;
use gents_store::test_support::STALL_CEILING;
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

    fn ends_with_tool_call(&self) -> bool {
        matches!(self.chunks.last(), Some(StreamChunk::ToolCall { .. }))
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
        StreamResponse::HttpStatus {
            status: StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            body: error_body(&message.into()),
        }
    }

    pub fn bad_request(body: impl Into<String>) -> Self {
        StreamResponse::HttpStatus {
            status: StatusCode::BAD_REQUEST.as_u16(),
            body: body.into(),
        }
    }

    fn not_found() -> Self {
        StreamResponse::HttpStatus {
            status: StatusCode::NOT_FOUND.as_u16(),
            body: r#"{"error":"not found"}"#.to_owned(),
        }
    }
}

/// What the backend answers to a completion no plan scripts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Unscripted {
    /// A short streamed reply.
    #[default]
    Reply,
    /// `404 Not Found`, as a server without a completions route answers.
    Refuse,
    /// Never answered while the backend lives.
    Hold,
}

/// How a held completion ends once the test lets it go.
#[derive(Clone, Debug)]
enum Release {
    Complete,
    Fail { status: u16, message: String },
    Close,
}

/// One HTTP request the backend received.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: RapidHashMap<String, String>,
    pub body: String,
}

pub struct ScriptedBackendBuilder {
    model_name: String,
    plans: Vec<StreamPlan>,
    unscripted: Unscripted,
    usage: bool,
    bearer: Option<String>,
}

impl ScriptedBackendBuilder {
    pub fn scripts(mut self, scripts: Vec<StreamScript>) -> Self {
        self.plans
            .extend(scripts.into_iter().map(StreamPlan::repeat_stream));
        self
    }

    pub fn plans(mut self, plans: Vec<StreamPlan>) -> Self {
        self.plans.extend(plans);
        self
    }

    pub fn unscripted(mut self, unscripted: Unscripted) -> Self {
        self.unscripted = unscripted;
        self
    }

    /// Streams and replies carry no token usage.
    pub fn without_usage(mut self) -> Self {
        self.usage = false;
        self
    }

    /// Every route answers `401` unless the request carries `Bearer {token}`.
    pub fn require_bearer(mut self, token: Option<&str>) -> Self {
        self.bearer = token.map(str::to_owned);
        self
    }

    pub fn start(self) -> anyhow::Result<ScriptedBackend> {
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(StreamingState {
            model_name: self.model_name,
            plans: self.plans,
            unscripted: self.unscripted,
            usage: self.usage,
            bearer: self.bearer,
            stop: stop.clone(),
            inner: Mutex::new(StreamingStateInner::default()),
            notify: Notify::new(),
        });
        let app = Router::new()
            .route("/v1/models", get(handle_models))
            .route("/models", get(handle_models))
            .route("/v1/key", get(handle_key))
            .route("/key", get(handle_key))
            .route("/v1/chat/completions", post(handle_chat))
            .route("/chat/completions", post(handle_chat))
            .route("/v1/messages", post(handle_messages))
            .route("/messages", post(handle_messages))
            .fallback(handle_fallback)
            .with_state(state.clone());

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        std::thread::Builder::new()
            .name("scripted-backend".to_owned())
            .spawn(move || {
                runtime.block_on(async move {
                    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                        return;
                    };
                    tokio::select! {
                        _ = axum::serve(listener, app) => {}
                        _ = shutdown_rx => {}
                    }
                });
            })?;

        Ok(ScriptedBackend {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            state,
            shutdown: Some(shutdown_tx),
        })
    }
}

/// The one model-provider double: an HTTP server speaking the OpenAI chat
/// completions and Claude Messages wires, answering each completion from the
/// plan whose marker it carries. The runtime reaches it through the production
/// backend client, so no production seam stands in for the provider. It serves
/// from a thread and runtime of its own, so a test that blocks its thread on a
/// spawned process is still answered; dropping it stops the server and its
/// open connections.
pub struct ScriptedBackend {
    endpoint: String,
    state: Arc<StreamingState>,
    shutdown: Option<oneshot::Sender<()>>,
}

/// A completion the backend holds before answering: [`Self::entered`]
/// resolves once a completion carrying the marker has arrived, and the call
/// ends as the test decides.
pub struct HeldCall<'a> {
    backend: &'a ScriptedBackend,
    marker: String,
}

impl HeldCall<'_> {
    pub async fn entered(&self) {
        let marker = self.marker.as_str();
        let entered = self
            .backend
            .state
            .wait_within_stall_ceiling(|inner| {
                inner
                    .held_entries
                    .get(marker)
                    .filter(|count| **count > 0)
                    .copied()
            })
            .await;
        if entered.is_none() {
            panic!("no completion carrying {marker} arrived within {STALL_CEILING:?}");
        }
    }

    /// Answers the held completion with its script.
    pub fn release(&self) {
        self.backend.state.release(&self.marker, Release::Complete);
    }

    /// Fails the held completion with `status`; a stream already under way
    /// carries the failure as its last event.
    pub fn fail(&self, status: u16, message: &str) {
        self.backend.state.release(
            &self.marker,
            Release::Fail {
                status,
                message: message.to_owned(),
            },
        );
    }

    /// Ends the held completion's stream with no terminal event.
    pub fn close(&self) {
        self.backend.state.release(&self.marker, Release::Close);
    }
}

impl ScriptedBackend {
    pub fn builder(model_name: &str) -> ScriptedBackendBuilder {
        ScriptedBackendBuilder {
            model_name: model_name.to_owned(),
            plans: Vec::new(),
            unscripted: Unscripted::default(),
            usage: true,
            bearer: None,
        }
    }

    pub fn start(model_name: &str, scripts: Vec<StreamScript>) -> anyhow::Result<Self> {
        Self::builder(model_name).scripts(scripts).start()
    }

    pub fn start_with_plans(model_name: &str, plans: Vec<StreamPlan>) -> anyhow::Result<Self> {
        Self::builder(model_name).plans(plans).start()
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Holds every completion carrying `marker` before its response starts,
    /// until the returned call ends it.
    pub fn hold(&self, marker: &str) -> HeldCall<'_> {
        self.state.lock().held.insert(marker.to_owned());
        HeldCall {
            backend: self,
            marker: marker.to_owned(),
        }
    }

    /// Lets the paused script or held call for `marker` complete.
    pub fn release(&self, marker: &str) {
        self.state.release(marker, Release::Complete);
    }

    pub fn observed_chunks(&self, marker: &str) -> usize {
        self.state.chunk_count(marker)
    }

    pub fn observed_requests(&self, marker: &str) -> usize {
        self.state.request_count(marker)
    }

    pub async fn wait_for_request(&self, marker: &str) {
        let observed = self
            .state
            .wait_within_stall_ceiling(|inner| {
                let count = inner
                    .request_counts
                    .get(marker)
                    .copied()
                    .unwrap_or_default();
                (count > 0).then_some(count)
            })
            .await;
        if observed.is_none() {
            panic!(
                "request for {marker} stalled for {STALL_CEILING:?}; observed={}",
                self.observed_requests(marker)
            );
        }
    }

    pub fn enable_dynamic_followups(&self, marker: &str) {
        self.state
            .lock()
            .dynamic_followups
            .insert(marker.to_string());
        self.state.notify.notify_waiters();
    }

    pub fn enqueue_response(&self, marker: &str, response: StreamResponse) {
        self.state
            .lock()
            .dynamic_responses
            .entry(marker.to_string())
            .or_default()
            .push_back(response);
        self.state.notify.notify_waiters();
    }

    pub fn observed_completion_bodies(&self) -> Vec<serde_json::Value> {
        self.state.lock().completion_bodies.clone()
    }

    pub fn observed_completion_requests(&self) -> usize {
        self.observed_completion_bodies().len()
    }

    /// The completions issued for requests' own turns. A session-title audit
    /// is its own auxiliary request racing the request that opened the
    /// session, so its completion has no fixed position among them.
    pub fn observed_turn_completion_bodies(&self) -> Vec<serde_json::Value> {
        self.observed_completion_bodies()
            .into_iter()
            .filter(|body| !body_is_session_title(body))
            .collect()
    }

    pub fn recorded_requests(&self) -> Vec<RecordedRequest> {
        self.state.lock().recorded.clone()
    }

    pub async fn wait_for_chunks(&self, marker: &str, expected: usize) {
        let observed = self
            .state
            .wait_within_stall_ceiling(|inner| {
                let count = inner.chunk_counts.get(marker).copied().unwrap_or_default();
                (count >= expected).then_some(count)
            })
            .await;
        assert!(
            observed.is_some(),
            "timed out waiting for {expected} chunk(s) for marker {marker}, observed {}",
            self.observed_chunks(marker)
        );
    }
}

impl Drop for ScriptedBackend {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Relaxed);
        self.state.notify.notify_waiters();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

struct StreamingState {
    model_name: String,
    plans: Vec<StreamPlan>,
    unscripted: Unscripted,
    usage: bool,
    bearer: Option<String>,
    stop: Arc<AtomicBool>,
    inner: Mutex<StreamingStateInner>,
    notify: Notify,
}

#[derive(Default)]
struct StreamingStateInner {
    chunk_counts: RapidHashMap<String, usize>,
    request_counts: RapidHashMap<String, usize>,
    completion_bodies: Vec<serde_json::Value>,
    recorded: Vec<RecordedRequest>,
    releases: RapidHashMap<String, Release>,
    held: RapidHashSet<String>,
    held_entries: RapidHashMap<String, usize>,
    dynamic_followups: RapidHashSet<String>,
    dynamic_responses: RapidHashMap<String, VecDeque<StreamResponse>>,
}

impl StreamingState {
    fn lock(&self) -> std::sync::MutexGuard<'_, StreamingStateInner> {
        self.inner.lock().expect("scripted backend mutex poisoned")
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Resolves with the first value `ready` yields, re-checking on each
    /// change to the backend's state; `None` once the backend stops.
    async fn wait_for<T>(
        &self,
        mut ready: impl FnMut(&mut StreamingStateInner) -> Option<T>,
    ) -> Option<T> {
        loop {
            let changed = self.notify.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.stopped() {
                return None;
            }
            if let Some(value) = ready(&mut self.lock()) {
                return Some(value);
            }
            changed.await;
        }
    }

    /// [`Self::wait_for`], giving up once nothing changes for
    /// [`STALL_CEILING`].
    async fn wait_within_stall_ceiling<T>(
        &self,
        mut ready: impl FnMut(&mut StreamingStateInner) -> Option<T>,
    ) -> Option<T> {
        loop {
            let changed = self.notify.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(value) = ready(&mut self.lock()) {
                return Some(value);
            }
            if tokio::time::timeout(STALL_CEILING, changed).await.is_err() {
                return None;
            }
        }
    }

    fn record(&self, method: &str, uri: &Uri, headers: &HeaderMap, body: &str) {
        self.lock().recorded.push(RecordedRequest {
            method: method.to_owned(),
            path: uri.path().to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_owned(),
                        value.to_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect(),
            body: body.to_owned(),
        });
        self.notify.notify_waiters();
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        self.bearer.as_ref().is_none_or(|expected| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                == Some(format!("Bearer {expected}").as_str())
        })
    }

    fn record_completion_body(&self, body: &str) {
        let value = serde_json::from_str::<serde_json::Value>(body)
            .unwrap_or_else(|_| json!({ "__unparsed__": body }));
        self.lock().completion_bodies.push(value);
        self.notify.notify_waiters();
    }

    async fn unscripted_response(&self, script: StreamScript) -> StreamResponse {
        match self.unscripted {
            Unscripted::Reply => StreamResponse::Stream(script),
            Unscripted::Refuse => StreamResponse::not_found(),
            Unscripted::Hold => {
                self.wait_for(|_| None::<()>).await;
                StreamResponse::not_found()
            }
        }
    }

    async fn next_response(&self, body: &str) -> StreamResponse {
        if request_is_session_title(body) {
            return self
                .unscripted_response(StreamScript::completes("__session_title__", ["mock-title"]))
                .await;
        }
        let matching_plans = match matching_plans(&self.plans, body) {
            Ok(plans) => plans,
            Err(diagnostic) => return StreamResponse::bad_request(diagnostic),
        };
        let Some(plan) = matching_plans.into_iter().next() else {
            let dynamic_markers =
                match matching_dynamic_markers(&self.lock().dynamic_followups, body) {
                    Ok(markers) => markers,
                    Err(diagnostic) => return StreamResponse::bad_request(diagnostic),
                };
            if let Some(marker) = dynamic_markers.into_iter().next() {
                self.reserve_response_index(&marker);
                self.notify.notify_waiters();
                if let Some(response) = self
                    .wait_for(|inner| {
                        inner
                            .dynamic_responses
                            .get_mut(&marker)
                            .and_then(VecDeque::pop_front)
                    })
                    .await
                {
                    return response;
                }
            }
            return self
                .unscripted_response(StreamScript::completes(
                    "__default__",
                    ["mock streamed response"],
                ))
                .await;
        };

        let response_index = self.reserve_response_index(&plan.marker);
        self.notify.notify_waiters();

        if response_index > 0 {
            let dynamic = self
                .wait_for(|inner| {
                    if !inner.dynamic_followups.contains(&plan.marker) {
                        return Some(None);
                    }
                    inner
                        .dynamic_responses
                        .get_mut(&plan.marker)
                        .and_then(VecDeque::pop_front)
                        .map(Some)
                })
                .await
                .flatten();
            if let Some(response) = dynamic {
                return response;
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

    /// The held marker the completion carries, counted as an entry to it.
    fn enter_hold(&self, body: &str) -> Option<String> {
        if request_is_session_title(body) {
            return None;
        }
        let mut inner = self.lock();
        let marker = inner
            .held
            .iter()
            .find(|marker| body.contains(marker.as_str()))
            .cloned()?;
        *inner.held_entries.entry(marker.clone()).or_default() += 1;
        drop(inner);
        self.notify.notify_waiters();
        Some(marker)
    }

    fn reserve_response_index(&self, marker: &str) -> usize {
        let mut inner = self.lock();
        let count = inner.request_counts.entry(marker.to_string()).or_default();
        let response_index = *count;
        *count += 1;
        response_index
    }

    fn record_chunk(&self, marker: &str) {
        *self
            .lock()
            .chunk_counts
            .entry(marker.to_string())
            .or_default() += 1;
        self.notify.notify_waiters();
    }

    fn chunk_count(&self, marker: &str) -> usize {
        self.lock()
            .chunk_counts
            .get(marker)
            .copied()
            .unwrap_or_default()
    }

    fn request_count(&self, marker: &str) -> usize {
        self.lock()
            .request_counts
            .get(marker)
            .copied()
            .unwrap_or_default()
    }

    fn release(&self, marker: &str, release: Release) {
        self.lock().releases.insert(marker.to_string(), release);
        self.notify.notify_waiters();
    }

    /// How the paused or held `marker` ends; `None` once the backend stops.
    async fn released(&self, marker: &str) -> Option<Release> {
        self.wait_for(|inner| inner.releases.get(marker).cloned())
            .await
    }
}

fn error_body(message: &str) -> String {
    json!({ "error": { "message": message, "type": "server_error" } }).to_string()
}

fn request_is_session_title(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body).is_ok_and(|body| body_is_session_title(&body))
}

const SESSION_TITLE_PROMPT: &str = "Generate concise conversation titles.";

fn body_is_session_title(body: &serde_json::Value) -> bool {
    let system_message = body
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .and_then(|messages| messages.first())
        .is_some_and(|message| {
            message.get("role").and_then(serde_json::Value::as_str) == Some("system")
                && message
                    .get("content")
                    .is_some_and(|content| content.to_string().contains(SESSION_TITLE_PROMPT))
        });
    let system_field = body
        .get("system")
        .is_some_and(|system| system.to_string().contains(SESSION_TITLE_PROMPT));
    body.get("tools").is_none() && (system_message || system_field)
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
    if markers.is_empty() {
        return Ok(Vec::new());
    }
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

fn json_response(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn unauthorized() -> Response {
    json_response(
        StatusCode::UNAUTHORIZED,
        r#"{"error":"unauthorized"}"#.to_owned(),
    )
}

async fn handle_models(
    State(state): State<Arc<StreamingState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record("GET", &uri, &headers, "");
    if !state.authorized(&headers) {
        return unauthorized();
    }
    json_response(
        StatusCode::OK,
        format!(r#"{{"data":[{{"id":"{}"}}]}}"#, state.model_name),
    )
}

async fn handle_key(
    State(state): State<Arc<StreamingState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record("GET", &uri, &headers, "");
    if !state.authorized(&headers) {
        return unauthorized();
    }
    json_response(
        StatusCode::OK,
        r#"{"data":{"label":"test-key"}}"#.to_owned(),
    )
}

#[derive(Clone, Copy)]
enum Wire {
    ChatCompletions,
    Messages,
}

async fn handle_chat(
    State(state): State<Arc<StreamingState>>,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    handle_completion(state, Wire::ChatCompletions, uri, headers, body).await
}

async fn handle_messages(
    State(state): State<Arc<StreamingState>>,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    handle_completion(state, Wire::Messages, uri, headers, body).await
}

async fn handle_completion(
    state: Arc<StreamingState>,
    wire: Wire,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    state.record("POST", &uri, &headers, &body);
    if !state.authorized(&headers) {
        return unauthorized();
    }
    state.record_completion_body(&body);
    if let Some(marker) = state.enter_hold(&body) {
        match state.released(&marker).await {
            Some(Release::Complete) => {}
            Some(Release::Fail { status, message }) => {
                return http_status(status, error_body(&message));
            }
            Some(Release::Close) | None => return closed_stream(),
        }
    }
    if !request_is_streaming(&body) {
        let fallback = StreamScript::completes("__complete__", ["mock-title"]);
        return match state.unscripted_response(fallback).await {
            StreamResponse::HttpStatus { status, body } => http_status(status, body),
            _ => json_response(StatusCode::OK, complete_reply(&state, wire)),
        };
    }
    match state.next_response(&body).await {
        StreamResponse::Stream(script) => streaming_response(script, state, wire),
        StreamResponse::HoldHeaders(script) => match state.released(&script.marker).await {
            Some(Release::Fail { status, message }) => http_status(status, error_body(&message)),
            Some(Release::Complete) => streaming_response(script, state, wire),
            Some(Release::Close) | None => closed_stream(),
        },
        StreamResponse::HttpStatus { status, body } => http_status(status, body),
    }
}

fn closed_stream() -> Response {
    Sse::new(futures::stream::empty::<Result<Event, Infallible>>()).into_response()
}

fn http_status(status: u16, body: String) -> Response {
    json_response(
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        body,
    )
}

async fn handle_fallback() -> Response {
    json_response(StatusCode::NOT_FOUND, r#"{"error":"not found"}"#.to_owned())
}

/// The non-streamed reply every unscripted completion receives.
fn complete_reply(state: &StreamingState, wire: Wire) -> String {
    let mut reply = match wire {
        Wire::ChatCompletions => json!({
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
            "usage": { "prompt_tokens": 4, "completion_tokens": 1, "total_tokens": 5 }
        }),
        Wire::Messages => json!({
            "id": "msg_scripted",
            "type": "message",
            "role": "assistant",
            "model": state.model_name,
            "content": [{ "type": "text", "text": "mock-title" }],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": { "input_tokens": 4, "output_tokens": 1 }
        }),
    };
    if !state.usage {
        if let Some(reply) = reply.as_object_mut() {
            reply.remove("usage");
        }
    }
    reply.to_string()
}

fn streaming_response(script: StreamScript, state: Arc<StreamingState>, wire: Wire) -> Response {
    #[derive(Clone, Copy)]
    enum Phase {
        Start,
        AwaitInitialRelease,
        Chunk(usize),
        AwaitRelease,
        Finish,
        Finished,
    }

    let init = (Phase::Start, script, state);
    let stream = futures::stream::unfold(init, move |(phase, script, state)| async move {
        let mut phase = phase;
        loop {
            match phase {
                Phase::Start => {
                    let next = if script.pause_before_chunks {
                        Phase::AwaitInitialRelease
                    } else {
                        Phase::Chunk(0)
                    };
                    let events = start_events(&state, wire);
                    if events.is_empty() {
                        phase = next;
                        continue;
                    }
                    return Some((events, (next, script, state)));
                }
                Phase::AwaitInitialRelease | Phase::AwaitRelease => {
                    let after = if matches!(phase, Phase::AwaitInitialRelease) {
                        Phase::Chunk(0)
                    } else {
                        Phase::Finish
                    };
                    match state.released(&script.marker).await {
                        Some(Release::Complete) => phase = after,
                        Some(Release::Fail { message, .. }) => {
                            return Some((
                                error_events(wire, &message),
                                (Phase::Finished, script, state),
                            ));
                        }
                        Some(Release::Close) | None => return None,
                    }
                }
                Phase::Chunk(index) => {
                    if state.stopped() {
                        return None;
                    }
                    if index < script.chunks.len() {
                        let events = chunk_events(wire, index, &script.chunks[index]);
                        state.record_chunk(&script.marker);
                        return Some((events, (Phase::Chunk(index + 1), script, state)));
                    }
                    phase = if script.pause_after_chunks {
                        Phase::AwaitRelease
                    } else {
                        Phase::Finish
                    };
                }
                Phase::Finish => {
                    let events = finish_events(&state, wire, &script);
                    return Some((events, (Phase::Finished, script, state)));
                }
                Phase::Finished => return None,
            }
        }
    })
    .flat_map(|events| futures::stream::iter(events.into_iter().map(Ok::<Event, Infallible>)));

    Sse::new(stream).into_response()
}

fn data(value: serde_json::Value) -> Event {
    Event::default().data(value.to_string())
}

fn named(name: &str, value: serde_json::Value) -> Event {
    Event::default().event(name).data(value.to_string())
}

fn start_events(state: &StreamingState, wire: Wire) -> Vec<Event> {
    match wire {
        Wire::ChatCompletions => Vec::new(),
        Wire::Messages => {
            let mut message = json!({
                "id": "msg_scripted",
                "type": "message",
                "role": "assistant",
                "model": state.model_name,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": { "input_tokens": 8, "output_tokens": 0 }
            });
            if !state.usage {
                if let Some(message) = message.as_object_mut() {
                    message.remove("usage");
                }
            }
            vec![named(
                "message_start",
                json!({ "type": "message_start", "message": message }),
            )]
        }
    }
}

fn chunk_events(wire: Wire, index: usize, chunk: &StreamChunk) -> Vec<Event> {
    match wire {
        Wire::ChatCompletions => vec![data(chat_chunk(index, chunk))],
        Wire::Messages => {
            let (block, delta) = match chunk {
                StreamChunk::Text(text) => (
                    json!({ "type": "text", "text": "" }),
                    vec![json!({ "type": "text_delta", "text": text })],
                ),
                StreamChunk::Reasoning(reasoning) => (
                    json!({ "type": "thinking", "thinking": "" }),
                    vec![
                        json!({ "type": "thinking_delta", "thinking": reasoning }),
                        json!({ "type": "signature_delta", "signature": "scripted" }),
                    ],
                ),
                StreamChunk::ToolCall {
                    id,
                    name,
                    arguments,
                } => (
                    json!({ "type": "tool_use", "id": id, "name": name, "input": {} }),
                    vec![json!({ "type": "input_json_delta", "partial_json": arguments })],
                ),
            };
            let mut events = vec![named(
                "content_block_start",
                json!({ "type": "content_block_start", "index": index, "content_block": block }),
            )];
            events.extend(delta.into_iter().map(|delta| {
                named(
                    "content_block_delta",
                    json!({ "type": "content_block_delta", "index": index, "delta": delta }),
                )
            }));
            events.push(named(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": index }),
            ));
            events
        }
    }
}

fn chat_chunk(index: usize, chunk: &StreamChunk) -> serde_json::Value {
    let delta = match chunk {
        StreamChunk::Text(content) => json!({ "content": content, "tool_calls": [] }),
        StreamChunk::Reasoning(reasoning) => {
            json!({ "content": null, "reasoning_content": reasoning, "tool_calls": [] })
        }
        StreamChunk::ToolCall {
            id,
            name,
            arguments,
        } => json!({
            "content": null,
            "tool_calls": [{
                "index": index,
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": arguments }
            }]
        }),
    };
    json!({
        "choices": [{ "delta": delta, "finish_reason": null }],
        "usage": null
    })
}

fn finish_events(state: &StreamingState, wire: Wire, script: &StreamScript) -> Vec<Event> {
    match wire {
        Wire::ChatCompletions => {
            let mut events = Vec::new();
            if state.usage {
                events.push(data(json!({
                    "choices": [],
                    "usage": { "prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11 }
                })));
            }
            events.push(Event::default().data("[DONE]"));
            events
        }
        Wire::Messages => {
            let stop_reason = if script.ends_with_tool_call() {
                "tool_use"
            } else {
                "end_turn"
            };
            let mut delta = json!({
                "type": "message_delta",
                "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                "usage": { "output_tokens": 3 }
            });
            if !state.usage {
                if let Some(delta) = delta.as_object_mut() {
                    delta.remove("usage");
                }
            }
            vec![
                named("message_delta", delta),
                named("message_stop", json!({ "type": "message_stop" })),
            ]
        }
    }
}

fn error_events(wire: Wire, message: &str) -> Vec<Event> {
    match wire {
        Wire::ChatCompletions => vec![data(
            json!({ "error": { "message": message, "type": "server_error" } }),
        )],
        Wire::Messages => vec![named(
            "error",
            json!({ "type": "error", "error": { "type": "api_error", "message": message } }),
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn complete(endpoint: String, marker: &'static str) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{endpoint}/chat/completions"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(
                json!({ "stream": true, "messages": [{ "role": "user", "content": marker }] })
                    .to_string(),
            )
            .send()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_held_call_ends_as_the_test_ends_it() {
        let backend = ScriptedBackend::builder("model")
            .without_usage()
            .start()
            .unwrap();

        let released = backend.hold("released-call");
        let call = tokio::spawn(complete(backend.endpoint().to_owned(), "released-call"));
        released.entered().await;
        released.release();
        let body = call.await.unwrap().text().await.unwrap();
        assert!(body.contains("mock streamed response"), "{body}");
        assert!(body.contains("[DONE]"), "{body}");
        assert!(!body.contains("prompt_tokens"), "usage is off: {body}");

        let failed = backend.hold("failed-call");
        let call = tokio::spawn(complete(backend.endpoint().to_owned(), "failed-call"));
        failed.entered().await;
        failed.fail(503, "provider unavailable");
        let response = call.await.unwrap();
        assert_eq!(response.status().as_u16(), 503);
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("provider unavailable"));

        let closed = backend.hold("closed-call");
        let call = tokio::spawn(complete(backend.endpoint().to_owned(), "closed-call"));
        closed.entered().await;
        closed.close();
        let body = call.await.unwrap().text().await.unwrap();
        assert!(
            !body.contains("[DONE]"),
            "a closed stream has no terminal event: {body}"
        );
    }

    #[tokio::test]
    async fn usage_is_reported_unless_turned_off() {
        let backend = ScriptedBackend::start("model", Vec::new()).unwrap();
        let body = complete(backend.endpoint().to_owned(), "any")
            .await
            .text()
            .await
            .unwrap();
        assert!(body.contains("prompt_tokens"), "{body}");
    }
}
