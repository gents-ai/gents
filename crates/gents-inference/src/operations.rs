//! The handlers of the inference operations `gents_protocol::operations::inference`
//! declares (Lean `InferenceOperations`). Every client calls these; a rule
//! that depends on who acts is decided here from the transaction's signer.

mod account;
mod decision;
mod discovery;
mod document;

pub use account::{
    admit_account_operation, admit_account_operation_on, remove_account, AccountRemoval,
};
pub use decision::{
    account_operation_admitted, acting, acting_for_authenticated_request, acting_on,
    acting_signed_by, actor_kind, backend_create_decision, backend_update_decision,
    discovery_answer, keeps_auth, profile_choice_decision, ActingDid, AuthClass, CurrentBackend,
    Signer,
};
pub use discovery::{
    discover, discover_for_authenticated_request, discovery_target, Discovery, DiscoveryTarget,
};
pub use document::{
    admit_backend_choice_in_txn, admit_plan_in_txn, merge_in_txn, plan_in_txn, remove,
    remove_in_txn, whole_document_write, write, write_in_txn, MergedWrite, PlanAdmissionFailure,
    PlannedWrite, WriteReceipt,
};

use gents_protocol::operations::inference::InferenceOperationError;
use gents_store::config_client::{ConfigClientError, TransactionFailure};

/// A handler's failure inside a transaction: the operation's own refusal, or
/// another owner's failure the transaction owner still classifies.
#[derive(Debug, thiserror::Error)]
pub enum OperationFailure {
    #[error(transparent)]
    Operation(InferenceOperationError),
    #[error(transparent)]
    Other(anyhow::Error),
}

impl From<InferenceOperationError> for OperationFailure {
    fn from(error: InferenceOperationError) -> Self {
        Self::Operation(error)
    }
}

impl From<anyhow::Error> for OperationFailure {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<ConfigClientError> for OperationFailure {
    fn from(error: ConfigClientError) -> Self {
        Self::Other(error.into())
    }
}

impl From<OperationFailure> for InferenceOperationError {
    fn from(failure: OperationFailure) -> Self {
        match failure {
            OperationFailure::Operation(error) => error,
            OperationFailure::Other(error) => {
                Self::Passthrough(error.reallocate_into_boxed_dyn_error_without_backtrace())
            }
        }
    }
}

impl TransactionFailure for OperationFailure {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Operation(_) => None,
            Self::Other(error) => error.config_client_error(),
        }
    }
}
