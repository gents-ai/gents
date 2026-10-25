use std::sync::OnceLock;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::backend_registry::{
    BackendFields, BackendRegistryError, InferenceBackend, HEALTHY_PROBE_STATUS,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum BackendAdmissionConfigError {
    #[error(transparent)]
    Backend(#[from] BackendRegistryError),
    #[error("backend observation does not match configuration reference")]
    ObservationMismatch,
    #[error(transparent)]
    Capacity(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    Fingerprint(#[from] serde_json::Error),
}

const BACKEND_CONFIG_FINGERPRINT_TAG: &str = "gents-backend-admission-config-v1";
const BACKEND_CONNECTION_FINGERPRINT_TAG: &str = "gents-backend-connection-v1";
const PUBLIC_FINGERPRINT_PREFIX: &str = "hmac-sha256:process-v1:";

// Never persist this key: a public salt or unkeyed digest would let readers
// test candidate credentials.
static FINGERPRINT_KEY: OnceLock<[u8; 32]> = OnceLock::new();

fn keyed_fingerprint(encoded: &[u8], key: &[u8; 32]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts a 32-byte key");
    mac.update(encoded);
    format!(
        "{PUBLIC_FINGERPRINT_PREFIX}{:x}",
        mac.finalize().into_bytes()
    )
}

fn process_keyed_fingerprint(encoded: &[u8]) -> String {
    keyed_fingerprint(
        encoded,
        FINGERPRINT_KEY.get_or_init(rand::random::<[u8; 32]>),
    )
}

/// The identity a provider client is built from; compared against the
/// admitting controller's (Lean `InferenceCall.Registry.Config.connection`).
pub(crate) fn backend_connection_fingerprint(fields: &BackendFields) -> String {
    let encoded = serde_json::to_vec(&(
        BACKEND_CONNECTION_FINGERPRINT_TAG,
        &fields.backend_id,
        &fields.backend_provider_kind,
        &fields.openai_wire_api,
        &fields.backend_endpoint,
        &fields.backend_auth,
    ))
    .expect("backend connection fields serialize");
    process_keyed_fingerprint(&encoded)
}

/// Only the current non-secret attribution format may leave the timeline.
/// Legacy Debug values contain inline credentials.
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
    /// THIS runtime's measured probe health vetoes routing. `false` on paths
    /// that run before the prober has an opinion.
    pub measured_unhealthy: bool,
    pub config_fingerprint: String,
}

impl BackendAdmissionConfig {
    pub(crate) fn from_backend(
        backend: &InferenceBackend,
        observation: &crate::document_config::InferenceBackendObservation,
    ) -> Result<Self, BackendAdmissionConfigError> {
        backend.validate()?;
        if backend.backend_id != observation.backend_id {
            return Err(BackendAdmissionConfigError::ObservationMismatch);
        }
        let max_concurrent = usize::try_from(backend.effective_max_concurrent())?;
        let max_queue_depth = usize::try_from(backend.effective_max_queue_depth())?;
        // The observation must come from the same owner-scoped document lookup.
        let fields = backend.backend_fields();
        let fingerprint_inputs = (
            BACKEND_CONFIG_FINGERPRINT_TAG,
            &backend.node_did,
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
        let config_fingerprint = process_keyed_fingerprint(&encoded);
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

    /// Effective availability: operator/bootstrap intent AND the local
    /// measurement not vetoing — mirrors `Proofs.BackendHealth.effectiveAvailable`
    /// (B6); the single owner of that comparison in the codebase.
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

/// The term of [`BackendAdmissionConfig::availability`] that failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendAvailability {
    Available,
    Disabled,
    ProbeNotHealthy,
    MeasuredUnhealthy,
}

/// Whether the *document* declares this backend enabled and healthy — not a
/// live availability verdict. Named "configured", not "available", so it
/// isn't mistaken for an admission decision.
pub fn document_configured_from_fields(enabled: bool, probe_status: &str) -> bool {
    enabled && probe_status == HEALTHY_PROBE_STATUS
}
