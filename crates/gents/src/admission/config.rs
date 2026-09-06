use std::collections::{HashMap, HashSet};

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::backend_registry::{InferenceBackend, HEALTHY_PROBE_STATUS};

/// Domain and version of the canonical resource identity encoding.
const BACKEND_CONFIG_FINGERPRINT_TAG: &str = "gents-backend-admission-config-v1";

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

        // Reuse the effective provider mapping and hash its resource fields
        // in a versioned, unambiguous encoding. Display/catalog metadata does
        // not replace controllers; availability remains separately owned.
        // Only the digest, never credential values, enters persisted call rows.
        let fields = backend.backend_fields();
        let fingerprint_inputs = (
            BACKEND_CONFIG_FINGERPRINT_TAG,
            &backend.backend_id,
            &fields.backend_provider_kind,
            &fields.openai_wire_api,
            &fields.backend_endpoint,
            &fields.backend_api_key,
            &fields.backend_api_key_env_var,
            backend.max_concurrent,
            backend.max_queue_depth,
        );
        let encoded = serde_json::to_vec(&fingerprint_inputs)?;
        let digest = Sha256::digest(&encoded);
        let config_fingerprint = format!("sha256:{digest:x}");

        Ok(Self {
            backend_id: backend.backend_id.clone(),
            max_concurrent: backend.max_concurrent as usize,
            max_queue_depth: backend.max_queue_depth as usize,
            enabled: backend.enabled,
            probe_status: backend.probe_status.clone(),
            measured_unhealthy: false,
            config_fingerprint,
        })
    }

    pub(crate) fn with_measured_unhealthy(mut self, measured_unhealthy: bool) -> Self {
        self.measured_unhealthy = measured_unhealthy;
        self
    }

    /// Effective availability: operator/bootstrap intent from the shared
    /// document AND the local measurement not vetoing — mirrors
    /// `Proofs.BackendHealth.effectiveAvailable` (B6). Names the failing
    /// term so callers can report *why* without recomputing the comparison
    /// — this is the single owner of that comparison in the codebase.
    pub(crate) fn availability(&self) -> BackendAvailability {
        if !self.enabled {
            BackendAvailability::Disabled
        } else if self.probe_status != HEALTHY_PROBE_STATUS {
            BackendAvailability::ProbeNotHealthy
        } else if self.measured_unhealthy {
            BackendAvailability::MeasuredUnhealthy
        } else {
            BackendAvailability::Available
        }
    }

    pub(crate) fn is_available(&self) -> bool {
        self.availability() == BackendAvailability::Available
    }
}

/// The term of [`BackendAdmissionConfig::availability`] that failed, so
/// callers that need to explain *why* a backend is unavailable don't
/// recompute `enabled`/`probe_status`/`measured_unhealthy` themselves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackendAvailability {
    Available,
    Disabled,
    ProbeNotHealthy,
    MeasuredUnhealthy,
}

/// Whether the *document* declares this backend enabled and healthy —
/// operator/bootstrap intent from raw `(enabled, probe_status)` fields, not
/// a live availability verdict: it has no `measured_unhealthy` term, so it
/// says nothing about this runtime's `BackendHealthMap` (#640). The one
/// caller is `gents diagnose`, which uses it to gate whether to even attempt
/// its own live reachability probe against an offline-exported config
/// blob (not a full `InferenceBackend`). Named "configured", not
/// "available", so it isn't mistaken for an admission decision; this file
/// stays the single owner of the `enabled`/`probe_status` comparison.
pub fn document_configured_from_fields(enabled: bool, probe_status: &str) -> bool {
    enabled && probe_status == HEALTHY_PROBE_STATUS
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_identity_normalizes_wire_defaults_and_protects_credentials() {
        let mut backend = InferenceBackend::from_value(&serde_json::json!({
            "backend_id": "resource-identity",
            "name": "Resource identity",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1/v1",
            "api_key": "fixture-only-secret",
            "max_concurrent": 2,
            "max_queue_depth": 0,
            "enabled": true,
            "probe_status": "healthy"
        }))
        .unwrap();
        let implicit = BackendAdmissionConfig::from_backend(&backend).unwrap();
        backend.openai_wire_api = Some(backend.backend_fields().openai_wire_api);
        assert_eq!(
            BackendAdmissionConfig::from_backend(&backend).unwrap(),
            implicit
        );
        assert!(!implicit.config_fingerprint.contains("fixture-only-secret"));

        backend.api_key = Some("rotated-fixture-only-secret".into());
        let rotated = BackendAdmissionConfig::from_backend(&backend).unwrap();
        assert_ne!(implicit.config_fingerprint, rotated.config_fingerprint);
        assert!(!rotated.config_fingerprint.contains("fixture-only-secret"));
    }

    fn config(
        enabled: bool,
        probe_status: &str,
        measured_unhealthy: bool,
    ) -> BackendAdmissionConfig {
        BackendAdmissionConfig {
            backend_id: "test".to_string(),
            max_concurrent: 1,
            max_queue_depth: 0,
            enabled,
            probe_status: probe_status.to_string(),
            measured_unhealthy,
            config_fingerprint: "test".to_string(),
        }
    }

    #[test]
    fn availability_requires_enabled_healthy_and_unvetoed() {
        assert_eq!(
            config(true, HEALTHY_PROBE_STATUS, false).availability(),
            BackendAvailability::Available
        );
        assert!(config(true, HEALTHY_PROBE_STATUS, false).is_available());

        assert_eq!(
            config(false, HEALTHY_PROBE_STATUS, false).availability(),
            BackendAvailability::Disabled
        );
        assert!(!config(false, HEALTHY_PROBE_STATUS, false).is_available());

        assert_eq!(
            config(true, "unhealthy", false).availability(),
            BackendAvailability::ProbeNotHealthy
        );
        assert!(!config(true, "unhealthy", false).is_available());

        assert_eq!(
            config(true, HEALTHY_PROBE_STATUS, true).availability(),
            BackendAvailability::MeasuredUnhealthy
        );
        assert!(!config(true, HEALTHY_PROBE_STATUS, true).is_available());

        // enabled=false wins over a bad probe_status: the disabled term is
        // checked first.
        assert_eq!(
            config(false, "unhealthy", true).availability(),
            BackendAvailability::Disabled
        );
    }
}
