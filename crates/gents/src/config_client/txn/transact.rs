use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use futures::future::BoxFuture;

#[cfg(test)]
use super::fault_injection::retry_backoff_reached_for_test;
use super::finalize::{CommitCleanup, CommitFailure};
use super::native::ACTIVE_EMBEDDED_TRANSACTION;
use super::telemetry::{AttemptReport, AttemptTelemetry, AttemptTelemetryState};
use super::{
    embedded_phase_timeout, retry, ConfigApplyTxn, ConfigClientError, Result, TransactionFailure,
    EMBEDDED_TRANSACTION_CALLBACK_TIMEOUT,
};
use crate::config_client::write_telemetry::{
    ConflictSource, RetryOwner, RollbackStatus, WriteAttemptOrdinal, WriteBackend, WriteMode,
    WriteOperation, WriteOutcome,
};

#[derive(Debug)]
pub(crate) struct TransactionFinalityError<E> {
    pub(crate) error: E,
    pub(crate) unconfirmed: bool,
}

/// Replay policy for idempotent transactions (stable identities, durable-state-guarded updates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotentTransactionRetry {
    Standard,
    FiveAttempts,
}

impl IdempotentTransactionRetry {
    const fn max_attempts(self) -> u32 {
        match self {
            Self::Standard => 4,
            Self::FiveAttempts => 5,
        }
    }
}

/// A competing commit delivered a new observation to reload rather than a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TransactionOutcome<T> {
    Committed(T),
    ConflictObserved,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum TransactionMode {
    ConflictRetry,
    Idempotent(IdempotentTransactionRetry),
    #[cfg(test)]
    ObserveConflict,
}

impl TransactionMode {
    const fn max_attempts(self) -> u32 {
        match self {
            Self::ConflictRetry => retry::TRANSACT_CONFLICT_MAX_RETRIES + 1,
            Self::Idempotent(policy) => policy.max_attempts(),
            #[cfg(test)]
            Self::ObserveConflict => 1,
        }
    }

    const fn retry_owner(self) -> RetryOwner {
        match self {
            Self::Idempotent(_) => RetryOwner::GentsIdempotentTransaction,
            Self::ConflictRetry => RetryOwner::GentsTransaction,
            #[cfg(test)]
            Self::ObserveConflict => RetryOwner::GentsTransaction,
        }
    }

    const fn retries_generic_errors(self) -> bool {
        matches!(self, Self::Idempotent(_))
    }

    fn retries_callback_error<E>(self, error: &E) -> bool
    where
        E: TransactionFailure + ?Sized,
    {
        matches!(self, Self::Idempotent(_))
            && !error
                .config_client_error()
                .is_some_and(|error| crate::graphql::is_storage_full(error))
            && retry::is_transaction_storage_failure(error)
    }

    fn retries_begin_error(self, error: &ConfigClientError) -> bool {
        self.retries_callback_error(error)
    }

    const fn observes_conflicts(self) -> bool {
        match self {
            #[cfg(test)]
            Self::ObserveConflict => true,
            Self::ConflictRetry | Self::Idempotent(_) => false,
        }
    }
}

struct TransactionAttemptFailure<E> {
    error: E,
    conflict_source: Option<ConflictSource>,
    rollback: RollbackStatus,
    retry_generic: bool,
}

enum FailureDisposition {
    Observe,
    Retry(std::time::Duration),
    Fail,
}

fn failure_disposition(
    mode: TransactionMode,
    conflict: Option<ConflictSource>,
    retry_generic: bool,
    attempt: u32,
    max_attempts: u32,
) -> FailureDisposition {
    if conflict.is_some() && mode.observes_conflicts() {
        FailureDisposition::Observe
    } else if attempt < max_attempts && (conflict.is_some() || retry_generic) {
        FailureDisposition::Retry(retry::transaction_backoff(attempt - 1))
    } else {
        FailureDisposition::Fail
    }
}

pub(super) fn transact_owned<'a, T, E, F, B>(
    operation: &'static str,
    backend: WriteBackend,
    mode: TransactionMode,
    begin: B,
    callback: F,
) -> BoxFuture<'a, Result<TransactionOutcome<T>, E>>
where
    T: Send + 'a,
    E: TransactionFailure + From<ConfigClientError> + Send + 'a,
    F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    B: FnMut(WriteOperation, Arc<AtomicBool>) -> BoxFuture<'a, Result<ConfigApplyTxn<'a>>>
        + Send
        + 'a,
{
    transact_owned_with_finality(operation, backend, mode, begin, callback, None)
}

pub(super) fn transact_owned_with_finality<'a, T, E, F, B>(
    operation: &'static str,
    backend: WriteBackend,
    mode: TransactionMode,
    mut begin: B,
    callback: F,
    unconfirmed: Option<Arc<AtomicBool>>,
) -> BoxFuture<'a, Result<TransactionOutcome<T>, E>>
where
    T: Send + 'a,
    E: TransactionFailure + From<ConfigClientError> + Send + 'a,
    F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T, E>> + Send + 'a,
    B: FnMut(WriteOperation, Arc<AtomicBool>) -> BoxFuture<'a, Result<ConfigApplyTxn<'a>>>
        + Send
        + 'a,
{
    Box::pin(async move {
        let operation = WriteOperation::new(operation).map_err(ConfigClientError::from)?;
        let mut max_attempts = mode.max_attempts();
        let mut attempt = 0;
        while attempt < max_attempts {
            attempt += 1;
            let ordinal =
                WriteAttemptOrdinal::new(attempt, max_attempts).map_err(ConfigClientError::from)?;
            let cancellation_rollback_scheduled = Arc::new(AtomicBool::new(false));
            let mut telemetry = AttemptTelemetry {
                operation,
                backend,
                mode: WriteMode::Transaction,
                retry_owner: mode.retry_owner(),
                ordinal,
                started: Instant::now(),
                state: StdMutex::new(AttemptTelemetryState::default()),
                cancelled_rollback: RollbackStatus::NotNeeded,
                cancellation_rollback_scheduled: Some(Arc::clone(&cancellation_rollback_scheduled)),
                dispatch: tracing::dispatcher::get_default(Clone::clone),
            };
            let mut txn = match begin(operation, cancellation_rollback_scheduled).await {
                Ok(txn) => txn,
                Err(error) if mode.retries_begin_error(&error) && attempt < max_attempts => {
                    let backoff = retry::transaction_backoff(attempt - 1);
                    telemetry.record(AttemptReport {
                        outcome: WriteOutcome::Retrying,
                        conflict_source: ConflictSource::None,
                        backoff: Some(backoff),
                        affected_documents: Some(0),
                        rollback: RollbackStatus::NotNeeded,
                    });
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                Err(error) => {
                    telemetry.record(AttemptReport {
                        outcome: WriteOutcome::Failed,
                        conflict_source: ConflictSource::None,
                        backoff: None,
                        affected_documents: Some(0),
                        rollback: RollbackStatus::NotNeeded,
                    });
                    return Err(error.into());
                }
            };
            telemetry.cancelled_rollback = match backend {
                WriteBackend::Embedded => RollbackStatus::NotNeeded,
                WriteBackend::Http => RollbackStatus::Scheduled,
            };
            let callback_result = match backend {
                WriteBackend::Embedded => ACTIVE_EMBEDDED_TRANSACTION
                    .scope(
                        operation.as_str(),
                        tokio::time::timeout(EMBEDDED_TRANSACTION_CALLBACK_TIMEOUT, callback(&txn)),
                    )
                    .await
                    .map_err(|_| {
                        E::from(embedded_phase_timeout(
                            "callback",
                            EMBEDDED_TRANSACTION_CALLBACK_TIMEOUT,
                        ))
                    })
                    .and_then(|result| result),
                WriteBackend::Http => callback(&txn).await,
            };
            let attempt_result: Result<(), TransactionAttemptFailure<E>> = match callback_result {
                Ok(value) => {
                    let affected_documents = txn.affected_documents();
                    match txn.commit_inner().await {
                        Ok(()) => {
                            txn.disarm_rollback();
                            telemetry.record(AttemptReport {
                                outcome: WriteOutcome::Committed,
                                conflict_source: ConflictSource::None,
                                backoff: None,
                                affected_documents: Some(affected_documents),
                                rollback: RollbackStatus::NotNeeded,
                            });
                            return Ok(TransactionOutcome::Committed(value));
                        }
                        Err(CommitFailure {
                            error,
                            cleanup,
                            definitely_not_committed,
                        }) => {
                            let rollback = match cleanup {
                                CommitCleanup::NotNeeded => {
                                    txn.disarm_rollback();
                                    RollbackStatus::NotNeeded
                                }
                                CommitCleanup::Required if txn.rollback_inner().await.is_ok() => {
                                    RollbackStatus::Succeeded
                                }
                                CommitCleanup::Required => RollbackStatus::Scheduled,
                            };
                            // Embedded finish consumes the transaction before commit starts;
                            // a successful discard proves commit did not take it. HTTP discard
                            // cannot establish that a commit response was not lost.
                            let discarded_before_commit = matches!(
                                (backend, cleanup, rollback),
                                (
                                    WriteBackend::Embedded,
                                    CommitCleanup::Required,
                                    RollbackStatus::Succeeded
                                )
                            );
                            if let (false, false, Some(unconfirmed)) = (
                                definitely_not_committed,
                                discarded_before_commit,
                                &unconfirmed,
                            ) {
                                unconfirmed.store(true, Ordering::Release);
                            }
                            Err(TransactionAttemptFailure {
                                retry_generic: mode.retries_generic_errors()
                                    && !crate::graphql::is_storage_full(&error),
                                conflict_source: retry::classified_transaction_conflict(&error),
                                error: E::from(error),
                                rollback,
                            })
                        }
                    }
                }
                Err(error) => {
                    let retry_generic = mode.retries_callback_error(&error);
                    let rollback = if txn.rollback_inner().await.is_ok() {
                        RollbackStatus::Succeeded
                    } else {
                        RollbackStatus::Scheduled
                    };
                    Err(TransactionAttemptFailure {
                        conflict_source: retry::classified_transaction_conflict(&error),
                        error,
                        rollback,
                        retry_generic,
                    })
                }
            };
            let failure = match attempt_result {
                Err(failure) => failure,
                Ok(_) => unreachable!("successful attempt returns after commit"),
            };
            let TransactionAttemptFailure {
                error,
                conflict_source,
                rollback,
                retry_generic,
            } = failure;
            if txn.embedded_poisoned() {
                telemetry.record(AttemptReport {
                    outcome: WriteOutcome::PendingNative,
                    conflict_source: ConflictSource::None,
                    backoff: None,
                    affected_documents: None,
                    rollback: RollbackStatus::Scheduled,
                });
                return Err(error);
            }
            let source = conflict_source.unwrap_or(ConflictSource::None);
            let sibling_contention = conflict_source.is_some()
                && txn.contends_with_request_siblings.load(Ordering::Relaxed);
            if sibling_contention {
                max_attempts = max_attempts.max(retry::REQUEST_SIBLING_MAX_ATTEMPTS);
            }
            match failure_disposition(mode, conflict_source, retry_generic, attempt, max_attempts) {
                FailureDisposition::Observe => {
                    telemetry.record(AttemptReport {
                        outcome: WriteOutcome::ConflictObserved,
                        conflict_source: source,
                        backoff: None,
                        affected_documents: Some(0),
                        rollback,
                    });
                    return Ok(TransactionOutcome::ConflictObserved);
                }
                FailureDisposition::Retry(backoff) => {
                    let backoff = if sibling_contention {
                        retry::request_sibling_backoff(attempt - 1)
                    } else {
                        backoff
                    };
                    telemetry.record(AttemptReport {
                        outcome: WriteOutcome::Retrying,
                        conflict_source: source,
                        backoff: Some(backoff),
                        affected_documents: Some(0),
                        rollback,
                    });
                    drop(txn);
                    #[cfg(test)]
                    retry_backoff_reached_for_test();
                    tokio::time::sleep(backoff).await;
                }
                FailureDisposition::Fail => {
                    telemetry.record(AttemptReport {
                        outcome: WriteOutcome::Failed,
                        conflict_source: source,
                        backoff: None,
                        affected_documents: Some(0),
                        rollback,
                    });
                    return Err(error);
                }
            }
        }
        unreachable!("bounded transaction loop returns on its final attempt")
    })
}

pub(super) fn expect_committed<T>(outcome: TransactionOutcome<T>) -> T {
    match outcome {
        TransactionOutcome::Committed(value) => value,
        TransactionOutcome::ConflictObserved => {
            unreachable!("only observing transactions expose observed conflicts")
        }
    }
}
