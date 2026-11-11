//! Application transaction closures may fail with `anyhow`; the transaction
//! owner still classifies the access error anywhere in their chain.

use super::{ConfigClientError, TransactionFailure};

impl TransactionFailure for anyhow::Error {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        let error: &(dyn std::error::Error + 'static) = self.as_ref();
        super::retry::access_error_in(error)
    }
}

#[cfg(test)]
mod tests {
    use super::super::retry::{
        classified_transaction_conflict, is_transaction_storage_failure, transaction_conflict,
        transaction_storage_failure,
    };
    use super::super::write_telemetry::ConflictSource;
    use super::super::ConfigClientError;
    use crate::graphql::GraphqlResponseError;

    #[test]
    fn application_callback_failures_keep_their_classification() {
        let conflict = anyhow::Error::from(transaction_conflict(ConflictSource::StructuredCode))
            .context("graph run callback");
        assert_eq!(
            classified_transaction_conflict(&conflict),
            Some(ConflictSource::StructuredCode)
        );
        let storage = anyhow::Error::from(transaction_storage_failure(
            ConfigClientError::InjectedReadStorageFailure,
        ))
        .context("graph run callback");
        assert!(is_transaction_storage_failure(&storage));
        let response = anyhow::Error::from(GraphqlResponseError::from(transaction_conflict(
            ConflictSource::StructuredCode,
        )));
        assert_eq!(
            classified_transaction_conflict(&response),
            Some(ConflictSource::StructuredCode)
        );
    }
}
