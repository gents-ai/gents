use rapidhash::{HashMapExt, RapidHashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct WorkerTicket {
    pub(crate) request_doc_id: String,
    pub(crate) execution_generation: String,
}

impl WorkerTicket {
    pub(crate) fn new(request_doc_id: impl Into<String>, generation: impl Into<String>) -> Self {
        Self {
            request_doc_id: request_doc_id.into(),
            execution_generation: generation.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CapacityError {
    #[error("request worker capacity acquisition cancelled")]
    Cancelled,
    #[cfg(test)]
    #[error("request worker capacity is full")]
    ActiveFull,
    #[error("request generation already has an active worker")]
    AlreadyOwned,
    #[error("request worker guard no longer owns its exact generation ticket")]
    OwnershipLost,
}

#[derive(Clone, Debug)]
struct RegisteredTicket {
    marker: Arc<()>,
}

pub(crate) struct WorkerCapacity {
    active: Arc<Semaphore>,
    #[cfg(test)]
    active_limit: usize,
    registered: Mutex<RapidHashMap<WorkerTicket, RegisteredTicket>>,
}

/// The worker permit one request execution owns from dequeue until it returns.
pub(crate) enum RequestGuard {
    Unbound(UnboundActiveGuard),
    Active { _guard: ActiveGuard },
}

impl RequestGuard {
    /// Bind after the canonical request claim has supplied an execution
    /// generation. A request run without a worker permit has nothing to bind.
    pub(crate) fn bind_claim(
        slot: &mut Option<Self>,
        ticket: WorkerTicket,
    ) -> Result<(), CapacityError> {
        let Some(guard) = slot.take() else {
            return Ok(());
        };
        let Self::Unbound(guard) = guard else {
            return Err(CapacityError::OwnershipLost);
        };
        match guard.bind(ticket) {
            Ok(active) => {
                *slot = Some(Self::Active { _guard: active });
                Ok(())
            }
            Err((guard, error)) => {
                *slot = Some(Self::Unbound(guard));
                Err(error)
            }
        }
    }
}

impl WorkerCapacity {
    pub(crate) fn new(active_limit: usize) -> Arc<Self> {
        Arc::new(Self {
            active: Arc::new(Semaphore::new(active_limit)),
            #[cfg(test)]
            active_limit,
            registered: Mutex::new(RapidHashMap::new()),
        })
    }

    #[cfg(test)]
    pub(crate) fn active_limit(&self) -> usize {
        self.active_limit
    }

    fn registry(&self) -> MutexGuard<'_, RapidHashMap<WorkerTicket, RegisteredTicket>> {
        self.registered
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Called after dequeue; a waiting or idle worker retains no active permit.
    pub(crate) async fn acquire_unbound(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
    ) -> Result<UnboundActiveGuard, CapacityError> {
        let permit = tokio::select! {
            permit = self.active.clone().acquire_owned() => permit.map_err(|_| CapacityError::Cancelled)?,
            _ = cancellation.cancelled() => return Err(CapacityError::Cancelled),
        };
        if cancellation.is_cancelled() {
            return Err(CapacityError::Cancelled);
        }
        Ok(UnboundActiveGuard {
            capacity: self.clone(),
            permit: Some(permit),
        })
    }

    #[cfg(test)]
    pub(crate) fn try_acquire_unbound(
        self: &Arc<Self>,
    ) -> Result<UnboundActiveGuard, CapacityError> {
        let permit = self
            .active
            .clone()
            .try_acquire_owned()
            .map_err(|_| CapacityError::ActiveFull)?;
        Ok(UnboundActiveGuard {
            capacity: self.clone(),
            permit: Some(permit),
        })
    }

    #[cfg(test)]
    fn snapshot(&self) -> CapacitySnapshot {
        let mut active = self.registry().keys().cloned().collect::<Vec<_>>();
        active.sort_by(|a, b| {
            (&a.request_doc_id, &a.execution_generation)
                .cmp(&(&b.request_doc_id, &b.execution_generation))
        });
        CapacitySnapshot { active }
    }
}

/// Active capacity held before `RequestLifecycle::claim` produces a generation.
pub(crate) struct UnboundActiveGuard {
    capacity: Arc<WorkerCapacity>,
    permit: Option<OwnedSemaphorePermit>,
}

impl UnboundActiveGuard {
    pub(crate) fn bind(
        mut self,
        ticket: WorkerTicket,
    ) -> Result<ActiveGuard, (Self, CapacityError)> {
        if ticket.request_doc_id.is_empty() || ticket.execution_generation.is_empty() {
            return Err((self, CapacityError::OwnershipLost));
        }
        let marker = Arc::new(());
        {
            let mut registered = self.capacity.registry();
            if registered.contains_key(&ticket) {
                drop(registered);
                return Err((self, CapacityError::AlreadyOwned));
            }
            registered.insert(
                ticket.clone(),
                RegisteredTicket {
                    marker: marker.clone(),
                },
            );
        }
        Ok(ActiveGuard {
            capacity: self.capacity.clone(),
            ticket,
            marker,
            permit: self.permit.take(),
        })
    }
}

pub(crate) struct ActiveGuard {
    capacity: Arc<WorkerCapacity>,
    ticket: WorkerTicket,
    marker: Arc<()>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        if self.permit.is_none() {
            return;
        }
        let mut registered = self.capacity.registry();
        if registered
            .get(&self.ticket)
            .is_some_and(|entry| Arc::ptr_eq(&entry.marker, &self.marker))
        {
            registered.remove(&self.ticket);
        }
    }
}

#[cfg(test)]
#[derive(Debug)]
struct CapacitySnapshot {
    active: Vec<WorkerTicket>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lean_vocab_test::{
        lean_canonical_worker_capacity_cases, LeanWorkerCapacityOperation, LeanWorkerCapacityState,
    };

    fn ticket(raw: [u64; 2]) -> WorkerTicket {
        WorkerTicket::new(raw[0].to_string(), raw[1].to_string())
    }

    fn assert_state(capacity: &WorkerCapacity, modeled: &LeanWorkerCapacityState) {
        assert_eq!(capacity.active_limit(), modeled.active_limit);
        let mut active: Vec<_> = modeled.active.iter().copied().map(ticket).collect();
        active.sort_by(|a, b| {
            (&a.request_doc_id, &a.execution_generation)
                .cmp(&(&b.request_doc_id, &b.execution_generation))
        });
        assert_eq!(capacity.snapshot().active, active);
    }

    #[tokio::test]
    async fn emitted_worker_capacity_resource_transitions_match_model() {
        let mut exercised = 0;
        for case in lean_canonical_worker_capacity_cases() {
            let capacity = WorkerCapacity::new(case.pre.active_limit);
            let mut active = RapidHashMap::<WorkerTicket, ActiveGuard>::new();
            for raw in &case.pre.active {
                let key = ticket(*raw);
                let guard = capacity
                    .try_acquire_unbound()
                    .unwrap()
                    .bind(key.clone())
                    .unwrap_or_else(|_| panic!("bind modeled active ticket"));
                active.insert(key, guard);
            }
            assert_state(&capacity, &case.pre);

            match &case.operation {
                LeanWorkerCapacityOperation::Acquire { ticket: raw } => {
                    let key = ticket(*raw);
                    let outcome = capacity
                        .try_acquire_unbound()
                        .and_then(|unbound| unbound.bind(key.clone()).map_err(|(_, error)| error));
                    if case.expected.is_some() {
                        active.insert(key, outcome.expect("Lean admitted fresh ticket"));
                    } else {
                        assert!(outcome.is_err(), "Lean refused ticket in {}", case.name);
                    }
                }
                LeanWorkerCapacityOperation::Release { ticket: raw } => {
                    drop(
                        active
                            .remove(&ticket(*raw))
                            .expect("modeled active release"),
                    );
                }
            }
            assert_state(&capacity, case.expected.as_ref().unwrap_or(&case.pre));
            exercised += 1;
        }
        assert_eq!(
            exercised,
            lean_canonical_worker_capacity_cases().len(),
            "every emitted resource transition is exercised"
        );
    }
}
