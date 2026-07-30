use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

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
use super::registry::AdmissionRegistryInner;

pub(super) struct BackendAdmissionController {
    pub(super) backend_id: String,
    pub(super) generation: u64,
    pub(super) config: BackendAdmissionConfig,
    semaphore: Arc<Semaphore>,
    waiters: AtomicUsize,
    running: AtomicUsize,
    closed: AtomicBool,
    registry: Weak<AdmissionRegistryInner>,
}

impl BackendAdmissionController {
    pub(super) fn new(
        generation: u64,
        config: BackendAdmissionConfig,
        registry: Weak<AdmissionRegistryInner>,
    ) -> Arc<Self> {
        let max_concurrent = config.max_concurrent;
        Arc::new(Self {
            backend_id: config.backend_id.clone(),
            generation,
            config,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            waiters: AtomicUsize::new(0),
            running: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            registry,
        })
    }

    pub(super) fn matches(&self, config: &BackendAdmissionConfig) -> bool {
        self.config == *config && !self.is_closed()
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.semaphore.close();
    }

    pub(super) fn is_drained(&self) -> bool {
        self.running.load(Ordering::SeqCst) == 0
    }

    pub(super) async fn acquire(
        self: Arc<Self>,
        node: Arc<EmbeddedNode>,
        pending: PendingCallMetadata,
        cancel_observer: Option<CancellationToken>,
        terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
    ) -> Result<AdmissionPermit, CompletionError> {
        if self.is_closed() {
            let call = self.call_record(pending, 0);
            if let Err(error) =
                persist_terminal_call(node, call, "cancelled", Some("BackendGone"), None).await
            {
                tracing::warn!(backend_id = %self.backend_id, error = %error, "failed to persist closed-controller inference call");
            }
            return Err(CompletionError::ProviderError(
                "BackendGone: backend admission controller is draining".into(),
            ));
        }

        match self.semaphore.clone().try_acquire_owned() {
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
                    tracing::warn!(backend_id = %self.backend_id, error = %error, "failed to persist closed-controller inference call");
                }
                return Err(CompletionError::ProviderError(
                    "BackendGone: backend admission controller is draining".into(),
                ));
            }
            Err(tokio::sync::TryAcquireError::NoPermits) => {}
        }

        let queue_depth = match self.try_enter_queue() {
            Some(queue_depth) => queue_depth,
            None => {
                let queue_depth = self.waiters.load(Ordering::SeqCst);
                match self.semaphore.clone().try_acquire_owned() {
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
                        return Err(CompletionError::ProviderError(
                            "BackendGone: backend admission controller is draining".into(),
                        ));
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
        let doc_id = super::persistence::persist_call_queued(node.clone(), &call)
            .await
            .map_err(super::persistence::completion_persistence_error)?;
        let queued_guard = QueuedCallGuard {
            node: node.clone(),
            controller: self.clone(),
            call: call.clone(),
            persist_on_drop: true,
        };
        let permit = match self.semaphore.clone().acquire_owned().await {
            Ok(permit) => permit,
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
                return Err(CompletionError::ProviderError(
                    "BackendGone: backend admission controller is draining".into(),
                ));
            }
        };
        drop(queued_guard.disarm());
        self.running.fetch_add(1, Ordering::SeqCst);
        if let Err(error) = persist_existing_call_running(node.clone(), &call).await {
            self.release_running();
            return Err(super::persistence::completion_persistence_error(error));
        }
        Ok(AdmissionPermit::new(
            node,
            self,
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
        self.running.fetch_add(1, Ordering::SeqCst);
        match persist_call_started(node.clone(), &call).await {
            Ok(doc_id) => Ok(AdmissionPermit::new(
                node,
                self,
                permit,
                call,
                doc_id,
                cancel_observer,
                terminal_failure_observer,
            )),
            Err(error) => {
                self.release_running();
                Err(error)
            }
        }
    }

    fn try_enter_queue(&self) -> Option<usize> {
        loop {
            let current = self.waiters.load(Ordering::SeqCst);
            if current >= self.config.max_queue_depth {
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

    pub(super) fn leave_queue(&self) {
        self.waiters.fetch_sub(1, Ordering::SeqCst);
    }

    pub(super) fn release_running(&self) {
        let previous = self.running.fetch_sub(1, Ordering::SeqCst);
        if previous == 1 && self.is_closed() {
            if let Some(registry) = self.registry.upgrade() {
                let backend_id = self.backend_id.clone();
                registry.controller_drained(backend_id);
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

pub(super) struct QueuedCallGuard {
    node: Arc<EmbeddedNode>,
    controller: Arc<BackendAdmissionController>,
    call: InferenceCallRecord,
    persist_on_drop: bool,
}

impl QueuedCallGuard {
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
