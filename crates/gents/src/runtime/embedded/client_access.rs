use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use gents_protocol::client_access::{ClientAccessRequest, DecideClientAccess};
use gents_store::{
    config_client::{client_authentication::ClientAuthorization, TransactionFailure},
    identity::client_access::{ClientAccessError, ClientAccessOwner},
};

use gents_store::local::home::ServedNode;

#[derive(Clone)]
struct ClientAccessState {
    served: ServedNode,
    decisions: std::sync::Arc<tokio::sync::Mutex<()>>,
}

pub(super) fn routes(served: ServedNode) -> axum::Router {
    axum::Router::new()
        .route(
            gents_protocol::client_access::REQUEST_PATH,
            axum::routing::post(request),
        )
        .route(
            gents_protocol::client_access::DECIDE_PATH,
            axum::routing::post(decide),
        )
        .with_state(ClientAccessState {
            served,
            decisions: Default::default(),
        })
}

fn owner(state: &ClientAccessState) -> Option<ClientAccessOwner> {
    let (node, runtime) = state.served.get()?;
    Some(ClientAccessOwner::new(
        node.upgrade()?,
        runtime.clone(),
        state.decisions.clone(),
    ))
}

fn respond(
    result: Result<gents_protocol::client_access::ClientAccessReceipt, ClientAccessError>,
) -> Response {
    match result {
        Ok(receipt) => Json(receipt).into_response(),
        Err(ClientAccessError::Refused) => StatusCode::FORBIDDEN.into_response(),
        Err(ClientAccessError::ExpiredUnretained) => StatusCode::GONE.into_response(),
        Err(error)
            if error
                .config_client_error()
                .is_some_and(|error| error.is_permission_denied()) =>
        {
            StatusCode::FORBIDDEN.into_response()
        }
        Err(error) => {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "client access operation failed");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn request(
    State(state): State<ClientAccessState>,
    actor: defra_http::ExtractIdentity,
    headers: HeaderMap,
    Json(request): Json<ClientAccessRequest>,
) -> Response {
    let Some(owner) = owner(&state) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(caller) = ClientAuthorization::from_http(&actor, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    respond(owner.request(caller, request).await)
}

async fn decide(
    State(state): State<ClientAccessState>,
    actor: defra_http::ExtractIdentity,
    headers: HeaderMap,
    Json(decision): Json<DecideClientAccess>,
) -> Response {
    let Some(owner) = owner(&state) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(caller) = ClientAuthorization::from_http(&actor, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    respond(owner.decide(caller, decision).await)
}
