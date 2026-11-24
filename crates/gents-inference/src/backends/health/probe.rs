use rapidhash::{HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use tokio_util::sync::CancellationToken;

use crate::backends::registry::backend_fields;
use gents_documents::inference_backend::UNKNOWN_PROBE_STATUS;
use gents_documents::InferenceBackend;

use super::oauth::{oauth_credential_for_probe, OAuthProbeContext};
use super::{
    bounded_probe_error, probe_event, step_backend, AdmittedCallProgress, BackendHealthEntry,
    BackendHealthMap, BackendProberOptions, ProbeEvent, ProbeOutcome,
};

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
    let connection = crate::admission::backend_connection_fingerprint(&backend_fields(backend));
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
        && previous.client_version == current.client_version
        && previous.connect_timeout_secs == current.connect_timeout_secs
        && previous.discovery_timeout_secs == current.discovery_timeout_secs
}

/// The probe's deadline: the backend's discovery timeout, never beyond the
/// prober's own. It is the probe's only timeout, so a probe that runs out of
/// time ends as a timeout rather than as a transport error.
pub(super) fn probe_timeout(
    backend: &InferenceBackend,
    options: &BackendProberOptions,
) -> Duration {
    Duration::from_secs(backend.effective_discovery_timeout_secs().max(1) as u64)
        .min(options.probe_timeout)
}

enum Probed {
    Answered(Vec<gents_documents::AdvertisedModel>),
    Failed(anyhow::Error),
    TimedOut { admitted_call_progressed: bool },
}

impl Probed {
    fn outcome(&self) -> (ProbeOutcome, bool) {
        match self {
            Self::Answered(_) => (ProbeOutcome::Answered, false),
            Self::Failed(_) => (ProbeOutcome::Failed, false),
            Self::TimedOut {
                admitted_call_progressed,
            } => (ProbeOutcome::TimedOut, *admitted_call_progressed),
        }
    }
}

/// One models-endpoint probe under its deadline; `None` once shutdown cancels
/// it. A timeout reports whether a call admitted to the backend received a
/// response item while the probe ran.
async fn probe_models<F>(
    env: ProbeEnv<'_>,
    backend: &InferenceBackend,
    discover: F,
) -> Option<Probed>
where
    F: std::future::Future<Output = anyhow::Result<Vec<gents_documents::AdvertisedModel>>>,
{
    let mark = env
        .progress
        .map(|progress| (progress, progress.progress_mark(&backend.backend_id)));
    let cancellation = env.cancel.cloned().unwrap_or_default();
    let result = cancellation
        .run_until_cancelled(tokio::time::timeout(
            probe_timeout(backend, env.options),
            discover,
        ))
        .await?;
    if cancellation.is_cancelled() {
        return None;
    }
    Some(match result {
        Ok(Ok(models)) => Probed::Answered(models),
        Ok(Err(error)) => Probed::Failed(error),
        Err(_) => Probed::TimedOut {
            admitted_call_progressed: mark
                .is_some_and(|(progress, mark)| progress.progress_mark(&backend.backend_id) > mark),
        },
    })
}

async fn record_discovered_catalog(
    node: &Arc<EmbeddedNode>,
    backend: &InferenceBackend,
    doc: &str,
    node_did: Option<&str>,
    models: Vec<gents_documents::AdvertisedModel>,
) {
    let catalog = gents_documents::BackendModelCatalog {
        node_did: node_did.map(str::to_string),
        observed_at: Utc::now().to_rfc3339(),
        models,
    };
    if let Err(error) =
        crate::backends::registry::record_model_catalog_for_document(node, backend, doc, catalog)
            .await
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
        progress: None,
    };
    probe_selected_backends(node, client, backends, env, oauth, None).await
}

#[derive(Clone, Copy)]
pub(super) struct ProbeEnv<'a> {
    pub(super) now: DateTime<Utc>,
    pub(super) health_map: &'a BackendHealthMap,
    pub(super) options: &'a BackendProberOptions,
    pub(super) cancel: Option<&'a CancellationToken>,
    pub(super) progress: Option<&'a dyn AdmittedCallProgress>,
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
        if backend.auth.uses_node_oauth() && oauth.is_none() {
            continue;
        }
        probed_ids.insert(backend.backend_id.clone());
        if selected.is_some_and(|due| !due.contains(&backend.backend_id)) {
            continue;
        }
        let Ok(doc) = crate::backends::registry::capture_backend_document(node, backend).await
        else {
            continue;
        };
        if backend.auth.uses_node_oauth() {
            let Some(context) = oauth.as_ref() else {
                continue;
            };
            let credential = oauth_credential_for_probe(context, backend).await;
            if env.cancel.is_some_and(CancellationToken::is_cancelled) {
                return outcome;
            }
            let (event, error_text) = match credential {
                Ok(credential) => {
                    let Some(probed) = probe_models(
                        env,
                        backend,
                        crate::backends::provider::discover_models(
                            client,
                            backend.provider_kind,
                            &backend.endpoint,
                            None,
                            Some(&credential),
                            backend.client_version.as_deref(),
                        ),
                    )
                    .await
                    else {
                        return outcome;
                    };
                    let (probe, progressed) = probed.outcome();
                    let error_text = match probed {
                        Probed::Answered(models) => {
                            record_discovered_catalog(
                                &context.node,
                                backend,
                                &doc,
                                Some(context.node_did),
                                models,
                            )
                            .await;
                            None
                        }
                        Probed::Failed(error) => Some(error.to_string()),
                        Probed::TimedOut { .. } => Some("probe timed out".to_string()),
                    };
                    (probe_event(probe, progressed), error_text)
                }
                Err(error) => {
                    use crate::auth::credential::{OAuthAuthProblem, OAuthCredentialError};
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
                    (Some(event), Some(format!("{error:#}")))
                }
            };
            record_probe_result(node, backend, event, error_text, env, &mut outcome).await;
            continue;
        }

        let resolved_auth = crate::auth::credential::resolve_runtime_backend_auth(
            node,
            &backend.node_did,
            backend.provider_kind,
            &backend.endpoint,
            &backend.auth,
        )
        .await;
        let private_client = if matches!(
            &backend.auth,
            gents_documents::BackendAuth::Credential { .. }
        ) {
            match reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
            {
                Ok(client) => Some(client),
                Err(error) => {
                    record_probe_event(
                        node,
                        backend,
                        ProbeEvent::ProbeFail,
                        Some(error.to_string()),
                        env,
                        &mut outcome,
                    )
                    .await;
                    continue;
                }
            }
        } else {
            None
        };
        let client = private_client.as_ref().unwrap_or(client);
        let (event, error_text) = match resolved_auth.and_then(|auth| auth.probe_key()) {
            Err(error) => (Some(ProbeEvent::ProbeFail), Some(error.to_string())),
            Ok(api_key) => {
                let Some(probed) = probe_models(
                    env,
                    backend,
                    crate::backends::provider::discover_models(
                        client,
                        backend.provider_kind,
                        &backend.endpoint,
                        api_key.as_deref(),
                        None,
                        backend.client_version.as_deref(),
                    ),
                )
                .await
                else {
                    return outcome;
                };
                let (probe, progressed) = probed.outcome();
                let error_text = match probed {
                    Probed::Answered(models) => {
                        record_discovered_catalog(node, backend, &doc, None, models).await;
                        None
                    }
                    Probed::Failed(error) => Some(
                        if matches!(
                            &backend.auth,
                            gents_documents::BackendAuth::Credential { .. }
                        ) {
                            "backend discovery failed".to_owned()
                        } else {
                            error.to_string()
                        },
                    ),
                    Probed::TimedOut { .. } => Some("probe timed out".to_string()),
                };
                (probe_event(probe, progressed), error_text)
            }
        };

        record_probe_result(node, backend, event, error_text, env, &mut outcome).await;
    }

    if env.cancel.is_none_or(|cancel| !cancel.is_cancelled()) {
        health_map.retain_backends(&probed_ids).await;
    }
    outcome
}

/// Records the event a probe supplied; a probe that supplied none (its models
/// endpoint timed out while an admitted call received responses) leaves the
/// backend's measurement as it was.
async fn record_probe_result(
    node: &Arc<EmbeddedNode>,
    backend: &InferenceBackend,
    event: Option<ProbeEvent>,
    error_text: Option<String>,
    env: ProbeEnv<'_>,
    outcome: &mut ProbeCycleOutcome,
) {
    match event {
        Some(event) => record_probe_event(node, backend, event, error_text, env, outcome).await,
        None => tracing::info!(
            backend_id = %backend.backend_id,
            endpoint = %backend.endpoint,
            "backend probe: models endpoint timed out while an admitted call received responses; \
             health unchanged"
        ),
    }
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
    let error_text = error_text.map(bounded_probe_error);
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
        match crate::backends::registry::lookup_backend_observation_for_runtime(
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
