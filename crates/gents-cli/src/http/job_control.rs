use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents_protocol::job::{JobReadError, OperatorJobControlRequest};

use super::router::RuntimeHttpState;

pub(crate) async fn job_control_handler(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(request): Json<OperatorJobControlRequest>,
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
    request: OperatorJobControlRequest,
) -> Response {
    let Some(actor) = identity.did() else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "Runtime operator authorization is required.",
        );
    };
    if !gents::plugin_job::control::operator_control_authorized(
        &state.node_did,
        &request.node_did,
        &state.node_did,
        Some(actor.as_ref()),
    ) {
        return failure(
            StatusCode::FORBIDDEN,
            "This endpoint does not authorize that job control.",
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
        .operator_job_control(Some(actor.as_ref()), &request)
        .await
    {
        Ok(outcome) => Json(outcome).into_response(),
        Err(error) => {
            let status = match error {
                JobReadError::NotAuthorized => StatusCode::FORBIDDEN,
                JobReadError::NotAJob | JobReadError::NotYetObservable => StatusCode::NOT_FOUND,
                _ => StatusCode::SERVICE_UNAVAILABLE,
            };
            failure(status, &error.to_string())
        }
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}
