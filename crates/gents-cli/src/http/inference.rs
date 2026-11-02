use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents::store::config_client::ConfigAccess;
use gents_desktop_core::inference::{
    load_inference_setup_state, InferenceSettingsResponse, INFERENCE_SETTINGS_CONTRACT_VERSION,
};

use super::router::RuntimeHttpState;

pub(crate) async fn inference_settings_handler(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
) -> Response {
    let mut response = inference_settings(state, identity).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn inference_settings(state: RuntimeHttpState, identity: ExtractIdentity) -> Response {
    let Some(member) = identity.did() else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "Connect this client to the runtime to view model settings.",
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
            "The runtime identity is unavailable. Reconnect the runtime.",
        );
    }
    match runtime.client_observation_authorized(member.as_ref()).await {
        Ok(true) => {}
        Ok(false) => {
            return failure(
                StatusCode::FORBIDDEN,
                "This client is no longer authorized. Reconnect it to the runtime.",
            )
        }
        Err(error) => {
            tracing::warn!(%error, "checking client model-settings authority failed");
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Client authorization is unavailable. Try again shortly.",
            );
        }
    }
    match load_inference_setup_state(&ConfigAccess::Graphql(state.p2p_graphql), &state.node_did)
        .await
    {
        Ok(settings) => Json(InferenceSettingsResponse {
            contract_version: INFERENCE_SETTINGS_CONTRACT_VERSION,
            state: settings.for_client_observation(),
        })
        .into_response(),
        Err(error) => {
            tracing::warn!(%error, "reading client model settings failed");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Model settings are unavailable. Check the runtime configuration and try again.",
            )
        }
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}
