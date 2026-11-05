use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents_protocol::file_inventory::DirectFileInventoryRequest;

use super::router::RuntimeHttpState;

pub(crate) async fn operator_inventory_handler(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(request): Json<DirectFileInventoryRequest>,
) -> Response {
    let mut response = operator_inventory(state, identity, request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn operator_inventory(
    state: RuntimeHttpState,
    identity: ExtractIdentity,
    request: DirectFileInventoryRequest,
) -> Response {
    let Some(actor) = identity.did() else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "Runtime operator authorization is required.",
        );
    };
    let Some(runtime) = state.activation_runtime.get() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The runtime is still starting. Try again shortly.",
        );
    };
    if runtime.node_did() != state.node_did {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The runtime identity is unavailable.",
        );
    }
    if request.node_did != state.node_did {
        return failure(
            StatusCode::BAD_REQUEST,
            "The requested runtime is not served here.",
        );
    }
    match runtime.read_file_inventory(actor.as_ref(), &request).await {
        Ok(reply) => Json(reply).into_response(),
        Err(error) => {
            tracing::warn!(%error, "operator file inventory is unavailable");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "File changes could not be loaded. Try again shortly.",
            )
        }
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}
