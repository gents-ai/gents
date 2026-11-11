use super::{access::failure, router::RuntimeHttpState};
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents_protocol::agent_identity::{AgentAdmissionRequest, ProvisionAgentIdentityRequest};

pub async fn provision(
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
    match gents_store::identity::provision_agent_identity_authorized(
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
        Err(gents_store::identity::AgentProvisionError::Unauthorized) => failure(
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

/// The runtime's observation of the native ACP grants the authenticated
/// caller holds for agent operations (`observe_agent_admission`). It answers
/// only for the caller and decides nothing: agent admission decides from these
/// facts and DefraDB authorizes every write.
pub async fn admission(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(request): Json<AgentAdmissionRequest>,
) -> Response {
    let Some(caller) = identity
        .did()
        .and_then(|did| identity::Did::new(did.as_ref()).ok())
    else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "Connect this client before configuring agents.",
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
    match gents_store::identity::observe_agent_admission(
        runtime.observation_node(),
        &state.node_did,
        &caller,
        &request.agent_dids,
    )
    .await
    {
        Ok(facts) => {
            let mut response = Json(facts).into_response();
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(error) => {
            tracing::warn!(%error, "agent admission observation failed");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "The runtime cannot observe agent grants.",
            )
        }
    }
}
