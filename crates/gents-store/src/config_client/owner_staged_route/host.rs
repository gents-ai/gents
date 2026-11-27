use super::OwnerStagedRoute;
use crate::config_client::host_grant::{submit, HostGrant, HostGrantError};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use defra_http::ExtractIdentity;

/// An Agent owner's `host` grant under its request-bound delegation
/// (`crate::config_client::host_grant`).
pub(super) async fn grant_host(
    State(route): State<OwnerStagedRoute>,
    headers: HeaderMap,
    actor: ExtractIdentity,
    Json(grant): Json<HostGrant>,
) -> StatusCode {
    use crate::config_client::client_authentication::ClientAuthorization;
    let Ok(caller) = ClientAuthorization::from_http(&actor, &headers) else {
        return StatusCode::UNAUTHORIZED;
    };
    let Some((node, _)) = route.node() else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    match submit(&node, caller.actor(), &grant).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(HostGrantError::NotVera) => StatusCode::CONFLICT,
        Err(HostGrantError::Target) => StatusCode::BAD_REQUEST,
        Err(error) => {
            tracing::warn!(%error, "granting an Agent's host failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}
