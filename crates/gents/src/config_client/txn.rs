//! Canonical committed-write owner for embedded and HTTP DefraDB access.

use std::cell::Cell;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use defra_node::{EmbeddedNode, ExecuteRetryPolicy, QueryRequest};
use futures::future::BoxFuture;
use identity::Did;
use query::TransactionHandle;
use serde_json::{json, Value};

use super::graphql;
use super::retry;
use super::write_telemetry::{
    ConflictSource, ReceiptRecovery, RetryOwner, RollbackStatus, WriteAttemptEvent,
    WriteAttemptOrdinal, WriteBackend, WriteMode, WriteOperation, WriteOutcome,
};
use super::{graphql_api_base, ConfigAccess};

enum TxnBackend<'a> {
    Http {
        endpoint: &'a str,
        id: String,
        client: reqwest::Client,
    },
    Embedded {
        node: &'a EmbeddedNode,
        handle: TransactionHandle,
        identity: Option<Did>,
    },
}

type MutationWriteGate = tokio::sync::Mutex<()>;

// Embedded DefraDB operations are local and normally complete in milliseconds.
// Bound every phase that can retain the process-wide mutation gate so one
// wedged transaction cannot stop response progress, recovery, and hydration.
const EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT: Duration = Duration::from_secs(60);
const EMBEDDED_TRANSACTION_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT: Duration = Duration::from_secs(15);

fn embedded_phase_timeout(phase: &str, timeout: Duration) -> anyhow::Error {
    tracing::error!(
        phase,
        timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        "embedded DefraDB canonical write phase timed out"
    );
    retry::transaction_storage_failure(anyhow::anyhow!(
        "embedded transaction {phase} timed out after {timeout:?}"
    ))
}

async fn cleanup_while_holding_write_gate<F, T>(
    write_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    cleanup: F,
) -> std::result::Result<T, tokio::time::error::Elapsed>
where
    F: Future<Output = T>,
{
    let _write_guard = write_guard;
    tokio::time::timeout(EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT, cleanup).await
}

fn mutation_write_gate(node: &EmbeddedNode) -> Arc<MutationWriteGate> {
    static GATES: OnceLock<StdMutex<HashMap<usize, Weak<MutationWriteGate>>>> = OnceLock::new();

    let node_key = node as *const EmbeddedNode as usize;
    let mut gates = GATES
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(gate) = gates.get(&node_key).and_then(Weak::upgrade) {
        return gate;
    }

    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    gates.insert(node_key, Arc::downgrade(&gate));
    gate
}

enum RollbackOnDrop {
    Http {
        endpoint: String,
        id: String,
        client: reqwest::Client,
        armed: bool,
    },
    Embedded {
        runner: Arc<dyn query::QueryExecutor>,
        handle: Option<TransactionHandle>,
        write_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
        armed: bool,
    },
}

impl RollbackOnDrop {
    fn disarm(&mut self) {
        match self {
            Self::Http { armed, .. } | Self::Embedded { armed, .. } => *armed = false,
        }
    }

    fn set_embedded_handle(&mut self, transaction_handle: TransactionHandle) {
        let Self::Embedded { handle, .. } = self else {
            unreachable!("only embedded rollback owners receive native handles")
        };
        *handle = Some(transaction_handle);
    }
}

impl Drop for RollbackOnDrop {
    fn drop(&mut self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        match self {
            Self::Http {
                endpoint,
                id,
                client,
                armed,
            } => {
                if !*armed {
                    return;
                }
                let endpoint = endpoint.clone();
                let id = id.clone();
                let client = client.clone();
                runtime.spawn(async move {
                    if let Err(error) = graphql::txn_discard(&endpoint, &id, &client).await {
                        tracing::debug!(%error, "discarding cancelled HTTP transaction");
                    }
                });
            }
            Self::Embedded {
                runner,
                handle,
                write_guard,
                armed,
            } => {
                if !*armed {
                    return;
                }
                let Some(handle) = handle.take() else {
                    return;
                };
                let runner = runner.clone();
                let write_guard = write_guard.take();
                runtime.spawn(async move {
                    match cleanup_while_holding_write_gate(
                        write_guard,
                        runner.rollback_txn(&handle),
                    )
                    .await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            tracing::debug!(%error, "discarding cancelled embedded transaction");
                        }
                        Err(_) => {
                            tracing::warn!(
                                timeout_ms = EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT.as_millis(),
                                "cancelled embedded transaction rollback timed out; releasing mutation gate"
                            );
                        }
                    }
                });
            }
        }
    }
}

async fn begin_embedded_owned<F, Fut>(
    runner: Arc<dyn query::QueryExecutor>,
    write_guard: tokio::sync::OwnedMutexGuard<()>,
    cancellation_rollback_scheduled: Arc<AtomicBool>,
    after_begin: F,
) -> Result<(RollbackOnDrop, TransactionHandle)>
where
    F: FnOnce(TransactionHandle) -> Fut,
    Fut: Future<Output = ()>,
{
    // Construct the cleanup owner before begin. The caller deliberately runs
    // this future in a detached task: if its JoinHandle is cancelled after
    // DefraDB registers the transaction, dropping the task output still owns
    // and rolls back the handle before releasing the write gate.
    let mut rollback = RollbackOnDrop::Embedded {
        runner: Arc::clone(&runner),
        handle: None,
        write_guard: Some(write_guard),
        armed: true,
    };
    let handle = tokio::time::timeout(
        EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
        runner.begin_txn(false),
    )
    .await
    .map_err(|_| embedded_phase_timeout("begin", EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT))?
    .map_err(|error| anyhow::anyhow!("begin_txn: {error}"))?;
    rollback.set_embedded_handle(handle.clone());
    cancellation_rollback_scheduled.store(true, Ordering::Release);
    after_begin(handle.clone()).await;
    Ok((rollback, handle))
}

async fn begin_http_owned(
    endpoint: String,
    cancellation_rollback_scheduled: Arc<AtomicBool>,
) -> Result<(reqwest::Client, String, RollbackOnDrop)> {
    // Keep the response owner alive if the caller is cancelled while DefraDB
    // is returning the newly registered ID. Once the ID arrives, dropping the
    // detached task output schedules DELETE through `RollbackOnDrop`.
    let (client, id) = graphql::txn_begin(&endpoint).await?;
    let rollback = RollbackOnDrop::Http {
        endpoint,
        id: id.clone(),
        client: client.clone(),
        armed: true,
    };
    cancellation_rollback_scheduled.store(true, Ordering::Release);
    Ok((client, id, rollback))
}

#[derive(Clone, Copy)]
enum CommitCleanup {
    NotNeeded,
    Required,
}

struct CommitFailure {
    error: anyhow::Error,
    cleanup: CommitCleanup,
}

/// Bounded replay policy for transactions whose callbacks are safe to repeat
/// because every created document has a stable identity and every update is
/// guarded by durable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotentTransactionRetry {
    /// One initial attempt plus three retries.
    Standard,
    /// One initial attempt plus four retries.
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

/// Result of a transaction whose caller treats a competing commit as a new
/// observation to reload rather than an operation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TransactionOutcome<T> {
    Committed(T),
    ConflictObserved,
}

#[derive(Debug, Clone, Copy)]
enum TransactionMode {
    ConflictRetry,
    Idempotent(IdempotentTransactionRetry),
    ObserveConflict,
}

impl TransactionMode {
    const fn max_attempts(self) -> u32 {
        match self {
            Self::ConflictRetry => retry::TRANSACT_CONFLICT_MAX_RETRIES + 1,
            Self::Idempotent(policy) => policy.max_attempts(),
            Self::ObserveConflict => 1,
        }
    }

    const fn retry_owner(self) -> RetryOwner {
        match self {
            Self::Idempotent(_) => RetryOwner::GentsIdempotentTransaction,
            Self::ConflictRetry | Self::ObserveConflict => RetryOwner::GentsTransaction,
        }
    }

    const fn retries_generic_errors(self) -> bool {
        matches!(self, Self::Idempotent(_))
    }

    fn retries_callback_error(self, error: &anyhow::Error) -> bool {
        matches!(self, Self::Idempotent(_)) && retry::is_transaction_storage_failure(error)
    }

    const fn observes_conflicts(self) -> bool {
        matches!(self, Self::ObserveConflict)
    }
}

/// An open explicit transaction. Callers receive this only inside
/// [`ConfigAccess::transact`] callbacks once migrations are complete.
///
/// The raw begin/commit/discard transport lives in the crate-private
/// `graphql` submodule and is not nameable from outside `gents`; external
/// callers must go through the owned [`ConfigAccess::write`] and
/// [`ConfigAccess::transact`] entry points:
///
/// ```compile_fail
/// use gents::config_client::graphql::txn_begin;
/// // The `graphql` transport module is private to `gents`, so this import
/// // fails with E0603 and raw transactions cannot be driven directly.
/// let _ = txn_begin("http://127.0.0.1:1/api/v0/graphql");
/// ```
pub struct ConfigApplyTxn<'a> {
    backend: TxnBackend<'a>,
    rollback_on_drop: Option<RollbackOnDrop>,
    affected_documents: AtomicU64,
}

impl<'a> ConfigApplyTxn<'a> {
    async fn begin_local_owned(
        node: &'a EmbeddedNode,
        identity: Option<Did>,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
    ) -> Result<Self> {
        let write_guard = tokio::time::timeout(
            EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
            mutation_write_gate(node).lock_owned(),
        )
        .await
        .map_err(|_| {
            embedded_phase_timeout(
                "write-gate acquisition",
                EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
            )
        })?;
        let runner = node.runner().clone();
        let (rollback_on_drop, handle) = tokio::spawn(begin_embedded_owned(
            runner,
            write_guard,
            cancellation_rollback_scheduled,
            |_| std::future::ready(()),
        ))
        .await
        .context("begin embedded transaction task")??;
        Ok(Self {
            backend: TxnBackend::Embedded {
                node,
                handle: handle.clone(),
                identity,
            },
            rollback_on_drop: Some(rollback_on_drop),
            affected_documents: AtomicU64::new(0),
        })
    }

    #[cfg(test)]
    pub(crate) async fn begin_local(node: &'a EmbeddedNode, identity: Option<Did>) -> Result<Self> {
        Self::begin_local_owned(node, identity, Arc::new(AtomicBool::new(false))).await
    }

    async fn begin_http(
        endpoint: &'a str,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
    ) -> Result<Self> {
        let (client, id, rollback_on_drop) = tokio::spawn(begin_http_owned(
            endpoint.to_owned(),
            cancellation_rollback_scheduled,
        ))
        .await
        .context("begin HTTP transaction task")??;
        let txn = Self {
            backend: TxnBackend::Http {
                endpoint,
                id: id.clone(),
                client: client.clone(),
            },
            rollback_on_drop: Some(rollback_on_drop),
            affected_documents: AtomicU64::new(0),
        };
        Ok(txn)
    }

    /// Execute exactly once inside this snapshot. Conflict replay belongs to
    /// the outer `transact` owner and always begins a fresh transaction.
    pub async fn execute(&self, document: &str) -> Result<Value> {
        self.execute_with_variables(document, &json!({})).await
    }

    /// Execute in this same transaction with typed GraphQL variables. JSON
    /// scalar payloads retain arbitrary keys and nested empty arrays; ordinary
    /// nillable list fields remain the caller's responsibility.
    pub async fn execute_with_variables(&self, document: &str, variables: &Value) -> Result<Value> {
        anyhow::ensure!(variables.is_object(), "GraphQL variables must be an object");
        let (expanded_document, expanded_variables) =
            gents_protocol::graphql::expand_mutation_input_variables(document, variables)?;
        let document = expanded_document.as_str();
        let variables = &expanded_variables;
        let response = match &self.backend {
            TxnBackend::Http {
                endpoint,
                id,
                client,
            } => graphql::txn_execute(endpoint, id, client, document, variables)
                .await
                .map_err(retry::transaction_storage_failure)?,
            TxnBackend::Embedded {
                node,
                handle,
                identity,
            } => {
                let response = tokio::time::timeout(
                    EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
                    node.execute_request_in_txn(
                        QueryRequest::new(document)
                            .with_variables(variables.clone())
                            .with_identity(identity.clone()),
                        handle,
                    ),
                )
                .await
                .map_err(|_| {
                    embedded_phase_timeout("execute", EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT)
                })?;
                if response.is_transaction_conflict() {
                    return Err(retry::transaction_conflict(ConflictSource::StructuredCode));
                }
                if response.has_errors() {
                    return Err(retry::transaction_storage_failure(anyhow::anyhow!(
                        "graphql returned errors: {:?}",
                        response.errors
                    )));
                }
                json!({"data": response.data.unwrap_or(Value::Null)})
            }
        };
        if document.trim_start().starts_with("mutation") {
            self.affected_documents
                .fetch_add(graphql::affected_documents(&response), Ordering::Relaxed);
        }
        Ok(response)
    }

    pub async fn collection_version(&self, collection: &str) -> Result<Option<Value>> {
        crate::graphql::validate_collection_identifier(collection)?;
        match &self.backend {
            TxnBackend::Embedded { node, .. } => node
                .get_collection(collection)?
                .map(serde_json::to_value)
                .transpose()
                .context("serializing active collection version"),
            TxnBackend::Http {
                endpoint, client, ..
            } => {
                let url = format!("{}/collections/versions", graphql_api_base(endpoint)?);
                let versions: Value = client
                    .get(&url)
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                Ok(versions.as_array().and_then(|versions| {
                    versions
                        .iter()
                        .find(|version| {
                            version.get("Name").and_then(Value::as_str) == Some(collection)
                                && version
                                    .get("IsActive")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true)
                        })
                        .cloned()
                }))
            }
        }
    }

    pub(crate) async fn execute_local_response(
        &self,
        document: &str,
    ) -> Result<defra_node::QueryResponse> {
        let TxnBackend::Embedded {
            node,
            handle,
            identity,
        } = &self.backend
        else {
            anyhow::bail!("native transaction responses require embedded access");
        };
        let response = tokio::time::timeout(
            EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
            node.execute_request_in_txn(
                QueryRequest::new(document).with_identity(identity.clone()),
                handle,
            ),
        )
        .await
        .map_err(|_| {
            embedded_phase_timeout("execute", EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT)
        })?;
        if response.is_transaction_conflict() {
            return Err(retry::transaction_conflict(ConflictSource::StructuredCode));
        }
        if response.has_errors() {
            return Err(retry::transaction_storage_failure(anyhow::anyhow!(
                "graphql returned errors: {:?}",
                response.errors
            )));
        }
        if document.trim_start().starts_with("mutation") {
            let envelope = json!({"data": response.data.as_ref().unwrap_or(&Value::Null)});
            self.affected_documents
                .fetch_add(graphql::affected_documents(&envelope), Ordering::Relaxed);
        }
        Ok(response)
    }

    async fn commit_inner(&mut self) -> std::result::Result<(), CommitFailure> {
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
                    },
                    graphql::TxnCommitError::TransactionConsumed(error) => CommitFailure {
                        error,
                        cleanup: CommitCleanup::NotNeeded,
                    },
                }),
            TxnBackend::Embedded { node, handle, .. } => {
                match tokio::time::timeout(
                    EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
                    node.runner().commit_txn(handle),
                )
                .await
                {
                    Err(_) => Err(CommitFailure {
                        error: embedded_phase_timeout(
                            "commit",
                            EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
                        ),
                        cleanup: CommitCleanup::Required,
                    }),
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(error)) => {
                        let conflict = matches!(
                            &error,
                            query::TransactionError::Execution(message)
                                if retry::is_transaction_conflict_text(message)
                        );
                        if conflict {
                            Err(CommitFailure {
                                error: retry::transaction_conflict(ConflictSource::TypedError),
                                cleanup: CommitCleanup::NotNeeded,
                            })
                        } else {
                            Err(CommitFailure {
                                error: anyhow::anyhow!("commit_txn: {error}"),
                                cleanup: CommitCleanup::NotNeeded,
                            })
                        }
                    }
                }
            }
        }
    }

    async fn rollback_inner(&mut self) -> Result<()> {
        let result = match &self.backend {
            TxnBackend::Http {
                endpoint,
                id,
                client,
            } => graphql::txn_discard(endpoint, id, client).await,
            TxnBackend::Embedded { node, handle, .. } => tokio::time::timeout(
                EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT,
                node.runner().rollback_txn(handle),
            )
            .await
            .map_err(|_| embedded_phase_timeout("rollback", EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT))?
            .map_err(|error| anyhow::anyhow!("rollback_txn: {error}")),
        };
        if result.is_ok() {
            self.disarm_rollback();
        }
        result
    }

    fn disarm_rollback(&mut self) {
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

    fn affected_documents(&self) -> u64 {
        self.affected_documents.load(Ordering::Relaxed)
    }
}

fn backend_for_access(access: &ConfigAccess) -> WriteBackend {
    match access {
        ConfigAccess::Graphql(_) => WriteBackend::Http,
        ConfigAccess::Local(_) => WriteBackend::Embedded,
    }
}

fn auto_commit_telemetry(
    operation: WriteOperation,
    backend: WriteBackend,
) -> Result<AttemptTelemetry> {
    Ok(AttemptTelemetry {
        operation,
        backend,
        mode: WriteMode::AutoCommit,
        retry_owner: RetryOwner::DefraDb,
        ordinal: WriteAttemptOrdinal::new(1, 1)?,
        started: Instant::now(),
        finished: Cell::new(false),
        cancelled_rollback: RollbackStatus::NotNeeded,
        cancellation_rollback_scheduled: None,
        dispatch: tracing::dispatcher::get_default(Clone::clone),
    })
}

fn finish_auto_commit<T>(
    telemetry: &AttemptTelemetry,
    result: Result<T>,
    affected_documents: impl FnOnce(&T) -> u64,
) -> Result<T> {
    let affected_documents = result.as_ref().ok().map(affected_documents).or(Some(0));
    telemetry.record(AttemptReport {
        outcome: if result.is_ok() {
            WriteOutcome::Committed
        } else {
            WriteOutcome::Failed
        },
        conflict_source: ConflictSource::None,
        backoff: None,
        affected_documents,
        rollback: RollbackStatus::NotNeeded,
    });
    result
}

async fn receipt_with_retry<F, Fut>(receipt: &F) -> Result<bool>
where
    F: Fn() -> Fut + Send + Sync,
    Fut: Future<Output = Result<bool>> + Send,
{
    for retry_index in 0..=retry::RECEIPT_WRITE_MAX_RETRIES {
        match receipt().await {
            Ok(present) => return Ok(present),
            Err(error)
                if retry::is_retryable_ambiguous_write(&error)
                    && retry_index < retry::RECEIPT_WRITE_MAX_RETRIES =>
            {
                tokio::time::sleep(retry::receipt_write_backoff(retry_index)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded receipt loop returns on its final attempt")
}

struct AttemptReport {
    outcome: WriteOutcome,
    conflict_source: ConflictSource,
    backoff: Option<std::time::Duration>,
    affected_documents: Option<u64>,
    rollback: RollbackStatus,
}

struct AttemptTelemetry {
    operation: WriteOperation,
    backend: WriteBackend,
    mode: WriteMode,
    retry_owner: RetryOwner,
    ordinal: WriteAttemptOrdinal,
    started: Instant,
    finished: Cell<bool>,
    cancelled_rollback: RollbackStatus,
    cancellation_rollback_scheduled: Option<Arc<AtomicBool>>,
    dispatch: tracing::Dispatch,
}

impl AttemptTelemetry {
    fn record(&self, report: AttemptReport) {
        self.record_with_receipt(report, ReceiptRecovery::NotAttempted);
    }

    fn record_with_receipt(&self, report: AttemptReport, receipt_recovery: ReceiptRecovery) {
        if self.finished.replace(true) {
            return;
        }
        let event = WriteAttemptEvent {
            operation: self.operation,
            backend: self.backend,
            mode: self.mode,
            retry_owner: self.retry_owner,
            ordinal: self.ordinal,
            outcome: report.outcome,
            conflict_source: report.conflict_source,
            receipt_recovery,
            backoff: report.backoff,
            elapsed: self.started.elapsed(),
            affected_documents: report.affected_documents,
            rollback: report.rollback,
        };
        tracing::dispatcher::with_default(&self.dispatch, || {
            event.record();
        });
    }
}

impl Drop for AttemptTelemetry {
    fn drop(&mut self) {
        let rollback = if self
            .cancellation_rollback_scheduled
            .as_ref()
            .is_some_and(|scheduled| scheduled.load(Ordering::Acquire))
        {
            RollbackStatus::Scheduled
        } else {
            self.cancelled_rollback
        };
        self.record(AttemptReport {
            outcome: WriteOutcome::Cancelled,
            conflict_source: ConflictSource::None,
            backoff: None,
            affected_documents: Some(0),
            rollback,
        });
    }
}

struct TransactionAttemptFailure {
    error: anyhow::Error,
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

fn transact_owned<'a, T, F, B>(
    operation: &'static str,
    backend: WriteBackend,
    mode: TransactionMode,
    mut begin: B,
    callback: F,
) -> BoxFuture<'a, Result<TransactionOutcome<T>>>
where
    T: Send + 'a,
    F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T>> + Send + 'a,
    B: FnMut(Arc<AtomicBool>) -> BoxFuture<'a, Result<ConfigApplyTxn<'a>>> + Send + 'a,
{
    Box::pin(async move {
        let operation = WriteOperation::new(operation)?;
        let max_attempts = mode.max_attempts();
        for attempt in 1..=max_attempts {
            let ordinal = WriteAttemptOrdinal::new(attempt, max_attempts)?;
            let cancellation_rollback_scheduled = Arc::new(AtomicBool::new(false));
            let mut telemetry = AttemptTelemetry {
                operation,
                backend,
                mode: WriteMode::Transaction,
                retry_owner: mode.retry_owner(),
                ordinal,
                started: Instant::now(),
                finished: Cell::new(false),
                cancelled_rollback: RollbackStatus::NotNeeded,
                cancellation_rollback_scheduled: Some(Arc::clone(&cancellation_rollback_scheduled)),
                dispatch: tracing::dispatcher::get_default(Clone::clone),
            };
            let mut txn = match begin(cancellation_rollback_scheduled).await {
                Ok(txn) => txn,
                Err(_error) if mode.retries_generic_errors() && attempt < max_attempts => {
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
                    return Err(error);
                }
            };
            telemetry.cancelled_rollback = RollbackStatus::Scheduled;
            let callback_result = match backend {
                WriteBackend::Embedded => {
                    tokio::time::timeout(EMBEDDED_TRANSACTION_CALLBACK_TIMEOUT, callback(&txn))
                        .await
                        .map_err(|_| {
                            embedded_phase_timeout(
                                "callback",
                                EMBEDDED_TRANSACTION_CALLBACK_TIMEOUT,
                            )
                        })
                        .and_then(|result| result)
                }
                WriteBackend::Http => callback(&txn).await,
            };
            let attempt_result: Result<(), TransactionAttemptFailure> = match callback_result {
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
                        Err(CommitFailure { error, cleanup }) => {
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
                            Err(TransactionAttemptFailure {
                                error,
                                rollback,
                                retry_generic: mode.retries_generic_errors(),
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
                rollback,
                retry_generic,
            } = failure;
            let conflict_source = retry::classified_transaction_conflict(&error);
            let source = conflict_source.unwrap_or(ConflictSource::None);
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
                    telemetry.record(AttemptReport {
                        outcome: WriteOutcome::Retrying,
                        conflict_source: source,
                        backoff: Some(backoff),
                        affected_documents: Some(0),
                        rollback,
                    });
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

fn expect_committed<T>(outcome: TransactionOutcome<T>) -> T {
    match outcome {
        TransactionOutcome::Committed(value) => value,
        TransactionOutcome::ConflictObserved => {
            unreachable!("only observing transactions expose observed conflicts")
        }
    }
}

impl ConfigAccess {
    pub(crate) async fn execute_local_response_with_retry(
        node: &EmbeddedNode,
        document: &str,
        retry_policy: ExecuteRetryPolicy,
    ) -> defra_node::QueryResponse {
        node.execute_with_retry(document, retry_policy).await
    }

    async fn begin_apply_txn(
        &self,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
    ) -> Result<ConfigApplyTxn<'_>> {
        match self {
            Self::Graphql(endpoint) => {
                ConfigApplyTxn::begin_http(endpoint, cancellation_rollback_scheduled).await
            }
            Self::Local(node) => {
                ConfigApplyTxn::begin_local_owned(node, None, cancellation_rollback_scheduled).await
            }
        }
    }

    /// Commit one mutation through DefraDB's native auto-commit owner.
    pub async fn write(&self, operation: &'static str, mutation: &str) -> Result<Value> {
        graphql::ensure_mutation_document(mutation)?;
        let operation = WriteOperation::new(operation)?;
        let backend = backend_for_access(self);
        let telemetry = auto_commit_telemetry(operation, backend)?;
        let result = match self {
            Self::Graphql(endpoint) => graphql::auto_commit(endpoint, mutation).await,
            Self::Local(node) => Self::write_local_inner(node, operation, mutation).await,
        };
        finish_auto_commit(&telemetry, result, graphql::affected_documents)
    }

    /// Commit a stable-id HTTP mutation with bounded ambiguous-result recovery.
    ///
    /// Each mutation is posted exactly once. A retryable response or transport
    /// failure is ambiguous, so the owner checks the caller's stable receipt
    /// before it may repost. Receipt reads are retried separately; a mutation
    /// is replayed only after a successful receipt read confirms absence.
    /// Embedded access has no ambiguous HTTP response and uses [`Self::write`].
    pub async fn write_with_receipt<F, Fut>(
        &self,
        operation: &'static str,
        mutation: &str,
        receipt: F,
    ) -> Result<()>
    where
        F: Fn() -> Fut + Send + Sync,
        Fut: Future<Output = Result<bool>> + Send,
    {
        graphql::ensure_mutation_document(mutation)?;
        let Self::Graphql(endpoint) = self else {
            self.write(operation, mutation).await?;
            return Ok(());
        };
        let operation = WriteOperation::new(operation)?;
        let max_attempts = retry::RECEIPT_WRITE_MAX_RETRIES + 1;

        for attempt in 1..=max_attempts {
            let telemetry = AttemptTelemetry {
                operation,
                backend: WriteBackend::Http,
                mode: WriteMode::AutoCommit,
                retry_owner: RetryOwner::GentsReceipt,
                ordinal: WriteAttemptOrdinal::new(attempt, max_attempts)?,
                started: Instant::now(),
                finished: Cell::new(false),
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
                    return Err(error);
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
                    return Err(error);
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
                    return Err(receipt_error).context("checking stable write receipt");
                }
            }
        }
        unreachable!("bounded receipt write loop returns on its final attempt")
    }

    /// Borrowed embedded-node auto-commit seam for runtime and desktop code.
    pub async fn write_local(
        node: &EmbeddedNode,
        operation: &'static str,
        mutation: &str,
    ) -> Result<Value> {
        graphql::ensure_mutation_document(mutation)?;
        let operation = WriteOperation::new(operation)?;
        let telemetry = auto_commit_telemetry(operation, WriteBackend::Embedded)?;
        let result = Self::write_local_inner(node, operation, mutation).await;
        finish_auto_commit(&telemetry, result, graphql::affected_documents)
    }

    /// Embedded auto-commit seam for callers that need DefraDB's typed
    /// response metadata.
    pub(crate) async fn write_local_response(
        node: &EmbeddedNode,
        operation: &'static str,
        mutation: &str,
    ) -> Result<defra_node::QueryResponse> {
        graphql::ensure_mutation_document(mutation)?;
        let operation = WriteOperation::new(operation)?;
        let telemetry = auto_commit_telemetry(operation, WriteBackend::Embedded)?;
        let result = Self::write_local_response_inner(node, operation, mutation).await;
        finish_auto_commit(&telemetry, result, |response| {
            crate::graphql::mutation_affected_documents(response) as u64
        })
    }

    /// Replay an embedded update through the canonical transaction owner.
    /// Creates need their own stable identity and durable reconciliation.
    pub(crate) async fn write_local_idempotent_update_response<'a>(
        node: &'a EmbeddedNode,
        operation: &'static str,
        mutation: &'a str,
    ) -> Result<defra_node::QueryResponse> {
        graphql::ensure_mutation_document(mutation)?;
        anyhow::ensure!(
            !mutation.contains("create_"),
            "idempotent update seam does not accept creates"
        );
        Self::transact_local_idempotent(
            node,
            None,
            IdempotentTransactionRetry::Standard,
            operation,
            move |txn| Box::pin(async move { txn.execute_local_response(mutation).await }),
        )
        .await
    }

    async fn write_local_inner(
        node: &EmbeddedNode,
        operation: WriteOperation,
        mutation: &str,
    ) -> Result<Value> {
        let response = Self::write_local_response_inner(node, operation, mutation).await?;
        Ok(json!({"data": response.data.unwrap_or(Value::Null)}))
    }

    async fn write_local_response_inner(
        node: &EmbeddedNode,
        operation: WriteOperation,
        mutation: &str,
    ) -> Result<defra_node::QueryResponse> {
        let retry_policy = ExecuteRetryPolicy::new(
            retry::TRANSACT_CONFLICT_MAX_RETRIES,
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(800),
        );
        Self::execute_local_mutation(node, operation, mutation, retry_policy).await
    }

    async fn execute_local_mutation(
        node: &EmbeddedNode,
        operation: WriteOperation,
        mutation: &str,
        retry_policy: ExecuteRetryPolicy,
    ) -> Result<defra_node::QueryResponse> {
        let gate = mutation_write_gate(node);
        let _write_guard =
            tokio::time::timeout(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT, gate.lock())
                .await
                .map_err(|_| {
                    embedded_phase_timeout(
                        "write-gate acquisition",
                        EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
                    )
                })?;
        let response = tokio::time::timeout(
            EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
            node.execute_with_retry(mutation, retry_policy),
        )
        .await
        .map_err(|_| {
            embedded_phase_timeout("auto-commit", EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT)
        })?;
        crate::graphql::ensure_no_errors(&response, operation.as_str())?;
        Ok(response)
    }

    pub async fn transact<'a, T, F>(&'a self, operation: &'static str, callback: F) -> Result<T>
    where
        T: Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T>> + Send + 'a,
    {
        transact_owned(
            operation,
            backend_for_access(self),
            TransactionMode::ConflictRetry,
            move |rollback| Box::pin(async move { self.begin_apply_txn(rollback).await }),
            callback,
        )
        .await
        .map(expect_committed)
    }

    /// Replay a stable-key transaction after any callback or commit error.
    ///
    /// Callers must make the callback idempotent across an ambiguous commit:
    /// creates need stable unique identities and retries must reconcile an
    /// already-durable winner before attempting another create.
    pub async fn transact_idempotent<'a, T, F>(
        &'a self,
        retry_policy: IdempotentTransactionRetry,
        operation: &'static str,
        callback: F,
    ) -> Result<T>
    where
        T: Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T>> + Send + 'a,
    {
        transact_owned(
            operation,
            backend_for_access(self),
            TransactionMode::Idempotent(retry_policy),
            move |rollback| Box::pin(async move { self.begin_apply_txn(rollback).await }),
            callback,
        )
        .await
        .map(expect_committed)
    }

    pub(crate) async fn transact_observing_conflict<'a, T, F>(
        &'a self,
        operation: &'static str,
        callback: F,
    ) -> Result<TransactionOutcome<T>>
    where
        T: Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T>> + Send + 'a,
    {
        transact_owned(
            operation,
            backend_for_access(self),
            TransactionMode::ObserveConflict,
            move |rollback| Box::pin(async move { self.begin_apply_txn(rollback).await }),
            callback,
        )
        .await
    }

    /// Borrowed embedded-node transaction seam preserving the supplied ACP DID.
    pub async fn transact_local<'a, T, F>(
        node: &'a EmbeddedNode,
        identity: Option<Did>,
        operation: &'static str,
        callback: F,
    ) -> Result<T>
    where
        T: Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T>> + Send + 'a,
    {
        transact_owned(
            operation,
            WriteBackend::Embedded,
            TransactionMode::ConflictRetry,
            move |rollback| {
                let identity = identity.clone();
                Box::pin(async move {
                    ConfigApplyTxn::begin_local_owned(node, identity, rollback).await
                })
            },
            callback,
        )
        .await
        .map(expect_committed)
    }

    /// Borrowed embedded variant of [`Self::transact_idempotent`].
    pub async fn transact_local_idempotent<'a, T, F>(
        node: &'a EmbeddedNode,
        identity: Option<Did>,
        retry_policy: IdempotentTransactionRetry,
        operation: &'static str,
        callback: F,
    ) -> Result<T>
    where
        T: Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T>> + Send + 'a,
    {
        transact_owned(
            operation,
            WriteBackend::Embedded,
            TransactionMode::Idempotent(retry_policy),
            move |rollback| {
                let identity = identity.clone();
                Box::pin(async move {
                    ConfigApplyTxn::begin_local_owned(node, identity, rollback).await
                })
            },
            callback,
        )
        .await
        .map(expect_committed)
    }

    pub(crate) async fn transact_local_observing_conflict<'a, T, F>(
        node: &'a EmbeddedNode,
        identity: Option<Did>,
        operation: &'static str,
        callback: F,
    ) -> Result<TransactionOutcome<T>>
    where
        T: Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T>> + Send + 'a,
    {
        transact_owned(
            operation,
            WriteBackend::Embedded,
            TransactionMode::ObserveConflict,
            move |rollback| {
                let identity = identity.clone();
                Box::pin(async move {
                    ConfigApplyTxn::begin_local_owned(node, identity, rollback).await
                })
            },
            callback,
        )
        .await
    }
}

#[cfg(test)]
#[path = "txn_owner_tests.rs"]
mod owner_tests;

#[cfg(test)]
#[path = "receipt_owner_tests.rs"]
mod receipt_owner_tests;

#[cfg(test)]
mod variable_tests {
    use super::*;

    #[tokio::test]
    async fn whole_input_variables_update_upsert_and_rollback_losslessly() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_schema("type MutationVariableProbe { key: String label: String enabled: Boolean count: Int tags: [String] payload: JSON }").await.unwrap();
        let payload = json!({"a-b": [], "nested": [{"": {}, "雪": [null, []]}]});
        for (query, input) in [
            (
                "mutation($input: MutationVariableProbeMutationInputArg!) { create_MutationVariableProbe(input:$input) {_docID} }",
                json!({"key":"one","label":"before","payload":null}),
            ),
            (
                "mutation($input: MutationVariableProbeMutationInputArg!) { update_MutationVariableProbe(filter:{key:{_eq:\"one\"}},input:$input) {_docID} }",
                json!({"label":"updated","enabled":true,"count":7,"tags":["tag"],"payload":payload}),
            ),
            (
                "mutation($input: MutationVariableProbeMutationInputArg!) { upsert_MutationVariableProbe(filter:{key:{_eq:\"one\"}},add:$input,update:$input) {_docID} }",
                json!({"label":"upserted","tags":null,"payload":payload}),
            ),
        ] {
            let vars = json!({"input": input});
            ConfigAccess::transact_local(&node, None, "variable_probe", |txn| {
                let vars = vars.clone();
                Box::pin(async move { txn.execute_with_variables(query, &vars).await.map(|_| ()) })
            })
            .await
            .unwrap();
        }
        let read = node
            .execute("{MutationVariableProbe(filter:{key:{_eq:\"one\"}}) {key label enabled count tags payload}}")
            .await;
        assert!(!read.has_errors(), "{:?}", read.errors);
        let before = read.data.unwrap()["MutationVariableProbe"][0].clone();
        assert_eq!(before["label"], "upserted");
        assert_eq!(before["enabled"], true);
        assert_eq!(before["count"], 7);
        assert!(before["tags"].is_null());
        assert_eq!(before["payload"], payload);
        ConfigAccess::transact_local(&node, None, "variable_upsert_insert", |txn| Box::pin(async move {
            txn.execute_with_variables("mutation($input: MutationVariableProbeMutationInputArg!) { upsert_MutationVariableProbe(filter:{key:{_eq:\"two\"}},add:$input,update:$input) {_docID} }", &json!({"input":{"key":"two","label":"inserted","payload":{"empty":[]}}})).await.map(|_| ())
        })).await.unwrap();
        let inserted = node
            .execute("{MutationVariableProbe(filter:{key:{_eq:\"two\"}}) {label payload}}")
            .await;
        assert!(!inserted.has_errors());
        assert_eq!(
            inserted.data.unwrap()["MutationVariableProbe"],
            json!([{"label":"inserted","payload":{"empty":[]}}])
        );
        let aborted: Result<()> = ConfigAccess::transact_local(&node, None, "variable_probe_rollback", |txn| Box::pin(async move {
            txn.execute_with_variables("mutation($input: MutationVariableProbeMutationInputArg!) { update_MutationVariableProbe(filter:{key:{_eq:\"one\"}},input:$input) {_docID} }", &json!({"input":{"label":"must rollback","payload":{"lost":[]}}})).await?;
            anyhow::bail!("intentional rollback")
        })).await;
        assert!(aborted.is_err());
        let read = node
            .execute("{MutationVariableProbe(filter:{key:{_eq:\"one\"}}) {key label enabled count tags payload}}")
            .await;
        assert!(!read.has_errors());
        assert_eq!(read.data.unwrap()["MutationVariableProbe"][0], before);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn embedded_transaction_preserves_json_variables_and_rollback() {
        let node = EmbeddedNode::builder().build().await.unwrap();
        node.add_schema("type TransactionJsonProbe { key: String payload: JSON }")
            .await
            .unwrap();
        let variables = json!({"payload": {"a-b": [], "nested": [{"": {}, "雪": [null, []]}]}});
        let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
        txn.execute_with_variables("mutation($payload: JSON) { create_TransactionJsonProbe(input: {key: \"rolled-back\", payload: $payload}) {_docID} }", &variables).await.unwrap();
        let read = txn
            .execute("{TransactionJsonProbe {payload}}")
            .await
            .unwrap();
        assert_eq!(
            read["data"]["TransactionJsonProbe"][0]["payload"],
            variables["payload"]
        );
        txn.discard().await.unwrap();
        let read = node.execute("{TransactionJsonProbe {payload}}").await;
        assert!(!read.has_errors());
        assert!(read.data.unwrap()["TransactionJsonProbe"]
            .as_array()
            .unwrap()
            .is_empty());
        node.shutdown().await;
    }
}
