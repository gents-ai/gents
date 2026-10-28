use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use gents::pack::plugin_references::{check_installed, Check};

use super::router::RuntimeHttpState;

pub(crate) async fn check_handler(
    State(state): State<RuntimeHttpState>,
    Json(request): Json<Check>,
) -> Response {
    if request
        .plugins
        .iter()
        .any(|coordinate| gents::plugin::store::parse_coordinate(coordinate).is_err())
    {
        return (StatusCode::BAD_REQUEST, "Invalid plugin coordinate").into_response();
    }
    let Some(home) = state.home else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Runtime plugin store is unavailable",
        )
            .into_response();
    };
    match tokio::task::spawn_blocking(move || {
        check_installed(std::path::Path::new(&home), &request)
    })
    .await
    {
        Ok(Ok(result)) => Json(result).into_response(),
        error => {
            tracing::warn!(?error, "checking installed plugin references");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unable to inspect installed plugins; see runtime logs",
            )
                .into_response()
        }
    }
}
