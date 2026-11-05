use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures_util::{stream, StreamExt};
use tokio::sync::Semaphore;

use super::router::RuntimeHttpState;

static WATCH_SLOTS: Semaphore = Semaphore::const_new(16);
const WATCH_LIFETIME: Duration = Duration::from_secs(30);

pub(super) async fn watch(
    State(state): State<RuntimeHttpState>,
    Json(request): Json<gents::observe::ux_data::UxDataCall>,
) -> Response {
    let Ok(permit) = WATCH_SLOTS.try_acquire() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Too many pack observations.",
        )
            .into_response();
    };
    let Some(runtime) = state.activation_runtime.get() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Runtime is not ready.").into_response();
    };
    let Some(home) = state.home.as_deref() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "Pack home is unavailable.").into_response();
    };
    let watcher = match tokio::time::timeout(
        Duration::from_secs(10),
        runtime.watch_ux_data(std::path::Path::new(home), request),
    )
    .await
    {
        Ok(Ok(watcher)) => watcher,
        Ok(Err(_)) => {
            return (StatusCode::BAD_REQUEST, "Pack query is unavailable.").into_response()
        }
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "Pack query timed out.").into_response()
        }
    };
    let initial = watcher.initial().to_string();
    let updates = stream::unfold(
        (watcher, permit, false),
        |(mut watcher, permit, finished)| async move {
            if finished {
                return None;
            }
            let event = match watcher.next().await {
                Ok(value) => (
                    Event::default().event("next").data(value.to_string()),
                    false,
                ),
                Err(_) => (
                    Event::default()
                        .event("error")
                        .data("Pack query is unavailable; reconnect."),
                    true,
                ),
            };
            Some((Ok::<_, Infallible>(event.0), (watcher, permit, event.1)))
        },
    );
    let stream =
        stream::once(
            async move { Ok::<_, Infallible>(Event::default().event("next").data(initial)) },
        )
        .chain(updates)
        .take_until(tokio::time::sleep(WATCH_LIFETIME));
    Sse::new(stream).into_response()
}
