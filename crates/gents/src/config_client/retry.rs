use std::time::Duration;

use super::error::{ConfigClientError, TransactionConflict};
use super::write_telemetry::ConflictSource;
use crate::graphql::GraphqlResponseError;

pub(super) const TRANSACT_CONFLICT_MAX_RETRIES: u32 = 3;
pub(super) const RECEIPT_WRITE_MAX_RETRIES: u32 = 4;
/// Qualifies a full admitted cohort plus two overflow receipts sharing the
/// request publication owner. This is bounded retry, not a progress guarantee
/// under unbounded concurrent writes.
pub(super) const REQUEST_SIBLING_MAX_ATTEMPTS: u32 =
    crate::hook::persistence::MAX_BACKGROUNDED_TOOLS_PER_PARENT as u32 + 2;

/// Exposes the DefraDB access error a transaction callback failed with, so the
/// transaction owner classifies conflicts and storage failures by type. A
/// caller error that wraps [`ConfigClientError`] or [`GraphqlResponseError`]
/// returns it here; an `#[error(transparent)]` wrapper converted into
/// `anyhow::Error` hides it from the source-chain search below unless
/// `access_error_in` names that wrapper.
pub trait TransactionFailure {
    fn config_client_error(&self) -> Option<&ConfigClientError>;
}

impl TransactionFailure for ConfigClientError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        Some(self)
    }
}

impl TransactionFailure for GraphqlResponseError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            GraphqlResponseError::Access(error) => Some(error),
            _ => None,
        }
    }
}

impl TransactionFailure for anyhow::Error {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        self.chain().find_map(access_error_in)
    }
}

impl TransactionFailure for Box<dyn std::error::Error + Send + Sync> {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        let root: &(dyn std::error::Error + 'static) = self.as_ref();
        std::iter::successors(Some(root), |cause| cause.source()).find_map(access_error_in)
    }
}

fn access_error_in<'a>(
    cause: &'a (dyn std::error::Error + 'static),
) -> Option<&'a ConfigClientError> {
    fn wrapped<'a, E>(cause: &'a (dyn std::error::Error + 'static)) -> Option<&'a ConfigClientError>
    where
        E: TransactionFailure + std::error::Error + 'static,
    {
        cause
            .downcast_ref::<E>()
            .and_then(TransactionFailure::config_client_error)
    }
    cause
        .downcast_ref::<ConfigClientError>()
        .or_else(|| wrapped::<GraphqlResponseError>(cause))
        .or_else(|| wrapped::<crate::document_config::DocumentConfigError>(cause))
        .or_else(|| wrapped::<crate::oauth_credential::OAuthCredentialError>(cause))
        .or_else(|| wrapped::<crate::usage_observation::UsageObservationError>(cause))
        .or_else(|| wrapped::<crate::rendered_request::RenderedRequestError>(cause))
        .or_else(|| wrapped::<crate::session::SessionError>(cause))
        .or_else(|| wrapped::<crate::session::SessionControlError>(cause))
        .or_else(|| wrapped::<crate::session_origin::SessionOriginError>(cause))
        .or_else(|| wrapped::<crate::lifecycle::LifecycleError>(cause))
        .or_else(|| wrapped::<crate::lifecycle::ExecutionLeaseError>(cause))
        .or_else(|| wrapped::<crate::lifecycle::ExecutionRenewalError>(cause))
        .or_else(|| wrapped::<crate::lifecycle::RecoveryError>(cause))
        .or_else(|| wrapped::<crate::lifecycle::CanonicalRecoveryError>(cause))
        .or_else(|| wrapped::<crate::lifecycle::queue::QueueError>(cause))
        .or_else(|| wrapped::<crate::trigger_engine::durable::DurableTriggerError>(cause))
        .or_else(|| wrapped::<crate::trigger_engine::event_delivery::EventDeliveryError>(cause))
        .or_else(|| wrapped::<crate::trigger_engine::event_source::EventSourceError>(cause))
}

pub(super) fn transaction_conflict(origin: ConflictSource) -> ConfigClientError {
    ConfigClientError::TransactionConflict(TransactionConflict { origin })
}

pub(super) fn classified_transaction_conflict<E>(error: &E) -> Option<ConflictSource>
where
    E: TransactionFailure + ?Sized,
{
    match error.config_client_error()? {
        ConfigClientError::TransactionConflict(conflict) => Some(conflict.origin),
        _ => None,
    }
}

pub(super) fn transaction_storage_failure(error: ConfigClientError) -> ConfigClientError {
    if classified_transaction_conflict(&error).is_some() {
        error
    } else {
        ConfigClientError::StorageFailure(Box::new(error))
    }
}

pub(super) fn is_transaction_storage_failure<E>(error: &E) -> bool
where
    E: TransactionFailure + ?Sized,
{
    matches!(
        error.config_client_error(),
        Some(ConfigClientError::StorageFailure(_))
    )
}

pub(super) fn transaction_backoff(retry_index: u32) -> Duration {
    Duration::from_millis(100u64.saturating_mul(1u64 << retry_index.min(10)))
}

pub(super) fn request_sibling_backoff(retry_index: u32) -> Duration {
    use rand::Rng as _;
    Duration::from_millis(20 * u64::from(retry_index + 1) + rand::rng().random_range(0..20))
}

pub(super) fn receipt_write_backoff(retry_index: u32) -> Duration {
    Duration::from_millis(50u64.saturating_mul(u64::from(retry_index) + 1))
}

pub(super) fn is_retryable_ambiguous_write(error: &(dyn std::error::Error + 'static)) -> bool {
    gents_protocol::graphql::graphql_error_is_retryable(error)
}

pub(super) fn is_transaction_conflict_text(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("transaction conflict") && (text.contains("retry") || text.contains("try again"))
}

pub(super) fn graphql_value_is_transaction_conflict(value: &serde_json::Value) -> bool {
    // Share the embedded DB's classification, including its JSON null/absent representation of no result.
    <query::QueryResponse as serde::Deserialize>::deserialize(value)
        .is_ok_and(|response| response.is_transaction_conflict())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_typed_conflicts_are_replayable() {
        let typed = transaction_conflict(ConflictSource::StructuredCode);
        assert_eq!(
            classified_transaction_conflict(&typed),
            Some(ConflictSource::StructuredCode)
        );
        assert_eq!(
            classified_transaction_conflict(&Box::<dyn std::error::Error + Send + Sync>::from(
                typed
            )),
            Some(ConflictSource::StructuredCode)
        );
        assert_eq!(
            classified_transaction_conflict(&Box::<dyn std::error::Error + Send + Sync>::from(
                "transaction conflict; please retry"
            )),
            None
        );
    }

    #[test]
    fn structured_graphql_code_is_required() {
        assert!(graphql_value_is_transaction_conflict(&serde_json::json!({
            "errors": [{"message": "opaque", "extensions": {"code": "TXN_CONFLICT"}}]
        })));
        assert!(!graphql_value_is_transaction_conflict(&serde_json::json!({
            "errors": [{"message": "transaction conflict; please retry"}]
        })));
        assert!(graphql_value_is_transaction_conflict(&serde_json::json!({
            "data": null,
            "errors": [{"message": "opaque", "extensions": {"code": "TXN_CONFLICT"}}]
        })));
        assert!(!graphql_value_is_transaction_conflict(&serde_json::json!({
            "data": {"update_X": [{"_docID": "doc"}]},
            "errors": [{"message": "opaque", "extensions": {"code": "TXN_CONFLICT"}}]
        })));
    }

    #[test]
    fn conflict_shape_matches_pinned_defradb_serialization() {
        let response = query::QueryResponse::transaction_conflict("opaque");
        let value = serde_json::to_value(response).expect("serialize DefraDB response");
        assert_eq!(value.get("data"), Some(&serde_json::Value::Null));
        assert!(graphql_value_is_transaction_conflict(&value));

        let success = serde_json::to_value(query::QueryResponse::success(serde_json::json!({
            "create_X": [{"_docID": "doc"}]
        })))
        .expect("serialize successful DefraDB response");
        assert!(success.get("errors").is_none());
    }

    #[test]
    fn malformed_or_mixed_replies_are_not_replayable() {
        for value in [
            serde_json::Value::Null,
            serde_json::json!({"errors": "TXN_CONFLICT"}),
            serde_json::json!({"errors": [{"extensions": {"code": "TXN_CONFLICT"}}]}),
            serde_json::json!({"data": null, "errors": [
                {"message": "conflict", "extensions": {"code": "TXN_CONFLICT"}},
                {"message": "another error"}
            ]}),
        ] {
            assert!(!graphql_value_is_transaction_conflict(&value), "{value}");
        }
    }
}
