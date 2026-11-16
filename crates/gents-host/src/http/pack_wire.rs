use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use gents_pack::pack::plugin_wire::{wire_in_spawned, WireCommand};

use super::router::RuntimeHttpState;

pub const MAX_BODY: usize = 64 * 1024;

/// The operator route a client's [`gents_pack::pack::plugin_wire::wire_remote`]
/// reaches (Lean `Configuration.operatorWireAvailable`): it writes through
/// the runtime's own embedded store, only while the runtime and its plugin
/// home are available.
pub async fn wire_handler(
    State(state): State<RuntimeHttpState>,
    Json(command): Json<WireCommand>,
) -> Response {
    if command.node_did != state.node_did {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "endpoint serves a different node"})),
        )
            .into_response();
    }
    let Some(runtime) = state.activation_runtime.get() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "runtime is not active"})),
        )
            .into_response();
    };
    let Some(home) = state.home.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "runtime plugin store unavailable"})),
        )
            .into_response();
    };
    let home = std::path::PathBuf::from(home);
    // A pin is published under the held User's approval, and the local
    // transaction must select that same User as its actor.
    let user = match gents_service::home_state::load_held_user_identity(&home).await {
        Ok(user) => user,
        Err(error) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": format!("the runtime's User identity is unavailable: {error:#}")})),
            )
                .into_response()
        }
    };
    let access = match identity::Did::new(user.did()) {
        Ok(actor) => gents_store::config_client::ConfigAccess::local_as(
            runtime.observation_node().clone(),
            actor,
        ),
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{error:#}")})),
            )
                .into_response()
        }
    };
    let owner = state.node_did;
    let request = command.request;
    // Dropping the HTTP client must not release the store lock before the embedded commit completes.
    match tokio::spawn(async move {
        wire_in_spawned(
            &access,
            &owner,
            &home,
            &request,
            gents_service::home_state::load_held_user_identity,
        )
        .await
    })
    .await
    {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => (StatusCode::UNPROCESSABLE_ENTITY, Json(serde_json::json!({"error": format!("{error:#}"), "outcome": "unconfirmed"}))).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": format!("pack wiring outcome unconfirmed: {error}"), "outcome": "unconfirmed"}))).into_response(),
    }
}
