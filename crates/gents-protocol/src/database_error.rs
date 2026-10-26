use std::fmt;

use serde_json::Value;

pub const STORAGE_FULL_CODE: &str = "STORAGE_FULL";
/// DefraDB's `extensions.code` for a store conflict the caller may replay;
/// the final, engine-exhausted case carries `TXN_RETRY_EXHAUSTED` instead.
pub const TXN_CONFLICT_CODE: &str = "TXN_CONFLICT";

/// Full storage does not establish whether a mutation committed. A latched
/// store must be reopened by its existing owner after space is available.
#[derive(Debug, Clone, Copy)]
pub struct StorageFull;

impl StorageFull {
    pub fn from_code(code: Option<&str>) -> Option<Self> {
        (code == Some(STORAGE_FULL_CODE)).then_some(Self)
    }
}

impl fmt::Display for StorageFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "storage is full; free disk space, then restart Gents with the same history folder",
        )
    }
}

impl std::error::Error for StorageFull {}

/// A database response retains its structured errors so callers need not
/// infer storage state or retry authority from localized message text.
#[derive(Debug)]
pub struct GraphqlFailure {
    operation: String,
    errors: Value,
    storage_full: Option<StorageFull>,
    retryable_conflict: bool,
}

impl GraphqlFailure {
    pub fn new(operation: impl Into<String>, errors: Value) -> Self {
        let codes = || {
            errors
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|error| error.pointer("/extensions/code").and_then(Value::as_str))
        };
        let full = codes().any(|code| StorageFull::from_code(Some(code)).is_some());
        let retryable_conflict = codes().any(|code| code == TXN_CONFLICT_CODE);
        Self {
            operation: operation.into(),
            errors,
            storage_full: full.then_some(StorageFull),
            retryable_conflict,
        }
    }

    /// Whether DefraDB reported a store conflict the caller may retry.
    pub fn is_retryable_conflict(&self) -> bool {
        self.retryable_conflict
    }
}

impl fmt::Display for GraphqlFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "graphql returned errors: {} ({})",
            self.errors, self.operation
        )
    }
}

impl std::error::Error for GraphqlFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.storage_full
            .as_ref()
            .map(|cause| cause as &dyn std::error::Error)
    }
}

pub fn is_storage_full(error: &(dyn std::error::Error + 'static)) -> bool {
    crate::error_chain::find_source::<StorageFull>(error).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("outer operation")]
    struct Outer(#[source] GraphqlFailure);

    #[test]
    fn generated_storage_failures_preserve_recovery_evidence() {
        let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases = contract["storage_failure_cases"].as_array().unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let error = Outer(GraphqlFailure::new(
                "test",
                serde_json::json!([{
                    "message": case["message"], "extensions": { "code": case["code"] }
                }]),
            ));
            assert_eq!(is_storage_full(&error), case["full"].as_bool().unwrap());
            #[cfg(feature = "native")]
            if is_storage_full(&error) {
                assert!(!crate::graphql::graphql_error_is_retryable(&error));
            }
        }
    }
}
