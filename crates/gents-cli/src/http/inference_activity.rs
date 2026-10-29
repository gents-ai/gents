use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents::inference_observation::InferenceKey;

use super::router::RuntimeHttpState;

pub(crate) async fn read(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(key): Json<InferenceKey>,
) -> Response {
    let mut response = read_authorized(state, identity, key).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn read_authorized(
    state: RuntimeHttpState,
    identity: ExtractIdentity,
    key: InferenceKey,
) -> Response {
    let Some(actor) = identity.did() else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "Connect this client to the runtime.",
        );
    };
    let Some(runtime) = state
        .activation_runtime
        .get()
        .filter(|runtime| runtime.node_did() == state.node_did)
    else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The runtime is unavailable.",
        );
    };
    match runtime
        .inference_observation_authorized(actor.as_ref())
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return failure(
                StatusCode::FORBIDDEN,
                "This client is no longer authorized.",
            )
        }
        Err(_) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Client authorization is unavailable.",
            )
        }
    }
    match runtime
        .read_inference_observation(actor.clone(), &key)
        .await
    {
        Ok(Some(observation)) => Json(observation).into_response(),
        Ok(None) => failure(StatusCode::NOT_FOUND, "Inference activity is unavailable."),
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Inference activity is unavailable.",
        ),
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}
