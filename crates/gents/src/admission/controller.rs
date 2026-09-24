use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use defra_node::EmbeddedNode;
use rig::completion::CompletionError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};
use tokio_util::sync::CancellationToken;

use super::client::CallKind;
use super::config::BackendAdmissionConfig;
use super::permit::AdmissionPermit;
use super::persistence::{
    persist_call_started, persist_existing_call_running, persist_existing_call_terminal,
    persist_terminal_call, spawn_persistence,
};

/// One backend's admission capacity, shared by every controller incarnation
/// and every open period of the backend (Lean `InferenceCall.Registry`).
/// Calls admitted before a rewrite or an outage keep counting against it
/// until they release; queued calls survive capacity-only rewrites.
pub(super) struct CapacityPool {
    ledger: Mutex<CapacityLedger>,
    waiters: AtomicUsize,
}

/// Lean `InferenceCall.Registry.Ledger`. Tokio permits cannot go negative,
/// so held permits above capacity are `owed` and paid by forgetting permits.
/// A permit taken from the semaphore counts as admitted only once registered
/// here: Tokio returns a permit it assigned to a dropped waiter directly to
/// the semaphore, bypassing this ledger, so registration pays outstanding
/// debt before admitting.
struct CapacityLedger {
    /// Replaced on reopening after an outage. Permits of a retired semaphore
    /// return to the current one through `held`.
    semaphore: Arc<Semaphore>,
    open: bool,
    capacity: usize,
    owed: usize,
    held: usize,
    connection: String,
}

pub(super) enum AdmitError {
    Closed,
    NoPermits,
    ConnectionChanged,
}

enum Registration {
    Admitted(PoolPermit),
    PaidDebt,
    Rejected(AdmitError),
}

impl CapacityPool {
    pub(super) fn open(capacity: usize, connection: &str) -> Arc<Self> {
        Arc::new(Self {
            ledger: Mutex::new(CapacityLedger {
                semaphore: Arc::new(Semaphore::new(capacity)),
                open: true,
                capacity,
                owed: 0,
                held: 0,
                connection: connection.to_owned(),
            }),
            waiters: AtomicUsize::new(0),
        })
    }

    fn ledger(&self) -> MutexGuard<'_, CapacityLedger> {
        self.ledger.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Applies an available configuration: reopens a closed pool on a fresh
    /// semaphore charged with the permits still held, or resizes an open one.
    pub(super) fn configure(&self, capacity: usize, connection: &str) {
        let mut ledger = self.ledger();
        if !ledger.open {
            let charged = ledger.held.min(capacity);
            ledger.semaphore = Arc::new(Semaphore::new(capacity - charged));
            ledger.owed = ledger.held - charged;
            ledger.open = true;
        } else if capacity >= ledger.capacity {
            let grow = capacity - ledger.capacity;
            let repaid = grow.min(ledger.owed);
            ledger.owed -= repaid;
            ledger.semaphore.add_permits(grow - repaid);
        } else {
            let shrink = ledger.capacity - capacity;
            let forgotten = ledger.semaphore.forget_permits(shrink);
            ledger.owed += shrink - forgotten;
        }
        ledger.capacity = capacity;
        ledger.connection = connection.to_owned();
    }

    /// Fails every queued waiter and every later acquisition with
    /// `BackendGone`. Held permits stay counted until they drop.
    pub(super) fn close(&self) {
        let mut ledger = self.ledger();
        ledger.open = false;
        ledger.owed = 0;
        ledger.semaphore.close();
    }

    pub(super) fn is_retired(&self) -> bool {
        let ledger = self.ledger();
        !ledger.open && ledger.held == 0
    }

    fn try_admit(self: &Arc<Self>, connection: &str) -> Result<PoolPermit, AdmitError> {
        loop {
            let semaphore = self.ledger().semaphore.clone();
            let permit = match semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(TryAcquireError::Closed) => return Err(AdmitError::Closed),
                Err(TryAcquireError::NoPermits) => return Err(AdmitError::NoPermits),
            };
            match self.register(semaphore, permit, connection) {
                Registration::Admitted(permit) => return Ok(permit),
                Registration::PaidDebt => continue,
                Registration::Rejected(error) => return Err(error),
            }
        }
    }

    async fn admit_waiting(self: &Arc<Self>, connection: &str) -> Result<PoolPermit, AdmitError> {
        loop {
            let semaphore = self.ledger().semaphore.clone();
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| AdmitError::Closed)?;
            match self.register(semaphore, permit, connection) {
                Registration::Admitted(permit) => return Ok(permit),
                Registration::PaidDebt => continue,
                Registration::Rejected(error) => return Err(error),
            }
        }
    }

    fn register(
        self: &Arc<Self>,
        semaphore: Arc<Semaphore>,
        permit: OwnedSemaphorePermit,
        connection: &str,
    ) -> Registration {
        let mut ledger = self.ledger();
        if !Arc::ptr_eq(&semaphore, &ledger.semaphore) {
            // Taken from a semaphore retired by an outage just before it
            // closed; charge the current one instead.
            permit.forget();
            if !ledger.open {
                return Registration::Rejected(AdmitError::Closed);
            }
            if ledger.connection != connection {
                return Registration::Rejected(AdmitError::ConnectionChanged);
            }
            if ledger.semaphore.forget_permits(1) == 0 {
                ledger.owed += 1;
            }
            ledger.held += 1;
            return Registration::Admitted(PoolPermit {
                pool: self.clone(),
                permit: None,
            });
        }
        if ledger.connection != connection {
            ledger.return_current(permit);
            return Registration::Rejected(AdmitError::ConnectionChanged);
        }
        if ledger.owed > 0 {
            ledger.owed -= 1;
            permit.forget();
            return Registration::PaidDebt;
        }
        ledger.held += 1;
        Registration::Admitted(PoolPermit {
            pool: self.clone(),
            permit: Some((semaphore, permit)),
        })
    }

    fn release(&self, permit: Option<(Arc<Semaphore>, OwnedSemaphorePermit)>) {
        let mut ledger = self.ledger();
        ledger.held -= 1;
        match permit {
            Some((semaphore, permit)) if Arc::ptr_eq(&semaphore, &ledger.semaphore) => {
                ledger.return_current(permit);
            }
            retired => {
                if let Some((_, permit)) = retired {
                    permit.forget();
                }
                if ledger.open {
                    if ledger.owed > 0 {
                        ledger.owed -= 1;
                    } else {
                        ledger.semaphore.add_permits(1);
                    }
                }
            }
        }
    }

    fn try_enter_queue(&self, max_queue_depth: usize) -> Option<usize> {
        loop {
            let current = self.waiters.load(Ordering::SeqCst);
            if current >= max_queue_depth {
                return None;
            }
            if self
                .waiters
                .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Some(current + 1);
            }
        }
    }
}

impl CapacityLedger {
    fn return_current(&mut self, permit: OwnedSemaphorePermit) {
        if self.owed > 0 {
            self.owed -= 1;
            permit.forget();
        } else {
            drop(permit);
        }
    }
}

/// An admitted permit. It returns through its pool's ledger on drop, into
/// whichever semaphore is current by then.
pub(super) struct PoolPermit {
    pool: Arc<CapacityPool>,
    permit: Option<(Arc<Semaphore>, OwnedSemaphorePermit)>,
}

impl Drop for PoolPermit {
    fn drop(&mut self) {
        self.pool.release(self.permit.take());
    }
}

pub(super) struct BackendAdmissionController {
    pub(super) backend_id: String,
    pub(super) generation: u64,
    pub(super) config: BackendAdmissionConfig,
    pub(super) pool: Arc<CapacityPool>,
}

impl BackendAdmissionController {
    pub(super) fn new(
        generation: u64,
        config: BackendAdmissionConfig,
        pool: Arc<CapacityPool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            backend_id: config.backend_id.clone(),
            generation,
            config,
            pool,
        })
    }

    pub(super) fn matches(&self, config: &BackendAdmissionConfig) -> bool {
        self.config == *config
    }

    /// Admits a call issued through a provider client built for
    /// `connection`. Every rejection persists a cancelled `BackendGone` row;
    /// the returned error distinguishes a closed pool from a replaced
    /// connection.
    pub(super) async fn acquire(
        self: Arc<Self>,
        node: Arc<EmbeddedNode>,
        pending: PendingCallMetadata,
        connection: &str,
        cancel_observer: Option<CancellationToken>,
        terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
    ) -> Result<AdmissionPermit, CompletionError> {
        let mut queue_depth = 0;
        let immediate = if connection != self.config.connection_fingerprint {
            Some(Err(AdmitError::ConnectionChanged))
        } else {
            match self.pool.try_admit(connection) {
                Err(AdmitError::NoPermits) => {
                    match self.pool.try_enter_queue(self.config.max_queue_depth) {
                        Some(depth) => {
                            queue_depth = depth;
                            None
                        }
                        None => {
                            queue_depth = self.pool.waiters.load(Ordering::SeqCst);
                            Some(self.pool.try_admit(connection))
                        }
                    }
                }
                other => Some(other),
            }
        };
        if let Some(outcome) = immediate {
            let call = self.call_record(pending, queue_depth);
            return match outcome {
                Ok(permit) => {
                    self.start_permit(
                        node,
                        permit,
                        call,
                        cancel_observer,
                        terminal_failure_observer,
                    )
                    .await
                }
                Err(AdmitError::NoPermits) => {
                    if let Err(error) =
                        persist_terminal_call(node, call, "failed", Some("QueueFull"), None).await
                    {
                        tracing::warn!(backend_id = %self.backend_id, error = %error, "failed to persist queue-full inference call");
                    }
                    Err(CompletionError::ProviderError(format!(
                        "QueueFull: backend {} admission queue is full",
                        self.backend_id
                    )))
                }
                Err(error) => {
                    if let Err(persist_error) =
                        persist_terminal_call(node, call, "cancelled", Some("BackendGone"), None)
                            .await
                    {
                        tracing::warn!(backend_id = %self.backend_id, error = %persist_error, "failed to persist rejected inference call");
                    }
                    Err(self.rejection(error))
                }
            };
        }

        let call = self.call_record(pending, queue_depth);
        // The guard exists before the fallible durable write: a persist error
        // must still release the waiter unit, or each failure permanently
        // shrinks the queue toward `QueueFull` (#1001; Lean
        // `InferenceCall.ControllerBookkeeping.persist_error_releases_waiter`).
        // It arms terminal-persist-on-drop only once the queued row is durable
        // — before that there is no row to terminalize.
        let mut queued_guard = QueuedCallGuard {
            node: node.clone(),
            controller: self.clone(),
            call: call.clone(),
            persist_on_drop: false,
        };
        let doc_id = match super::persistence::persist_call_queued(node.clone(), &call).await {
            Ok(doc_id) => {
                queued_guard.arm();
                doc_id
            }
            Err(error) => {
                return Err(super::persistence::completion_persistence_error(error));
            }
        };
        let admitted = self.pool.admit_waiting(connection).await;
        drop(queued_guard.disarm());
        let permit = match admitted {
            Ok(permit) => permit,
            Err(error) => {
                if let Err(persist_error) = persist_existing_call_terminal(
                    node,
                    &call,
                    "cancelled",
                    Some("BackendGone"),
                    None,
                )
                .await
                {
                    tracing::warn!(backend_id = %self.backend_id, call_id = %call.call_id, error = %persist_error, "failed to persist rejected queued inference call");
                }
                return Err(self.rejection(error));
            }
        };
        // Only the queued row can acquire this running state. If recovery has
        // already terminalized it, release the permit before provider dispatch.
        // Other persistence failures leave a queued row for ordered recovery.
        if let Err(error) = persist_existing_call_running(node.clone(), &call).await {
            drop(permit);
            return Err(super::persistence::completion_persistence_error(error));
        }
        Ok(AdmissionPermit::new(
            node,
            permit,
            call,
            doc_id,
            cancel_observer,
            terminal_failure_observer,
        ))
    }

    async fn start_permit(
        self: Arc<Self>,
        node: Arc<EmbeddedNode>,
        permit: PoolPermit,
        call: InferenceCallRecord,
        cancel_observer: Option<CancellationToken>,
        terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
    ) -> Result<AdmissionPermit, CompletionError> {
        let doc_id = persist_call_started(node.clone(), &call).await?;
        Ok(AdmissionPermit::new(
            node,
            permit,
            call,
            doc_id,
            cancel_observer,
            terminal_failure_observer,
        ))
    }

    fn leave_queue(&self) {
        self.pool.waiters.fetch_sub(1, Ordering::SeqCst);
    }

    fn rejection(&self, error: AdmitError) -> CompletionError {
        match error {
            AdmitError::ConnectionChanged => CompletionError::ProviderError(format!(
                "{}: backend {} connection changed after this behavior was built; resubmit the request",
                crate::error::BACKEND_CONNECTION_CHANGED,
                self.backend_id
            )),
            AdmitError::Closed | AdmitError::NoPermits => {
                CompletionError::ProviderError(format!(
                    "BackendGone: backend {} was removed or became unavailable",
                    self.backend_id
                ))
            }
        }
    }

    fn call_record(
        &self,
        pending: PendingCallMetadata,
        queue_depth_at_enqueue: usize,
    ) -> InferenceCallRecord {
        InferenceCallRecord {
            call_id: pending.call_id,
            runtime_instance_id: pending.runtime_instance_id,
            request_id: pending.request_id,
            request_doc_id: pending.request_doc_id,
            call_seq: pending.call_seq,
            backend_id: pending.backend_id,
            behavior_id: pending.behavior_id,
            agent_did: pending.agent_did,
            call_kind: pending.call_kind,
            attempt: pending.attempt,
            queue_depth_at_enqueue,
            controller_generation: self.generation,
            backend_config_fingerprint: self.config.config_fingerprint.clone(),
        }
    }
}

#[cfg(test)]
impl CapacityPool {
    pub(super) fn held_for_test(&self) -> usize {
        self.ledger().held
    }

    pub(super) fn owed_for_test(&self) -> usize {
        self.ledger().owed
    }

    /// Permits a Tokio waiter holds but the ledger has not registered.
    pub(super) fn in_transit_for_test(&self) -> usize {
        let ledger = self.ledger();
        if !ledger.open {
            return 0;
        }
        (ledger.capacity + ledger.owed)
            .saturating_sub(ledger.held + ledger.semaphore.available_permits())
    }

    pub(super) fn queue_waiters_for_test(&self) -> usize {
        self.waiters.load(Ordering::SeqCst)
    }

    pub(super) fn available_permits_for_test(&self) -> usize {
        self.ledger().semaphore.available_permits()
    }
}

pub(super) struct QueuedCallGuard {
    node: Arc<EmbeddedNode>,
    controller: Arc<BackendAdmissionController>,
    call: InferenceCallRecord,
    persist_on_drop: bool,
}

impl QueuedCallGuard {
    fn arm(&mut self) {
        self.persist_on_drop = true;
    }

    pub(super) fn disarm(mut self) -> Self {
        self.persist_on_drop = false;
        self
    }
}

impl Drop for QueuedCallGuard {
    fn drop(&mut self) {
        self.controller.leave_queue();
        if !self.persist_on_drop {
            return;
        }
        let node = self.node.clone();
        let call = self.call.clone();
        spawn_persistence(async move {
            if let Err(error) =
                persist_existing_call_terminal(node, &call, "cancelled", Some("Cancelled"), None)
                    .await
            {
                tracing::warn!(call_id = %call.call_id, error = %error, "failed to persist cancelled queued inference call");
            }
        });
    }
}

pub(super) struct PendingCallMetadata {
    pub(super) call_id: String,
    pub(super) runtime_instance_id: String,
    pub(super) request_id: String,
    pub(super) request_doc_id: String,
    pub(super) call_seq: u64,
    pub(super) backend_id: String,
    pub(super) behavior_id: String,
    pub(super) agent_did: String,
    pub(super) call_kind: CallKind,
    pub(super) attempt: i64,
}

#[derive(Clone)]
pub(super) struct InferenceCallRecord {
    pub(super) call_id: String,
    pub(super) runtime_instance_id: String,
    pub(super) request_id: String,
    pub(super) request_doc_id: String,
    pub(super) call_seq: u64,
    pub(super) backend_id: String,
    pub(super) behavior_id: String,
    pub(super) agent_did: String,
    pub(super) call_kind: CallKind,
    pub(super) attempt: i64,
    pub(super) queue_depth_at_enqueue: usize,
    pub(super) controller_generation: u64,
    pub(super) backend_config_fingerprint: String,
}

impl InferenceCallRecord {
    pub(super) fn without_controller(pending: PendingCallMetadata) -> Self {
        Self {
            call_id: pending.call_id,
            runtime_instance_id: pending.runtime_instance_id,
            request_id: pending.request_id,
            request_doc_id: pending.request_doc_id,
            call_seq: pending.call_seq,
            backend_id: pending.backend_id,
            behavior_id: pending.behavior_id,
            agent_did: pending.agent_did,
            call_kind: pending.call_kind,
            attempt: pending.attempt,
            queue_depth_at_enqueue: 0,
            controller_generation: 0,
            backend_config_fingerprint: String::new(),
        }
    }
}
