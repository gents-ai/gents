use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents::store::config_client::ConfigClientError;
use gents_protocol::pairing_reader::{PairingReaderCommand, PairingReaderRefusal};

use super::router::RuntimeHttpState;

/// Lean `Identity.OwnerStaged.pairingOverHttp`: the command is admitted only
/// for the authenticated signer (`HttpAccess.decideCommand`), then decided by
/// the same `set_pairing_reader` owner as that signer's embedded request.
pub(crate) async fn pairing_reader_handler(
    State(state): State<RuntimeHttpState>,
    actor: ExtractIdentity,
    Json(command): Json<PairingReaderCommand>,
) -> Response {
    let mut response = decide(state, actor, command).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn decide(
    state: RuntimeHttpState,
    actor: ExtractIdentity,
    command: PairingReaderCommand,
) -> Response {
    let Some(service) = state.enrollment_decisions.read().await.clone() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "starting",
            "The runtime is still starting.",
        );
    };
    if actor.did().map(identity::Did::as_str) != Some(command.signer_did.as_str()) {
        return failure(
            StatusCode::FORBIDDEN,
            "signerMismatch",
            "The authenticated client did not sign this command.",
        );
    }
    if let Err(invalid) = command.validate_at(chrono::Utc::now()) {
        return failure(
            StatusCode::FORBIDDEN,
            "invalidCommand",
            &invalid.to_string(),
        );
    }
    if command.node_did != state.node_did {
        return failure(
            StatusCode::FORBIDDEN,
            "differentNode",
            "This endpoint serves a different node.",
        );
    }
    if let Err(refused) = service
        .authenticate_signer(
            &command.signer_did,
            &command.signing_payload(),
            &command.sig,
            &command.nonce,
        )
        .await
    {
        tracing::debug!(error = %format!("{refused:#}"), "pairing reader command refused");
        return failure(
            StatusCode::FORBIDDEN,
            "invalidSignature",
            "The command signature is invalid or was already used.",
        );
    }
    let (requester, reader) = match (
        identity::Did::new(&command.signer_did),
        identity::Did::new(&command.reader_did),
    ) {
        (Ok(requester), Ok(reader)) => (requester, reader),
        _ => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalidCommand",
                "The command names an invalid DID.",
            )
        }
    };
    match service
        .client_access(requester)
        .set_verified_client_pairing_reader(
            command.observation,
            &command.doc_id,
            command.reason,
            &reader,
            command.grant,
        )
        .await
    {
        Ok(()) => Json(serde_json::json!({
            "collection": command.observation.collection(),
            "doc_id": command.doc_id,
            "agent_did": command.reader_did,
            "reason": command.reason.as_str(),
            "granted": command.grant,
        }))
        .into_response(),
        // The refusal text is fixed so the response reveals neither whether
        // the observation exists nor who owns it.
        Err(ConfigClientError::OwnerStagedRelationshipDenied { .. }) => refusal(
            StatusCode::FORBIDDEN,
            PairingReaderRefusal::Denied,
            "The requesting client may not change this pairing observation's readers.",
        ),
        Err(ConfigClientError::OwnerStagedRelationshipUnmanaged { .. }) => refusal(
            StatusCode::CONFLICT,
            PairingReaderRefusal::Unmanaged,
            "This pairing observation is not owned by this runtime.",
        ),
        Err(error) => {
            tracing::warn!(%error, "pairing reader change failed");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "The pairing reader change could not be applied.",
            )
        }
    }
}

fn refusal(status: StatusCode, code: PairingReaderRefusal, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "code": code, "error": message })),
    )
        .into_response()
}

fn failure(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "code": code, "error": message })),
    )
        .into_response()
}
