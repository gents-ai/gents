use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents_protocol::process_control::{OperatorProcessControlRequest, ProcessControlOutcome};

use super::router::RuntimeHttpState;

pub(crate) async fn process_control_handler(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(request): Json<OperatorProcessControlRequest>,
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
    request: OperatorProcessControlRequest,
) -> Response {
    let Some(actor) = identity.did() else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "Runtime operator authorization is required.",
        );
    };
    if !gents::tools::background::job::control::operator_control_authorized(
        &state.node_did,
        &request.node_did,
        &state.node_did,
        Some(actor.as_ref()),
    ) {
        return failure(
            StatusCode::FORBIDDEN,
            "This identity cannot control that runtime process.",
        );
    }
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
    match runtime
        .operator_process_control(Some(actor.as_ref()), &request)
        .await
    {
        Ok(outcome) => {
            let status = match outcome {
                ProcessControlOutcome::NotFound => StatusCode::NOT_FOUND,
                ProcessControlOutcome::NotBackground
                | ProcessControlOutcome::Lost
                | ProcessControlOutcome::Unverified => StatusCode::CONFLICT,
                _ => StatusCode::OK,
            };
            (status, Json(outcome)).into_response()
        }
        Err(error) => {
            tracing::warn!(error = %error, "native process control failed");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "The process could not be controlled. Refresh its state and try again.",
            )
        }
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}
