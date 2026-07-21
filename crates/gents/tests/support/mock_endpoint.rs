//! Robust shared mock OpenAI-compatible backend for full-daemon tests.
//!
//! Built on `axum` rather than a hand-rolled `TcpListener` accept loop so it
//! cannot flake the way the previous nonblocking-poll servers did (a transient
//! `accept()` error there killed the listener via `Err(_) => break`, causing
//! "error sending request" failures under CI load). One robust implementation
//! replaces the several near-identical hand-rolled copies that used to live in
//! this support module and inline in individual test files.
//!
//! Serves the routes the startup probe and oneshot/inference paths touch:
//! `GET /v1/models` (+ `/models`), `GET /v1/key` (+ `/key`), and
//! `POST /v1/chat/completions` (+ `/chat/completions`) with a fixed
//! non-streaming completion. An optional required bearer token gates every
//! route (401 on mismatch), and every request is recorded for inspection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use tokio::sync::oneshot;

use super::http_mock::HttpRequestData;

#[derive(Clone)]
struct MockState {
    model_name: String,
    required_bearer: Option<String>,
    recorded: Arc<Mutex<Vec<HttpRequestData>>>,
}

pub struct MockModelEndpoint {
    endpoint: String,
    recorded: Arc<Mutex<Vec<HttpRequestData>>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl MockModelEndpoint {
    /// Start a mock backend with no auth requirement (the common case: the
    /// startup probe just needs `/models` to answer so the backend promotes to
    /// healthy).
    pub fn start(model_name: &str) -> anyhow::Result<Self> {
        Self::start_with_required_bearer(model_name, None)
    }

    /// Start a mock backend that requires `Authorization: Bearer <token>` on
    /// every route when `required_bearer` is `Some`, returning 401 otherwise.
    pub fn start_with_required_bearer(
        model_name: &str,
        required_bearer: Option<&str>,
    ) -> anyhow::Result<Self> {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let state = MockState {
            model_name: model_name.to_string(),
            required_bearer: required_bearer.map(ToOwned::to_owned),
            recorded: recorded.clone(),
        };
        let app = Router::new()
            .route("/v1/models", get(handle_models))
            .route("/models", get(handle_models))
            .route("/v1/key", get(handle_key))
            .route("/key", get(handle_key))
            .route("/v1/chat/completions", post(handle_chat))
            .route("/chat/completions", post(handle_chat))
            .fallback(handle_fallback)
            .with_state(state);

        // Bind synchronously so the port is reserved before we return, then
        // serve on the ambient tokio runtime (every consumer is a
        // `#[tokio::test]`). `from_std` requires the listener be nonblocking.
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
            recorded,
            shutdown: Some(shutdown_tx),
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn recorded_requests(&self) -> Vec<HttpRequestData> {
        self.recorded
            .lock()
            .expect("mock recorded-requests mutex poisoned")
            .clone()
    }
}

impl Drop for MockModelEndpoint {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn headers_to_map(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn record(state: &MockState, method: &str, path: &str, headers: &HeaderMap, body: String) {
    state
        .recorded
        .lock()
        .expect("mock recorded-requests mutex poisoned")
        .push(HttpRequestData {
            method: method.to_string(),
            path: path.to_string(),
            headers: headers_to_map(headers),
            body,
        });
}

fn bearer_authorized(state: &MockState, headers: &HeaderMap) -> bool {
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

fn unauthorized() -> Response {
    json_response(
        StatusCode::UNAUTHORIZED,
        r#"{"error":"unauthorized"}"#.to_string(),
    )
}

async fn handle_models(State(state): State<MockState>, uri: Uri, headers: HeaderMap) -> Response {
    record(&state, "GET", uri.path(), &headers, String::new());
    if !bearer_authorized(&state, &headers) {
        return unauthorized();
    }
    json_response(
        StatusCode::OK,
        format!(r#"{{"data":[{{"id":"{}"}}]}}"#, state.model_name),
    )
}

async fn handle_key(State(state): State<MockState>, uri: Uri, headers: HeaderMap) -> Response {
    record(&state, "GET", uri.path(), &headers, String::new());
    if !bearer_authorized(&state, &headers) {
        return unauthorized();
    }
    json_response(
        StatusCode::OK,
        r#"{"data":{"label":"test-key"}}"#.to_string(),
    )
}

async fn handle_chat(
    State(state): State<MockState>,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    // The owned loop (#400) streams (`"stream":true`); other callers send a
    // non-streaming request. Mirror the real OpenAI API: reply with an SSE chunk
    // stream when streaming is requested, otherwise the JSON completion below.
    let streaming = body.contains("\"stream\":true") || body.contains("\"stream\": true");
    record(&state, "POST", uri.path(), &headers, body);
    if !bearer_authorized(&state, &headers) {
        return unauthorized();
    }
    if streaming {
        let delta = format!(
            r#"{{"id":"chatcmpl-test","provider":"Mock","object":"chat.completion.chunk","model":"{}","choices":[{{"index":0,"delta":{{"role":"assistant","content":"mock response"}},"finish_reason":null}}]}}"#,
            state.model_name
        );
        let finish = format!(
            r#"{{"id":"chatcmpl-test","object":"chat.completion.chunk","model":"{}","choices":[{{"index":0,"delta":{{}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}}}"#,
            state.model_name
        );
        let sse = format!("data: {delta}\n\ndata: {finish}\n\ndata: [DONE]\n\n");
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            sse,
        )
            .into_response();
    }
    let completion = json!({
        "id": "chatcmpl-test",
        "provider": "Mock",
        "object": "chat.completion",
        "created": 1_710_000_000_u64,
        "model": state.model_name,
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "mock response",
                "refusal": null,
                "reasoning": null
            }
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 2,
            "total_tokens": 12
        }
    });
    json_response(StatusCode::OK, completion.to_string())
}

async fn handle_fallback(State(state): State<MockState>, uri: Uri, headers: HeaderMap) -> Response {
    record(&state, "OTHER", uri.path(), &headers, String::new());
    json_response(
        StatusCode::NOT_FOUND,
        r#"{"error":"not found"}"#.to_string(),
    )
}
