use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use defra_http::ExtractIdentity;
use gents_protocol::owner_staged::{OwnerStagedCommand, OwnerStagedRefusal};

use super::OwnerStagedRoute;
use crate::store::config_client::{ConfigAccess, ConfigClientError};

/// Lean `Identity.OwnerStaged.overHttp`: the command is admitted only for the
/// authenticated signer at the endpoint it addressed
/// (`HttpAccess.decideCommand`), then decided by the same owner-staged owner
/// as that signer's embedded request.
pub(super) async fn owner_staged_handler(
    State(route): State<OwnerStagedRoute>,
    headers: HeaderMap,
    actor: ExtractIdentity,
    Json(command): Json<OwnerStagedCommand>,
) -> Response {
    let mut response = decide(route, &headers, actor, command).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn decide(
    route: OwnerStagedRoute,
    headers: &HeaderMap,
    actor: ExtractIdentity,
    command: OwnerStagedCommand,
) -> Response {
    let Some(node) = route.node() else {
        return failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "starting",
            "The runtime is still starting.",
        );
    };
    if headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        != Some(command.audience.as_str())
    {
        return failure(
            StatusCode::MISDIRECTED_REQUEST,
            "misdirected",
            "This command addresses a different endpoint.",
        );
    }
    if actor.did().map(identity::Did::as_str) != Some(command.signer_did.as_str()) {
        return failure(
            StatusCode::FORBIDDEN,
            "signerMismatch",
            "The authenticated client did not sign this command.",
        );
    }
    let now = chrono::Utc::now();
    if let Err(invalid) = command.validate_at(now) {
        return failure(
            StatusCode::FORBIDDEN,
            "invalidCommand",
            &invalid.to_string(),
        );
    }
    let Ok(requester) = identity::Did::new(&command.signer_did) else {
        return failure(
            StatusCode::FORBIDDEN,
            "invalidCommand",
            "The command names an invalid signer.",
        );
    };
    let verified = crate::store::identity::verify_did_signature(
        requester.as_str(),
        &command.signing_payload(),
        &command.sig,
    );
    if !matches!(verified, Ok(true)) {
        return failure(
            StatusCode::FORBIDDEN,
            "invalidSignature",
            "The command signature is invalid or was already used.",
        );
    }
    let access = ConfigAccess::local_as(node.clone(), requester.clone());
    // Authority before the nonce: an unauthorized signer is refused with the
    // decision's own fixed denial and makes the runtime write nothing.
    match access
        .verified_client_may_request_owner_staged(&command.change)
        .await
    {
        Ok(true) => {}
        Ok(false) => return denied(),
        Err(error) => {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "owner-staged authority check failed");
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "The relationship change could not be applied.",
            );
        }
    }
    if let Err(refused) =
        super::consume_signed_command_nonce(&node, requester.as_str(), &command.nonce, now).await
    {
        tracing::debug!(error = %refused, "owner-staged command nonce refused");
        return failure(
            StatusCode::FORBIDDEN,
            "invalidSignature",
            "The command signature is invalid or was already used.",
        );
    }
    match access
        .request_verified_client_owner_staged(&command.change, command.grant)
        .await
    {
        Ok(()) => Json(serde_json::json!({ "change": command.change, "grant": command.grant }))
            .into_response(),
        // The refusal text is fixed so the response reveals neither whether
        // the document exists nor who owns it.
        Err(ConfigClientError::OwnerStagedRelationshipDenied { .. }) => denied(),
        Err(ConfigClientError::OwnerStagedRelationshipUnmanaged { .. }) => failure(
            StatusCode::CONFLICT,
            OwnerStagedRefusal::Unmanaged,
            "This document is not owned by this runtime or one of its Agents.",
        ),
        Err(error) => {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "owner-staged change failed");
            failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "The relationship change could not be applied.",
            )
        }
    }
}

/// The fixed denial, whichever check refused, so a response reveals neither
/// whether the document exists nor who owns it.
fn denied() -> Response {
    failure(
        StatusCode::FORBIDDEN,
        OwnerStagedRefusal::Denied,
        "The requesting client may not change this document's relationships.",
    )
}

fn failure(status: StatusCode, code: impl serde::Serialize, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "code": code, "error": message })),
    )
        .into_response()
}
