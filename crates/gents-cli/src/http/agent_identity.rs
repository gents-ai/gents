use super::{access::failure, router::RuntimeHttpState};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents_protocol::agent_identity::ProvisionAgentIdentityRequest;

pub(crate) async fn provision(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(request): Json<ProvisionAgentIdentityRequest>,
) -> Response {
    let Some(caller) = identity.did() else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "Connect this client before creating an agent.",
        );
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
    if runtime.node_did() != state.node_did {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The runtime identity is unavailable.",
        );
    }
    let caller = match identity::Did::new(caller.as_ref()) {
        Ok(caller) => caller,
        Err(_) => {
            return failure(
                StatusCode::UNAUTHORIZED,
                "Connect this client before creating an agent.",
            )
        }
    };
    match gents::store::identity::provision_agent_identity_authorized(
        runtime.observation_node(),
        runtime.runtime_node().identity.as_ref(),
        &caller,
    )
    .await
    {
        Ok(binding) => {
            let mut response = Json(binding).into_response();
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(gents::store::identity::AgentProvisionError::Unauthorized) => failure(
            StatusCode::FORBIDDEN,
            "This client cannot create an agent on this node.",
        ),
        Err(error) => {
            tracing::warn!(%error, "agent identity provisioning failed");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "The runtime cannot provision an agent identity.",
            )
        }
    }
}
