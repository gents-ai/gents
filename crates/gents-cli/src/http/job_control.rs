use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents_protocol::process_control::ToolControlIntent;

use super::router::RuntimeHttpState;

pub(crate) async fn job_control_handler(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(request): Json<ToolControlIntent>,
) -> Response {
    let mut response = control(state, identity, request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn control(
    state: RuntimeHttpState,
    identity: ExtractIdentity,
    request: ToolControlIntent,
) -> Response {
    let Some(actor) = identity.did() else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "Runtime operator authorization is required.",
        );
    };
    let Some(runtime) = state
        .activation_runtime
        .get()
        .filter(|runtime| runtime.node_did() == state.node_did)
    else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The runtime is still starting. Try again shortly.",
        );
    };
    match runtime.operator_job_control(actor.as_ref(), &request).await {
        Ok(outcome) => (StatusCode::ACCEPTED, Json(outcome)).into_response(),
        Err(error) => {
            let status = match &error {
                gents::tools::call::control::client::ClientControlError::WrongNode
                | gents::tools::call::control::client::ClientControlError::NotAuthorized => {
                    StatusCode::FORBIDDEN
                }
                gents::tools::call::control::client::ClientControlError::NotFound => {
                    StatusCode::NOT_FOUND
                }
                gents::tools::call::control::client::ClientControlError::Unavailable(_)
                | gents::tools::call::control::client::ClientControlError::Unconfirmed { .. } => {
                    StatusCode::SERVICE_UNAVAILABLE
                }
            };
            failure(status, &error.to_string())
        }
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}
