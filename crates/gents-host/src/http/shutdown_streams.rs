use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Router,
};
use futures_util::StreamExt;
use tokio::sync::watch;

/// Ends the runtime's server-sent event responses once serve shutdown begins.
///
/// The pinned DefraDB ends its GraphQL subscription and event streams when
/// `Node::shutdown` starts closing the node's request scope (defradb.rs#2793),
/// but serve shutdown begins earlier: the runtime task is joined first. The
/// runtime's own event streams (the pack UX watch) belong to no DefraDB
/// request scope, so DefraDB's drain would wait its network drain timeout for
/// them. Ending every stream on the serve shutdown signal lets them finish
/// before the runtime stops; their subscribers (a remote observer arms one
/// per observed collection) resubscribe and resnapshot as after any stream
/// end, and a stream refused during shutdown fails as unavailable.
#[derive(Clone)]
pub struct ShutdownStreams {
    shutdown: watch::Receiver<bool>,
}

impl ShutdownStreams {
    /// `shutdown` turns `true` (or its sender drops) when serve shutdown
    /// begins.
    pub fn new(shutdown: watch::Receiver<bool>) -> Self {
        Self { shutdown }
    }

    pub fn wrap(&self, router: Router) -> Router {
        router.layer(axum::middleware::from_fn_with_state(
            self.clone(),
            bound_event_streams,
        ))
    }

    fn begun(&self) -> bool {
        *self.shutdown.borrow() || self.shutdown.has_changed().is_err()
    }

    fn begins(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut shutdown = self.shutdown.clone();
        async move {
            // A dropped sender means serve is gone: the stream ends either way.
            let _ = shutdown.wait_for(|begun| *begun).await;
        }
    }
}

async fn bound_event_streams(
    State(streams): State<ShutdownStreams>,
    request: Request,
    next: Next,
) -> Response {
    if streams.begun() && names_event_stream(request.headers(), header::ACCEPT) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the runtime is shutting down",
        )
            .into_response();
    }
    let response = next.run(request).await;
    if !names_event_stream(response.headers(), header::CONTENT_TYPE) {
        return response;
    }
    let (parts, body) = response.into_parts();
    let body = Body::from_stream(body.into_data_stream().take_until(streams.begins()));
    Response::from_parts(parts, body)
}

fn names_event_stream(headers: &HeaderMap, name: header::HeaderName) -> bool {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.to_ascii_lowercase().contains("text/event-stream"))
}

#[cfg(test)]
#[path = "shutdown_streams_tests.rs"]
mod tests;
