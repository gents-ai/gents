use super::{access::failure, router::RuntimeHttpState};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents_protocol::owed_settlement::{OwedSettlementReport, OwedSettlementRequest};
use gents_store::config_client::client_authentication::ClientAuthorization;

/// One settlement pass for the authenticated caller: the administration
/// withdrawals its home owes that it may issue
/// (`Identity.EnrollmentGrant.settlementStep`), then, on a Vera home, its own
/// owed entries under the delegation its request presented
/// (`Identity.OwedRegistration`).
pub async fn settle(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    headers: HeaderMap,
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
    let (Some(runtime), Some(owner)) = (
        state.activation_runtime.get(),
        super::client_access::owner(&state),
    ) else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The runtime is still starting.",
        );
    };
    let Ok(authorization) = ClientAuthorization::from_http(&identity, &headers) else {
        return failure(StatusCode::UNAUTHORIZED, "Connect this client first.");
    };
    let node = runtime.observation_node();
    let vera = gents_store::local::document_acp::node_delegation_audience(node).is_some();
    let owed = match gents_store::config_client::owed::settle_pass(
        node,
        &identity.context(),
        caller.as_str(),
    )
    .await
    {
        Ok(standings) => standings,
        Err(gents_store::config_client::owed::OwedPassError::DelegationRequired) if vera => {
            return failure(
                StatusCode::UNAUTHORIZED,
                gents_store::config_client::user_write::REFUSED_FOR_A_PERSON,
            )
        }
        Err(gents_store::config_client::owed::OwedPassError::DelegationRequired) => Vec::new(),
        Err(error) => {
            tracing::warn!(%error, "owed settlement pass failed");
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "The runtime cannot answer now.",
            );
        }
    };
    let mut standings = match owner.settle_withdrawals(authorization).await {
        Ok(standings) => standings,
        Err(error) => {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "owed withdrawal settlement failed");
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "The runtime cannot answer now.",
            );
        }
    };
    standings.extend(owed);
    let mut response = Json(OwedSettlementReport { standings }).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// The administration removals the caller's home owes that it may complete
/// (`Identity.EnrollmentGrant.settlementStep`).
pub async fn removals(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    headers: HeaderMap,
    Json(request): Json<OwedSettlementRequest>,
) -> Response {
    if request.node_did != state.node_did {
        return failure(
            StatusCode::BAD_REQUEST,
            "The requested node does not serve this endpoint.",
        );
    }
    let Some(owner) = super::client_access::owner(&state) else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The runtime is still starting.",
        );
    };
    let Ok(caller) = ClientAuthorization::from_http(&identity, &headers) else {
        return failure(StatusCode::UNAUTHORIZED, "Connect this client first.");
    };
    match owner.owed_removals(caller).await {
        Ok(removals) => {
            let mut response =
                Json(gents_protocol::owed_settlement::OwedRemovalsReport { removals })
                    .into_response();
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(error) => {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "owed removals could not be read");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "The runtime cannot answer now.",
            )
        }
    }
}

/// Wait, for the caller, until the access changes its own committed
/// transaction owes are settled or come to rest
/// (`Identity.OwedRegistration.dependentStep`).
pub async fn await_owed(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(request): Json<gents_protocol::owed_settlement::OwedAwaitRequest>,
) -> Response {
    if identity.did().is_none() {
        return failure(StatusCode::UNAUTHORIZED, "Connect this client first.");
    }
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
    match gents_store::config_client::owed::serve_await(
        runtime.observation_node(),
        &identity.context(),
        &request.transaction,
    )
    .await
    {
        Ok(wait) => {
            let mut response =
                Json(gents_protocol::owed_settlement::OwedAwaitReport { wait }).into_response();
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(gents_store::config_client::ConfigClientError::OwedAwaitTransaction) => failure(
            StatusCode::BAD_REQUEST,
            "The transaction to wait for is not one this node opened.",
        ),
        Err(error) => {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "owed access wait failed");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "The runtime cannot answer now.",
            )
        }
    }
}
