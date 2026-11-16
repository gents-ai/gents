//! Inputs and outputs of the `AccessClassification` families: production
//! access errors for each modeled failure and the access owner's
//! classification of whatever holds one.

use super::retry::{classified_transaction_conflict, transaction_conflict};
use super::write_telemetry::ConflictSource;
use super::{ConfigClientError, TransactionFailure};

/// `AccessClassification.Failure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessFailure {
    Unauthorized,
    Forbidden,
    PermissionDeniedCode,
    TransactionConflict,
    Other,
}

impl AccessFailure {
    pub fn error(self) -> ConfigClientError {
        let begin = |status| ConfigClientError::TxBeginStatus {
            status,
            endpoint: "http://node/api/v0/tx".into(),
            body: String::new(),
        };
        match self {
            Self::Unauthorized => begin(reqwest::StatusCode::UNAUTHORIZED),
            Self::Forbidden => begin(reqwest::StatusCode::FORBIDDEN),
            Self::PermissionDeniedCode => {
                ConfigClientError::Database(gents_protocol::database_error::GraphqlFailure::new(
                    "read",
                    serde_json::json!([{
                        "message": "opaque",
                        "extensions": {
                            "code": gents_protocol::database_error::PERMISSION_DENIED_CODE
                        }
                    }]),
                ))
            }
            Self::TransactionConflict => transaction_conflict(ConflictSource::StructuredCode),
            Self::Other => begin(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
}

/// `AccessClassification.Erasure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Erasure {
    Typed,
    Anyhow,
    Boxed,
}

/// `AccessClassification.Classification`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessClassification {
    Refused,
    Conflict,
    Neither,
}

pub fn classify<E>(error: &E) -> AccessClassification
where
    E: TransactionFailure + ?Sized,
{
    if error.permission_denied() {
        AccessClassification::Refused
    } else if classified_transaction_conflict(error).is_some() {
        AccessClassification::Conflict
    } else {
        AccessClassification::Neither
    }
}

/// The classification of `error` as a receiver of `erasure` gets it, for an
/// owner with a classifier of its own.
pub fn classify_owner<E>(erasure: Erasure, error: E) -> AccessClassification
where
    E: TransactionFailure + std::error::Error + Send + Sync + 'static,
{
    match erasure {
        Erasure::Typed => classify(&error),
        Erasure::Anyhow | Erasure::Boxed => classify_erased(erasure, error),
    }
}

/// [`classify_owner`] for an owner with no classifier of its own: its typed
/// value is searched as the erased one is.
pub fn classify_erased<E>(erasure: Erasure, error: E) -> AccessClassification
where
    E: std::error::Error + Send + Sync + 'static,
{
    match erasure {
        Erasure::Anyhow => classify(&anyhow::Error::new(error)),
        Erasure::Typed | Erasure::Boxed => {
            classify(&(Box::new(error) as Box<dyn std::error::Error + Send + Sync>))
        }
    }
}
