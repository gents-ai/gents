//! Private conflict classification and shared retry policy for committed writes.

use std::fmt;
use std::time::Duration;

use super::write_telemetry::ConflictSource;

pub(super) const TRANSACT_CONFLICT_MAX_RETRIES: u32 = 3;
pub(super) const RECEIPT_WRITE_MAX_RETRIES: u32 = 4;

#[derive(Debug)]
struct TransactionConflict {
    source: ConflictSource,
}

impl fmt::Display for TransactionConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DefraDB transaction conflict")
    }
}

impl std::error::Error for TransactionConflict {}

#[derive(Debug)]
struct TransactionStorageFailure {
    error: anyhow::Error,
}

impl fmt::Display for TransactionStorageFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:#}", self.error)
    }
}

impl std::error::Error for TransactionStorageFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

pub(super) fn transaction_conflict(source: ConflictSource) -> anyhow::Error {
    anyhow::Error::new(TransactionConflict { source })
}

/// Only errors explicitly wrapped by the transaction owner are replayable.
pub(super) fn classified_transaction_conflict(error: &anyhow::Error) -> Option<ConflictSource> {
    error
        .downcast_ref::<TransactionConflict>()
        .map(|conflict| conflict.source)
}

pub(super) fn transaction_storage_failure(error: anyhow::Error) -> anyhow::Error {
    if classified_transaction_conflict(&error).is_some() {
        error
    } else {
        anyhow::Error::new(TransactionStorageFailure { error })
    }
}

pub(super) fn is_transaction_storage_failure(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TransactionStorageFailure>().is_some()
}

pub(super) fn transaction_backoff(retry_index: u32) -> Duration {
    Duration::from_millis(100u64.saturating_mul(1u64 << retry_index.min(10)))
}

pub(super) fn receipt_write_backoff(retry_index: u32) -> Duration {
    Duration::from_millis(50u64.saturating_mul(u64::from(retry_index) + 1))
}

pub(super) fn is_retryable_ambiguous_write(error: &anyhow::Error) -> bool {
    gents_protocol::graphql::graphql_error_is_retryable(error)
}

pub(super) fn is_transaction_conflict_text(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("transaction conflict") && (text.contains("retry") || text.contains("try again"))
}

pub(super) fn graphql_value_is_transaction_conflict(value: &serde_json::Value) -> bool {
    // Share the embedded DB's classification, including its JSON null/absent
    // representation of no result. Malformed replies remain non-replayable.
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
            classified_transaction_conflict(&anyhow::anyhow!("transaction conflict; please retry")),
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
