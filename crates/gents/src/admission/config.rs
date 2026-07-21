use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::backend_registry::{InferenceBackend, HEALTHY_PROBE_STATUS};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendAdmissionConfig {
    pub backend_id: String,
    pub max_concurrent: usize,
    pub max_queue_depth: usize,
    pub enabled: bool,
    pub probe_status: String,
    /// THIS runtime's measured probe health vetoes routing (#640). Merged
    /// from the `BackendHealthMap` at snapshot resolution; `false` on paths
    /// that run before the prober has an opinion (startup, tests). Feeds
    /// `is_available()` and — via the snapshot configuration fingerprint —
    /// makes a measured-health flip propose a new generation even when no
    /// document changed.
    pub measured_unhealthy: bool,
    pub config_fingerprint: String,
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
            measured_unhealthy: false,
            config_fingerprint: format!("{backend:?}"),
        })
    }

    pub(crate) fn with_measured_unhealthy(mut self, measured_unhealthy: bool) -> Self {
        self.measured_unhealthy = measured_unhealthy;
        self
    }

    /// Effective availability: operator/bootstrap intent from the shared
    /// document AND the local measurement not vetoing — mirrors
    /// `Proofs.BackendHealth.effectiveAvailable` (B6).
    pub(crate) fn is_available(&self) -> bool {
        self.enabled && self.probe_status == HEALTHY_PROBE_STATUS && !self.measured_unhealthy
    }
}

pub(crate) fn backend_admission_configs_from_backends<'a>(
    backends: impl IntoIterator<Item = &'a InferenceBackend>,
    measured_vetoed: &HashSet<String>,
) -> Result<HashMap<String, BackendAdmissionConfig>> {
    let mut configs = HashMap::new();
    for backend in backends {
        configs.insert(
            backend.backend_id.clone(),
            BackendAdmissionConfig::from_backend(backend)?
                .with_measured_unhealthy(measured_vetoed.contains(&backend.backend_id)),
        );
    }
    Ok(configs)
}
