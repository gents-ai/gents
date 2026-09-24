use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use defra_node::EmbeddedNode;
use rig::completion::CompletionError;

use super::client::current_context;
#[cfg(test)]
use super::client::{scope_request, AdmissionCallContext, CallKind};
use super::config::BackendAdmissionConfig;
use super::controller::{BackendAdmissionController, CapacityPool, InferenceCallRecord};
use super::permit::AdmissionPermit;
use super::persistence::persist_terminal_call;

#[cfg(test)]
#[path = "registry_contract_tests.rs"]
mod contract_tests;

#[derive(Clone)]
pub(crate) struct AdmissionRegistry {
    inner: Arc<AdmissionRegistryInner>,
}

struct AdmissionRegistryInner {
    node: Arc<EmbeddedNode>,
    runtime_instance_id: String,
    /// The admitting controller per available backend. Replaced incarnations
    /// are owned only by their outstanding permits and queued waiters.
    active: Mutex<HashMap<String, Arc<BackendAdmissionController>>>,
}

impl AdmissionRegistry {
    pub(crate) fn new(node: Arc<EmbeddedNode>) -> Self {
        Self {
            inner: Arc::new(AdmissionRegistryInner {
                node,
                runtime_instance_id: uuid::Uuid::new_v4().to_string(),
                active: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Installs the admitting controller for every available backend in this
    /// complete snapshot. A changed backend is replaced in the same step, on
    /// the same capacity pool, so new calls never observe a gap; calls already
    /// admitted or queued finish under the incarnation that took them. Removed
    /// and unavailable backends close their pools.
    pub(crate) fn reconcile(
        &self,
        generation: u64,
        configs: &HashMap<String, BackendAdmissionConfig>,
    ) {
        let mut active = self
            .inner
            .active
            .lock()
            .expect("AdmissionRegistry state lock poisoned");
        let mut previous = std::mem::take(&mut *active);
        for (backend_id, config) in configs {
            if !config.is_available() {
                continue;
            }
            let controller = match previous.remove(backend_id) {
                Some(current) if current.matches(config) => current,
                Some(current) => {
                    current.pool.resize(config.max_concurrent);
                    tracing::info!(
                        backend_id = %backend_id,
                        from_generation = current.generation,
                        to_generation = generation,
                        max_concurrent = config.max_concurrent,
                        "replaced backend admission controller"
                    );
                    BackendAdmissionController::new(
                        generation,
                        config.clone(),
                        current.pool.clone(),
                    )
                }
                None => BackendAdmissionController::new(
                    generation,
                    config.clone(),
                    CapacityPool::new(config.max_concurrent),
                ),
            };
            active.insert(backend_id.clone(), controller);
        }
        for (backend_id, retired) in previous {
            tracing::info!(
                backend_id = %backend_id,
                generation = retired.generation,
                "closed backend admission pool"
            );
            retired.pool.close();
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn acquire_for_test(
        &self,
        request_id: impl Into<String>,
        backend_id: impl Into<String>,
        behavior_id: impl Into<String>,
        agent_did: impl Into<String>,
        call_kind: CallKind,
    ) -> Result<AdmissionPermit, CompletionError> {
        use std::sync::atomic::AtomicU64;
        let context = AdmissionCallContext {
            request_id: request_id.into(),
            request_doc_id: "doc-test".to_string(),
            backend_id: backend_id.into(),
            behavior_id: behavior_id.into(),
            agent_did: agent_did.into(),
            session_id: "session-test".to_string(),
            call_kind,
            attempt: 1,
            call_seq: Arc::new(AtomicU64::new(0)),
            current_call: Arc::new(std::sync::Mutex::new(None)),
            inference_token: None,
            terminal_failure_reason: None,
        };
        scope_request(context, async { self.acquire_current_call().await }).await
    }

    #[cfg(test)]
    pub(super) fn active_for_test(
        &self,
        backend_id: &str,
    ) -> Option<Arc<BackendAdmissionController>> {
        self.inner.active.lock().unwrap().get(backend_id).cloned()
    }

    pub(super) async fn acquire_current_call(&self) -> Result<AdmissionPermit, CompletionError> {
        let context = current_context()?;
        let cancel_observer = context.inference_token.clone();
        let terminal_failure_observer = context.terminal_failure_reason.clone();
        let pending = context.next_call(&self.inner.runtime_instance_id);
        if pending.backend_id.trim().is_empty() {
            return Err(CompletionError::ProviderError(format!(
                "behavior {} has no backend binding",
                pending.behavior_id
            )));
        }

        let controller = self
            .inner
            .active
            .lock()
            .expect("AdmissionRegistry state lock poisoned")
            .get(&pending.backend_id)
            .cloned();

        match controller {
            Some(controller) => {
                controller
                    .acquire(
                        self.inner.node.clone(),
                        pending,
                        cancel_observer,
                        terminal_failure_observer,
                    )
                    .await
            }
            None => {
                let backend_id = pending.backend_id.clone();
                let call = InferenceCallRecord::without_controller(pending);
                if let Err(error) = persist_terminal_call(
                    self.inner.node.clone(),
                    call,
                    "cancelled",
                    Some("BackendGone"),
                    None,
                )
                .await
                {
                    tracing::warn!(error = %error, "failed to persist backend-gone inference call");
                }
                Err(CompletionError::ProviderError(format!(
                    "BackendGone: backend admission controller is not active for backend {backend_id}; it is not configured or not available"
                )))
            }
        }
    }
}
