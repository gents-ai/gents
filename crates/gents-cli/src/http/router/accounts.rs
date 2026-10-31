use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

use super::RuntimeHttpState;

/// Only the runtime's own operator may ask.
pub(super) async fn account_usage_read_handler(
    State(state): State<RuntimeHttpState>,
    axum::Json(command): axum::Json<crate::commands::accounts::UsageReadCommand>,
) -> Response {
    let error = |status: StatusCode, error: String| {
        (status, axum::Json(json!({ "error": error }))).into_response()
    };
    let Some(service) = state.enrollment_decisions.read().await.clone() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime is not ready".to_string(),
        );
    };
    let authenticated = match command.validate_at(chrono::Utc::now()) {
        Ok(()) => {
            service
                .authenticate_signed(
                    &command.signer_did,
                    &command.signing_payload(),
                    &command.sig,
                    &command.nonce,
                )
                .await
        }
        Err(invalid) => Err(invalid),
    };
    if let Err(refused) = authenticated {
        return error(StatusCode::FORBIDDEN, format!("{refused:#}"));
    }
    let Some(runtime) = state.activation_runtime.get() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime is not activated yet".to_string(),
        );
    };
    match runtime
        .read_usage(command.trigger, command.provider.as_deref())
        .await
    {
        Ok(reads) => (
            StatusCode::OK,
            axum::Json(json!({ "node_did": runtime.node_did(), "reads": reads })),
        )
            .into_response(),
        Err(failed) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("{failed:#}")),
    }
}

pub(super) async fn enrollment_decision_handler(
    State(state): State<RuntimeHttpState>,
    axum::Json(command): axum::Json<gents_protocol::enrollment::EnrollmentOperatorDecisionCommand>,
) -> Response {
    let Some(service) = state.enrollment_decisions.read().await.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error": "enrollment authority is not ready"})),
        )
            .into_response();
    };
    match service.decide(&command).await {
        Ok(outcome) => (
            StatusCode::OK,
            axum::Json(json!({
                "request_id": outcome.request_id,
                "state": outcome.state,
                "decision_doc_id": outcome.decision_doc_id,
                "revision_doc_id": outcome.revision_doc_id,
                "delivery_pending": outcome.delivery_pending,
            })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "error": error.to_string(),
                "code": gents_protocol::error_chain::find_source::<
                    gents::runtime::agent::p2p_reconcile::EnrollmentDecisionRefusal,
                >(error.as_ref())
                .map(gents::runtime::agent::p2p_reconcile::EnrollmentDecisionRefusal::code),
            })),
        )
            .into_response(),
    }
}

pub(super) async fn enrollment_pending_handler(
    State(state): State<RuntimeHttpState>,
    axum::Json(command): axum::Json<gents_protocol::enrollment::EnrollmentOperatorQueryCommand>,
) -> Response {
    let Some(service) = state.enrollment_decisions.read().await.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error": "enrollment authority is not ready"})),
        )
            .into_response();
    };
    match service.pending(&command).await {
        Ok(pending) => (StatusCode::OK, axum::Json(json!({"pending": pending}))).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}
