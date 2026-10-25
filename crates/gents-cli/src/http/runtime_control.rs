use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents_protocol::serve_lifecycle::{
    control::{
        current_capabilities, RuntimeControlCommand, RuntimeControlDisposition,
        RuntimeControlReceipt, RuntimeOperation, RUNTIME_CONTROL_VERSION,
    },
    replacement::RetirementAuthority,
    ServeLifecycle,
};

use super::router::RuntimeHttpState;

pub(crate) async fn runtime_control_handler(
    State(state): State<RuntimeHttpState>,
    identity: ExtractIdentity,
    Json(command): Json<RuntimeControlCommand>,
) -> Response {
    let mut response = control(state, identity, command).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn control(
    state: RuntimeHttpState,
    identity: ExtractIdentity,
    command: RuntimeControlCommand,
) -> Response {
    use RuntimeControlDisposition as Disposition;
    if let Err(error) = command.validate_at(chrono::Utc::now()) {
        return failure(
            StatusCode::BAD_REQUEST,
            "invalidCommand",
            &error.to_string(),
        );
    }
    let command_digest = match command.digest() {
        Ok(digest) => digest,
        Err(error) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalidCommand",
                &error.to_string(),
            )
        }
    };
    if command.node_did != state.node_did {
        return failure(
            StatusCode::FORBIDDEN,
            "differentNode",
            "This endpoint serves a different node.",
        );
    }
    if matches!(command.operation, RuntimeOperation::Retire { .. })
        && !identity
            .did()
            .is_some_and(|actor| actor.as_ref() == state.node_did)
    {
        return failure(
            StatusCode::FORBIDDEN,
            "operatorRequired",
            "Runtime operator authorization is required.",
        );
    }
    let Some(runtime) = state.activation_runtime.get() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "starting",
            "The runtime is still starting.",
        );
    };
    if runtime.node_did() != state.node_did {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "The runtime identity is unavailable.",
        );
    }
    let signer = runtime.runtime_node().identity.clone();
    let build = match gents::runtime_service::executable_build_id().await {
        Ok(build) => build,
        Err(error) => {
            tracing::warn!(%error, "runtime build observation unavailable");
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "The runtime build could not be observed.",
            );
        }
    };
    let lifecycle = state.serve_lifecycle.current();
    let admission = runtime.service_admission();
    let mut retiring = match admission.is_retiring() {
        Ok(retiring) => retiring,
        Err(error) => {
            tracing::warn!(%error, "runtime retirement observation unavailable");
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Runtime control is unavailable.",
            );
        }
    };
    let disposition = match &command.operation {
        RuntimeOperation::Retire { expected_build } if expected_build != &build => {
            Disposition::BuildChanged
        }
        _ if retiring => Disposition::Stopping,
        RuntimeOperation::Observe if lifecycle != ServeLifecycle::Ready => Disposition::Starting,
        RuntimeOperation::Observe => Disposition::Observed,
        RuntimeOperation::Retire { .. } => {
            match admission.retire(RetirementAuthority {
                operator_authenticated: true,
                observed_build_matches: true,
            }) {
                Ok(()) => {
                    retiring = true;
                    Disposition::Retiring
                }
                Err(gents::runtime_service::RetirementError::Busy) => Disposition::Busy,
                Err(gents::runtime_service::RetirementError::Stopping) => {
                    retiring = true;
                    Disposition::Stopping
                }
                Err(error) => {
                    tracing::warn!(%error, "runtime retirement refused");
                    return failure(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "Runtime control is unavailable.",
                    );
                }
            }
        }
    };
    let mut receipt = RuntimeControlReceipt {
        version: RUNTIME_CONTROL_VERSION,
        node_did: state.node_did,
        command_nonce: command.nonce,
        command_digest,
        build,
        capabilities: current_capabilities(),
        replicated_schema: state.replicated_schema.get().cloned(),
        lifecycle,
        retiring,
        disposition,
        observed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        signature: Vec::new(),
    };
    if let Err(error) =
        gents::runtime_service::sign_control_receipt(signer.as_ref(), &mut receipt).await
    {
        tracing::warn!(%error, "runtime control receipt unavailable");
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "outcomeUnknown",
            "The runtime control outcome could not be confirmed. Inspect the runtime again.",
        );
    }
    Json(receipt).into_response()
}

fn failure(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "code": code, "error": message })),
    )
        .into_response()
}
