//! Bounded local request workers with retained foreground child continuations.
//!
//! This owns only process-local capacity. Request claims, execution leases,
//! accepted tool identity, child completion and cancellation remain with their
//! existing owners. A worker acquires an unbound active permit after dequeue,
//! then binds it to the exact request document and claimed generation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// The physical request and execution generation retained by one continuation.
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
    #[error("request worker capacity is full")]
    ActiveFull,
    #[error("parked request continuation capacity is full")]
    ParkedFull,
    #[error("request generation already has an active or parked continuation")]
    AlreadyOwned,
    #[error("request worker guard no longer owns its exact generation ticket")]
    OwnershipLost,
    #[error("request continuation owner revalidation failed: {0:#}")]
    OwnerRevalidation(#[source] anyhow::Error),
}

#[derive(Clone, Debug)]
enum RegisteredState {
    Active,
    Parked { child_tool_doc_id: String },
}

#[derive(Clone, Debug)]
struct RegisteredTicket {
    /// Identity of the guard, distinct from the reusable physical ticket.
    marker: Arc<()>,
    state: RegisteredState,
}

/// One slot generation's independent active and parked capacities.
pub(crate) struct WorkerCapacity {
    active: Arc<Semaphore>,
    parked: Arc<Semaphore>,
    active_limit: usize,
    parked_limit: usize,
    registered: Mutex<HashMap<WorkerTicket, RegisteredTicket>>,
}

tokio::task_local! {
    static SLOT_CAPACITY: Arc<WorkerCapacity>;
    static REQUEST_GUARD: RefCell<Option<RequestGuard>>;
}

enum RequestGuard {
    Unbound(UnboundActiveGuard),
    Active(ActiveGuard),
    Parked(ParkedGuard),
}

/// Slot generation scope shared by its fixed worker tasks. Tool and request
/// execution futures remain on these tasks; no spawned task inherits it.
pub(crate) async fn scope_slot_capacity<F: Future>(
    capacity: Arc<WorkerCapacity>,
    future: F,
) -> F::Output {
    SLOT_CAPACITY.scope(capacity, future).await
}

pub(crate) fn current_slot_capacity() -> Option<Arc<WorkerCapacity>> {
    SLOT_CAPACITY.try_with(Arc::clone).ok()
}

/// Owns one post-dequeue permit for the entire `process_request` future. The
/// scoped value drops on every return, cancellation, or unwind.
pub(crate) async fn scope_request_capacity<F: Future>(
    guard: UnboundActiveGuard,
    future: F,
) -> F::Output {
    REQUEST_GUARD
        .scope(RefCell::new(Some(RequestGuard::Unbound(guard))), future)
        .await
}

/// Bind after the canonical request claim has supplied an execution generation.
/// Direct unit calls without a worker scope preserve their existing behavior.
pub(crate) fn bind_current_claim(ticket: WorkerTicket) -> Result<(), CapacityError> {
    REQUEST_GUARD
        .try_with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(RequestGuard::Unbound(guard)) = slot.take() else {
                return Err(CapacityError::OwnershipLost);
            };
            match guard.bind(ticket) {
                Ok(active) => {
                    *slot = Some(RequestGuard::Active(active));
                    Ok(())
                }
                Err((guard, error)) => {
                    *slot = Some(RequestGuard::Unbound(guard));
                    Err(error)
                }
            }
        })
        .unwrap_or(Ok(()))
}

/// Reserve before a foreground bridge starts running. `None` means this call
/// came from a direct test or another path outside a behavior slot.
pub(crate) fn reserve_current_park(
    child_tool_doc_id: impl Into<String>,
) -> Result<Option<ParkReservation>, CapacityError> {
    let child_tool_doc_id = child_tool_doc_id.into();
    REQUEST_GUARD
        .try_with(|slot| {
            let slot = slot.borrow();
            let Some(RequestGuard::Active(active)) = slot.as_ref() else {
                return Err(CapacityError::OwnershipLost);
            };
            active.reserve_park(child_tool_doc_id).map(Some)
        })
        .unwrap_or(Ok(None))
}

/// Commit a caller-validated running bridge and yield active capacity. The
/// reservation's Drop rolls back capacity if dispatch fails before this call.
pub(crate) fn park_current(reservation: Option<ParkReservation>) -> Result<bool, CapacityError> {
    let Some(reservation) = reservation else {
        return Ok(false);
    };
    REQUEST_GUARD
        .try_with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(RequestGuard::Active(active)) = slot.take() else {
                return Err(CapacityError::OwnershipLost);
            };
            match active.park(reservation) {
                Ok(parked) => {
                    *slot = Some(RequestGuard::Parked(parked));
                    Ok(true)
                }
                Err((active, error)) => {
                    *slot = Some(RequestGuard::Active(active));
                    Err(error)
                }
            }
        })
        .unwrap_or(Err(CapacityError::OwnershipLost))
}

/// Reacquire active capacity before the parent continuation proceeds. The
/// callback must only read existing request/lease/tool-owner facts; it must not
/// write durable state or recursively acquire worker capacity.
pub(crate) async fn resume_current<F, Fut>(
    cancellation: &CancellationToken,
    revalidate: F,
) -> Result<bool, CapacityError>
where
    F: FnOnce(WorkerTicket, String) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let parked = match REQUEST_GUARD.try_with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.take() {
            Some(RequestGuard::Parked(parked)) => Ok(parked),
            other => {
                *slot = other;
                Err(CapacityError::OwnershipLost)
            }
        }
    }) {
        Ok(result) => result?,
        Err(_) => return Ok(false),
    };
    let active = parked.resume(cancellation, revalidate).await?;
    REQUEST_GUARD
        .try_with(|slot| {
            *slot.borrow_mut() = Some(RequestGuard::Active(active));
        })
        .map_err(|_| CapacityError::OwnershipLost)?;
    Ok(true)
}

impl WorkerCapacity {
    pub(crate) fn new(active_limit: usize, parked_limit: usize) -> Arc<Self> {
        Arc::new(Self {
            active: Arc::new(Semaphore::new(active_limit)),
            parked: Arc::new(Semaphore::new(parked_limit)),
            active_limit,
            parked_limit,
            registered: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn active_limit(&self) -> usize {
        self.active_limit
    }

    pub(crate) fn parked_limit(&self) -> usize {
        self.parked_limit
    }

    fn registry(&self) -> MutexGuard<'_, HashMap<WorkerTicket, RegisteredTicket>> {
        self.registered
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Called after dequeue. A waiting or idle worker retains no active permit.
    /// Cancellation drops the semaphore wait without leaving a registry entry.
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

    /// Nonblocking counterpart for admission probes and model conformance.
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
        let mut active = Vec::new();
        let mut parked = Vec::new();
        let mut dependencies = Vec::new();
        for (ticket, entry) in self.registry().iter() {
            match &entry.state {
                RegisteredState::Active => active.push(ticket.clone()),
                RegisteredState::Parked { child_tool_doc_id } => {
                    parked.push(ticket.clone());
                    dependencies.push((ticket.clone(), child_tool_doc_id.clone()));
                }
            }
        }
        active.sort_by(|a, b| {
            (&a.request_doc_id, &a.execution_generation)
                .cmp(&(&b.request_doc_id, &b.execution_generation))
        });
        parked.sort_by(|a, b| {
            (&a.request_doc_id, &a.execution_generation)
                .cmp(&(&b.request_doc_id, &b.execution_generation))
        });
        dependencies.sort_by(|a, b| {
            (&a.0.request_doc_id, &a.0.execution_generation)
                .cmp(&(&b.0.request_doc_id, &b.0.execution_generation))
        });
        CapacitySnapshot {
            active,
            parked,
            dependencies,
        }
    }
}

/// Active capacity held before `RequestLifecycle::claim` produces a generation.
pub(crate) struct UnboundActiveGuard {
    capacity: Arc<WorkerCapacity>,
    permit: Option<OwnedSemaphorePermit>,
}

impl UnboundActiveGuard {
    /// Binds a successfully claimed request to its exact generation. A parked
    /// continuation or retained dependency with this ticket cannot enter as
    /// fresh work. On refusal, the caller retains the active permit.
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
                    state: RegisteredState::Active,
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

/// One claimed request currently using an active worker.
pub(crate) struct ActiveGuard {
    capacity: Arc<WorkerCapacity>,
    ticket: WorkerTicket,
    marker: Arc<()>,
    permit: Option<OwnedSemaphorePermit>,
}

impl ActiveGuard {
    pub(crate) fn ticket(&self) -> &WorkerTicket {
        &self.ticket
    }

    /// Reserve parked room before starting a foreground wait or bridge. A
    /// refusal keeps this active guard intact for the existing tool owner.
    /// The caller supplies the exact accepted tool document identity here;
    /// before `park`, it must verify that bridge is running and owned.
    pub(crate) fn reserve_park(
        &self,
        child_tool_doc_id: impl Into<String>,
    ) -> Result<ParkReservation, CapacityError> {
        let child_tool_doc_id = child_tool_doc_id.into();
        if child_tool_doc_id.is_empty() {
            return Err(CapacityError::OwnershipLost);
        }
        let registered = self.capacity.registry();
        let Some(entry) = registered.get(&self.ticket) else {
            return Err(CapacityError::OwnershipLost);
        };
        if !Arc::ptr_eq(&entry.marker, &self.marker)
            || !matches!(entry.state, RegisteredState::Active)
        {
            return Err(CapacityError::OwnershipLost);
        }
        drop(registered);
        let permit = self
            .capacity
            .parked
            .clone()
            .try_acquire_owned()
            .map_err(|_| CapacityError::ParkedFull)?;
        Ok(ParkReservation {
            capacity: self.capacity.clone(),
            ticket: self.ticket.clone(),
            marker: self.marker.clone(),
            child_tool_doc_id,
            permit,
        })
    }

    /// Transfer the retained continuation from active to parked capacity.
    /// A failed transfer returns the still-active guard to its caller.
    pub(crate) fn park(
        mut self,
        reservation: ParkReservation,
    ) -> Result<ParkedGuard, (Self, CapacityError)> {
        if !Arc::ptr_eq(&self.capacity, &reservation.capacity)
            || self.ticket != reservation.ticket
            || !Arc::ptr_eq(&self.marker, &reservation.marker)
        {
            return Err((self, CapacityError::OwnershipLost));
        }
        {
            let mut registered = self.capacity.registry();
            let Some(entry) = registered.get_mut(&self.ticket) else {
                drop(registered);
                return Err((self, CapacityError::OwnershipLost));
            };
            if !Arc::ptr_eq(&entry.marker, &self.marker)
                || !matches!(entry.state, RegisteredState::Active)
            {
                drop(registered);
                return Err((self, CapacityError::OwnershipLost));
            }
            entry.state = RegisteredState::Parked {
                child_tool_doc_id: reservation.child_tool_doc_id.clone(),
            };
        }
        // Only after the parked state and dependency are registered may a
        // child consume the freed active permit.
        drop(self.permit.take());
        Ok(ParkedGuard {
            capacity: self.capacity.clone(),
            ticket: self.ticket.clone(),
            marker: self.marker.clone(),
            child_tool_doc_id: reservation.child_tool_doc_id,
            permit: Some(reservation.permit),
        })
    }
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
        // `permit` then drops, making a new active worker available.
    }
}

/// Parked room reserved while the parent still holds active capacity.
pub(crate) struct ParkReservation {
    capacity: Arc<WorkerCapacity>,
    ticket: WorkerTicket,
    marker: Arc<()>,
    child_tool_doc_id: String,
    permit: OwnedSemaphorePermit,
}

/// A retained process future waiting for one exact child bridge.
pub(crate) struct ParkedGuard {
    capacity: Arc<WorkerCapacity>,
    ticket: WorkerTicket,
    marker: Arc<()>,
    child_tool_doc_id: String,
    permit: Option<OwnedSemaphorePermit>,
}

impl ParkedGuard {
    pub(crate) fn ticket(&self) -> &WorkerTicket {
        &self.ticket
    }

    pub(crate) fn child_tool_doc_id(&self) -> &str {
        &self.child_tool_doc_id
    }

    /// Reacquire active room while retaining parked ownership, then ask the
    /// request/lease/tool owners to revalidate the current claim, generation,
    /// cancellation and exact terminal child bridge. The callback cannot
    /// be read-only and must not reacquire capacity. It cannot authorize
    /// capacity on its own; this guard must still own the parked
    /// ticket and dependency when it transitions.
    pub(crate) async fn resume<F, Fut>(
        mut self,
        cancellation: &CancellationToken,
        revalidate: F,
    ) -> Result<ActiveGuard, CapacityError>
    where
        F: FnOnce(WorkerTicket, String) -> Fut,
        Fut: Future<Output = anyhow::Result<()>>,
    {
        let active_permit = tokio::select! {
            permit = self.capacity.active.clone().acquire_owned() => permit.map_err(|_| CapacityError::Cancelled)?,
            _ = cancellation.cancelled() => return Err(CapacityError::Cancelled),
        };
        if cancellation.is_cancelled() {
            return Err(CapacityError::Cancelled);
        }
        revalidate(self.ticket.clone(), self.child_tool_doc_id.clone())
            .await
            .map_err(CapacityError::OwnerRevalidation)?;
        if cancellation.is_cancelled() {
            return Err(CapacityError::Cancelled);
        }
        {
            let mut registered = self.capacity.registry();
            let Some(entry) = registered.get_mut(&self.ticket) else {
                return Err(CapacityError::OwnershipLost);
            };
            if !Arc::ptr_eq(&entry.marker, &self.marker)
                || !matches!(&entry.state, RegisteredState::Parked { child_tool_doc_id } if child_tool_doc_id == &self.child_tool_doc_id)
            {
                return Err(CapacityError::OwnershipLost);
            }
            entry.state = RegisteredState::Active;
        }
        drop(self.permit.take());
        Ok(ActiveGuard {
            capacity: self.capacity.clone(),
            ticket: self.ticket.clone(),
            marker: self.marker.clone(),
            permit: Some(active_permit),
        })
    }
}

impl Drop for ParkedGuard {
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
        // `permit` then drops, releasing parked room even for stale leases.
    }
}

#[cfg(test)]
#[derive(Debug)]
struct CapacitySnapshot {
    active: Vec<WorkerTicket>,
    parked: Vec<WorkerTicket>,
    dependencies: Vec<(WorkerTicket, String)>,
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
        assert_eq!(capacity.parked_limit(), modeled.parked_limit);
        let actual = capacity.snapshot();
        let mut active: Vec<_> = modeled.active.iter().copied().map(ticket).collect();
        let mut parked: Vec<_> = modeled.parked.iter().copied().map(ticket).collect();
        let mut dependencies: Vec<_> = modeled
            .dependencies
            .iter()
            .map(|entry| (ticket(entry.ticket), entry.document.to_string()))
            .collect();
        active.sort_by(|a, b| {
            (&a.request_doc_id, &a.execution_generation)
                .cmp(&(&b.request_doc_id, &b.execution_generation))
        });
        parked.sort_by(|a, b| {
            (&a.request_doc_id, &a.execution_generation)
                .cmp(&(&b.request_doc_id, &b.execution_generation))
        });
        dependencies.sort_by(|a, b| {
            (&a.0.request_doc_id, &a.0.execution_generation)
                .cmp(&(&b.0.request_doc_id, &b.0.execution_generation))
        });
        assert_eq!(actual.active, active);
        assert_eq!(actual.parked, parked);
        assert_eq!(actual.dependencies, dependencies);
    }

    /// The resource transitions are compared to evaluated Lean states. Owner
    /// refusal cases involving an invalid claim, lease, cancellation, or tool
    /// bridge require the real request/tool owners at the integration seam;
    /// this module does not substitute a test-only policy implementation.
    #[tokio::test]
    async fn emitted_worker_capacity_resource_transitions_match_model() {
        let mut exercised = 0;
        let mut exercised_existing_wait = false;
        let mut exercised_existing_resume = false;
        for case in lean_canonical_worker_capacity_cases() {
            // RAII guards cannot construct an orphan dependency. The Lean
            // model includes that adversarial state to require fresh refusal.
            if case.pre.dependencies.len() != case.pre.parked.len()
                || case
                    .pre
                    .dependencies
                    .iter()
                    .any(|entry| !case.pre.parked.contains(&entry.ticket))
            {
                continue;
            }
            if matches!(
                &case.operation,
                LeanWorkerCapacityOperation::ResumeAfterChild { .. }
                    | LeanWorkerCapacityOperation::WaitForExistingChild { .. }
                    | LeanWorkerCapacityOperation::ResumeAfterExistingChild { .. }
            ) && case.expected.is_none()
            {
                continue;
            }
            let capacity = WorkerCapacity::new(case.pre.active_limit, case.pre.parked_limit);
            let mut active = HashMap::<WorkerTicket, ActiveGuard>::new();
            let mut parked = HashMap::<WorkerTicket, ParkedGuard>::new();
            for raw in &case.pre.parked {
                let key = ticket(*raw);
                let document = case
                    .pre
                    .dependencies
                    .iter()
                    .find(|entry| entry.ticket == *raw)
                    .expect("parked modeled ticket has exact dependency")
                    .document
                    .to_string();
                let guard = capacity
                    .try_acquire_unbound()
                    .unwrap()
                    .bind(key.clone())
                    .unwrap_or_else(|_| panic!("bind modeled parked ticket"));
                let reservation = guard.reserve_park(document).unwrap();
                let guard = guard
                    .park(reservation)
                    .unwrap_or_else(|_| panic!("park modeled ticket"));
                parked.insert(key, guard);
            }
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
                LeanWorkerCapacityOperation::AdmitFresh { ticket: raw } => {
                    let key = ticket(*raw);
                    let outcome = capacity
                        .try_acquire_unbound()
                        .and_then(|unbound| unbound.bind(key.clone()).map_err(|(_, error)| error));
                    if case.expected.is_some() {
                        active.insert(key, outcome.expect("Lean admitted fresh ticket"));
                    } else {
                        assert!(
                            outcome.is_err(),
                            "Lean refused fresh ticket in {}",
                            case.name
                        );
                    }
                }
                LeanWorkerCapacityOperation::WaitForChild {
                    generation,
                    document,
                }
                | LeanWorkerCapacityOperation::WaitForExistingChild {
                    generation,
                    document,
                    ..
                } => {
                    if let LeanWorkerCapacityOperation::WaitForExistingChild { selection, .. } =
                        &case.operation
                    {
                        assert_eq!(
                            *document, selection.bridge_document,
                            "resource dependency is the selected physical bridge"
                        );
                        exercised_existing_wait = true;
                    }
                    let key = WorkerTicket::new(
                        case.world.request_id.to_string(),
                        generation.to_string(),
                    );
                    let guard = active.remove(&key).expect("modeled parent active");
                    match guard.reserve_park(document.to_string()) {
                        Ok(reservation) => {
                            assert!(
                                case.expected.is_some(),
                                "Lean refused available parked capacity in {}",
                                case.name
                            );
                            let guard = guard
                                .park(reservation)
                                .unwrap_or_else(|_| panic!("park accepted modeled bridge"));
                            parked.insert(key, guard);
                        }
                        Err(error) => {
                            assert!(
                                case.expected.is_none(),
                                "unexpected parked refusal in {}: {error}",
                                case.name
                            );
                            active.insert(key, guard);
                        }
                    }
                }
                LeanWorkerCapacityOperation::ResumeAfterChild {
                    generation,
                    cancellation_allows,
                }
                | LeanWorkerCapacityOperation::ResumeAfterExistingChild {
                    generation,
                    cancellation_allows,
                    ..
                } => {
                    assert!(*cancellation_allows);
                    let key = WorkerTicket::new(
                        case.world.request_id.to_string(),
                        generation.to_string(),
                    );
                    let guard = parked.remove(&key).expect("modeled parent parked");
                    let selected_document = match &case.operation {
                        LeanWorkerCapacityOperation::ResumeAfterExistingChild {
                            selection, ..
                        } => {
                            exercised_existing_resume = true;
                            selection.bridge_document.to_string()
                        }
                        _ => case
                            .world
                            .selected_tool
                            .as_ref()
                            .expect("modeled exact child bridge")
                            .document
                            .to_string(),
                    };
                    let active_guard = guard
                        .resume(
                            &CancellationToken::new(),
                            move |observed_ticket, observed_document| {
                                assert_eq!(observed_ticket, key);
                                assert_eq!(observed_document, selected_document);
                                std::future::ready(Ok(()))
                            },
                        )
                        .await
                        .expect("resume after caller's owner revalidation");
                    active.insert(active_guard.ticket().clone(), active_guard);
                }
                LeanWorkerCapacityOperation::Release { ticket: raw } => {
                    drop(
                        active
                            .remove(&ticket(*raw))
                            .expect("modeled active release"),
                    );
                }
            }
            if let Some(expected) = &case.expected {
                assert_state(&capacity, expected);
            } else {
                assert_state(&capacity, &case.pre);
            }
            exercised += 1;
        }
        assert!(
            exercised >= 8,
            "model export must exercise all reachable resource transitions"
        );
        assert!(
            exercised_existing_wait && exercised_existing_resume,
            "model export must exercise existing-child resource handoff"
        );
    }

    #[tokio::test]
    async fn cancelled_resume_releases_parked_room_without_resuming() {
        let capacity = WorkerCapacity::new(1, 1);
        let parent = capacity
            .try_acquire_unbound()
            .unwrap()
            .bind(WorkerTicket::new("parent", "generation-a"))
            .unwrap_or_else(|_| panic!("bind parent"));
        let reservation = parent.reserve_park("accepted-child-tool").unwrap();
        let parked = parent
            .park(reservation)
            .unwrap_or_else(|_| panic!("park parent"));
        let child = capacity
            .try_acquire_unbound()
            .unwrap()
            .bind(WorkerTicket::new("child", "generation-b"))
            .unwrap_or_else(|_| panic!("bind child"));
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(matches!(
            parked
                .resume(&cancellation, |_, _| std::future::ready(Ok(())))
                .await,
            Err(CapacityError::Cancelled)
        ));
        assert!(capacity.snapshot().parked.is_empty());
        drop(child);
        assert!(capacity.snapshot().active.is_empty());
    }
}
