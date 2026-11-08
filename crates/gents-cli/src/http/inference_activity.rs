use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents::observe::inference_observation::{InferenceKey, InferenceObservationRead};

use super::router::RuntimeHttpState;

pub(crate) const BACKEND_DISCOVERY_PATH: &str = "/inference/backend/discover";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackendDiscoveryRequest {
    pub(crate) node_did: String,
    pub(crate) backend_id: String,
}

pub(crate) async fn discover_backend(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(request): Json<BackendDiscoveryRequest>,
) -> Response {
    let Some(actor) = identity.did() else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "Connect this client to the runtime.",
        );
    };
    let Some(runtime) = state.activation_runtime.get().filter(|runtime| {
        runtime.node_did() == state.node_did && runtime.node_did() == request.node_did
    }) else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The selected runtime is unavailable.",
        );
    };
    match gents::inference::backends::registry::discover_backend_for_actor(
        runtime.observation_node(),
        actor,
        &request.node_did,
        &request.backend_id,
    )
    .await
    {
        Ok(models) => {
            let mut response = Json(serde_json::json!({"backend_id":request.backend_id, "discovered_models":models, "catalog_written":true})).into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(gents::inference::backends::registry::ActorBackendDiscoveryError::Unavailable) => {
            failure(
                StatusCode::NOT_FOUND,
                "The selected backend is unavailable.",
            )
        }
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Backend discovery is unavailable.",
        ),
    }
}

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
        .read_inference_observation(actor.clone(), &key)
        .await
    {
        Ok(InferenceObservationRead::Visible(observation)) => Json(observation).into_response(),
        Ok(InferenceObservationRead::Unavailable) => {
            failure(StatusCode::NOT_FOUND, "Inference activity is unavailable.")
        }
        Ok(InferenceObservationRead::Forbidden) => failure(
            StatusCode::FORBIDDEN,
            "This client is not authorized to read this runtime.",
        ),
        Err(_) => failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Inference activity is unavailable.",
        ),
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    let mut response = (status, Json(serde_json::json!({"error": message}))).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
