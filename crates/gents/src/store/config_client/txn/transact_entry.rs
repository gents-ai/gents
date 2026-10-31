use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use defra_node::EmbeddedNode;
use futures::future::BoxFuture;
use identity::Did;

use super::apply_txn::EmbeddedTransactionPurpose;
use super::native::NativeOperationTracker;
use super::telemetry::backend_for_access;
use super::transact::TransactionFinalityError;
#[cfg(test)]
use super::transact::TransactionOutcome;
use super::transact::{
    expect_committed, transact_owned, transact_owned_with_finality, TransactionMode,
};
use super::{
    ConfigAccess, ConfigApplyTxn, ConfigClientError, IdempotentTransactionRetry, Result,
    TransactionFailure,
};
use crate::store::config_client::write_telemetry::{WriteBackend, WriteOperation};

impl ConfigAccess {
    async fn begin_apply_txn(
        &self,
        operation: WriteOperation,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
    ) -> Result<ConfigApplyTxn<'_>> {
        match self {
            Self::Graphql(endpoint) => {
                ConfigApplyTxn::begin_http(endpoint, cancellation_rollback_scheduled, false).await
            }
            Self::Local(local) => {
                ConfigApplyTxn::begin_local_owned(
                    local.node(),
                    None,
                    operation,
                    cancellation_rollback_scheduled,
                    EmbeddedTransactionPurpose::Mutation,
                )
                .await
            }
        }
    }

    pub async fn transact_write<'a, T, E, F>(
        &'a self,
        operation: &'static str,
        callback: F,
    ) -> Result<T, E>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    {
        transact_owned(
            operation,
            backend_for_access(self),
            TransactionMode::ConflictRetry,
            move |operation, rollback| {
                Box::pin(async move { self.begin_apply_txn(operation, rollback).await })
            },
            callback,
        )
        .await
        .map(expect_committed)
    }

    pub(crate) async fn transact_write_with_finality<'a, T, E, F>(
        &'a self,
        operation: &'static str,
        callback: F,
    ) -> std::result::Result<T, TransactionFinalityError<E>>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    {
        let unconfirmed = Arc::new(AtomicBool::new(false));
        let result = transact_owned_with_finality(
            operation,
            backend_for_access(self),
            TransactionMode::ConflictRetry,
            move |operation, rollback| {
                Box::pin(async move { self.begin_apply_txn(operation, rollback).await })
            },
            callback,
            Some(unconfirmed.clone()),
        )
        .await;
        result
            .map(expect_committed)
            .map_err(|error| TransactionFinalityError {
                error,
                unconfirmed: unconfirmed.load(Ordering::Acquire),
            })
    }

    /// Query-only transaction; DefraDB enforces read-only mode on both transports
    /// and embedded access avoids the mutation gate.
    pub async fn transact_readonly<'a, T, E, F>(
        &'a self,
        operation: &'static str,
        callback: F,
    ) -> Result<T, E>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    {
        match self {
            Self::Local(local) => {
                Self::transact_local_readonly(local.node(), None, operation, callback).await
            }
            Self::Graphql(endpoint) => transact_owned(
                operation,
                WriteBackend::Http,
                TransactionMode::ConflictRetry,
                move |_operation, rollback| {
                    Box::pin(
                        async move { ConfigApplyTxn::begin_http(endpoint, rollback, true).await },
                    )
                },
                callback,
            )
            .await
            .map(expect_committed),
        }
    }

    /// Replay a stable-key transaction under its bounded retry policy.
    /// Full storage returns for recovery without replaying the mutation.
    /// Callers must make the callback idempotent across an ambiguous commit:
    /// creates need stable unique identities and retries must reconcile an
    /// already-durable winner before attempting another create.
    pub async fn transact_idempotent<'a, T, E, F>(
        &'a self,
        retry_policy: IdempotentTransactionRetry,
        operation: &'static str,
        callback: F,
    ) -> Result<T, E>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    {
        transact_owned(
            operation,
            backend_for_access(self),
            TransactionMode::Idempotent(retry_policy),
            move |operation, rollback| {
                Box::pin(async move { self.begin_apply_txn(operation, rollback).await })
            },
            callback,
        )
        .await
        .map(expect_committed)
    }

    #[cfg(test)]
    pub(crate) async fn transact_observing_conflict<'a, T, E, F>(
        &'a self,
        operation: &'static str,
        callback: F,
    ) -> Result<TransactionOutcome<T>, E>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    {
        transact_owned(
            operation,
            backend_for_access(self),
            TransactionMode::ObserveConflict,
            move |operation, rollback| {
                Box::pin(async move { self.begin_apply_txn(operation, rollback).await })
            },
            callback,
        )
        .await
    }

    pub async fn transact_local_write<'a, T, E, F>(
        node: &'a Arc<EmbeddedNode>,
        identity: Option<Did>,
        operation: &'static str,
        callback: F,
    ) -> Result<T, E>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    {
        transact_owned(
            operation,
            WriteBackend::Embedded,
            TransactionMode::ConflictRetry,
            move |operation, rollback| {
                let identity = identity.clone();
                Box::pin(async move {
                    ConfigApplyTxn::begin_local_owned(
                        node,
                        identity,
                        operation,
                        rollback,
                        EmbeddedTransactionPurpose::Mutation,
                    )
                    .await
                })
            },
            callback,
        )
        .await
        .map(expect_committed)
    }

    pub(crate) async fn transact_local_write_with_finality<'a, T, E, F>(
        node: &'a Arc<EmbeddedNode>,
        identity: Option<Did>,
        operation: &'static str,
        callback: F,
    ) -> std::result::Result<T, TransactionFinalityError<E>>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    {
        let unconfirmed = Arc::new(AtomicBool::new(false));
        let result = transact_owned_with_finality(
            operation,
            WriteBackend::Embedded,
            TransactionMode::ConflictRetry,
            move |operation, rollback| {
                let identity = identity.clone();
                Box::pin(async move {
                    ConfigApplyTxn::begin_local_owned(
                        node,
                        identity,
                        operation,
                        rollback,
                        EmbeddedTransactionPurpose::Mutation,
                    )
                    .await
                })
            },
            callback,
            Some(unconfirmed.clone()),
        )
        .await;
        result
            .map(expect_committed)
            .map_err(|error| TransactionFinalityError {
                error,
                unconfirmed: unconfirmed.load(Ordering::Acquire),
            })
    }

    /// Native snapshot reads retain the supplied ACP DID without taking the mutation gate.
    pub async fn transact_local_readonly<'a, T, E, F>(
        node: &'a Arc<EmbeddedNode>,
        identity: Option<Did>,
        operation: &'static str,
        callback: F,
    ) -> Result<T, E>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    {
        transact_owned(
            operation,
            WriteBackend::Embedded,
            TransactionMode::ConflictRetry,
            move |operation, rollback| {
                let identity = identity.clone();
                Box::pin(async move {
                    ConfigApplyTxn::begin_local_owned(
                        node,
                        identity,
                        operation,
                        rollback,
                        EmbeddedTransactionPurpose::ReadOnly,
                    )
                    .await
                })
            },
            callback,
        )
        .await
        .map(expect_committed)
    }

    /// Renew an execution lease: generation/deadline CAS, native point-read OCC,
    /// ordinary writes under the mutation gate. Authorization is re-checked
    /// before commit admission; native commit does not atomically check wall
    /// clock, so this is not a durable-commit-before-expiry guarantee.
    pub(crate) async fn renew_execution_lease<'a, T, E, F>(
        node: &'a Arc<EmbeddedNode>,
        renew: F,
    ) -> Result<T, E>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    {
        let native_operations = Arc::new(NativeOperationTracker::default());
        let operations_for_attempt = Arc::clone(&native_operations);
        let result = transact_owned(
            "lifecycle.renew_execution_lease",
            WriteBackend::Embedded,
            TransactionMode::Idempotent(IdempotentTransactionRetry::Standard),
            move |operation, rollback| {
                Box::pin(ConfigApplyTxn::begin_local_owned(
                    node,
                    None,
                    operation,
                    rollback,
                    EmbeddedTransactionPurpose::ExecutionRenewal(Arc::clone(
                        &operations_for_attempt,
                    )),
                ))
            },
            renew,
        )
        .await;
        if native_operations.is_abandoned() {
            native_operations.wait_idle().await;
        }
        result.map(expect_committed)
    }

    pub async fn transact_local_idempotent<'a, T, E, F>(
        node: &'a Arc<EmbeddedNode>,
        identity: Option<Did>,
        retry_policy: IdempotentTransactionRetry,
        operation: &'static str,
        callback: F,
    ) -> Result<T, E>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    {
        transact_owned(
            operation,
            WriteBackend::Embedded,
            TransactionMode::Idempotent(retry_policy),
            move |operation, rollback| {
                let identity = identity.clone();
                Box::pin(async move {
                    ConfigApplyTxn::begin_local_owned(
                        node,
                        identity,
                        operation,
                        rollback,
                        EmbeddedTransactionPurpose::Mutation,
                    )
                    .await
                })
            },
            callback,
        )
        .await
        .map(expect_committed)
    }
}
