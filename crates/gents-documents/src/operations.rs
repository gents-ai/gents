//! The handlers every client calls for configuration documents this crate
//! owns (Lean `DocumentOperations`). Each runs in one transaction as whoever
//! the access signs for, and decides from the request, the stored documents
//! and the actor.

#[cfg(test)]
mod conformance;
pub mod delete;
pub mod mcp_service;
pub mod skill;
pub mod write;

use gents_store::config_client::{ConfigClientError, TransactionFailure};

/// A handler failure inside a transaction: the operation's own refusal, or
/// another owner's failure on its way through.
#[derive(Debug)]
pub enum OperationFailure<E> {
    Operation(E),
    Other(anyhow::Error),
}

impl<E> OperationFailure<E> {
    pub fn map<F>(self, convert: impl FnOnce(E) -> F) -> OperationFailure<F> {
        match self {
            Self::Operation(error) => OperationFailure::Operation(convert(error)),
            Self::Other(error) => OperationFailure::Other(error),
        }
    }

    pub fn finish(
        self,
        passthrough: impl FnOnce(Box<dyn std::error::Error + Send + Sync>) -> E,
    ) -> E {
        match self {
            Self::Operation(error) => error,
            Self::Other(error) => {
                passthrough(error.reallocate_into_boxed_dyn_error_without_backtrace())
            }
        }
    }
}

impl<E> From<anyhow::Error> for OperationFailure<E> {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl<E> From<ConfigClientError> for OperationFailure<E> {
    fn from(error: ConfigClientError) -> Self {
        Self::Other(error.into())
    }
}

impl<E> TransactionFailure for OperationFailure<E> {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Operation(_) => None,
            Self::Other(error) => error.config_client_error(),
        }
    }
}
