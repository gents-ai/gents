use gents_documents::{
    apply_desired_state_plan, list_inference_backends_in_txn, list_inference_profiles_in_txn,
    DesiredStateApplyPlan,
};
use gents_protocol::operations::inference::{AccountOperation, InferenceOperationError};
use gents_store::collection::Collection;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};

use super::decision::{account_operation_admitted, acting, acting_on};
use super::OperationFailure;
use crate::auth::credential::{backend_account, remove_account_in_txn, AccountSummary};
use crate::resolved::ProviderProfilePolicy;

/// Lean `InferenceOperations.accountOperationAdmitted` for the actor of
/// `txn`.
pub async fn admit_account_operation(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    operation: AccountOperation,
) -> Result<(), OperationFailure> {
    if account_operation_admitted(acting(txn, node_did).await?, operation) {
        Ok(())
    } else {
        Err(InferenceOperationError::AccountOperation(operation).into())
    }
}

pub async fn admit_account_operation_on(
    access: &ConfigAccess,
    node_did: &str,
    operation: AccountOperation,
) -> Result<(), OperationFailure> {
    if account_operation_admitted(acting_on(access, node_did).await?, operation) {
        Ok(())
    } else {
        Err(InferenceOperationError::AccountOperation(operation).into())
    }
}

/// What removing an account did to the backends that used it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRemoval {
    pub deleted_backends: Vec<String>,
    /// Backends a profile still selects; they stay and report the account as
    /// missing.
    pub kept_backends: Vec<String>,
}

/// Removes a signed-in account and, in the same transaction, the backends of
/// that account no profile selects.
pub async fn remove_account(
    access: &ConfigAccess,
    node_did: &str,
    account: &AccountSummary,
) -> Result<AccountRemoval, InferenceOperationError> {
    access
        .transact_write("inference.account.remove", |txn| {
            Box::pin(async move {
                admit_account_operation(txn, node_did, AccountOperation::Remove).await?;
                remove_account_in_txn(txn, node_did, &account.credential_id).await?;
                let profiles = list_inference_profiles_in_txn(txn, node_did).await?;
                let (kept_backends, deleted_backends): (Vec<_>, Vec<_>) =
                    list_inference_backends_in_txn(txn, node_did)
                        .await?
                        .into_iter()
                        .filter(|backend| {
                            account.account_ref.is_some()
                                && backend_account(backend, std::slice::from_ref(account))
                                    .flatten()
                                    .is_some()
                        })
                        .map(|backend| backend.backend_id)
                        .partition(|backend_id| {
                            profiles
                                .iter()
                                .any(|profile| &profile.backend_id == backend_id)
                        });
                if !deleted_backends.is_empty() {
                    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(
                        deleted_backends
                            .iter()
                            .map(|id| {
                                (
                                    Collection::InferenceBackend,
                                    node_did.to_owned(),
                                    id.clone(),
                                )
                            })
                            .collect(),
                    )?;
                    apply_desired_state_plan(txn, &plan, &ProviderProfilePolicy).await?;
                }
                Ok::<_, OperationFailure>(AccountRemoval {
                    deleted_backends,
                    kept_backends,
                })
            })
        })
        .await
        .map_err(Into::into)
}
