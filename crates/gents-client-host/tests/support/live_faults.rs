use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use gents::support::live_inference::InferenceTarget;
use serde_json::Value;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

struct RelayState {
    endpoint: String,
    client: reqwest::Client,
    matching_input: String,
    status: StatusCode,
    failure: Value,
    armed: AtomicBool,
    injected: watch::Sender<bool>,
}

pub struct FaultRelay {
    endpoint: String,
    state: Arc<RelayState>,
    shutdown: CancellationToken,
}

impl FaultRelay {
    pub async fn start(
        target: &InferenceTarget,
        matching_input: String,
        status: StatusCode,
        failure: Value,
    ) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let endpoint = format!("http://{}/v1", listener.local_addr()?);
        let state = Arc::new(RelayState {
            endpoint: target.endpoint().trim_end_matches('/').to_owned(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
            matching_input,
            status,
            failure,
            armed: AtomicBool::new(true),
            injected: watch::channel(false).0,
        });
        let router = Router::new()
            .route("/v1/models", any(forward))
            .route("/v1/chat/completions", any(forward))
            .route("/v1/messages", any(forward))
            .with_state(state.clone());
        let shutdown = CancellationToken::new();
        let cancelled = shutdown.clone();
        tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router)
                .with_graceful_shutdown(cancelled.cancelled_owned())
                .await
            {
                tracing::error!(%error, "live fault relay failed");
            }
        });
        Ok(Self {
            endpoint,
            state,
            shutdown,
        })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn arm(&self) {
        self.state.injected.send_replace(false);
        self.state.armed.store(true, Ordering::Release);
    }

    pub async fn wait_injected(&self, deadline: Duration) -> Result<()> {
        let mut injected = self.state.injected.subscribe();
        tokio::time::timeout(deadline, injected.wait_for(|injected| *injected))
            .await
            .context("live provider fault was not reached before the deadline")?
            .context("live provider fault notification closed")?;
        Ok(())
    }
}

impl Drop for FaultRelay {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn forward(
    State(state): State<Arc<RelayState>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let matches = serde_json::from_slice::<Value>(&body)
        .ok()
        .is_some_and(|request| {
            request["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["role"] == "user"
                        && match &message["content"] {
                            Value::String(text) => text.contains(&state.matching_input),
                            Value::Array(parts) => parts.iter().any(|part| {
                                part["text"]
                                    .as_str()
                                    .is_some_and(|text| text.contains(&state.matching_input))
                            }),
                            _ => false,
                        }
                })
            })
        });
    if matches && state.armed.swap(false, Ordering::AcqRel) {
        state.injected.send_replace(true);
        return (state.status, axum::Json(state.failure.clone())).into_response();
    }
    let path = uri.path().strip_prefix("/v1").unwrap_or(uri.path());
    let endpoint = format!("{}{path}", state.endpoint);
    let upstream = match state
        .client
        .request(method, endpoint)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("live target request failed: {error}"),
            )
                .into_response()
        }
    };
    let status = upstream.status();
    let content_type = upstream.headers().get("content-type").cloned();
    match upstream.bytes().await {
        Ok(bytes) => {
            let mut response = (status, bytes).into_response();
            if let Some(content_type) = content_type {
                response.headers_mut().insert("content-type", content_type);
            }
            response
        }
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("live target response failed: {error}"),
        )
            .into_response(),
    }
}
