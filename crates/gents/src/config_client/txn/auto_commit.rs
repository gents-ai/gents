use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use defra_node::EmbeddedNode;
use serde_json::Value;
use tracing::{instrument::WithSubscriber, Instrument};

#[cfg(test)]
use super::fault_injection::SUCCESSFUL_MUTATION_FAULT;
use super::gate::{existing_mutation_write_gate, mutation_write_gate};
use super::native::native_operation_abandoned;
use super::telemetry::{
    auto_commit_telemetry, finish_auto_commit, AttemptReport, AttemptTelemetry,
    AttemptTelemetryState, AutoCommitCallerObservation, AutoCommitNativeCompletion,
};
use super::{
    graphql, retry, ConfigAccess, ConfigClientError, LocalConfigAccess, ReceiptWriteError, Result,
    EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
};
use crate::config_client::write_telemetry::{
    record_slow_native_phase, ConflictSource, ReceiptRecovery, RetryOwner, RollbackStatus,
    WriteAttemptOrdinal, WriteBackend, WriteMode, WriteOperation, WriteOutcome,
};

async fn receipt_with_retry<F, Fut, E>(
    receipt: &F,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = Result<bool, E>> + Send,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    for retry_index in 0..=retry::RECEIPT_WRITE_MAX_RETRIES {
        match receipt().await.map_err(Into::into) {
            Ok(present) => return Ok(present),
            Err(error)
                if retry::is_retryable_ambiguous_write(error.as_ref())
                    && retry_index < retry::RECEIPT_WRITE_MAX_RETRIES =>
            {
                tokio::time::sleep(retry::receipt_write_backoff(retry_index)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded receipt loop returns on its final attempt")
}

impl ConfigAccess {
    pub(crate) fn claimed_for_node(node: &EmbeddedNode) -> Option<Self> {
        let gate = existing_mutation_write_gate(node)?;
        let claim = gate.store_claim()?;
        Some(Self::Local(LocalConfigAccess {
            node: Arc::clone(&claim.node),
            gate,
        }))
    }

    pub(crate) async fn read_local_response_owned(
        local: &LocalConfigAccess,
        document: &str,
        operation: &str,
    ) -> Result<defra_node::QueryResponse> {
        graphql::ensure_query_document(document)?;
        Self::read_local_response_owned_validated(local, document, operation).await
    }

    async fn read_local_response_owned_validated(
        local: &LocalConfigAccess,
        document: &str,
        operation: &str,
    ) -> Result<defra_node::QueryResponse> {
        let document = document.to_owned();
        let operation = operation.to_owned();
        local
            .run_owned(move |node| async move {
                Self::execute_local_read(&node, &document, &operation).await
            })
            .await
    }

    pub(crate) async fn read_local_response(
        node: &EmbeddedNode,
        document: &str,
        operation: &str,
    ) -> Result<defra_node::QueryResponse> {
        graphql::ensure_query_document(document)?;
        if let Some(Self::Local(local)) = Self::claimed_for_node(node) {
            return Self::read_local_response_owned_validated(&local, document, operation).await;
        }
        #[cfg(test)]
        if let Some(reached) = SUCCESSFUL_MUTATION_FAULT
            .try_with(|fault| {
                fault
                    .read_pause
                    .as_ref()
                    .filter(|(marker, _)| document.contains(marker))
                    .map(|(_, reached)| Arc::clone(reached))
            })
            .ok()
            .flatten()
        {
            reached.notify_one();
            std::future::pending::<()>().await;
        }
        Self::execute_local_read(node, document, operation).await
    }

    async fn execute_local_read(
        node: &EmbeddedNode,
        document: &str,
        operation: &str,
    ) -> Result<defra_node::QueryResponse> {
        let started = Instant::now();
        let response = node.execute(document).await;
        let elapsed = started.elapsed();
        if elapsed > Duration::from_secs(1) {
            tracing::warn!(
                target: "gents::config_client::txn",
                operation,
                elapsed_ms = elapsed.as_millis() as u64,
                "DefraDB GraphQL completed"
            );
        } else {
            tracing::debug!(
                target: "gents::config_client::txn",
                operation,
                elapsed_ms = elapsed.as_millis() as u64,
                "DefraDB GraphQL completed"
            );
        }
        Ok(response)
    }

    pub async fn write(&self, operation: &'static str, mutation: &str) -> Result<Value> {
        graphql::ensure_mutation_document(mutation)?;
        let operation = WriteOperation::new(operation)?;
        match self {
            Self::Graphql(endpoint) => {
                let telemetry = auto_commit_telemetry(operation, WriteBackend::Http)?;
                let result = graphql::auto_commit(endpoint, mutation).await;
                finish_auto_commit(&telemetry, result, graphql::affected_documents)
            }
            Self::Local(local) => {
                let telemetry = Arc::new(auto_commit_telemetry(operation, WriteBackend::Embedded)?);
                let response =
                    Self::execute_local_mutation(local.node(), operation, mutation, telemetry)
                        .await?;
                Ok(crate::graphql::into_data_envelope(response.data))
            }
        }
    }

    /// Committed stable-id HTTP mutation with bounded ambiguous-result recovery:
    /// each mutation is posted exactly once; a retryable response or transport
    /// failure is ambiguous, so the owner checks the caller's stable receipt
    /// before it may repost, and replays only after a successful receipt read
    /// confirms absence. Embedded access uses [`Self::write`].
    pub async fn write_with_receipt<F, Fut, E>(
        &self,
        operation: &'static str,
        mutation: &str,
        receipt: F,
    ) -> Result<(), ReceiptWriteError>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: Future<Output = Result<bool, E>> + Send,
        E: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        graphql::ensure_mutation_document(mutation)?;
        let Self::Graphql(endpoint) = self else {
            self.write(operation, mutation).await?;
            return Ok(());
        };
        let operation = WriteOperation::new(operation).map_err(ConfigClientError::from)?;
        let max_attempts = retry::RECEIPT_WRITE_MAX_RETRIES + 1;

        for attempt in 1..=max_attempts {
            let telemetry = AttemptTelemetry {
                operation,
                backend: WriteBackend::Http,
                mode: WriteMode::AutoCommit,
                retry_owner: RetryOwner::GentsReceipt,
                ordinal: WriteAttemptOrdinal::new(attempt, max_attempts)
                    .map_err(ConfigClientError::from)?,
                started: Instant::now(),
                state: StdMutex::new(AttemptTelemetryState::default()),
                cancelled_rollback: RollbackStatus::NotNeeded,
                cancellation_rollback_scheduled: None,
                dispatch: tracing::dispatcher::get_default(Clone::clone),
            };
            let error = match graphql::auto_commit(endpoint, mutation).await {
                Ok(response) => {
                    telemetry.record(AttemptReport {
                        outcome: WriteOutcome::Committed,
                        conflict_source: ConflictSource::None,
                        backoff: None,
                        affected_documents: Some(graphql::affected_documents(&response)),
                        rollback: RollbackStatus::NotNeeded,
                    });
                    return Ok(());
                }
                Err(error) if retry::is_retryable_ambiguous_write(&error) => error,
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

            let receipt_status = receipt_with_retry(&receipt).await;
            match receipt_status {
                Ok(true) => {
                    telemetry.record_with_receipt(
                        AttemptReport {
                            outcome: WriteOutcome::Recovered,
                            conflict_source: ConflictSource::None,
                            backoff: None,
                            affected_documents: None,
                            rollback: RollbackStatus::NotNeeded,
                        },
                        ReceiptRecovery::StableIdConfirmed,
                    );
                    return Ok(());
                }
                Ok(false) if attempt < max_attempts => {
                    let backoff = retry::receipt_write_backoff(attempt - 1);
                    telemetry.record_with_receipt(
                        AttemptReport {
                            outcome: WriteOutcome::Retrying,
                            conflict_source: ConflictSource::None,
                            backoff: Some(backoff),
                            affected_documents: Some(0),
                            rollback: RollbackStatus::NotNeeded,
                        },
                        ReceiptRecovery::StableIdAbsent,
                    );
                    tokio::time::sleep(backoff).await;
                }
                Ok(false) => {
                    telemetry.record_with_receipt(
                        AttemptReport {
                            outcome: WriteOutcome::Failed,
                            conflict_source: ConflictSource::None,
                            backoff: None,
                            affected_documents: Some(0),
                            rollback: RollbackStatus::NotNeeded,
                        },
                        ReceiptRecovery::StableIdAbsent,
                    );
                    return Err(error.into());
                }
                Err(receipt_error) => {
                    telemetry.record_with_receipt(
                        AttemptReport {
                            outcome: WriteOutcome::Failed,
                            conflict_source: ConflictSource::None,
                            backoff: None,
                            affected_documents: Some(0),
                            rollback: RollbackStatus::NotNeeded,
                        },
                        ReceiptRecovery::StableIdReadFailed,
                    );
                    return Err(ReceiptWriteError::Receipt(receipt_error));
                }
            }
        }
        unreachable!("bounded receipt write loop returns on its final attempt")
    }

    pub async fn write_local(
        node: &Arc<EmbeddedNode>,
        operation: &'static str,
        mutation: &str,
    ) -> Result<Value> {
        graphql::ensure_mutation_document(mutation)?;
        let operation = WriteOperation::new(operation)?;
        let telemetry = Arc::new(auto_commit_telemetry(operation, WriteBackend::Embedded)?);
        let response = Self::execute_local_mutation(node, operation, mutation, telemetry).await?;
        Ok(crate::graphql::into_data_envelope(response.data))
    }

    pub(crate) async fn write_local_response(
        node: &Arc<EmbeddedNode>,
        operation: &'static str,
        mutation: &str,
    ) -> Result<defra_node::QueryResponse> {
        graphql::ensure_mutation_document(mutation)?;
        let operation = WriteOperation::new(operation)?;
        let telemetry = Arc::new(auto_commit_telemetry(operation, WriteBackend::Embedded)?);
        Self::execute_local_mutation(node, operation, mutation, telemetry).await
    }

    async fn execute_local_mutation(
        node: &Arc<EmbeddedNode>,
        operation: WriteOperation,
        mutation: &str,
        telemetry: Arc<AttemptTelemetry>,
    ) -> Result<defra_node::QueryResponse> {
        let write_guard = match mutation_write_gate(node).acquire(operation).await {
            Ok(guard) => guard,
            Err(error) => {
                telemetry.record(AttemptReport {
                    outcome: WriteOutcome::Failed,
                    conflict_source: ConflictSource::None,
                    backoff: None,
                    affected_documents: Some(0),
                    rollback: RollbackStatus::NotNeeded,
                });
                return Err(error);
            }
        };
        let mut caller_observation = AutoCommitCallerObservation {
            telemetry: Arc::clone(&telemetry),
            completed: false,
        };
        let (send, receive) = tokio::sync::oneshot::channel();
        let node = Arc::clone(node);
        let mutation = mutation.to_owned();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        let span = tracing::Span::current();
        #[cfg(test)]
        let native_pause = SUCCESSFUL_MUTATION_FAULT
            .try_with(|fault| fault.native_pause.clone())
            .ok()
            .flatten();
        tokio::spawn(
            async move {
                let mut native_completion = AutoCommitNativeCompletion {
                    telemetry: Arc::clone(&telemetry),
                    started: false,
                    completed: false,
                };
                #[cfg(test)]
                if let Some((reached, release)) = native_pause {
                    reached.notify_one();
                    release.notified().await;
                }
                let started = Instant::now();
                native_completion.started = true;
                let result = node.execute(&mutation).await;
                let elapsed = started.elapsed();
                record_slow_native_phase(Some(operation.as_str()), "auto-commit", elapsed);
                let successful = !result.has_errors();
                let affected = if successful {
                    Some(crate::graphql::mutation_affected_documents(&result) as u64)
                } else {
                    Some(0)
                };
                telemetry.record(AttemptReport {
                    outcome: if successful {
                        WriteOutcome::Committed
                    } else {
                        WriteOutcome::Failed
                    },
                    conflict_source: ConflictSource::None,
                    backoff: None,
                    affected_documents: affected,
                    rollback: RollbackStatus::NotNeeded,
                });
                native_completion.completed = true;
                let result = crate::graphql::ensure_no_errors(&result, operation.as_str())
                    .map(|()| result)
                    .map_err(ConfigClientError::from);
                drop(write_guard);
                let _ = send.send(result);
            }
            .with_subscriber(dispatcher)
            .instrument(span),
        );
        let result = tokio::time::timeout(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT, receive)
            .await
            .map_err(|_| native_operation_abandoned(ConfigClientError::AutoCommitCallerTimeout))?;
        caller_observation.complete();
        result.map_err(|_| native_operation_abandoned(ConfigClientError::AutoCommitTaskEnded))?
    }
}
