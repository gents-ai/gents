use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use defra_node::EmbeddedNode;
use rig::completion::CompletionError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use super::client::CallKind;
use super::config::BackendAdmissionConfig;
use super::permit::AdmissionPermit;
use super::persistence::{
    persist_call_started, persist_existing_call_running, persist_existing_call_terminal,
    persist_terminal_call, spawn_persistence,
};
/// One backend's admission capacity, shared by every controller incarnation
/// installed while the backend stays available (Lean
/// `InferenceCall.Registry`). Calls admitted or queued under a replaced
/// incarnation keep their permits and waiter units here, so a rewrite never
/// leaves the backend without an admitting controller and never admits past
/// the current capacity.
pub(super) struct CapacityPool {
    semaphore: Arc<Semaphore>,
    ledger: Mutex<CapacityLedger>,
    waiters: AtomicUsize,
}

/// Tokio semaphores cannot hold negative permits, so a decrease below the
/// permits currently held is recorded as `owed` and paid by forgetting
/// returned permits. Resizing and permit return share this lock: otherwise a
/// permit returned between `forget_permits` and the `owed` update would be
/// admissible while the pool is still over capacity.
struct CapacityLedger {
    capacity: usize,
    owed: usize,
}

impl CapacityPool {
    pub(super) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(capacity)),
            ledger: Mutex::new(CapacityLedger { capacity, owed: 0 }),
            waiters: AtomicUsize::new(0),
        })
    }

    pub(super) fn resize(&self, capacity: usize) {
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        if capacity >= ledger.capacity {
            let grow = capacity - ledger.capacity;
            let repaid = grow.min(ledger.owed);
            ledger.owed -= repaid;
            self.semaphore.add_permits(grow - repaid);
        } else {
            let shrink = ledger.capacity - capacity;
            ledger.owed += shrink - self.semaphore.forget_permits(shrink);
        }
        ledger.capacity = capacity;
    }

    /// Fails every queued waiter and every later acquisition with
    /// `BackendGone`. Permits already held stay valid until they drop.
    pub(super) fn close(&self) {
        self.semaphore.close();
    }

    fn return_permit(&self, permit: OwnedSemaphorePermit) {
        let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        if ledger.owed > 0 {
            ledger.owed -= 1;
            permit.forget();
        } else {
            drop(permit);
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

/// A semaphore permit that returns through its pool's ledger on drop.
pub(super) struct PoolPermit {
    pool: Arc<CapacityPool>,
    permit: Option<OwnedSemaphorePermit>,
}

impl PoolPermit {
    fn new(pool: Arc<CapacityPool>, permit: OwnedSemaphorePermit) -> Self {
        Self {
            pool,
            permit: Some(permit),
        }
    }
}

impl Drop for PoolPermit {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            self.pool.return_permit(permit);
        }
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

    pub(super) async fn acquire(
        self: Arc<Self>,
        node: Arc<EmbeddedNode>,
        pending: PendingCallMetadata,
        cancel_observer: Option<CancellationToken>,
        terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
    ) -> Result<AdmissionPermit, CompletionError> {
        match self.pool.semaphore.clone().try_acquire_owned() {
            Ok(permit) => {
                let call = self.call_record(pending, 0);
                return self
                    .start_permit(
                        node,
                        permit,
                        call,
                        cancel_observer,
                        terminal_failure_observer,
                    )
                    .await;
            }
            Err(tokio::sync::TryAcquireError::Closed) => {
                let call = self.call_record(pending, 0);
                if let Err(error) =
                    persist_terminal_call(node, call, "cancelled", Some("BackendGone"), None).await
                {
                    tracing::warn!(backend_id = %self.backend_id, error = %error, "failed to persist closed-pool inference call");
                }
                return Err(self.backend_gone());
            }
            Err(tokio::sync::TryAcquireError::NoPermits) => {}
        }

        let queue_depth = match self.pool.try_enter_queue(self.config.max_queue_depth) {
            Some(queue_depth) => queue_depth,
            None => {
                let queue_depth = self.pool.waiters.load(Ordering::SeqCst);
                match self.pool.semaphore.clone().try_acquire_owned() {
                    Ok(permit) => {
                        let call = self.call_record(pending, queue_depth);
                        return self
                            .start_permit(
                                node,
                                permit,
                                call,
                                cancel_observer,
                                terminal_failure_observer,
                            )
                            .await;
                    }
                    Err(tokio::sync::TryAcquireError::Closed) => {
                        let call = self.call_record(pending, queue_depth);
                        if let Err(error) = persist_terminal_call(
                            node,
                            call,
                            "cancelled",
                            Some("BackendGone"),
                            None,
                        )
                        .await
                        {
                            tracing::warn!(backend_id = %self.backend_id, error = %error, "failed to persist backend-gone inference call");
                        }
                        return Err(self.backend_gone());
                    }
                    Err(tokio::sync::TryAcquireError::NoPermits) => {
                        let call = self.call_record(pending, queue_depth);
                        if let Err(error) =
                            persist_terminal_call(node, call, "failed", Some("QueueFull"), None)
                                .await
                        {
                            tracing::warn!(backend_id = %self.backend_id, error = %error, "failed to persist queue-full inference call");
                        }
                        return Err(CompletionError::ProviderError(format!(
                            "QueueFull: backend {} admission queue is full",
                            self.backend_id
                        )));
                    }
                }
            }
        };

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
        let permit = match self.pool.semaphore.clone().acquire_owned().await {
            Ok(permit) => PoolPermit::new(self.pool.clone(), permit),
            Err(_) => {
                drop(queued_guard.disarm());
                if let Err(error) = persist_existing_call_terminal(
                    node,
                    &call,
                    "cancelled",
                    Some("BackendGone"),
                    None,
                )
                .await
                {
                    tracing::warn!(backend_id = %self.backend_id, call_id = %call.call_id, error = %error, "failed to persist backend-gone queued inference call");
                }
                return Err(self.backend_gone());
            }
        };
        drop(queued_guard.disarm());
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
        permit: OwnedSemaphorePermit,
        call: InferenceCallRecord,
        cancel_observer: Option<CancellationToken>,
        terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
    ) -> Result<AdmissionPermit, CompletionError> {
        let permit = PoolPermit::new(self.pool.clone(), permit);
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
    pub(super) fn capacity_for_test(&self) -> usize {
        self.ledger.lock().unwrap().capacity
    }

    /// Permits held by every incarnation sharing this pool.
    pub(super) fn held_for_test(&self) -> usize {
        let ledger = self.ledger.lock().unwrap();
        ledger.capacity + ledger.owed - self.semaphore.available_permits()
    }

    pub(super) fn queue_waiters_for_test(&self) -> usize {
        self.waiters.load(Ordering::SeqCst)
    }

    pub(super) fn available_permits_for_test(&self) -> usize {
        self.semaphore.available_permits()
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
