use rapidhash::{RapidHashMap, RapidHashSet};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use gents_store::test_support::STALL_CEILING;
use serde_json::{json, Value};
use tokio::sync::{oneshot, Notify};

mod listener;
mod wire;

use listener::{PeerAddr, TrackedListener};
use wire::Wire;

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamScript {
    marker: String,
    chunks: Vec<StreamChunk>,
    /// The stream waits for the marker's release before this chunk.
    pause_at: Option<usize>,
}

impl StreamScript {
    /// Streams `chunks`, then waits for the marker's release before ending.
    pub fn paused(
        marker: impl Into<String>,
        chunks: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        let chunks: Vec<_> = chunks.into_iter().map(StreamChunk::text).collect();
        Self {
            marker: marker.into(),
            pause_at: Some(chunks.len()),
            chunks,
        }
    }

    pub fn completes(
        marker: impl Into<String>,
        chunks: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        Self::streams(marker, chunks.into_iter().map(StreamChunk::text).collect())
    }

    pub fn streams(marker: impl Into<String>, chunks: Vec<StreamChunk>) -> Self {
        Self {
            marker: marker.into(),
            chunks,
            pause_at: None,
        }
    }

    /// Opens the stream, then waits for the marker's release before any chunk.
    pub fn paused_before(marker: impl Into<String>, chunks: Vec<StreamChunk>) -> Self {
        Self {
            marker: marker.into(),
            chunks,
            pause_at: Some(0),
        }
    }

    /// Chunks streamed after the pause, once the marker is released.
    pub fn then(mut self, chunks: Vec<StreamChunk>) -> Self {
        self.chunks.extend(chunks);
        self
    }

    fn ends_with_tool_call(&self) -> bool {
        matches!(self.chunks.last(), Some(StreamChunk::ToolCall { .. }))
    }

    fn text(&self) -> String {
        self.chunks
            .iter()
            .filter_map(|chunk| match chunk {
                StreamChunk::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamResponse {
    Stream(StreamScript),
    /// Answered with `then` once `gate` is released; failing or closing the
    /// gate ends the call before its response starts.
    Held {
        gate: String,
        then: Box<StreamResponse>,
    },
    HttpStatus {
        status: u16,
        body: String,
    },
    /// The server closes the connection the request arrived on without
    /// answering, as a server whose idle timeout fires as a request arrives.
    Disconnect,
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

    /// One streamed text reply.
    pub fn text(text: impl Into<String>) -> Self {
        Self::streams(REPLY_MARKER, vec![StreamChunk::text(text)])
    }

    /// One streamed call to `name`.
    pub fn tool_call(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self::streams(
            REPLY_MARKER,
            vec![StreamChunk::tool_call(id, name, arguments)],
        )
    }

    pub fn status(status: u16, body: impl Into<String>) -> Self {
        StreamResponse::HttpStatus {
            status,
            body: body.into(),
        }
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::status(
            StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            error_body(&message.into()),
        )
    }

    pub fn bad_request(body: impl Into<String>) -> Self {
        Self::status(StatusCode::BAD_REQUEST.as_u16(), body)
    }

    /// This response, sent once `gate` is released.
    pub fn after(self, gate: impl Into<String>) -> Self {
        StreamResponse::Held {
            gate: gate.into(),
            then: Box::new(self),
        }
    }

    fn not_found() -> Self {
        Self::status(StatusCode::NOT_FOUND.as_u16(), r#"{"error":"not found"}"#)
    }
}

const REPLY_MARKER: &str = "__reply__";
const DEFAULT_REPLY: &str = "mock response";
const MODELS_GATE: &str = "__models__";

/// What the backend answers to a completion no responder or plan scripts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Unscripted {
    /// A short streamed reply.
    #[default]
    Reply,
    /// `404 Not Found`, as a server without a completions route answers.
    Refuse,
    /// Never answered while the backend lives.
    Hold,
    /// Answers as a reasoning model: a tool-bearing request gets a reasoning
    /// item and a call to its first tool, and once that call's output arrives,
    /// a reasoning item and a reply.
    ReasoningToolTurn,
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

type Responder = Arc<dyn Fn(&Value) -> Option<StreamResponse> + Send + Sync>;

pub struct ScriptedBackendBuilder {
    models: Vec<String>,
    plans: Vec<StreamPlan>,
    responder: Option<Responder>,
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

    /// The model ids `/v1/models` lists after the first.
    pub fn models(mut self, models: &[&str]) -> Self {
        self.models
            .extend(models.iter().map(|model| (*model).to_owned()));
        self
    }

    /// Answers each completion `respond` returns a response for, before any
    /// plan; it sees the parsed request body.
    pub fn respond(
        mut self,
        respond: impl Fn(&Value) -> Option<StreamResponse> + Send + Sync + 'static,
    ) -> Self {
        self.responder = Some(Arc::new(respond));
        self
    }

    /// Answers every completion with `text`.
    pub fn replies(self, text: &str) -> Self {
        let text = text.to_owned();
        self.respond(move |_| Some(StreamResponse::text(text.clone())))
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
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let connections = listener::Connections::default();
        let state = Arc::new(StreamingState {
            models: self.models,
            plans: self.plans,
            responder: self.responder,
            unscripted: self.unscripted,
            usage: self.usage,
            bearer: self.bearer,
            stop: stop.clone(),
            connections: connections.clone(),
            inner: Mutex::new(StreamingStateInner::default()),
            notify: Notify::new(),
        });
        let app = Router::new()
            .route("/v1/models", get(handle_models))
            .route("/models", get(handle_models))
            .route("/v1/models-v2", get(handle_models))
            .route("/models-v2", get(handle_models))
            .route("/v1/key", get(handle_key))
            .route("/key", get(handle_key))
            .route("/v1/chat/completions", post(handle_chat))
            .route("/chat/completions", post(handle_chat))
            .route("/v1/messages", post(handle_messages))
            .route("/messages", post(handle_messages))
            .route("/v1/responses", post(handle_responses))
            .route("/responses", post(handle_responses))
            .fallback(handle_fallback)
            .with_state(state.clone())
            .into_make_service_with_connect_info::<PeerAddr>();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let (stopped_tx, stopped_rx) = oneshot::channel::<()>();
        std::thread::Builder::new()
            .name("scripted-backend".to_owned())
            .spawn(move || {
                runtime.block_on(async move {
                    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                        return;
                    };
                    let listener = TrackedListener::new(listener, connections);
                    tokio::select! {
                        _ = axum::serve(listener, app) => {}
                        _ = shutdown_rx => {}
                    }
                });
                drop(runtime);
                let _ = stopped_tx.send(());
            })?;

        Ok(ScriptedBackend {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            state,
            shutdown: Some(shutdown_tx),
            stopped: Some(stopped_rx),
        })
    }
}

/// The one model-provider double: an HTTP server speaking the OpenAI chat
/// completions, OpenAI Responses and Claude Messages wires, answering each
/// completion from its responder, else the plan whose marker it carries. The
/// runtime reaches it through the production backend client, so no production
/// seam stands in for the provider. It serves from a thread and runtime of its
/// own, so a test that blocks its thread on a spawned process is still
/// answered; dropping it stops the server and its open connections.
pub struct ScriptedBackend {
    endpoint: String,
    state: Arc<StreamingState>,
    shutdown: Option<oneshot::Sender<()>>,
    stopped: Option<oneshot::Receiver<()>>,
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
            panic!("no request carrying {marker} arrived within {STALL_CEILING:?}");
        }
    }

    /// Answers the held request with its script.
    pub fn release(&self) {
        self.backend.state.release(&self.marker, Release::Complete);
    }

    /// Fails the held request with `status`; a stream already under way
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

    /// Ends the held request's stream with no terminal event.
    pub fn close(&self) {
        self.backend.state.release(&self.marker, Release::Close);
    }
}

impl ScriptedBackend {
    pub fn builder(model_name: &str) -> ScriptedBackendBuilder {
        ScriptedBackendBuilder {
            models: vec![model_name.to_owned()],
            plans: Vec::new(),
            responder: None,
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

    /// Stops the server and returns once its port no longer accepts.
    pub async fn shutdown(mut self) {
        let stopped = self.stopped.take();
        drop(self);
        if let Some(stopped) = stopped {
            let _ = stopped.await;
        }
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

    /// Holds every `/v1/models` request until the returned call ends it.
    pub fn hold_models(&self) -> HeldCall<'_> {
        self.hold(MODELS_GATE)
    }

    /// Lets the paused script, gated response or held call for `marker`
    /// complete, now and for every later request it gates.
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

    pub fn observed_completion_bodies(&self) -> Vec<Value> {
        self.state.lock().completion_bodies.clone()
    }

    pub fn observed_completion_requests(&self) -> usize {
        self.observed_completion_bodies().len()
    }

    /// The completions issued for requests' own turns. A session-title audit
    /// is its own auxiliary request racing the request that opened the
    /// session, so its completion has no fixed position among them.
    pub fn observed_turn_completion_bodies(&self) -> Vec<Value> {
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
    models: Vec<String>,
    plans: Vec<StreamPlan>,
    responder: Option<Responder>,
    unscripted: Unscripted,
    usage: bool,
    bearer: Option<String>,
    stop: Arc<AtomicBool>,
    connections: listener::Connections,
    inner: Mutex<StreamingStateInner>,
    notify: Notify,
}

#[derive(Default)]
struct StreamingStateInner {
    chunk_counts: RapidHashMap<String, usize>,
    request_counts: RapidHashMap<String, usize>,
    completion_bodies: Vec<Value>,
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

    fn model_name(&self) -> &str {
        &self.models[0]
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

    /// Records the request; returns its position in the request log, from 1.
    fn record(&self, method: &str, uri: &Uri, headers: &HeaderMap, body: &str) -> usize {
        let mut inner = self.lock();
        inner.recorded.push(RecordedRequest {
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
        let ordinal = inner.recorded.len();
        drop(inner);
        self.notify.notify_waiters();
        ordinal
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        self.bearer.as_ref().is_none_or(|expected| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                == Some(format!("Bearer {expected}").as_str())
        })
    }

    fn record_completion_body(&self, body: &Value) {
        self.lock().completion_bodies.push(body.clone());
        self.notify.notify_waiters();
    }

    async fn unscripted_response(
        &self,
        script: StreamScript,
        body: &Value,
        ordinal: usize,
    ) -> StreamResponse {
        match self.unscripted {
            Unscripted::Reply => StreamResponse::Stream(script),
            Unscripted::Refuse => StreamResponse::not_found(),
            Unscripted::Hold => {
                self.wait_for(|_| None::<()>).await;
                StreamResponse::not_found()
            }
            Unscripted::ReasoningToolTurn => wire::reasoning_tool_turn(body, ordinal)
                .map_or(StreamResponse::Stream(script), StreamResponse::Stream),
        }
    }

    async fn next_response(&self, raw: &str, body: &Value, ordinal: usize) -> StreamResponse {
        if body_is_session_title(body) {
            return self
                .unscripted_response(
                    StreamScript::completes("__session_title__", ["mock-title"]),
                    body,
                    ordinal,
                )
                .await;
        }
        let default = || StreamScript::completes("__default__", [DEFAULT_REPLY]);
        let matching_plans = match matching_plans(&self.plans, raw, body) {
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
            return self.unscripted_response(default(), body, ordinal).await;
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
            .unwrap_or_else(|| StreamResponse::Stream(default()))
    }

    /// The held marker the request carries, counted as an entry to it.
    fn enter_hold(&self, body: &str) -> Option<String> {
        let mut inner = self.lock();
        let marker = inner
            .held
            .iter()
            .filter(|marker| marker.as_str() != MODELS_GATE)
            .find(|marker| body.contains(marker.as_str()))
            .cloned()?;
        *inner.held_entries.entry(marker.clone()).or_default() += 1;
        drop(inner);
        self.notify.notify_waiters();
        Some(marker)
    }

    /// Whether `/v1/models` is held, counting this request as an entry.
    fn enter_models_hold(&self) -> bool {
        let mut inner = self.lock();
        if !inner.held.contains(MODELS_GATE) {
            return false;
        }
        *inner
            .held_entries
            .entry(MODELS_GATE.to_owned())
            .or_default() += 1;
        drop(inner);
        self.notify.notify_waiters();
        true
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

const SESSION_TITLE_PROMPT: &str = "Generate concise conversation titles.";

fn body_is_session_title(body: &Value) -> bool {
    let system_message = body
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.first())
        .is_some_and(|message| {
            message.get("role").and_then(Value::as_str) == Some("system")
                && message
                    .get("content")
                    .is_some_and(|content| content.to_string().contains(SESSION_TITLE_PROMPT))
        });
    let system_field = body
        .get("system")
        .is_some_and(|system| system.to_string().contains(SESSION_TITLE_PROMPT));
    body.get("tools").is_none() && (system_message || system_field)
}

fn current_authored_user_input(body: &Value) -> Result<String, String> {
    if body.get("__unparsed__").is_some() {
        return Err("invalid mock provider request JSON".to_string());
    }
    body.get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| "mock provider request is missing messages array".to_string())?
        .iter()
        .rev()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .find_map(|message| match message.get("content")? {
            Value::String(text) => Some(text.clone()),
            Value::Array(blocks) => {
                let text = blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        })
        .ok_or_else(|| "mock provider request has no authored user input".to_string())
}

fn matching_plans<'a>(
    plans: &'a [StreamPlan],
    raw: &str,
    body: &Value,
) -> Result<Vec<&'a StreamPlan>, String> {
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
        .find(|plan| matches!(plan.selector, PlanSelector::WholeBody) && raw.contains(&plan.marker))
        .into_iter()
        .collect())
}

fn matching_dynamic_markers(
    markers: &RapidHashSet<String>,
    body: &Value,
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

fn json_response(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn http_status(status: u16, body: String) -> Response {
    json_response(
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        body,
    )
}

fn unauthorized() -> Response {
    json_response(
        StatusCode::UNAUTHORIZED,
        r#"{"error":"unauthorized"}"#.to_owned(),
    )
}

async fn handle_models(
    State(state): State<Arc<StreamingState>>,
    ConnectInfo(peer): ConnectInfo<PeerAddr>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record("GET", &uri, &headers, "");
    if !state.authorized(&headers) {
        return unauthorized();
    }
    if state.enter_models_hold() {
        match state.released(MODELS_GATE).await {
            Some(Release::Complete) => {}
            Some(Release::Fail { status, message }) => {
                return http_status(status, error_body(&message));
            }
            Some(Release::Close) | None => {
                state.connections.close(peer.0);
                return wire::closed_stream();
            }
        }
    }
    let data = state
        .models
        .iter()
        .map(|model| json!({ "id": model }))
        .collect::<Vec<_>>();
    let models = state
        .models
        .iter()
        .map(|model| json!({ "model": model, "name": model }))
        .collect::<Vec<_>>();
    json_response(
        StatusCode::OK,
        json!({ "data": data, "models": models }).to_string(),
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

async fn handle_chat(
    State(state): State<Arc<StreamingState>>,
    ConnectInfo(peer): ConnectInfo<PeerAddr>,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let request = Request {
        wire: Wire::ChatCompletions,
        peer: peer.0,
    };
    handle_completion(state, request, uri, headers, body).await
}

async fn handle_messages(
    State(state): State<Arc<StreamingState>>,
    ConnectInfo(peer): ConnectInfo<PeerAddr>,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let request = Request {
        wire: Wire::Messages,
        peer: peer.0,
    };
    handle_completion(state, request, uri, headers, body).await
}

async fn handle_responses(
    State(state): State<Arc<StreamingState>>,
    ConnectInfo(peer): ConnectInfo<PeerAddr>,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let request = Request {
        wire: Wire::Responses,
        peer: peer.0,
    };
    handle_completion(state, request, uri, headers, body).await
}

#[derive(Clone, Copy)]
struct Request {
    wire: Wire,
    peer: SocketAddr,
}

async fn handle_completion(
    state: Arc<StreamingState>,
    request: Request,
    uri: Uri,
    headers: HeaderMap,
    raw: String,
) -> Response {
    let ordinal = state.record("POST", &uri, &headers, &raw);
    if !state.authorized(&headers) {
        return unauthorized();
    }
    let body =
        serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| json!({ "__unparsed__": raw }));
    state.record_completion_body(&body);
    let streaming = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let title = body_is_session_title(&body);
    if !title {
        if let Some(marker) = state.enter_hold(&raw) {
            match state.released(&marker).await {
                Some(Release::Complete) => {}
                Some(Release::Fail { status, message }) => {
                    return http_status(status, error_body(&message));
                }
                Some(Release::Close) | None => return wire::closed_stream(),
            }
        }
    }
    let response = match state.responder.as_ref().and_then(|respond| respond(&body)) {
        Some(response) => response,
        None if streaming => state.next_response(&raw, &body, ordinal).await,
        None => {
            let fallback = StreamScript::completes("__complete__", ["mock-title"]);
            state.unscripted_response(fallback, &body, ordinal).await
        }
    };
    let reply = wire::Reply {
        wire: request.wire,
        streaming,
        ordinal,
    };
    answer(state, request, reply, response).await
}

async fn answer(
    state: Arc<StreamingState>,
    request: Request,
    reply: wire::Reply,
    response: StreamResponse,
) -> Response {
    let mut response = response;
    loop {
        match response {
            StreamResponse::Stream(script) => return wire::stream(script, state, reply),
            StreamResponse::Held { gate, then } => match state.released(&gate).await {
                Some(Release::Complete) => response = *then,
                Some(Release::Fail { status, message }) => {
                    return http_status(status, error_body(&message));
                }
                Some(Release::Close) | None => return wire::closed_stream(),
            },
            StreamResponse::HttpStatus { status, body } => return http_status(status, body),
            StreamResponse::Disconnect => {
                state.connections.close(request.peer);
                return wire::closed_stream();
            }
        }
    }
}

async fn handle_fallback(
    State(state): State<Arc<StreamingState>>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    state.record("OTHER", &uri, &headers, "");
    json_response(StatusCode::NOT_FOUND, r#"{"error":"not found"}"#.to_owned())
}

#[cfg(test)]
mod tests;
