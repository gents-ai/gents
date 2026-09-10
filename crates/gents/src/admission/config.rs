use std::sync::OnceLock;

use anyhow::Result;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::backend_registry::{InferenceBackend, HEALTHY_PROBE_STATUS};

/// Domain and version of the canonical resource identity encoding.
const BACKEND_CONFIG_FINGERPRINT_TAG: &str = "gents-backend-admission-config-v1";
const PUBLIC_FINGERPRINT_PREFIX: &str = "hmac-sha256:process-v1:";

// Equality is needed only within a live registry. Never persist this key: a
// public salt or unkeyed digest would let readers test candidate credentials.
// Restarting changes attribution fingerprints; runtime_instance_id already
// scopes the associated controller ownership to that runtime.
static FINGERPRINT_KEY: OnceLock<[u8; 32]> = OnceLock::new();

fn keyed_fingerprint(encoded: &[u8], key: &[u8; 32]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 32-byte key");
    mac.update(encoded);
    format!(
        "{PUBLIC_FINGERPRINT_PREFIX}{:x}",
        mac.finalize().into_bytes()
    )
}

/// Only the current non-secret attribution format may leave the timeline.
/// Legacy Debug values contain inline credentials; older SHA-256 values permit
/// candidate-key confirmation. Repair of persisted history is tracked in #1394.
pub(crate) fn exportable_backend_fingerprint(value: Option<&str>) -> Option<String> {
    let value = value?;
    let digest = value.strip_prefix(PUBLIC_FINGERPRINT_PREFIX)?;
    (digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())).then(|| value.to_owned())
}

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
    pub(crate) fn from_backend(
        backend: &InferenceBackend,
        observation: &crate::document_config::InferenceBackendObservation,
    ) -> Result<Self> {
        backend.validate()?;
        anyhow::ensure!(
            backend.backend_id == observation.backend_id,
            "backend observation does not match configuration reference"
        );
        let max_concurrent = usize::try_from(backend.effective_max_concurrent())?;
        let max_queue_depth = usize::try_from(backend.effective_max_queue_depth())?;
        // The observation must come from the same owner-scoped document lookup.
        // Fingerprint connection identity, credentials and effective capacity;
        // catalogs and health remain separately owned observations.
        let fields = backend.backend_fields();
        let fingerprint_inputs = (
            BACKEND_CONFIG_FINGERPRINT_TAG,
            &backend.agent_did,
            &backend.backend_id,
            &fields.backend_provider_kind,
            &fields.openai_wire_api,
            &fields.backend_endpoint,
            &fields.backend_auth,
            backend.connect_timeout_secs.unwrap_or(10),
            max_concurrent,
            max_queue_depth,
        );
        let encoded = serde_json::to_vec(&fingerprint_inputs)?;
        let config_fingerprint = keyed_fingerprint(
            &encoded,
            FINGERPRINT_KEY.get_or_init(rand::random::<[u8; 32]>),
        );
        Ok(Self {
            backend_id: backend.backend_id.clone(),
            max_concurrent,
            max_queue_depth,
            enabled: backend.enabled,
            probe_status: observation
                .probe_status
                .clone()
                .unwrap_or_else(|| crate::backend_registry::UNKNOWN_PROBE_STATUS.into()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribution_is_process_keyed_and_legacy_values_are_not_exported() {
        let bytes = b"synthetic-credential-and-config";
        let first = keyed_fingerprint(bytes, &[1; 32]);
        assert_eq!(first, keyed_fingerprint(bytes, &[1; 32]));
        assert_ne!(first, keyed_fingerprint(bytes, &[2; 32]));
        assert_ne!(
            first,
            keyed_fingerprint(b"rotated-synthetic-credential", &[1; 32])
        );
        assert_eq!(exportable_backend_fingerprint(Some(&first)), Some(first));
        for legacy in [
            "InferenceBackend { api_key: Some(\"synthetic-secret\") }",
            "sha256:abc",
            "hmac-sha256:process-v1:synthetic-secret",
            "",
        ] {
            assert_eq!(exportable_backend_fingerprint(Some(legacy)), None);
        }
    }

    #[test]
    fn resource_identity_normalizes_wire_defaults_and_protects_credentials() {
        let mut backend = InferenceBackend::from_value(&serde_json::json!({
            "backend_id": "resource-identity",
            "agent_did": "did:test:admission",
            "name": "Resource identity",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1/v1",
            "auth": {"kind":"api_key", "key":"fixture-only-secret"},
            "max_concurrent": 2,
            "max_queue_depth": 0,
            "enabled": true
        }))
        .unwrap();
        let observation = crate::document_config::InferenceBackendObservation {
            backend_id: backend.backend_id.clone(),
            catalogs: Vec::new(),
            probe_status: Some(HEALTHY_PROBE_STATUS.into()),
            last_probe: None,
        };
        let implicit = BackendAdmissionConfig::from_backend(&backend, &observation).unwrap();
        backend.openai_wire_api = Some(backend.backend_fields().openai_wire_api);
        assert_eq!(
            BackendAdmissionConfig::from_backend(&backend, &observation).unwrap(),
            implicit
        );
        assert!(!implicit.config_fingerprint.contains("fixture-only-secret"));

        backend.auth = crate::document_config::BackendAuth::ApiKey {
            key: "rotated-fixture-only-secret".into(),
        };
        let rotated = BackendAdmissionConfig::from_backend(&backend, &observation).unwrap();
        assert_ne!(implicit.config_fingerprint, rotated.config_fingerprint);
        assert!(!rotated.config_fingerprint.contains("fixture-only-secret"));

        // Lean Registry.Config.key includes queue capacity. Its separately
        // modeled capacity is the semaphore, not the entire resource identity.
        backend.max_queue_depth = Some(3);
        let resized_queue = BackendAdmissionConfig::from_backend(&backend, &observation).unwrap();
        assert_ne!(rotated.config_fingerprint, resized_queue.config_fingerprint);
        assert_eq!(rotated.max_concurrent, resized_queue.max_concurrent);
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
