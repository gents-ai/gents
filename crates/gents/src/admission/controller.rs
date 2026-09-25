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
}

pub(super) enum AdmitError {
    Closed,
    NoPermits,
}

enum Registration {
    Admitted(PoolPermit),
    /// The permit paid debt or belonged to a retired semaphore.
    Reacquire,
    Closed,
}

impl CapacityPool {
    pub(super) fn open(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            ledger: Mutex::new(CapacityLedger {
                semaphore: Arc::new(Semaphore::new(capacity)),
                open: true,
                capacity,
                owed: 0,
                held: 0,
            }),
            waiters: AtomicUsize::new(0),
        })
    }

    fn ledger(&self) -> MutexGuard<'_, CapacityLedger> {
        self.ledger.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Applies an available configuration: reopens a closed pool on a fresh
    /// semaphore charged with the permits still held, or resizes an open one.
    pub(super) fn configure(&self, capacity: usize) {
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
    }

    /// Fails every queued waiter and every later acquisition with
    /// `BackendGone`. Held permits stay counted until they drop.
    pub(super) fn close(&self) {
        let mut ledger = self.ledger();
        ledger.open = false;
        ledger.owed = 0;
        ledger.semaphore.close();
    }

    /// A closed pool never reopens once pruned; a permit still in transit
    /// from it is rejected at registration because the pool is closed.
    pub(super) fn is_retired(&self) -> bool {
        let ledger = self.ledger();
        !ledger.open && ledger.held == 0
    }

    fn try_admit(self: &Arc<Self>) -> Result<PoolPermit, AdmitError> {
        loop {
            let semaphore = self.ledger().semaphore.clone();
            let permit = match semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(TryAcquireError::Closed) => return Err(AdmitError::Closed),
                Err(TryAcquireError::NoPermits) => return Err(AdmitError::NoPermits),
            };
            match self.register(semaphore, permit) {
                Registration::Admitted(permit) => return Ok(permit),
                Registration::Reacquire => continue,
                Registration::Closed => return Err(AdmitError::Closed),
            }
        }
    }

    /// `None` when admission closed while the call waited.
    async fn admit_waiting(self: &Arc<Self>) -> Option<PoolPermit> {
        loop {
            let semaphore = self.ledger().semaphore.clone();
            let permit = semaphore.clone().acquire_owned().await.ok()?;
            match self.register(semaphore, permit) {
                Registration::Admitted(permit) => return Some(permit),
                Registration::Reacquire => continue,
                Registration::Closed => return None,
            }
        }
    }

    fn register(
        self: &Arc<Self>,
        semaphore: Arc<Semaphore>,
        permit: OwnedSemaphorePermit,
    ) -> Registration {
        let mut ledger = self.ledger();
        // The single admission point: pool identity, openness and debt are
        // checked together under the ledger lock (Lean
        // `Registry.Ledger.register`).
        if !Arc::ptr_eq(&semaphore, &ledger.semaphore) {
            // Taken from a semaphore an outage retired; it grants no slot in
            // the current one, so the caller must acquire again there.
            permit.forget();
            return Registration::Reacquire;
        }
        if !ledger.open {
            // Assigned before admission closed; the closed semaphore absorbs it.
            drop(permit);
            return Registration::Closed;
        }
        if ledger.owed > 0 {
            ledger.owed -= 1;
            permit.forget();
            return Registration::Reacquire;
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
    /// `slot_connection`, which the call's row records as its backend
    /// fingerprint. A connection change never rejects a call: in-progress
    /// work finishes on the connection its slot started with.
    pub(super) async fn acquire(
        self: Arc<Self>,
        node: Arc<EmbeddedNode>,
        pending: PendingCallMetadata,
        slot_connection: &str,
        cancel_observer: Option<CancellationToken>,
        terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
    ) -> Result<AdmissionPermit, CompletionError> {
        let mut queue_depth = 0;
        let immediate = match self.pool.try_admit() {
            Err(AdmitError::NoPermits) => {
                match self.pool.try_enter_queue(self.config.max_queue_depth) {
                    Some(depth) => {
                        queue_depth = depth;
                        None
                    }
                    None => {
                        queue_depth = self.pool.waiters.load(Ordering::SeqCst);
                        Some(self.pool.try_admit())
                    }
                }
            }
            other => Some(other),
        };
        if let Some(outcome) = immediate {
            let call = self.call_record(pending, queue_depth, slot_connection);
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
                Err(AdmitError::Closed) => {
                    if let Err(persist_error) =
                        persist_terminal_call(node, call, "cancelled", Some("BackendGone"), None)
                            .await
                    {
                        tracing::warn!(backend_id = %self.backend_id, error = %persist_error, "failed to persist rejected inference call");
                    }
                    Err(self.backend_gone())
                }
            };
        }

        let call = self.call_record(pending, queue_depth, slot_connection);
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
        let admitted = self.pool.admit_waiting().await;
        drop(queued_guard.disarm());
        let permit = match admitted {
            Some(permit) => permit,
            None => {
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
                return Err(self.backend_gone());
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

    fn backend_gone(&self) -> CompletionError {
        CompletionError::ProviderError(format!(
            "BackendGone: backend {} was removed or became unavailable",
            self.backend_id
        ))
    }

    fn call_record(
        &self,
        pending: PendingCallMetadata,
        queue_depth_at_enqueue: usize,
        slot_connection: &str,
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
            backend_config_fingerprint: slot_connection.to_owned(),
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

    /// Takes a permit from the current semaphore without registering it,
    /// standing for a waiter Tokio has woken but that has not reached the
    /// ledger lock yet.
    pub(super) fn take_unregistered_for_test(&self) -> (Arc<Semaphore>, OwnedSemaphorePermit) {
        let semaphore = self.ledger().semaphore.clone();
        let permit = semaphore
            .clone()
            .try_acquire_owned()
            .expect("a free permit");
        (semaphore, permit)
    }

    /// Registers such a permit: `Ok(Some)` admits, `Ok(None)` means the
    /// caller must acquire again, `Err` rejects as closed.
    pub(super) fn register_for_test(
        self: &Arc<Self>,
        taken: (Arc<Semaphore>, OwnedSemaphorePermit),
    ) -> Result<Option<PoolPermit>, ()> {
        match self.register(taken.0, taken.1) {
            Registration::Admitted(permit) => Ok(Some(permit)),
            Registration::Reacquire => Ok(None),
            Registration::Closed => Err(()),
        }
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
    /// Keyed connection fingerprint of the behavior slot that made the call;
    /// one `controller_generation` can carry two values across a connection
    /// change.
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
