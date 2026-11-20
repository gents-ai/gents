use super::router::RuntimeHttpState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use gents_protocol::goal_control::ApplyGoalControl;

pub async fn apply(
    State(state): State<RuntimeHttpState>,
    actor: defra_http::ExtractIdentity,
    headers: HeaderMap,
    Json(request): Json<ApplyGoalControl>,
) -> Response {
    let Some(runtime) = state.activation_runtime.get() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    gents_automation::goal::client_application::apply_http(
        runtime.observation_node(),
        &runtime.runtime_node().identity,
        &actor,
        &headers,
        request,
    )
    .await
}
