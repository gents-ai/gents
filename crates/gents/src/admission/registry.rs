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
    backends: Mutex<HashMap<String, BackendAdmission>>,
}

/// A backend's capacity pool outlives its admitting incarnations and its
/// outages while any permit it issued is held. Replaced incarnations are
/// owned only by their outstanding permits and queued waiters.
struct BackendAdmission {
    pool: Arc<CapacityPool>,
    admitting: Option<Arc<BackendAdmissionController>>,
}

impl AdmissionRegistry {
    pub(crate) fn new(node: Arc<EmbeddedNode>) -> Self {
        Self {
            inner: Arc::new(AdmissionRegistryInner {
                node,
                runtime_instance_id: uuid::Uuid::new_v4().to_string(),
                backends: Mutex::new(HashMap::new()),
            }),
        }
    }

    fn backends(&self) -> std::sync::MutexGuard<'_, HashMap<String, BackendAdmission>> {
        self.inner
            .backends
            .lock()
            .expect("AdmissionRegistry state lock poisoned")
    }

    /// Installs the admitting controller for every available backend in this
    /// complete snapshot, in the same step and on the backend's existing
    /// pool, so new calls never observe a gap. Removed and unavailable
    /// backends close admission; their held permits keep counting if the
    /// backend returns.
    pub(crate) fn reconcile(
        &self,
        generation: u64,
        configs: &HashMap<String, BackendAdmissionConfig>,
    ) {
        let mut backends = self.backends();
        for (backend_id, config) in configs {
            if !config.is_available() {
                continue;
            }
            let entry = backends
                .entry(backend_id.clone())
                .or_insert_with(|| BackendAdmission {
                    pool: CapacityPool::open(config.max_concurrent, &config.connection_fingerprint),
                    admitting: None,
                });
            if entry
                .admitting
                .as_ref()
                .is_some_and(|current| current.matches(config))
            {
                continue;
            }
            entry
                .pool
                .configure(config.max_concurrent, &config.connection_fingerprint);
            tracing::info!(
                backend_id = %backend_id,
                from_generation = entry.admitting.as_ref().map(|current| current.generation),
                to_generation = generation,
                max_concurrent = config.max_concurrent,
                "installed backend admission controller"
            );
            entry.admitting = Some(BackendAdmissionController::new(
                generation,
                config.clone(),
                entry.pool.clone(),
            ));
        }
        for (backend_id, entry) in backends.iter_mut() {
            let available = configs
                .get(backend_id)
                .is_some_and(BackendAdmissionConfig::is_available);
            if !available {
                if let Some(retired) = entry.admitting.take() {
                    tracing::info!(
                        backend_id = %backend_id,
                        generation = retired.generation,
                        "closed backend admission"
                    );
                    entry.pool.close();
                }
            }
        }
        backends.retain(|_, entry| entry.admitting.is_some() || !entry.pool.is_retired());
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
        let backend_id = backend_id.into();
        let connection = self.admitting_connection_for_test(&backend_id);
        self.acquire_with_connection_for_test(
            request_id,
            backend_id,
            behavior_id,
            agent_did,
            call_kind,
            &connection,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn acquire_with_connection_for_test(
        &self,
        request_id: impl Into<String>,
        backend_id: impl Into<String>,
        behavior_id: impl Into<String>,
        agent_did: impl Into<String>,
        call_kind: CallKind,
        connection: &str,
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
        scope_request(context, async {
            self.acquire_current_call(connection).await
        })
        .await
    }

    /// Acquires in the ambient request scope as a caller built for the
    /// backend's current connection.
    #[cfg(test)]
    pub(crate) async fn acquire_current_call_for_test(
        &self,
    ) -> Result<AdmissionPermit, CompletionError> {
        let backend_id = current_context()?.backend_id;
        let connection = self.admitting_connection_for_test(&backend_id);
        self.acquire_current_call(&connection).await
    }

    #[cfg(test)]
    pub(super) fn admitting_connection_for_test(&self, backend_id: &str) -> String {
        self.active_for_test(backend_id)
            .map(|controller| controller.config.connection_fingerprint.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(super) fn active_for_test(
        &self,
        backend_id: &str,
    ) -> Option<Arc<BackendAdmissionController>> {
        self.backends()
            .get(backend_id)
            .and_then(|entry| entry.admitting.clone())
    }

    #[cfg(test)]
    pub(super) fn pool_for_test(&self, backend_id: &str) -> Option<Arc<CapacityPool>> {
        self.backends()
            .get(backend_id)
            .map(|entry| entry.pool.clone())
    }

    /// Admits one provider call issued through a client built for
    /// `connection` (see `backend_connection_fingerprint`).
    pub(super) async fn acquire_current_call(
        &self,
        connection: &str,
    ) -> Result<AdmissionPermit, CompletionError> {
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
            .backends()
            .get(&pending.backend_id)
            .and_then(|entry| entry.admitting.clone());

        match controller {
            Some(controller) => {
                controller
                    .acquire(
                        self.inner.node.clone(),
                        pending,
                        connection,
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
