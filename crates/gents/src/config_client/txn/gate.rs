use rapidhash::{HashMapExt, RapidHashMap};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use defra_node::EmbeddedNode;
use tracing::{instrument::WithSubscriber, Instrument};

#[cfg(test)]
use super::fault_injection::{LOCAL_OPERATION_PAUSE, SUCCESSFUL_MUTATION_FAULT};
use super::native::ensure_not_reentrant_embedded_write;
use super::{retry, ConfigClientError, Result, EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT};
use crate::config_client::write_telemetry::{record_slow_native_phase, WriteOperation};

pub(super) struct MutationWriteGate {
    pub(super) permits: Arc<tokio::sync::Semaphore>,
    pub(super) owners: StdMutex<RapidHashMap<u64, MutationWriteOwner>>,
    next_lease_id: AtomicU64,
    pub(super) pending_acquires: StdMutex<RapidHashMap<u64, PendingWriteAcquire>>,
    next_pending_acquire_id: AtomicU64,
    pub(super) store_claim: StdMutex<Option<Arc<StoreClaim>>>,
}

pub(super) const MUTATION_WRITE_GATE_CAPACITY: usize = 64;

pub(super) struct StoreClaim {
    pub(super) node: Arc<EmbeddedNode>,
    _lock: crate::store::local::home::StoreLock,
}

/// Embedded access plus a node-scoped store claim held across Gents-owned native operations.
#[derive(Clone)]
pub struct LocalConfigAccess {
    pub(super) node: Arc<EmbeddedNode>,
    pub(super) gate: Arc<MutationWriteGate>,
}

impl LocalConfigAccess {
    pub(crate) fn new(node: Arc<EmbeddedNode>) -> Self {
        let gate = mutation_write_gate(&node);
        Self { node, gate }
    }

    pub(crate) fn claimed(
        node: Arc<EmbeddedNode>,
        lock: crate::store::local::home::StoreLock,
    ) -> Result<Self> {
        let gate = mutation_write_gate(&node);
        gate.attach_store_claim(Arc::clone(&node), lock)?;
        Ok(Self { node, gate })
    }

    pub(crate) fn node(&self) -> &Arc<EmbeddedNode> {
        &self.node
    }

    pub(crate) async fn run_owned<T, E, F, Fut>(&self, operation: F) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<ConfigClientError> + Send + 'static,
        F: FnOnce(Arc<EmbeddedNode>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
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
            .map_err(ConfigClientError::OwnedOperationEnded)?
    }
}

#[derive(Clone, Copy)]
pub(super) struct MutationWriteOwner {
    pub(super) operation: WriteOperation,
    acquired_at: Instant,
}

#[derive(Clone)]
pub(super) struct PendingWriteAcquire {
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

pub(super) struct OldestPendingWriteAcquire {
    pub(super) count: usize,
    pub(super) operation: WriteOperation,
    task_id: Option<String>,
    span_name: Option<&'static str>,
    wait_age: Duration,
    pub(super) last_poll_age: Duration,
}

pub(super) struct MutationWriteGuard {
    gate: Arc<MutationWriteGate>,
    _permit: tokio::sync::OwnedSemaphorePermit,
    lease_id: u64,
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
    pub(super) fn new() -> Self {
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(MUTATION_WRITE_GATE_CAPACITY)),
            owners: StdMutex::new(RapidHashMap::new()),
            next_lease_id: AtomicU64::new(1),
            pending_acquires: StdMutex::new(RapidHashMap::new()),
            next_pending_acquire_id: AtomicU64::new(1),
            store_claim: StdMutex::new(None),
        }
    }

    fn attach_store_claim(
        &self,
        node: Arc<EmbeddedNode>,
        lock: crate::store::local::home::StoreLock,
    ) -> Result<()> {
        let mut claim = self
            .store_claim
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if claim.is_some() {
            return Err(ConfigClientError::StoreClaimHeld);
        }
        *claim = Some(Arc::new(StoreClaim { node, _lock: lock }));
        Ok(())
    }

    pub(super) fn store_claim(&self) -> Option<Arc<StoreClaim>> {
        self.store_claim
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(super) fn oldest_pending_acquire(&self) -> Option<OldestPendingWriteAcquire> {
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

    pub(super) async fn acquire(
        self: &Arc<Self>,
        operation: WriteOperation,
    ) -> Result<MutationWriteGuard> {
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
        let guard =
            match tokio::time::timeout(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT, acquire_future)
                .await
            {
                Ok(Ok(permit)) => permit,
                Ok(Err(error)) => {
                    return Err(ConfigClientError::WriteGateClosed { error });
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
                            u64::try_from(owner.acquired_at.elapsed().as_millis())
                                .unwrap_or(u64::MAX),
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
                                u64::try_from(pending.last_poll_age.as_millis())
                                    .unwrap_or(u64::MAX),
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
                    return Err(retry::transaction_storage_failure(
                        ConfigClientError::WriteAdmissionTimeout {
                            operation: operation.as_str(),
                            timeout: EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
                            active_owner_count,
                            owner_operation,
                            owner_held_ms,
                        },
                    ));
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

fn mutation_write_gates() -> &'static StdMutex<RapidHashMap<usize, Weak<MutationWriteGate>>> {
    static GATES: OnceLock<StdMutex<RapidHashMap<usize, Weak<MutationWriteGate>>>> =
        OnceLock::new();
    GATES.get_or_init(|| StdMutex::new(RapidHashMap::new()))
}

pub(super) fn mutation_write_gate(node: &EmbeddedNode) -> Arc<MutationWriteGate> {
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

pub(super) fn existing_mutation_write_gate(node: &EmbeddedNode) -> Option<Arc<MutationWriteGate>> {
    let node_key = node as *const EmbeddedNode as usize;
    mutation_write_gates()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&node_key)
        .and_then(Weak::upgrade)
}
