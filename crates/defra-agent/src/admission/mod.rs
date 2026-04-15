pub(crate) mod stream_guard;

use anyhow::Result;

use crate::backend_registry::InferenceBackend;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BackendAdmissionConfig {
    pub(crate) backend_id: String,
    pub(crate) max_concurrent: usize,
    pub(crate) max_queue_depth: usize,
    pub(crate) enabled: bool,
    pub(crate) probe_status: String,
    pub(crate) config_fingerprint: String,
}

impl BackendAdmissionConfig {
    pub(crate) fn from_backend(backend: &InferenceBackend) -> Result<Self> {
        if backend.max_concurrent < 1 {
            anyhow::bail!(
                "backend {} has invalid max_concurrent {}; expected >= 1",
                backend.backend_id,
                backend.max_concurrent
            );
        }
        if backend.max_queue_depth < 0 {
            anyhow::bail!(
                "backend {} has invalid max_queue_depth {}; expected >= 0",
                backend.backend_id,
                backend.max_queue_depth
            );
        }

        Ok(Self {
            backend_id: backend.backend_id.clone(),
            max_concurrent: backend.max_concurrent as usize,
            max_queue_depth: backend.max_queue_depth as usize,
            enabled: backend.enabled,
            probe_status: backend.probe_status.clone(),
            config_fingerprint: format!("{backend:?}"),
        })
    }

    pub(crate) fn is_available(&self) -> bool {
        self.enabled && self.probe_status == "healthy"
    }
}
