use std::time::Duration;

use super::error::{ConfigClientError, TransactionConflict};
use super::write_telemetry::ConflictSource;
use crate::store::graphql::GraphqlResponseError;

pub(super) const TRANSACT_CONFLICT_MAX_RETRIES: u32 = 3;
pub(super) const RECEIPT_WRITE_MAX_RETRIES: u32 = 4;
/// Qualifies a full admitted cohort plus two overflow receipts sharing the
/// request publication owner. This is bounded retry, not a progress guarantee
/// under unbounded concurrent writes.
pub(super) const REQUEST_SIBLING_MAX_ATTEMPTS: u32 =
    crate::tools::call::hook::persistence::MAX_BACKGROUNDED_TOOLS_PER_PARENT as u32 + 2;

/// Exposes the DefraDB access error a transaction callback failed with, so the
/// transaction owner classifies conflicts and storage failures by type. A
/// caller error that wraps [`ConfigClientError`] or [`GraphqlResponseError`]
/// returns it here by matching its own variants; erased (`anyhow` or boxed)
/// callback errors are searched by [`access_error_in`].
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

impl<T> TransactionFailure for Box<T>
where
    T: TransactionFailure,
{
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        (**self).config_client_error()
    }
}

impl TransactionFailure for Box<dyn std::error::Error + Send + Sync> {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        access_error_in(self.as_ref())
    }
}

/// The first access owner in an erased error's chain. Both owners wrap each
/// other transparently, so each is asked for its access error rather than
/// found by `source()` alone.
pub(super) fn access_error_in<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a ConfigClientError> {
    gents_protocol::error_chain::find_map_source(error, |cause| {
        cause.downcast_ref::<ConfigClientError>().or_else(|| {
            cause
                .downcast_ref::<GraphqlResponseError>()?
                .config_client_error()
        })
    })
}

pub(super) fn transaction_conflict(origin: ConflictSource) -> ConfigClientError {
    ConfigClientError::TransactionConflict(TransactionConflict { origin })
}

pub(super) fn classified_transaction_conflict<E>(error: &E) -> Option<ConflictSource>
where
    E: TransactionFailure + ?Sized,
{
    error.config_client_error()?.transaction_conflict()
}

pub(super) fn transaction_storage_failure(error: ConfigClientError) -> ConfigClientError {
    if error.transaction_conflict().is_some() {
        error
    } else {
        ConfigClientError::StorageFailure(Box::new(error))
    }
}

pub(super) fn is_transaction_storage_failure<E>(error: &E) -> bool
where
    E: TransactionFailure + ?Sized,
{
    error
        .config_client_error()
        .is_some_and(ConfigClientError::is_storage_failure)
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
    !crate::store::graphql::is_storage_full(error)
        && (gents_protocol::graphql::graphql_error_is_retryable(error)
            || access_error_in(error).is_some_and(|error| {
                error.transaction_conflict().is_some() || error.is_retryable_database_conflict()
            }))
}

/// External premise: DefraDB's HTTP commit handler answers a store conflict
/// with `409 Conflict` and says so only in its body text.
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

    fn access(error: ConfigClientError) -> ConfigClientError {
        ConfigClientError::Response(GraphqlResponseError::Access(Box::new(error)))
    }

    fn erased(error: ConfigClientError) -> Box<dyn std::error::Error + Send + Sync> {
        #[derive(Debug, thiserror::Error)]
        #[error("callback step")]
        struct Step(#[source] ConfigClientError);
        Box::new(Step(error))
    }

    #[test]
    fn conflict_and_storage_classification_sees_through_access_wrappers() {
        let conflict = || transaction_conflict(ConflictSource::StructuredCode);
        let storage = || transaction_storage_failure(ConfigClientError::InjectedReadStorageFailure);
        for error in [conflict(), access(conflict()), access(access(conflict()))] {
            assert_eq!(
                error.transaction_conflict(),
                Some(ConflictSource::StructuredCode)
            );
            assert!(!error.is_storage_failure());
            assert_eq!(
                classified_transaction_conflict(&erased(error)),
                Some(ConflictSource::StructuredCode)
            );
        }
        for error in [storage(), access(storage())] {
            assert!(error.is_storage_failure());
            assert_eq!(error.transaction_conflict(), None);
            assert!(is_transaction_storage_failure(&erased(error)));
        }
        let response = GraphqlResponseError::Access(Box::new(conflict()));
        assert_eq!(
            classified_transaction_conflict(&response),
            Some(ConflictSource::StructuredCode)
        );
        assert!(!is_transaction_storage_failure(&erased(
            ConfigClientError::InjectedReadStorageFailure
        )));
    }

    #[test]
    fn abandoned_native_operations_classify_through_every_wrapper() {
        use super::super::NativeOperationAbandoned as Abandoned;
        let phases: [fn(Abandoned) -> ConfigClientError; 7] = [
            ConfigClientError::PoisonedByAbandonedOperation,
            ConfigClientError::ExecuteCallerTimeout,
            ConfigClientError::ExecutionTaskEnded,
            ConfigClientError::CommitAfterAbandonedOperation,
            ConfigClientError::RollbackDeferred,
            ConfigClientError::AutoCommitCallerTimeout,
            ConfigClientError::AutoCommitTaskEnded,
        ];
        for phase in phases {
            let wrapped = [
                phase(Abandoned),
                transaction_storage_failure(phase(Abandoned)),
                access(phase(Abandoned)),
                ConfigClientError::HttpQuery {
                    endpoint: "http://node/graphql".into(),
                    source: Box::new(transaction_storage_failure(phase(Abandoned))),
                },
            ];
            for error in wrapped {
                assert!(error.native_operation_abandoned(), "{error:?}");
            }
        }
        assert!(
            !transaction_storage_failure(ConfigClientError::InjectedReadStorageFailure)
                .native_operation_abandoned()
        );
    }

    #[test]
    fn insecure_identity_keys_classify_through_access_wrappers() {
        let identity = || {
            ConfigClientError::from(crate::store::identity::IdentityError::InsecureKey(
                crate::store::identity::InsecureKeyPermissions {
                    path: "/k".into(),
                    mode: 0o644,
                },
            ))
        };
        for error in [
            identity(),
            access(identity()),
            transaction_storage_failure(identity()),
        ] {
            assert_eq!(error.insecure_key().map(|key| key.mode), Some(0o644));
        }
        assert!(ConfigClientError::InjectedReadStorageFailure
            .insecure_key()
            .is_none());
    }

    #[test]
    fn database_conflicts_replay_on_every_access_path() {
        assert_eq!(
            gents_protocol::database_error::TXN_CONFLICT_CODE,
            query::executor::TXN_CONFLICT_ERROR_CODE
        );
        let failure = |code: &str| {
            gents_protocol::database_error::GraphqlFailure::new(
                "read",
                serde_json::json!([{"message": "opaque", "extensions": {"code": code}}]),
            )
        };
        let http_read = |code: &str| ConfigClientError::HttpQuery {
            endpoint: "http://node/graphql".into(),
            source: Box::new(ConfigClientError::Protocol(
                gents_protocol::graphql::GraphqlError::Database(failure(code)),
            )),
        };
        let embedded = |code: &str| ConfigClientError::EmbeddedGraphqlErrors {
            errors: vec![query::QueryResponseError::new("opaque").with_code(code)],
        };
        for conflict in [
            http_read(query::executor::TXN_CONFLICT_ERROR_CODE),
            ConfigClientError::Database(failure(query::executor::TXN_CONFLICT_ERROR_CODE)),
            access(embedded(query::executor::TXN_CONFLICT_ERROR_CODE)),
        ] {
            assert!(conflict.is_retryable_database_conflict(), "{conflict:?}");
            assert!(conflict.is_graphql_rejection(), "{conflict:?}");
            assert!(is_retryable_ambiguous_write(erased(conflict).as_ref()));
        }
        for rejection in [
            http_read(query::executor::TXN_RETRY_EXHAUSTED_ERROR_CODE),
            embedded(query::executor::TXN_RETRY_EXHAUSTED_ERROR_CODE),
        ] {
            assert!(rejection.is_graphql_rejection(), "{rejection:?}");
            assert!(!rejection.is_retryable_database_conflict(), "{rejection:?}");
            assert!(!is_retryable_ambiguous_write(erased(rejection).as_ref()));
        }
    }

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

#[cfg(test)]
mod storage_failure_tests {
    use super::*;

    #[test]
    fn full_storage_does_not_enter_ambiguous_write_retries() {
        let contract: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        for case in contract["storage_failure_cases"].as_array().unwrap() {
            let error = GraphqlResponseError::from(
                gents_protocol::database_error::GraphqlFailure::new(
                    "write",
                    serde_json::json!([{"message": case["message"], "extensions": {"code": case["code"]}}]),
                ),
            );
            if case["stop_automatic_retry"].as_bool().unwrap() {
                assert!(!is_retryable_ambiguous_write(&error));
            }
        }
        #[derive(Debug, thiserror::Error)]
        #[error("native transaction")]
        struct NativeTransaction(#[source] storage::corekv::Error);
        let error = NativeTransaction(storage::corekv::Error::from(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "please retry",
        )));
        assert!(crate::store::graphql::is_storage_full(&error));
        assert!(!is_retryable_ambiguous_write(&error));
    }
}
