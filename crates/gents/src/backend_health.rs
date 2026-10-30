//! Scheduled inference-backend prober.
//!
//! Measured reachability is deliberately NOT persisted to the fleet-replicated
//! `InferenceBackend` document: the shared document's `probe_status` stays the
//! operator/bootstrap intent knob, and the prober's only doc write is the
//! recurring `unknown → healthy` promotion.

use rapidhash::{RapidHashMap, RapidHashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use crate::backend_registry::InferenceBackend;

pub(crate) mod oauth;
mod probe;
mod prober;
#[cfg(test)]
mod tests;

pub use oauth::OAuthProbeContext;
pub use probe::{probe_backends_cycle, ProbeCycleOutcome};
pub use prober::{run_backend_probe_cycle, spawn_backend_prober};

use probe::probe_configuration_fingerprint;

/// Node-local health observation settings, independent of an agent's
/// inference execution and provider retry policy.
#[derive(Clone, Debug)]
pub struct BackendProberOptions {
    pub probe_interval: Duration,
    pub probe_timeout: Duration,
    pub failure_threshold_k: u32,
}

impl Default for BackendProberOptions {
    fn default() -> Self {
        Self {
            probe_interval: Duration::from_secs(60),
            probe_timeout: Duration::from_secs(10),
            failure_threshold_k: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendHealthState {
    Unknown,
    Healthy,
    Degraded,
    Unhealthy,
    CredentialsRequired,
    SubscriptionNotEntitled,
}

impl BackendHealthState {
    pub fn blocks_routing(self) -> bool {
        matches!(
            self,
            Self::Unhealthy | Self::CredentialsRequired | Self::SubscriptionNotEntitled
        )
    }

    pub(crate) fn readiness_reason(
        self,
    ) -> Option<gents_protocol::node_readiness::AgentReadinessUnavailableReason> {
        use gents_protocol::node_readiness::AgentReadinessUnavailableReason as Reason;
        match self {
            Self::CredentialsRequired => Some(Reason::CredentialsRequired),
            Self::SubscriptionNotEntitled => Some(Reason::SubscriptionNotEntitled),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
            Self::CredentialsRequired => "credentials_required",
            Self::SubscriptionNotEntitled => "subscription_not_entitled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProbeEvent {
    ProbeSuccess,
    ProbeFail,
    CredentialsRejected,
    SubscriptionNotEntitled,
    ConfigurationChanged,
}

#[derive(Debug, Clone)]
pub(super) struct BackendHealthEntry {
    pub(super) state: BackendHealthState,
    pub(super) failure_count: u32,
    pub(super) last_probe_at: DateTime<Utc>,
    pub(super) last_error: Option<String>,
    pub(super) configuration_fingerprint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BackendHealthSnapshot {
    pub backend_id: String,
    pub state: BackendHealthState,
    pub failure_count: u32,
    pub last_probe_at: DateTime<Utc>,
    pub last_error: Option<String>,
    configuration_fingerprint: Option<String>,
}

impl BackendHealthSnapshot {
    pub(crate) fn state_for(&self, backend: &InferenceBackend) -> Option<BackendHealthState> {
        let fingerprint = probe_configuration_fingerprint(backend);
        self.configuration_fingerprint
            .as_ref()
            .is_none_or(|measured| measured == &fingerprint)
            .then_some(self.state)
    }
}

#[derive(Clone, Default)]
pub struct BackendHealthMap {
    inner: Arc<RwLock<RapidHashMap<String, BackendHealthEntry>>>,
}

impl BackendHealthMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, backend_id: &str) -> Option<BackendHealthSnapshot> {
        self.inner
            .read()
            .await
            .get(backend_id)
            .map(|entry| snapshot_from_entry(backend_id, entry))
    }

    pub async fn snapshot(&self) -> RapidHashMap<String, BackendHealthSnapshot> {
        self.inner
            .read()
            .await
            .iter()
            .map(|(backend_id, entry)| (backend_id.clone(), snapshot_from_entry(backend_id, entry)))
            .collect()
    }

    pub async fn vetoed_backend_ids(&self) -> RapidHashSet<String> {
        self.inner
            .read()
            .await
            .iter()
            .filter(|(_, entry)| entry.state.blocks_routing())
            .map(|(backend_id, _)| backend_id.clone())
            .collect()
    }

    pub async fn measured_blocks_routing(&self, backend_id: &str) -> bool {
        self.inner
            .read()
            .await
            .get(backend_id)
            .is_some_and(|entry| entry.state.blocks_routing())
    }

    pub(super) async fn get_model(
        &self,
        backend_id: &str,
        fingerprint: &str,
        threshold_k: u32,
    ) -> ((BackendHealthState, u32), BackendHealthState) {
        self.inner
            .read()
            .await
            .get(backend_id)
            .map(|entry| {
                let previous = (entry.state, entry.failure_count);
                let model = if entry
                    .configuration_fingerprint
                    .as_ref()
                    .is_some_and(|measured| measured != fingerprint)
                {
                    step_backend(previous, ProbeEvent::ConfigurationChanged, threshold_k)
                } else {
                    previous
                };
                (model, entry.state)
            })
            .unwrap_or((
                (BackendHealthState::Unknown, 0),
                BackendHealthState::Unknown,
            ))
    }

    pub(super) async fn set_entry(&self, backend_id: String, entry: BackendHealthEntry) {
        self.inner.write().await.insert(backend_id, entry);
    }

    pub(super) async fn retain_backends(&self, backend_ids: &RapidHashSet<String>) {
        self.inner
            .write()
            .await
            .retain(|backend_id, _| backend_ids.contains(backend_id));
    }

    #[cfg(test)]
    pub(crate) async fn set_for_test(
        &self,
        backend_id: impl Into<String>,
        state: BackendHealthState,
        failure_count: u32,
    ) {
        self.set_entry(
            backend_id.into(),
            BackendHealthEntry {
                state,
                failure_count,
                last_probe_at: Utc::now(),
                last_error: None,
                configuration_fingerprint: None,
            },
        )
        .await;
    }
}

fn snapshot_from_entry(backend_id: &str, entry: &BackendHealthEntry) -> BackendHealthSnapshot {
    BackendHealthSnapshot {
        backend_id: backend_id.to_string(),
        state: entry.state,
        failure_count: entry.failure_count,
        last_probe_at: entry.last_probe_at,
        last_error: entry.last_error.clone(),
        configuration_fingerprint: entry.configuration_fingerprint.clone(),
    }
}

pub(super) fn step_backend(
    prev: (BackendHealthState, u32),
    event: ProbeEvent,
    threshold_k: u32,
) -> (BackendHealthState, u32) {
    let threshold_k = threshold_k.max(1);
    match event {
        ProbeEvent::ProbeSuccess => (BackendHealthState::Healthy, 0),
        ProbeEvent::CredentialsRejected => (
            BackendHealthState::CredentialsRequired,
            prev.1.saturating_add(1),
        ),
        ProbeEvent::SubscriptionNotEntitled => (
            BackendHealthState::SubscriptionNotEntitled,
            prev.1.saturating_add(1),
        ),
        ProbeEvent::ConfigurationChanged => (BackendHealthState::Unknown, 0),
        ProbeEvent::ProbeFail => {
            let n = prev.1.saturating_add(1);
            if prev.0.readiness_reason().is_some() {
                (prev.0, n)
            } else if n >= threshold_k {
                (BackendHealthState::Unhealthy, n)
            } else {
                (BackendHealthState::Degraded, n)
            }
        }
    }
}
