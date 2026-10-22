use std::collections::HashMap;
use std::future::Future;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use defra_node::{EmbeddedNode, QueryRequest};
use futures::future::BoxFuture;
use identity::Did;
use query::TransactionHandle;
use serde_json::{json, Value};
use tracing::{instrument::WithSubscriber, Instrument};

use super::graphql;
use super::retry;
use super::write_telemetry::{
    record_slow_native_phase, ConflictSource, ReceiptRecovery, RetryOwner, RollbackStatus,
    WriteAttemptEvent, WriteAttemptOrdinal, WriteBackend, WriteMode, WriteOperation, WriteOutcome,
};
use super::{graphql_api_base, ConfigAccess, GraphqlEndpoint};

enum TxnBackend<'a> {
    Http {
        endpoint: &'a GraphqlEndpoint,
        id: String,
        client: reqwest::Client,
    },
    Embedded {
        node: Arc<EmbeddedNode>,
        handle: TransactionHandle,
        identity: Option<Did>,
        native_operations: Arc<NativeOperationTracker>,
    },
}

struct MutationWriteGate {
    permits: Arc<tokio::sync::Semaphore>,
    owners: StdMutex<HashMap<u64, MutationWriteOwner>>,
    next_lease_id: AtomicU64,
    pending_acquires: StdMutex<HashMap<u64, PendingWriteAcquire>>,
    next_pending_acquire_id: AtomicU64,
    store_claim: StdMutex<Option<Arc<StoreClaim>>>,
}

const MUTATION_WRITE_GATE_CAPACITY: usize = 64;

struct StoreClaim {
    node: Arc<EmbeddedNode>,
    _lock: crate::home::StoreLock,
}

/// Embedded access plus a node-scoped store claim held across Gents-owned native operations.
#[derive(Clone)]
pub struct LocalConfigAccess {
    node: Arc<EmbeddedNode>,
    gate: Arc<MutationWriteGate>,
}

impl LocalConfigAccess {
    pub(crate) fn new(node: Arc<EmbeddedNode>) -> Self {
        let gate = mutation_write_gate(&node);
        Self { node, gate }
    }

    pub(crate) fn claimed(node: Arc<EmbeddedNode>, lock: crate::home::StoreLock) -> Result<Self> {
        let gate = mutation_write_gate(&node);
        gate.attach_store_claim(Arc::clone(&node), lock)?;
        Ok(Self { node, gate })
    }

    pub(crate) fn node(&self) -> &Arc<EmbeddedNode> {
        &self.node
    }

    pub(crate) async fn run_owned<T, F, Fut>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<EmbeddedNode>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let node = Arc::clone(&self.node);
        let gate = Arc::clone(&self.gate);
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        let span = tracing::Span::current();
        #[cfg(test)]
        let local_operation_pause = LOCAL_OPERATION_PAUSE.try_with(Arc::clone).ok();
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::spawn(
            async move {
                #[cfg(test)]
                if let Some(pause) = local_operation_pause.as_ref() {
                    pause.reached.notify_one();
                    pause.release.notified().await;
                }
                let result = operation(Arc::clone(&node)).await;
                drop(node);
                drop(gate);
                #[cfg(test)]
                if let Some(completed) = local_operation_pause
                    .as_ref()
                    .and_then(|pause| pause.completed.as_ref())
                {
                    completed.notify_one();
                }
                let _ = send.send(result);
            }
            .with_subscriber(dispatcher)
            .instrument(span),
        );
        receive
            .await
            .context("owned embedded operation ended without a result")?
    }
}

#[derive(Clone, Copy)]
struct MutationWriteOwner {
    operation: WriteOperation,
    acquired_at: Instant,
}

#[derive(Clone)]
struct PendingWriteAcquire {
    operation: WriteOperation,
    task_id: Option<String>,
    span_name: Option<&'static str>,
    first_queued_at: Instant,
    last_polled_at: Instant,
}

struct PendingWriteAcquireRegistration {
    gate: Arc<MutationWriteGate>,
    id: u64,
    operation: WriteOperation,
    registered: bool,
}

impl PendingWriteAcquireRegistration {
    fn record_pending_poll(&mut self) {
        let now = Instant::now();
        let mut pending = self
            .gate
            .pending_acquires
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.registered = true;
        pending
            .entry(self.id)
            .and_modify(|entry| entry.last_polled_at = now)
            .or_insert_with(|| PendingWriteAcquire {
                operation: self.operation,
                task_id: tokio::task::try_id().map(|id| id.to_string()),
                span_name: tracing::Span::current()
                    .metadata()
                    .map(|metadata| metadata.name()),
                first_queued_at: now,
                last_polled_at: now,
            });
    }
}

impl Drop for PendingWriteAcquireRegistration {
    fn drop(&mut self) {
        if self.registered {
            self.gate
                .pending_acquires
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&self.id);
        }
    }
}

struct OldestPendingWriteAcquire {
    count: usize,
    operation: WriteOperation,
    task_id: Option<String>,
    span_name: Option<&'static str>,
    wait_age: Duration,
    last_poll_age: Duration,
}

struct MutationWriteGuard {
    gate: Arc<MutationWriteGate>,
    _permit: tokio::sync::OwnedSemaphorePermit,
    lease_id: u64,
}

#[cfg(test)]
pub(crate) struct MutationWriteGateFillers {
    _guards: Vec<MutationWriteGuard>,
}

impl Drop for MutationWriteGuard {
    fn drop(&mut self) {
        self.gate
            .owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.lease_id);
    }
}

impl MutationWriteGate {
    fn new() -> Self {
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(MUTATION_WRITE_GATE_CAPACITY)),
            owners: StdMutex::new(HashMap::new()),
            next_lease_id: AtomicU64::new(1),
            pending_acquires: StdMutex::new(HashMap::new()),
            next_pending_acquire_id: AtomicU64::new(1),
            store_claim: StdMutex::new(None),
        }
    }

    fn attach_store_claim(
        &self,
        node: Arc<EmbeddedNode>,
        lock: crate::home::StoreLock,
    ) -> Result<()> {
        let mut claim = self
            .store_claim
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        anyhow::ensure!(
            claim.is_none(),
            "embedded node already has a live store claim"
        );
        *claim = Some(Arc::new(StoreClaim { node, _lock: lock }));
        Ok(())
    }

    fn store_claim(&self) -> Option<Arc<StoreClaim>> {
        self.store_claim
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn oldest_pending_acquire(&self) -> Option<OldestPendingWriteAcquire> {
        let now = Instant::now();
        let pending = self
            .pending_acquires
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let oldest = pending.values().min_by_key(|entry| entry.first_queued_at)?;
        Some(OldestPendingWriteAcquire {
            count: pending.len(),
            operation: oldest.operation,
            task_id: oldest.task_id.clone(),
            span_name: oldest.span_name,
            wait_age: now.saturating_duration_since(oldest.first_queued_at),
            last_poll_age: now.saturating_duration_since(oldest.last_polled_at),
        })
    }

    async fn acquire(self: &Arc<Self>, operation: WriteOperation) -> Result<MutationWriteGuard> {
        ensure_not_reentrant_embedded_write(operation)?;
        let wait_started = Instant::now();
        let acquire = Arc::clone(&self.permits).acquire_owned();
        tokio::pin!(acquire);
        let mut registration = None;
        #[cfg(test)]
        let observation = SUCCESSFUL_MUTATION_FAULT
            .try_with(|fault| fault.write_gate_observation.clone())
            .ok()
            .flatten();
        let acquire_future = std::future::poll_fn(|cx| match acquire.as_mut().poll(cx) {
            std::task::Poll::Pending => {
                if registration.is_none() {
                    let id = self.next_pending_acquire_id.fetch_add(1, Ordering::Relaxed);
                    registration = Some(PendingWriteAcquireRegistration {
                        gate: Arc::clone(self),
                        id,
                        operation,
                        registered: false,
                    });
                }
                registration
                    .as_mut()
                    .expect("pending acquisition is registered")
                    .record_pending_poll();
                #[cfg(test)]
                if let Some(observation) = observation.as_ref() {
                    if !observation.queued_fired.swap(true, Ordering::Relaxed) {
                        observation.queued.notify_one();
                    }
                }
                std::task::Poll::Pending
            }
            std::task::Poll::Ready(permit) => {
                registration.take();
                #[cfg(test)]
                if permit.is_ok() {
                    if let Some(observation) = observation.as_ref() {
                        observation.acquired.store(true, Ordering::Release);
                    }
                }
                std::task::Poll::Ready(permit)
            }
        });
        let guard = match tokio::time::timeout(
            EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
            acquire_future,
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(error)) => {
                return Err(anyhow::anyhow!("embedded write gate is closed: {error}"));
            }
            Err(_) => {
                registration.take();
                let (active_owner_count, owner) = {
                    let owners = self
                        .owners
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    (
                        owners.len(),
                        owners
                            .values()
                            .copied()
                            .min_by_key(|owner| owner.acquired_at),
                    )
                };
                let (owner_operation, owner_held_ms) = owner.map_or(("unknown", 0), |owner| {
                    (
                        owner.operation.as_str(),
                        u64::try_from(owner.acquired_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                    )
                });
                let pending = self.oldest_pending_acquire();
                let (
                    pending_count,
                    oldest_wait_operation,
                    oldest_wait_age_ms,
                    oldest_wait_last_poll_age_ms,
                    oldest_wait_task_id,
                    oldest_wait_span,
                ) = pending
                    .as_ref()
                    .map_or((0, "none", 0, 0, None, None), |pending| {
                        (
                            pending.count,
                            pending.operation.as_str(),
                            u64::try_from(pending.wait_age.as_millis()).unwrap_or(u64::MAX),
                            u64::try_from(pending.last_poll_age.as_millis()).unwrap_or(u64::MAX),
                            pending.task_id.as_deref(),
                            pending.span_name,
                        )
                    });
                let owner_status = if active_owner_count > 0 {
                    "tracked_owners"
                } else if pending_count > 0 {
                    "no_tracked_owner; semaphore capacity may be reserved for a pending acquire"
                } else {
                    "no_tracked_owner_or_pending_acquire"
                };
                tracing::error!(
                    phase = "write-gate acquisition",
                    waiting_operation = operation.as_str(),
                    owner_operation,
                    owner_held_ms,
                    active_owner_count,
                    owner_status,
                    pending_acquire_count = pending_count,
                    oldest_wait_operation,
                    oldest_wait_age_ms,
                    oldest_wait_last_poll_age_ms,
                    oldest_wait_task_id = oldest_wait_task_id.unwrap_or("unknown"),
                    oldest_wait_span = oldest_wait_span.unwrap_or("unknown"),
                    timeout_ms =
                        u64::try_from(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT.as_millis())
                            .unwrap_or(u64::MAX),
                    "embedded DefraDB canonical write admission timed out"
                );
                return Err(retry::transaction_storage_failure(anyhow::anyhow!(
                    "embedded transaction write admission for {} timed out after {:?}; {} active owners, longest-held owner {} held for {}ms",
                    operation.as_str(),
                    EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
                    active_owner_count,
                    owner_operation,
                    owner_held_ms,
                )));
            }
        };
        record_slow_native_phase(
            Some(operation.as_str()),
            "gate_wait",
            wait_started.elapsed(),
        );
        let lease_id = self.next_lease_id.fetch_add(1, Ordering::Relaxed);
        self.owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                lease_id,
                MutationWriteOwner {
                    operation,
                    acquired_at: Instant::now(),
                },
            );
        Ok(MutationWriteGuard {
            gate: Arc::clone(self),
            _permit: guard,
            lease_id,
        })
    }
}

tokio::task_local! {
    static ACTIVE_EMBEDDED_TRANSACTION: &'static str;
}

#[derive(Default)]
struct NativeOperationState {
    in_flight: usize,
    abandoned: bool,
}

#[derive(Default)]
struct NativeOperationTracker {
    state: StdMutex<NativeOperationState>,
    idle: tokio::sync::Notify,
}

impl NativeOperationTracker {
    fn start(self: &Arc<Self>) -> Result<NativeOperationPermit> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.abandoned {
            return Err(native_operation_abandoned(
                "embedded transaction is poisoned by an abandoned native operation",
            ));
        }
        state.in_flight += 1;
        drop(state);
        Ok(NativeOperationPermit(Arc::clone(self)))
    }

    fn abandon(&self) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .abandoned = true;
    }

    fn is_abandoned(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .abandoned
    }

    async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let idle = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .in_flight
                == 0;
            if idle {
                return;
            }
            notified.await;
        }
    }
}

struct NativeOperationPermit(Arc<NativeOperationTracker>);

impl Drop for NativeOperationPermit {
    fn drop(&mut self) {
        let became_idle = {
            let mut state = self
                .0
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.in_flight -= 1;
            state.in_flight == 0
        };
        if became_idle {
            self.0.idle.notify_waiters();
        }
    }
}

struct CallerOperationGuard {
    tracker: Arc<NativeOperationTracker>,
    completed: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("native operation remains owned after caller abandonment")]
struct NativeOperationAbandoned;

fn native_operation_abandoned(message: impl Into<String>) -> anyhow::Error {
    retry::transaction_storage_failure(
        anyhow::Error::new(NativeOperationAbandoned).context(message.into()),
    )
}

fn is_native_operation_abandoned(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<NativeOperationAbandoned>().is_some())
}

impl CallerOperationGuard {
    fn new(tracker: Arc<NativeOperationTracker>) -> Self {
        Self {
            tracker,
            completed: false,
        }
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for CallerOperationGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.tracker.abandon();
        }
    }
}

async fn execute_owned_native(
    node: Arc<EmbeddedNode>,
    request: QueryRequest,
    handle: TransactionHandle,
    tracker: Arc<NativeOperationTracker>,
    operation: Option<&'static str>,
) -> Result<defra_node::QueryResponse> {
    let permit = tracker.start()?;
    let mut caller = CallerOperationGuard::new(Arc::clone(&tracker));
    let dispatcher = tracing::dispatcher::get_default(Clone::clone);
    let span = tracing::Span::current();
    let (send, receive) = tokio::sync::oneshot::channel();
    #[cfg(test)]
    let native_pause = SUCCESSFUL_MUTATION_FAULT
        .try_with(|fault| fault.native_pause.clone())
        .ok()
        .flatten();
    let dispatched_at = Instant::now();
    tokio::spawn(
        async move {
            record_slow_native_phase(operation, "execute-dispatch", dispatched_at.elapsed());
            #[cfg(test)]
            if let Some((reached, release)) = native_pause {
                reached.notify_one();
                release.notified().await;
            }
            let native_started = Instant::now();
            let result = match operation {
                Some(operation) => {
                    ACTIVE_EMBEDDED_TRANSACTION
                        .scope(operation, node.execute_request_in_txn(request, &handle))
                        .await
                }
                None => node.execute_request_in_txn(request, &handle).await,
            };
            record_slow_native_phase(operation, "execute-native-await", native_started.elapsed());
            drop(permit);
            let _ = send.send(result);
        }
        .with_subscriber(dispatcher)
        .instrument(span),
    );
    let result = tokio::time::timeout(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT, receive)
        .await
        .map_err(|_| {
            native_operation_abandoned(format!(
                "embedded execute caller timed out after {:?}; native operation remains owned",
                EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT
            ))
        })?
        .map_err(|_| {
            native_operation_abandoned("native signed execution task ended without a response")
        })?;
    caller.complete();
    Ok(result)
}

#[cfg(test)]
struct SuccessfulMutationFault {
    fail_after: Option<usize>,
    count: AtomicUsize,
    lose_receipt: bool,
    receipt_operation: Option<&'static str>,
    receipt_fired: AtomicBool,
    pause: Option<SuccessfulMutationPause>,
    write_gate_observation: Option<Arc<WriteGateObservation>>,
    read_storage_failure: Option<&'static str>,
    read_pause: Option<(&'static str, Arc<tokio::sync::Notify>)>,
    native_pause: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    fail_rollback: bool,
    retry_backoff_reached: Option<Arc<tokio::sync::Notify>>,
    cleanup_scheduled: Option<Arc<AtomicBool>>,
}

#[cfg(test)]
struct LocalOperationPause {
    reached: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    completed: Option<Arc<tokio::sync::Notify>>,
}

#[cfg(test)]
tokio::task_local! {
    static LOCAL_OPERATION_PAUSE: Arc<LocalOperationPause>;
}

#[cfg(test)]
struct SuccessfulMutationPause {
    after: usize,
    reached: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    fired: AtomicBool,
}

#[cfg(test)]
struct WriteGateObservation {
    queued: Arc<tokio::sync::Notify>,
    acquired: Arc<AtomicBool>,
    queued_fired: AtomicBool,
}

#[cfg(test)]
tokio::task_local! {
    static SUCCESSFUL_MUTATION_FAULT: Arc<SuccessfulMutationFault>;
}

#[cfg(test)]
async fn after_successful_mutation_for_test() -> Result<()> {
    let Ok(fault) = SUCCESSFUL_MUTATION_FAULT.try_with(Arc::clone) else {
        return Ok(());
    };
    let count = fault.count.fetch_add(1, Ordering::Relaxed) + 1;
    anyhow::ensure!(
        fault.fail_after != Some(count),
        "injected failure after successful transaction mutation {count}"
    );
    if let Some(pause) = fault.pause.as_ref() {
        if pause.after == count && !pause.fired.swap(true, Ordering::Relaxed) {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }
    Ok(())
}

#[cfg(test)]
fn fail_rollback_for_test() -> bool {
    SUCCESSFUL_MUTATION_FAULT
        .try_with(|fault| fault.fail_rollback)
        .unwrap_or(false)
}

#[cfg(test)]
fn retry_backoff_reached_for_test() {
    let _ = SUCCESSFUL_MUTATION_FAULT.try_with(|fault| {
        if let Some(reached) = fault.retry_backoff_reached.as_ref() {
            reached.notify_one();
        }
    });
}

#[cfg(test)]
fn mark_cleanup_scheduled_for_test() {
    let _ = SUCCESSFUL_MUTATION_FAULT.try_with(|fault| {
        if let Some(scheduled) = fault.cleanup_scheduled.as_ref() {
            scheduled.store(true, Ordering::Release);
        }
    });
}

#[cfg(test)]
fn post_commit_receipt_loss_for_test(operation: Option<&str>) -> bool {
    SUCCESSFUL_MUTATION_FAULT
        .try_with(|fault| {
            if !fault.lose_receipt
                || fault
                    .receipt_operation
                    .is_some_and(|target| Some(target) != operation)
            {
                return false;
            }
            !fault.receipt_fired.swap(true, Ordering::Relaxed)
        })
        .unwrap_or(false)
}

#[derive(Debug, thiserror::Error)]
#[error(
    "embedded canonical write {waiting_operation} is nested inside transaction {owner_operation}; use the supplied transaction owner"
)]
pub(crate) struct ReentrantEmbeddedWrite {
    waiting_operation: &'static str,
    owner_operation: &'static str,
}

fn ensure_not_reentrant_embedded_write(operation: WriteOperation) -> Result<()> {
    if let Ok(owner_operation) = ACTIVE_EMBEDDED_TRANSACTION.try_with(|owner| *owner) {
        tracing::error!(
            waiting_operation = operation.as_str(),
            owner_operation,
            "re-entrant embedded canonical write rejected"
        );
        return Err(ReentrantEmbeddedWrite {
            waiting_operation: operation.as_str(),
            owner_operation,
        }
        .into());
    }
    Ok(())
}

// Bound every phase retaining the process-wide mutation gate so one wedged
// transaction cannot stop response progress, recovery, and hydration.
pub(crate) const EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT: Duration = Duration::from_secs(60);
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

async fn cleanup_while_holding_write_gate<G, F, T>(
    write_guard: Option<G>,
    cleanup: F,
) -> std::result::Result<T, tokio::time::error::Elapsed>
where
    F: Future<Output = T>,
{
    let _write_guard = write_guard;
    tokio::time::timeout(EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT, cleanup).await
}

fn mutation_write_gates() -> &'static StdMutex<HashMap<usize, Weak<MutationWriteGate>>> {
    static GATES: OnceLock<StdMutex<HashMap<usize, Weak<MutationWriteGate>>>> = OnceLock::new();
    GATES.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn mutation_write_gate(node: &EmbeddedNode) -> Arc<MutationWriteGate> {
    let node_key = node as *const EmbeddedNode as usize;
    let mut gates = mutation_write_gates()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(gate) = gates.get(&node_key).and_then(Weak::upgrade) {
        return gate;
    }

    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(MutationWriteGate::new());
    gates.insert(node_key, Arc::downgrade(&gate));
    gate
}

fn existing_mutation_write_gate(node: &EmbeddedNode) -> Option<Arc<MutationWriteGate>> {
    let node_key = node as *const EmbeddedNode as usize;
    mutation_write_gates()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&node_key)
        .and_then(Weak::upgrade)
}

// Cleanup owners may drop on threads without a Tokio context, so each retains
// the runtime that created it.
enum RollbackOnDrop {
    Http {
        runtime: tokio::runtime::Handle,
        endpoint: GraphqlEndpoint,
        id: String,
        client: reqwest::Client,
        armed: bool,
    },
    Embedded {
        runtime: tokio::runtime::Handle,
        node: Arc<EmbeddedNode>,
        native_gate: Arc<MutationWriteGate>,
        native_owner: StdMutex<Option<BoxFuture<'static, Result<()>>>>,
        write_guard: Option<MutationWriteGuard>,
        #[cfg(test)]
        drop_cleanup_pause: Option<Arc<RollbackDropCleanupPause>>,
        native_operations: Arc<NativeOperationTracker>,
        armed: bool,
    },
}

#[cfg(test)]
pub(crate) struct RollbackDropCleanupPause {
    pub(crate) started: Arc<tokio::sync::Notify>,
    pub(crate) release: Arc<tokio::sync::Notify>,
}

impl RollbackOnDrop {
    fn disarm(&mut self) {
        match self {
            Self::Http { armed, .. } | Self::Embedded { armed, .. } => *armed = false,
        }
    }
}

impl Drop for RollbackOnDrop {
    fn drop(&mut self) {
        match self {
            Self::Http {
                runtime,
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
                let runtime = runtime.clone();
                runtime.spawn(async move {
                    if let Err(error) = graphql::txn_discard(&endpoint, &id, &client).await {
                        tracing::debug!(%error, "discarding cancelled HTTP transaction");
                    }
                });
            }
            Self::Embedded {
                runtime,
                node,
                native_gate,
                native_owner,
                write_guard,
                #[cfg(test)]
                drop_cleanup_pause,
                native_operations,
                armed,
            } => {
                drop(
                    native_owner
                        .get_mut()
                        .unwrap_or_else(|e| e.into_inner())
                        .take(),
                );
                if !*armed {
                    return;
                }
                #[cfg(test)]
                let pause_cleanup = drop_cleanup_pause.is_some();
                #[cfg(not(test))]
                let pause_cleanup = false;
                let in_flight = native_operations
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .in_flight;
                if in_flight == 0 && !pause_cleanup {
                    return;
                }
                let node = Arc::clone(node);
                let runtime = runtime.clone();
                let native_gate = Arc::clone(native_gate);
                let native_operations = Arc::clone(native_operations);
                let write_guard = write_guard.take();
                #[cfg(test)]
                let drop_cleanup_pause = drop_cleanup_pause.take();
                let dispatcher = tracing::dispatcher::get_default(Clone::clone);
                let span = tracing::Span::current();
                runtime.spawn(async move {
                    let _native_gate = native_gate;
                    native_operations.wait_idle().await;
                    let cleanup = async move {
                        #[cfg(test)]
                        if let Some(pause) = drop_cleanup_pause {
                            pause.started.notify_one();
                            pause.release.notified().await;
                        }
                        drop(node);
                        Ok::<(), anyhow::Error>(())
                    };
                    match cleanup_while_holding_write_gate(
                        write_guard,
                        cleanup,
                    )
                    .await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            tracing::debug!(%error, "releasing cancelled embedded transaction ownership");
                        }
                        Err(_) => {
                            tracing::warn!(
                                timeout_ms = EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT.as_millis(),
                                "cancelled embedded transaction gate-release pause timed out"
                            );
                        }
                    }
                }
                .with_subscriber(dispatcher)
                .instrument(span));
                #[cfg(test)]
                mark_cleanup_scheduled_for_test();
            }
        }
    }
}

async fn begin_embedded_owned<F, Fut>(
    node: Arc<EmbeddedNode>,
    native_gate: Arc<MutationWriteGate>,
    read_only: bool,
    operation: WriteOperation,
    write_guard: Option<MutationWriteGuard>,
    native_operations: Arc<NativeOperationTracker>,
    cancellation_rollback_scheduled: Arc<AtomicBool>,
    after_begin: F,
) -> Result<(RollbackOnDrop, TransactionHandle)>
where
    F: FnOnce(TransactionHandle) -> Fut,
    Fut: Future<Output = ()>,
{
    let (registered, registration) = tokio::sync::oneshot::channel();
    let owner_node = Arc::clone(&node);
    let mut native_owner: BoxFuture<'static, Result<()>> = Box::pin(async move {
        let transaction = owner_node
            .begin_transaction_guard(read_only)
            .await
            .map_err(|error| {
                retry::transaction_storage_failure(anyhow::anyhow!("begin_txn: {error}"))
            })?;
        let _ = registered.send(transaction.handle().clone());
        std::future::pending::<()>().await;
        drop(transaction);
        Ok(())
    });
    let started = Instant::now();
    let result = tokio::time::timeout(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT, async {
        tokio::select! {
            result = &mut native_owner => {
                result?;
                anyhow::bail!("native transaction owner ended before registration")
            }
            result = registration => result.context("native transaction registration"),
        }
    })
    .await;
    record_slow_native_phase(Some(operation.as_str()), "begin", started.elapsed());
    let handle = result.map_err(|_| {
        embedded_phase_timeout("begin", EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT)
    })??;
    let rollback = RollbackOnDrop::Embedded {
        runtime: tokio::runtime::Handle::current(),
        node: Arc::clone(&node),
        native_gate,
        native_owner: StdMutex::new(Some(native_owner)),
        write_guard,
        #[cfg(test)]
        drop_cleanup_pause: None,
        native_operations,
        armed: true,
    };
    let _ = cancellation_rollback_scheduled;
    after_begin(handle.clone()).await;
    Ok((rollback, handle))
}

async fn begin_http_owned(
    endpoint: GraphqlEndpoint,
    cancellation_rollback_scheduled: Arc<AtomicBool>,
    read_only: bool,
) -> Result<(reqwest::Client, String, RollbackOnDrop)> {
    let (client, id) = graphql::txn_begin(&endpoint, read_only)
        .await
        .map_err(retry::transaction_storage_failure)?;
    let rollback = RollbackOnDrop::Http {
        runtime: tokio::runtime::Handle::current(),
        endpoint: endpoint.clone(),
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

    fn retries_begin_error(self, error: &anyhow::Error) -> bool {
        self.retries_callback_error(error)
    }

    const fn observes_conflicts(self) -> bool {
        matches!(self, Self::ObserveConflict)
    }
}

/// An open explicit transaction, handed to [`ConfigAccess::transact_write`] callbacks.
/// Raw begin/commit/discard transport lives in the crate-private `graphql`
/// submodule; external callers must use the owned entry points.
pub struct ConfigApplyTxn<'a> {
    backend: TxnBackend<'a>,
    rollback_on_drop: Option<RollbackOnDrop>,
    affected_documents: AtomicU64,
    operation: Option<&'static str>,
    contends_for_background_slot: AtomicBool,
}

enum EmbeddedTransactionPurpose {
    Mutation,
    ExecutionRenewal(Arc<NativeOperationTracker>),
    ReadOnly,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum EmbeddedFinalizationBoundary {
    BeforeCommit,
    AfterCommit,
    BeforeRollback,
}

#[cfg(test)]
tokio::task_local! {
    static EMBEDDED_FINALIZATION_PAUSE: (
        EmbeddedFinalizationBoundary,
        Arc<tokio::sync::Notify>,
    );
}

#[cfg(test)]
async fn pause_embedded_finalization_for_test(boundary: EmbeddedFinalizationBoundary) {
    let reached = EMBEDDED_FINALIZATION_PAUSE
        .try_with(|(selected, reached)| (*selected == boundary).then(|| Arc::clone(reached)))
        .ok()
        .flatten();
    if let Some(reached) = reached {
        reached.notify_one();
        std::future::pending::<()>().await;
    }
}

impl<'a> ConfigApplyTxn<'a> {
    #[cfg(test)]
    pub(crate) async fn with_owned_local_operation_pause<F: Future>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output {
        LOCAL_OPERATION_PAUSE
            .scope(
                Arc::new(LocalOperationPause {
                    reached,
                    release,
                    completed: None,
                }),
                future,
            )
            .await
    }

    #[cfg(test)]
    pub(crate) async fn with_owned_local_operation_completion<F: Future>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        completed: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output {
        LOCAL_OPERATION_PAUSE
            .scope(
                Arc::new(LocalOperationPause {
                    reached,
                    release,
                    completed: Some(completed),
                }),
                future,
            )
            .await
    }

    fn embedded_poisoned(&self) -> bool {
        matches!(
            &self.backend,
            TxnBackend::Embedded { native_operations, .. } if native_operations.is_abandoned()
        )
    }

    #[cfg(test)]
    pub(crate) async fn with_successful_mutation_failure_at<F: Future>(
        fail_after: Option<usize>,
        future: F,
    ) -> (F::Output, usize) {
        assert!(fail_after.is_none_or(|index| index > 0));
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        let output = SUCCESSFUL_MUTATION_FAULT
            .scope(Arc::clone(&fault), future)
            .await;
        (output, fault.count.load(Ordering::Relaxed))
    }

    #[cfg(test)]
    pub(crate) async fn assert_every_mutation_rolls_back<T, E, S, A, AF, O, OF>(
        mut attempt: A,
        observe: O,
    ) -> (T, usize)
    where
        E: std::fmt::Debug,
        S: PartialEq + std::fmt::Debug,
        A: FnMut() -> AF,
        AF: Future<Output = std::result::Result<T, E>>,
        O: Fn() -> OF,
        OF: Future<Output = S>,
    {
        let before = observe().await;
        let mut index = 1;
        loop {
            let (result, observed) =
                Self::with_successful_mutation_failure_at(Some(index), attempt()).await;
            match result {
                Ok(value) => {
                    assert_eq!(
                        observed,
                        index - 1,
                        "a committed attempt must not pass the injected position"
                    );
                    assert_ne!(
                        observe().await,
                        before,
                        "the committed attempt must be visible to the rollback observation"
                    );
                    return (value, observed);
                }
                Err(error) => {
                    let detail = format!("{error:?}");
                    assert!(
                        detail.contains(&format!(
                            "injected failure after successful transaction mutation {index}"
                        )),
                        "unexpected failure at mutation {index}: {detail}"
                    );
                    assert_eq!(observed, index, "injection must fire exactly once");
                    assert_eq!(
                        observe().await,
                        before,
                        "mutation {index} leaked a partial write"
                    );
                }
            }
            index += 1;
        }
    }

    #[cfg(test)]
    pub(crate) async fn with_post_commit_receipt_loss<F: Future>(future: F) -> (F::Output, bool) {
        Self::with_post_commit_receipt_loss_for_operation(None, future).await
    }

    #[cfg(test)]
    pub(crate) async fn with_post_commit_receipt_loss_for_operation<F: Future>(
        operation: Option<&'static str>,
        future: F,
    ) -> (F::Output, bool) {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: true,
            receipt_operation: operation,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        let output = SUCCESSFUL_MUTATION_FAULT
            .scope(Arc::clone(&fault), future)
            .await;
        (output, fault.receipt_fired.load(Ordering::Relaxed))
    }

    #[cfg(test)]
    pub(crate) async fn with_successful_mutation_pause_at<F: Future>(
        after: usize,
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> (F::Output, bool) {
        assert!(after > 0);
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: Some(SuccessfulMutationPause {
                after,
                reached,
                release,
                fired: AtomicBool::new(false),
            }),
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        let output = SUCCESSFUL_MUTATION_FAULT
            .scope(Arc::clone(&fault), future)
            .await;
        (
            output,
            fault
                .pause
                .as_ref()
                .expect("pause configured")
                .fired
                .load(Ordering::Relaxed),
        )
    }

    #[cfg(test)]
    pub(crate) async fn with_write_gate_observation<F: Future>(
        queued: Arc<tokio::sync::Notify>,
        acquired: Arc<AtomicBool>,
        future: F,
    ) -> F::Output {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: Some(Arc::new(WriteGateObservation {
                queued,
                acquired,
                queued_fired: AtomicBool::new(false),
            })),
            read_storage_failure: None,
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    #[cfg(test)]
    pub(crate) async fn fill_mutation_write_gate_for_test(
        node: &EmbeddedNode,
    ) -> MutationWriteGateFillers {
        let gate = mutation_write_gate(node);
        let held = gate
            .owners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        let mut fillers = Vec::with_capacity(MUTATION_WRITE_GATE_CAPACITY.saturating_sub(held));
        for _ in held..MUTATION_WRITE_GATE_CAPACITY {
            fillers.push(
                gate.acquire(
                    WriteOperation::new("test.write_gate_capacity_filler")
                        .expect("static write operation"),
                )
                .await
                .expect("fill the remaining write admission permits"),
            );
        }
        MutationWriteGateFillers { _guards: fillers }
    }

    #[cfg(test)]
    pub(crate) async fn with_read_storage_failure<F: Future>(
        marker: &'static str,
        future: F,
    ) -> F::Output {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: Some(marker),
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    #[cfg(test)]
    pub(crate) async fn with_read_pause<F: Future>(
        marker: &'static str,
        reached: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: Some((marker, reached)),
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    #[cfg(test)]
    pub(crate) async fn with_native_operation_pause<F: Future>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: None,
            native_pause: Some((reached, release)),
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    #[cfg(test)]
    pub(crate) async fn with_rollback_failure_retry_boundary<F: Future>(
        reached: Arc<tokio::sync::Notify>,
        cleanup_scheduled: Arc<AtomicBool>,
        future: F,
    ) -> F::Output {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: None,
            native_pause: None,
            fail_rollback: true,
            retry_backoff_reached: Some(reached),
            cleanup_scheduled: Some(cleanup_scheduled),
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    async fn begin_local_owned<'txn>(
        node: &Arc<EmbeddedNode>,
        identity: Option<Did>,
        operation: WriteOperation,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
        purpose: EmbeddedTransactionPurpose,
    ) -> Result<ConfigApplyTxn<'txn>> {
        let read_only = matches!(&purpose, EmbeddedTransactionPurpose::ReadOnly);
        let native_gate = mutation_write_gate(node);
        let write_guard = match &purpose {
            EmbeddedTransactionPurpose::Mutation => {
                Some(Arc::clone(&native_gate).acquire(operation).await?)
            }
            EmbeddedTransactionPurpose::ExecutionRenewal(_)
            | EmbeddedTransactionPurpose::ReadOnly => None,
        };
        let native_operations = match purpose {
            EmbeddedTransactionPurpose::ExecutionRenewal(native_operations) => native_operations,
            EmbeddedTransactionPurpose::Mutation | EmbeddedTransactionPurpose::ReadOnly => {
                Arc::new(NativeOperationTracker::default())
            }
        };
        let (rollback_on_drop, handle) = begin_embedded_owned(
            Arc::clone(node),
            native_gate,
            read_only,
            operation,
            write_guard,
            Arc::clone(&native_operations),
            cancellation_rollback_scheduled,
            |_| std::future::ready(()),
        )
        .await?;
        Ok(ConfigApplyTxn {
            backend: TxnBackend::Embedded {
                node: Arc::clone(node),
                handle: handle.clone(),
                identity,
                native_operations,
            },
            rollback_on_drop: Some(rollback_on_drop),
            affected_documents: AtomicU64::new(0),
            contends_for_background_slot: AtomicBool::new(false),
            operation: Some(operation.as_str()),
        })
    }

    #[cfg(test)]
    pub(crate) async fn begin_execution_renewal_for_test(
        node: &Arc<EmbeddedNode>,
    ) -> Result<ConfigApplyTxn<'static>> {
        Self::begin_local_owned(
            node,
            None,
            WriteOperation::new("test.execution_renewal")?,
            Arc::new(AtomicBool::new(false)),
            EmbeddedTransactionPurpose::ExecutionRenewal(Arc::new(
                NativeOperationTracker::default(),
            )),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn begin_local(
        node: &Arc<EmbeddedNode>,
        identity: Option<Did>,
    ) -> Result<ConfigApplyTxn<'static>> {
        Self::begin_local_owned(
            node,
            identity,
            WriteOperation::new("test.begin_local")?,
            Arc::new(AtomicBool::new(false)),
            EmbeddedTransactionPurpose::Mutation,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn pause_drop_rollback_for_test(&mut self, pause: Arc<RollbackDropCleanupPause>) {
        let rollback = self
            .rollback_on_drop
            .as_mut()
            .expect("new transaction owns its rollback guard");
        let RollbackOnDrop::Embedded {
            drop_cleanup_pause, ..
        } = rollback
        else {
            panic!("test cleanup pause requires an embedded transaction");
        };
        *drop_cleanup_pause = Some(pause);
    }

    async fn begin_http(
        endpoint: &'a GraphqlEndpoint,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
        read_only: bool,
    ) -> Result<Self> {
        let (client, id, rollback_on_drop) = tokio::spawn(begin_http_owned(
            endpoint.clone(),
            cancellation_rollback_scheduled,
            read_only,
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
            contends_for_background_slot: AtomicBool::new(false),
            operation: None,
        };
        Ok(txn)
    }

    /// Execute exactly once inside this snapshot; conflict replay is the outer `transact` owner's job.
    pub async fn execute(&self, document: &str) -> Result<Value> {
        self.execute_with_variables(document, &json!({})).await
    }

    /// Execute with typed GraphQL variables. JSON scalar payloads retain
    /// arbitrary keys and nested empty arrays; ordinary nillable list fields
    /// remain the caller's responsibility.
    pub async fn execute_with_variables(&self, document: &str, variables: &Value) -> Result<Value> {
        anyhow::ensure!(variables.is_object(), "GraphQL variables must be an object");
        #[cfg(test)]
        if !document.trim_start().starts_with("mutation")
            && SUCCESSFUL_MUTATION_FAULT
                .try_with(|fault| {
                    fault
                        .read_storage_failure
                        .is_some_and(|m| document.contains(m))
                })
                .unwrap_or(false)
        {
            return Err(retry::transaction_storage_failure(anyhow::anyhow!(
                "injected transaction read storage failure"
            )));
        }
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
                native_operations,
            } => {
                let started = Instant::now();
                let result = execute_owned_native(
                    Arc::clone(node),
                    QueryRequest::new(document)
                        .with_variables(variables.clone())
                        .with_identity(identity.clone()),
                    handle.clone(),
                    Arc::clone(native_operations),
                    self.operation,
                )
                .await;
                record_slow_native_phase(self.operation, "execute", started.elapsed());
                let response = result?;
                if response.is_transaction_conflict() {
                    return Err(retry::transaction_conflict(ConflictSource::StructuredCode));
                }
                if response.has_errors() {
                    return Err(retry::transaction_storage_failure(anyhow::anyhow!(
                        "graphql returned errors: {:?}",
                        response.errors
                    )));
                }
                crate::graphql::into_data_envelope(response.data)
            }
        };
        if document.trim_start().starts_with("mutation") {
            self.affected_documents
                .fetch_add(graphql::affected_documents(&response), Ordering::Relaxed);
            #[cfg(test)]
            after_successful_mutation_for_test().await?;
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
                let url = format!("{}/collections/versions", graphql_api_base(endpoint.url())?);
                let versions: Value = endpoint
                    .authorize(client.get(&url))?
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
            native_operations,
        } = &self.backend
        else {
            anyhow::bail!("native transaction responses require embedded access");
        };
        let started = Instant::now();
        let result = execute_owned_native(
            Arc::clone(node),
            QueryRequest::new(document).with_identity(identity.clone()),
            handle.clone(),
            Arc::clone(native_operations),
            self.operation,
        )
        .await;
        record_slow_native_phase(self.operation, "execute", started.elapsed());
        let response = result?;
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
            self.affected_documents.fetch_add(
                crate::graphql::mutation_affected_documents(&response) as u64,
                Ordering::Relaxed,
            );
            #[cfg(test)]
            after_successful_mutation_for_test().await?;
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
                if self.embedded_poisoned() {
                    return Err(CommitFailure {
                        error: native_operation_abandoned(
                            "commit rejected after an abandoned native transaction operation",
                        ),
                        cleanup: CommitCleanup::Required,
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
                    }),
                    Ok(Ok(())) => {
                        #[cfg(test)]
                        if post_commit_receipt_loss_for_test(self.operation) {
                            return Err(CommitFailure {
                                error: retry::transaction_storage_failure(anyhow::anyhow!(
                                    "injected receipt loss after committed transaction"
                                )),
                                cleanup: CommitCleanup::NotNeeded,
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
        if self.embedded_poisoned() {
            return Err(native_operation_abandoned(
                "rollback deferred until abandoned native transaction operations return",
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
            return Err(anyhow::anyhow!("injected rollback failure"));
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
                    node.rollback_transaction(handle).await
                })
                .await
                {
                    Ok(result) => result.map_err(|error| anyhow::anyhow!("rollback_txn: {error}")),
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

    /// Marks this attempt as contending for a request's background admission
    /// slot. A conflict then retries within the admission budget, since every
    /// concurrent admission to the request conflicts until one commits.
    pub(crate) fn contend_for_background_slot(&self) {
        self.contends_for_background_slot
            .store(true, Ordering::Relaxed);
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
        state: StdMutex::new(AttemptTelemetryState::default()),
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
    let pending_native = result
        .as_ref()
        .err()
        .is_some_and(is_native_operation_abandoned);
    let affected_documents = if pending_native {
        None
    } else {
        result.as_ref().ok().map(affected_documents).or(Some(0))
    };
    telemetry.record(AttemptReport {
        outcome: if pending_native {
            WriteOutcome::PendingNative
        } else if result.is_ok() {
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

#[derive(Default)]
struct AttemptTelemetryState {
    finished: bool,
    pending_native: bool,
}

struct AttemptTelemetry {
    operation: WriteOperation,
    backend: WriteBackend,
    mode: WriteMode,
    retry_owner: RetryOwner,
    ordinal: WriteAttemptOrdinal,
    started: Instant,
    state: StdMutex<AttemptTelemetryState>,
    cancelled_rollback: RollbackStatus,
    cancellation_rollback_scheduled: Option<Arc<AtomicBool>>,
    dispatch: tracing::Dispatch,
}

struct AutoCommitCallerObservation {
    telemetry: Arc<AttemptTelemetry>,
    completed: bool,
}

struct AutoCommitNativeCompletion {
    telemetry: Arc<AttemptTelemetry>,
    started: bool,
    completed: bool,
}

impl AutoCommitCallerObservation {
    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for AutoCommitCallerObservation {
    fn drop(&mut self) {
        if !self.completed {
            self.telemetry.record_pending_native();
        }
    }
}

impl Drop for AutoCommitNativeCompletion {
    fn drop(&mut self) {
        if self.started && !self.completed {
            self.telemetry.record_pending_native();
        }
    }
}

impl AttemptTelemetry {
    fn record(&self, report: AttemptReport) {
        self.record_with_receipt(report, ReceiptRecovery::NotAttempted);
    }

    fn record_with_receipt(&self, report: AttemptReport, receipt_recovery: ReceiptRecovery) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.finished {
            return;
        }
        state.finished = true;
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

    fn record_pending_native(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.finished || state.pending_native {
            return;
        }
        state.pending_native = true;
        let event = WriteAttemptEvent {
            operation: self.operation,
            backend: self.backend,
            mode: self.mode,
            retry_owner: self.retry_owner,
            ordinal: self.ordinal,
            outcome: WriteOutcome::PendingNative,
            conflict_source: ConflictSource::None,
            receipt_recovery: ReceiptRecovery::NotAttempted,
            backoff: None,
            elapsed: self.started.elapsed(),
            affected_documents: None,
            rollback: RollbackStatus::NotNeeded,
        };
        tracing::dispatcher::with_default(&self.dispatch, || event.record());
    }
}

impl Drop for AttemptTelemetry {
    fn drop(&mut self) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.finished || state.pending_native {
            return;
        }
        drop(state);
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
            affected_documents: if matches!(rollback, RollbackStatus::Scheduled) {
                None
            } else {
                Some(0)
            },
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
    B: FnMut(WriteOperation, Arc<AtomicBool>) -> BoxFuture<'a, Result<ConfigApplyTxn<'a>>>
        + Send
        + 'a,
{
    Box::pin(async move {
        let operation = WriteOperation::new(operation)?;
        let mut max_attempts = mode.max_attempts();
        let mut attempt = 0;
        while attempt < max_attempts {
            attempt += 1;
            let ordinal = WriteAttemptOrdinal::new(attempt, max_attempts)?;
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
                    return Err(error);
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
                        embedded_phase_timeout("callback", EMBEDDED_TRANSACTION_CALLBACK_TIMEOUT)
                    })
                    .and_then(|result| result),
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
            let conflict_source = retry::classified_transaction_conflict(&error);
            let source = conflict_source.unwrap_or(ConflictSource::None);
            let slot_contention = conflict_source.is_some()
                && txn.contends_for_background_slot.load(Ordering::Relaxed);
            if slot_contention {
                max_attempts = max_attempts.max(retry::BACKGROUND_SLOT_MAX_ATTEMPTS);
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
                    let backoff = if slot_contention {
                        retry::background_slot_backoff(attempt - 1)
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

fn expect_committed<T>(outcome: TransactionOutcome<T>) -> T {
    match outcome {
        TransactionOutcome::Committed(value) => value,
        TransactionOutcome::ConflictObserved => {
            unreachable!("only observing transactions expose observed conflicts")
        }
    }
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
                operation,
                elapsed_ms = elapsed.as_millis() as u64,
                "DefraDB GraphQL completed"
            );
        } else {
            tracing::debug!(
                operation,
                elapsed_ms = elapsed.as_millis() as u64,
                "DefraDB GraphQL completed"
            );
        }
        Ok(response)
    }

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
                let result =
                    crate::graphql::ensure_no_errors(&result, operation.as_str()).map(|()| result);
                drop(write_guard);
                let _ = send.send(result);
            }
            .with_subscriber(dispatcher)
            .instrument(span),
        );
        let result = tokio::time::timeout(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT, receive)
            .await
            .map_err(|_| {
                native_operation_abandoned(format!(
                    "embedded auto-commit caller timed out after {:?}; native operation and write gate remain owned",
                    EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT
                ))
            })?;
        caller_observation.complete();
        result.map_err(|_| {
            native_operation_abandoned("native auto-commit task ended without a response")
        })?
    }

    pub async fn transact_write<'a, T, F>(
        &'a self,
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
            TransactionMode::ConflictRetry,
            move |operation, rollback| {
                Box::pin(async move { self.begin_apply_txn(operation, rollback).await })
            },
            callback,
        )
        .await
        .map(expect_committed)
    }

    /// Query-only transaction; DefraDB enforces read-only mode on both transports
    /// and embedded access avoids the mutation gate.
    pub async fn transact_readonly<'a, T, F>(
        &'a self,
        operation: &'static str,
        callback: F,
    ) -> Result<T>
    where
        T: Send + 'a,
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<T>> + Send + 'a,
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

    /// Replay a stable-key transaction after any callback or commit error.
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
            move |operation, rollback| {
                Box::pin(async move { self.begin_apply_txn(operation, rollback).await })
            },
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
            move |operation, rollback| {
                Box::pin(async move { self.begin_apply_txn(operation, rollback).await })
            },
            callback,
        )
        .await
    }

    pub async fn transact_local_write<'a, T, F>(
        node: &'a Arc<EmbeddedNode>,
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

    /// Native snapshot reads retain the supplied ACP DID without taking the mutation gate.
    pub async fn transact_local_readonly<'a, T, F>(
        node: &'a Arc<EmbeddedNode>,
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
    pub(crate) async fn renew_execution_lease(
        node: &Arc<EmbeddedNode>,
        request_doc_id: &str,
        generation: &str,
        expected_deadline: Option<chrono::DateTime<chrono::Utc>>,
        fixture_now: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<crate::lifecycle::RenewalAttemptOutcome> {
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
            move |txn| {
                Box::pin(crate::lifecycle::renew_in_transaction(
                    txn,
                    request_doc_id,
                    generation,
                    expected_deadline,
                    fixture_now,
                ))
            },
        )
        .await;
        if native_operations.is_abandoned() {
            native_operations.wait_idle().await;
        }
        result.map(expect_committed)
    }

    pub async fn transact_local_idempotent<'a, T, F>(
        node: &'a Arc<EmbeddedNode>,
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

    pub(crate) async fn transact_local_observing_conflict<'a, T, F>(
        node: &'a Arc<EmbeddedNode>,
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
    }
}

#[cfg(test)]
#[path = "txn_owner_tests.rs"]
mod owner_tests;

#[cfg(test)]
#[path = "txn_native_completion_conformance.rs"]
mod native_completion_conformance;

#[cfg(test)]
#[path = "txn_native_claim_lifetime_conformance.rs"]
mod native_claim_lifetime_conformance;

#[cfg(test)]
#[path = "receipt_owner_tests.rs"]
mod receipt_owner_tests;

#[cfg(test)]
#[path = "txn_stream_waiter_conformance.rs"]
mod stream_waiter_conformance;

#[cfg(test)]
mod variable_tests {
    use super::*;

    #[tokio::test]
    async fn whole_input_variables_update_upsert_and_rollback_losslessly() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
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
            ConfigAccess::transact_local_write(&node, None, "variable_probe", |txn| {
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
        ConfigAccess::transact_local_write(&node, None, "variable_upsert_insert", |txn| Box::pin(async move {
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
        let aborted: Result<()> = ConfigAccess::transact_local_write(&node, None, "variable_probe_rollback", |txn| Box::pin(async move {
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
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
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
