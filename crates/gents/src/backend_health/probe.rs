use rapidhash::{HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio_util::sync::CancellationToken;

use crate::backend_registry::{InferenceBackend, UNKNOWN_PROBE_STATUS};

use super::oauth::{oauth_credential_for_probe, OAuthProbeContext};
use super::{step_backend, BackendHealthEntry, BackendHealthMap, BackendProberOptions, ProbeEvent};

#[derive(Debug, Default)]
pub struct ProbeCycleOutcome {
    pub flipped: Vec<String>,
    pub promotable: Vec<String>,
}

#[derive(Default)]
pub(super) struct ProbeSchedule {
    pub(super) known: RapidHashMap<String, InferenceBackend>,
}

impl ProbeSchedule {
    pub(super) fn select(
        &mut self,
        backends: &[InferenceBackend],
        periodic: bool,
    ) -> RapidHashSet<String> {
        let due = backends
            .iter()
            .filter(|backend| backend.enabled)
            .filter(|backend| {
                periodic
                    || self
                        .known
                        .get(&backend.backend_id)
                        .is_none_or(|previous| !same_probe_inputs(previous, backend))
            })
            .map(|backend| backend.backend_id.clone())
            .collect();
        self.known = backends
            .iter()
            .filter(|backend| backend.enabled)
            .map(|backend| (backend.backend_id.clone(), backend.clone()))
            .collect();
        due
    }
}

pub(super) fn probe_configuration_fingerprint(backend: &InferenceBackend) -> String {
    let connection = crate::admission::backend_connection_fingerprint(&backend.backend_fields());
    format!(
        "{connection}:connect={:?}:discovery={:?}",
        backend.connect_timeout_secs, backend.discovery_timeout_secs
    )
}

fn same_probe_inputs(previous: &InferenceBackend, current: &InferenceBackend) -> bool {
    previous.provider_kind == current.provider_kind
        && previous.openai_wire_api == current.openai_wire_api
        && previous.endpoint == current.endpoint
        && previous.auth == current.auth
        && previous.connect_timeout_secs == current.connect_timeout_secs
        && previous.discovery_timeout_secs == current.discovery_timeout_secs
}

pub(super) fn probe_timeout(backend: &InferenceBackend) -> Duration {
    Duration::from_secs(backend.discovery_timeout_secs.unwrap_or(10).max(1) as u64)
}

async fn record_discovered_catalog(
    node: &Arc<EmbeddedNode>,
    backend: &InferenceBackend,
    node_did: Option<&str>,
    models: Vec<crate::document_config::AdvertisedModel>,
) {
    let catalog = crate::document_config::BackendModelCatalog {
        node_did: node_did.map(str::to_string),
        observed_at: Utc::now().to_rfc3339(),
        models,
    };
    if let Err(error) = crate::backend_registry::record_model_catalog(node, backend, catalog).await
    {
        tracing::warn!(
            node_did = %backend.node_did,
            backend_id = %backend.backend_id,
            credential_scope = node_did.unwrap_or("shared"),
            error = %error,
            "backend probe: discovered models but could not record the catalog; \
             previous observation preserved"
        );
    }
}

pub async fn probe_backends_cycle(
    node: &Arc<EmbeddedNode>,
    client: &reqwest::Client,
    backends: &[InferenceBackend],
    now: DateTime<Utc>,
    health_map: &BackendHealthMap,
    options: &BackendProberOptions,
    oauth: Option<OAuthProbeContext<'_>>,
) -> ProbeCycleOutcome {
    let env = ProbeEnv {
        now,
        health_map,
        options,
        cancel: None,
    };
    probe_selected_backends(node, client, backends, env, oauth, None).await
}

#[derive(Clone, Copy)]
pub(super) struct ProbeEnv<'a> {
    pub(super) now: DateTime<Utc>,
    pub(super) health_map: &'a BackendHealthMap,
    pub(super) options: &'a BackendProberOptions,
    pub(super) cancel: Option<&'a CancellationToken>,
}

/// Shutdown cancels remote observation, never an admitted catalog write. An
/// interrupted observation supplies no health event or complete probe-cycle view.
pub(super) async fn probe_selected_backends(
    node: &Arc<EmbeddedNode>,
    client: &reqwest::Client,
    backends: &[InferenceBackend],
    env: ProbeEnv<'_>,
    oauth: Option<OAuthProbeContext<'_>>,
    selected: Option<&RapidHashSet<String>>,
) -> ProbeCycleOutcome {
    let ProbeEnv { health_map, .. } = env;
    let mut outcome = ProbeCycleOutcome::default();
    let mut probed_ids = RapidHashSet::new();

    for backend in backends {
        if env.cancel.is_some_and(CancellationToken::is_cancelled) {
            return outcome;
        }
        if backend.provider_kind.is_node_scoped_oauth() && oauth.is_none() {
            continue;
        }
        probed_ids.insert(backend.backend_id.clone());
        if selected.is_some_and(|due| !due.contains(&backend.backend_id)) {
            continue;
        }
        if backend.provider_kind.is_node_scoped_oauth() {
            let Some(context) = oauth.as_ref() else {
                continue;
            };
            let credential = oauth_credential_for_probe(context, backend).await;
            if env.cancel.is_some_and(CancellationToken::is_cancelled) {
                return outcome;
            }
            let (event, error_text) = match credential {
                Ok(credential) => {
                    let cancellation = env.cancel.cloned().unwrap_or_default();
                    let Some(probe_result) = cancellation
                        .run_until_cancelled(tokio::time::timeout(
                            probe_timeout(backend),
                            crate::backend_provider::discover_models(
                                client,
                                backend.provider_kind,
                                &backend.endpoint,
                                None,
                                Some(&credential),
                            ),
                        ))
                        .await
                    else {
                        return outcome;
                    };
                    if cancellation.is_cancelled() {
                        return outcome;
                    }
                    match probe_result {
                        Ok(Ok(models)) => {
                            record_discovered_catalog(
                                &context.node,
                                backend,
                                Some(context.node_did),
                                models,
                            )
                            .await;
                            (ProbeEvent::ProbeSuccess, None)
                        }
                        Ok(Err(error)) => (ProbeEvent::ProbeFail, Some(error.to_string())),
                        Err(_) => (ProbeEvent::ProbeFail, Some("probe timed out".to_string())),
                    }
                }
                Err(error) => {
                    use crate::inference::auth::credential::{
                        OAuthAuthProblem, OAuthCredentialError,
                    };
                    let event = match &error.source {
                        OAuthCredentialError::Auth(auth) => match &auth.problem {
                            OAuthAuthProblem::Missing
                            | OAuthAuthProblem::WrongMode { .. }
                            | OAuthAuthProblem::Expired => ProbeEvent::CredentialsRejected,
                            OAuthAuthProblem::NotEntitled => ProbeEvent::SubscriptionNotEntitled,
                            OAuthAuthProblem::Other(_) => ProbeEvent::ProbeFail,
                        },
                        OAuthCredentialError::Other(_) => ProbeEvent::ProbeFail,
                    };
                    (event, Some(format!("{error:#}")))
                }
            };
            record_probe_event(node, backend, event, error_text, env, &mut outcome).await;
            continue;
        }

        let (event, error_text) = match backend.auth.resolve_api_key() {
            Err(error) => (ProbeEvent::ProbeFail, Some(error.to_string())),
            Ok(api_key) => {
                let cancellation = env.cancel.cloned().unwrap_or_default();
                let Some(probe_result) = cancellation
                    .run_until_cancelled(tokio::time::timeout(
                        probe_timeout(backend),
                        crate::backend_provider::discover_models(
                            client,
                            backend.provider_kind,
                            &backend.endpoint,
                            api_key.as_deref(),
                            None,
                        ),
                    ))
                    .await
                else {
                    return outcome;
                };

                if cancellation.is_cancelled() {
                    return outcome;
                }
                match probe_result {
                    Ok(Ok(models)) => {
                        record_discovered_catalog(node, backend, None, models).await;
                        (ProbeEvent::ProbeSuccess, None)
                    }
                    Ok(Err(error)) => (ProbeEvent::ProbeFail, Some(error.to_string())),
                    Err(_) => (ProbeEvent::ProbeFail, Some("probe timed out".to_string())),
                }
            }
        };

        record_probe_event(node, backend, event, error_text, env, &mut outcome).await;
    }

    if env.cancel.is_none_or(|cancel| !cancel.is_cancelled()) {
        health_map.retain_backends(&probed_ids).await;
    }
    outcome
}

pub(super) async fn record_probe_event(
    node: &Arc<EmbeddedNode>,
    backend: &InferenceBackend,
    event: ProbeEvent,
    error_text: Option<String>,
    env: ProbeEnv<'_>,
    outcome: &mut ProbeCycleOutcome,
) {
    let ProbeEnv {
        now,
        health_map,
        options,
        ..
    } = env;
    let fingerprint = probe_configuration_fingerprint(backend);
    let (previous, observed_state) = health_map
        .get_model(
            &backend.backend_id,
            &fingerprint,
            options.failure_threshold_k,
        )
        .await;
    let next = step_backend(previous, event, options.failure_threshold_k);
    let veto_flipped = observed_state.blocks_routing() != next.0.blocks_routing();

    if veto_flipped || observed_state.readiness_reason() != next.0.readiness_reason() {
        tracing::warn!(
            backend_id = %backend.backend_id,
            endpoint = %backend.endpoint,
            previous_state = %observed_state.as_str(),
            next_state = %next.0.as_str(),
            failure_count = next.1,
            error = error_text.as_deref().unwrap_or(""),
            "backend probe: routing availability or account recovery changed"
        );
        outcome.flipped.push(backend.backend_id.clone());
    } else if event == ProbeEvent::ProbeFail {
        tracing::debug!(
            backend_id = %backend.backend_id,
            endpoint = %backend.endpoint,
            state = %next.0.as_str(),
            failure_count = next.1,
            error = error_text.as_deref().unwrap_or(""),
            "backend probe failed"
        );
    }

    if event == ProbeEvent::ProbeSuccess {
        match crate::backend_registry::lookup_backend_observation(
            node,
            &backend.node_did,
            &backend.backend_id,
        )
        .await
        {
            Ok(Some(observation))
                if observation
                    .probe_status
                    .as_deref()
                    .unwrap_or(UNKNOWN_PROBE_STATUS)
                    == UNKNOWN_PROBE_STATUS =>
            {
                outcome.promotable.push(backend.backend_id.clone());
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(node_did = %backend.node_did,
                backend_id = %backend.backend_id, %error,
                "backend probe: could not read promotion observation"),
        }
    }

    health_map
        .set_entry(
            backend.backend_id.clone(),
            BackendHealthEntry {
                state: next.0,
                failure_count: next.1,
                last_probe_at: now,
                last_error: error_text,
                configuration_fingerprint: Some(fingerprint),
            },
        )
        .await;
}
