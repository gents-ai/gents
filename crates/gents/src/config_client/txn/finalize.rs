use std::time::Instant;

use super::apply_txn::TxnBackend;
#[cfg(test)]
use super::fault_injection::{
    fail_rollback_for_test, pause_embedded_finalization_for_test,
    post_commit_receipt_loss_for_test, EmbeddedFinalizationBoundary,
};
use super::native::native_operation_abandoned;
use super::{
    embedded_phase_timeout, graphql, retry, ConfigApplyTxn, ConfigClientError, Result,
    EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT, EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
};
use crate::config_client::write_telemetry::{record_slow_native_phase, ConflictSource};

#[derive(Clone, Copy)]
pub(super) enum CommitCleanup {
    NotNeeded,
    Required,
}

pub(super) struct CommitFailure {
    pub(super) error: ConfigClientError,
    pub(super) cleanup: CommitCleanup,
    pub(super) definitely_not_committed: bool,
}

impl ConfigApplyTxn<'_> {
    pub(super) async fn commit_inner(&mut self) -> std::result::Result<(), CommitFailure> {
        match &self.backend {
            TxnBackend::Http {
                endpoint,
                id,
                client,
            } => graphql::txn_commit(endpoint, id, client)
                .await
                .map_err(|failure| match failure {
                    graphql::TxnCommitError::CleanupRequired(error) => CommitFailure {
                        error,
                        cleanup: CommitCleanup::Required,
                        definitely_not_committed: false,
                    },
                    graphql::TxnCommitError::TransactionConsumed(error) => CommitFailure {
                        error,
                        cleanup: CommitCleanup::NotNeeded,
                        definitely_not_committed: false,
                    },
                }),
            TxnBackend::Embedded { node, handle, .. } => {
                if self.embedded_poisoned() {
                    return Err(CommitFailure {
                        error: native_operation_abandoned(
                            ConfigClientError::CommitAfterAbandonedOperation,
                        ),
                        cleanup: CommitCleanup::Required,
                        definitely_not_committed: true,
                    });
                }
                if let TxnBackend::Embedded {
                    native_operations, ..
                } = &self.backend
                {
                    let started = Instant::now();
                    native_operations.wait_idle().await;
                    record_slow_native_phase(
                        self.operation,
                        "commit-inflight-drain",
                        started.elapsed(),
                    );
                }
                let started = Instant::now();
                let result =
                    tokio::time::timeout(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT, async {
                        #[cfg(test)]
                        pause_embedded_finalization_for_test(
                            EmbeddedFinalizationBoundary::BeforeCommit,
                        )
                        .await;
                        let result = node.commit_transaction(handle).await;
                        #[cfg(test)]
                        if result.is_ok() {
                            pause_embedded_finalization_for_test(
                                EmbeddedFinalizationBoundary::AfterCommit,
                            )
                            .await;
                        }
                        result
                    })
                    .await;
                record_slow_native_phase(self.operation, "commit", started.elapsed());
                match result {
                    Err(_) => Err(CommitFailure {
                        error: embedded_phase_timeout(
                            "commit",
                            EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
                        ),
                        cleanup: CommitCleanup::Required,
                        definitely_not_committed: false,
                    }),
                    Ok(Ok(())) => {
                        #[cfg(test)]
                        if post_commit_receipt_loss_for_test(self.operation) {
                            return Err(CommitFailure {
                                error: retry::transaction_storage_failure(
                                    ConfigClientError::InjectedReceiptLoss,
                                ),
                                cleanup: CommitCleanup::NotNeeded,
                                definitely_not_committed: false,
                            });
                        }
                        Ok(())
                    }
                    Ok(Err(error)) => {
                        let conflict = matches!(&error, query::TransactionError::Conflict(_));
                        if conflict {
                            Err(CommitFailure {
                                error: retry::transaction_conflict(ConflictSource::TypedError),
                                cleanup: CommitCleanup::NotNeeded,
                                definitely_not_committed: true,
                            })
                        } else {
                            Err(CommitFailure {
                                error: ConfigClientError::CommitTransaction { source: error },
                                cleanup: CommitCleanup::NotNeeded,
                                definitely_not_committed: false,
                            })
                        }
                    }
                }
            }
        }
    }

    pub(super) async fn rollback_inner(&mut self) -> Result<()> {
        if self.embedded_poisoned() {
            return Err(native_operation_abandoned(
                ConfigClientError::RollbackDeferred,
            ));
        }
        if let TxnBackend::Embedded {
            native_operations, ..
        } = &self.backend
        {
            native_operations.wait_idle().await;
        }
        #[cfg(test)]
        if fail_rollback_for_test() {
            return Err(ConfigClientError::InjectedRollbackFailure);
        }
        let result = match &self.backend {
            TxnBackend::Http {
                endpoint,
                id,
                client,
            } => graphql::txn_discard(endpoint, id, client).await,
            TxnBackend::Embedded { node, handle, .. } => {
                match tokio::time::timeout(EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT, async {
                    #[cfg(test)]
                    pause_embedded_finalization_for_test(
                        EmbeddedFinalizationBoundary::BeforeRollback,
                    )
                    .await;
                    node.discard_transaction(handle).await
                })
                .await
                {
                    Ok(result) => result
                        .map_err(|error| ConfigClientError::RollbackTransaction { source: error }),
                    Err(_) => {
                        let error = embedded_phase_timeout(
                            "rollback",
                            EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT,
                        );
                        tracing::warn!(
                            "embedded rollback timed out; releasing mutation gate with transaction outcome ambiguous"
                        );
                        self.disarm_rollback();
                        return Err(error);
                    }
                }
            }
        };
        if result.is_ok() {
            self.disarm_rollback();
        }
        result
    }

    pub(super) fn disarm_rollback(&mut self) {
        if let Some(rollback) = self.rollback_on_drop.as_mut() {
            rollback.disarm();
        }
        self.rollback_on_drop = None;
    }

    #[cfg(test)]
    pub(crate) async fn commit(mut self) -> Result<()> {
        let result = self.commit_inner().await;
        match &result {
            Ok(())
            | Err(CommitFailure {
                cleanup: CommitCleanup::NotNeeded,
                ..
            }) => self.disarm_rollback(),
            Err(CommitFailure {
                cleanup: CommitCleanup::Required,
                ..
            }) => {
                if let Err(error) = self.rollback_inner().await {
                    tracing::debug!(%error, "rolling back ambiguous failed transaction");
                }
            }
        }
        result.map_err(|failure| failure.error)
    }

    #[cfg(test)]
    pub(crate) async fn discard(mut self) -> Result<()> {
        self.rollback_inner().await
    }
}
