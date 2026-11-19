use super::{access::failure, router::RuntimeHttpState};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents_protocol::owed_settlement::{OwedSettlementReport, OwedSettlementRequest};

/// One settlement pass for the authenticated caller's own entries, under the
/// delegation its request presented (`Identity.OwedRegistration`).
pub async fn settle(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(request): Json<OwedSettlementRequest>,
) -> Response {
    let Some(caller) = identity.did().cloned() else {
        return failure(StatusCode::UNAUTHORIZED, "Connect this client first.");
    };
    if request.node_did != state.node_did {
        return failure(
            StatusCode::BAD_REQUEST,
            "The requested node does not serve this endpoint.",
        );
    }
    let Some(runtime) = state.activation_runtime.get() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The runtime is still starting.",
        );
    };
    match gents_store::config_client::owed::settle_pass(
        runtime.observation_node(),
        &identity.context(),
        caller.as_str(),
    )
    .await
    {
        Ok(standings) => {
            let mut response = Json(OwedSettlementReport { standings }).into_response();
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(gents_store::config_client::owed::OwedPassError::DelegationRequired) => failure(
            StatusCode::UNAUTHORIZED,
            gents_store::config_client::user_write::REFUSED_FOR_A_PERSON,
        ),
        Err(error) => {
            tracing::warn!(%error, "owed settlement pass failed");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "The runtime cannot answer now.",
            )
        }
    }
}
