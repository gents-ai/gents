mod cycle;
mod oauth;

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{routing::get, Json, Router};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::probe::{probe_selected_backends, record_probe_event, ProbeEnv, ProbeSchedule};
use super::prober::run_scheduled_backend_probe_cycle;
use super::*;
use crate::auth::credential::test_support::{
    one_shot_token_server, seed_credential, seed_credential_with_refresh_token, test_node,
    TOKEN_URL_ENV,
};
use gents_documents::inference_defaults::DEFAULT_MAX_QUEUE_DEPTH;

fn probe_event(event: &str) -> ProbeEvent {
    match event {
        "probeSuccess" => ProbeEvent::ProbeSuccess,
        "probeFail" => ProbeEvent::ProbeFail,
        "credentialsRejected" => ProbeEvent::CredentialsRejected,
        "subscriptionNotEntitled" => ProbeEvent::SubscriptionNotEntitled,
        "configurationChanged" => ProbeEvent::ConfigurationChanged,
        other => panic!("unknown probe event {other:?}"),
    }
}

#[tokio::test]
async fn every_health_measurement_notifies_parked_request_watchers() {
    let health = BackendHealthMap::new();
    let mut observations = health.subscribe_observations();
    health
        .set_for_test("backend", BackendHealthState::Healthy, 0)
        .await;
    observations.changed().await.unwrap();
    health
        .set_for_test("backend", BackendHealthState::Healthy, 0)
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(1), observations.changed())
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn backend_health_transitions_follow_the_lean_model() {
    let cases = gents_lean_vocab::families::contracts_backend_health_transition::cases::<
        ProbeEvent,
        BackendHealthState,
    >();
    assert!(cases.iter().any(|case| {
        case.input.threshold_k == u64::from(BackendProberOptions::default().failure_threshold_k)
    }));
    for state in [
        BackendHealthState::Unknown,
        BackendHealthState::Healthy,
        BackendHealthState::Degraded,
        BackendHealthState::Unhealthy,
        BackendHealthState::CredentialsRequired,
        BackendHealthState::SubscriptionNotEntitled,
    ] {
        assert!(
            cases.iter().any(|case| case.input.start_state == state),
            "no modeled transition starts from {state:?}"
        );
    }
    for case in cases {
        let (next_state, next_count) = step_backend(
            (
                case.input.start_state,
                u32::try_from(case.input.start_count).unwrap(),
            ),
            case.input.event,
            u32::try_from(case.input.threshold_k).unwrap(),
        );
        case.check(
            gents_lean_vocab::families::contracts_backend_health_transition::Output {
                next_state,
                next_count: u64::from(next_count),
                blocks_routing: next_state.blocks_routing(),
                readiness_reason: next_state.readiness_reason().map(|reason| {
                    serde_json::to_value(reason)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned()
                }),
            },
        );
    }
}

#[test]
fn generated_backend_probe_schedule_cases_match_runtime_selection() {
    fn configuration(value: &gents_lean_vocab::LeanBackendProbeConfiguration) -> InferenceBackend {
        let mut backend = backend(
            &format!("backend-{}", value.backend_id),
            format!("http://localhost/revision-{}", value.revision),
        );
        backend.enabled = value.enabled;
        backend
    }

    for case in gents_lean_vocab::lean_backend_probe_schedule_cases() {
        let mut schedule = ProbeSchedule::default();
        schedule.select(
            &case.known.iter().map(configuration).collect::<Vec<_>>(),
            true,
        );
        let selected = schedule.select(
            &case.current.iter().map(configuration).collect::<Vec<_>>(),
            case.periodic,
        );
        let expected = case
            .due
            .iter()
            .map(|id| format!("backend-{id}"))
            .collect::<RapidHashSet<_>>();
        assert_eq!(selected, expected, "{}", case.name);
        let observed = case
            .observed
            .iter()
            .map(configuration)
            .map(|backend| (backend.backend_id.clone(), backend))
            .collect::<RapidHashMap<_, _>>();
        assert_eq!(
            serde_json::to_value(&schedule.known).unwrap(),
            serde_json::to_value(observed).unwrap(),
            "{}",
            case.name
        );
    }
}

#[test]
fn backend_fields_refine_the_modeled_probe_revision() {
    type Edit = (&'static str, fn(&mut InferenceBackend), u32);
    let edits: &[Edit] = &[
        (
            "provider",
            |value| {
                value.provider_kind = crate::backends::provider::BackendProviderKind::OpenRouter
            },
            1,
        ),
        ("endpoint", |value| value.endpoint.push_str("/changed"), 1),
        (
            "auth",
            |value| {
                value.auth = gents_documents::BackendAuth::ApiKey {
                    key: "test-key".into(),
                }
            },
            1,
        ),
        (
            "connect timeout",
            |value| value.connect_timeout_secs = Some(20),
            1,
        ),
        (
            "discovery timeout",
            |value| value.discovery_timeout_secs = Some(20),
            1,
        ),
        ("name", |value| value.name = "Renamed".into(), 0),
        ("tags", |value| value.tags = vec!["changed".into()], 0),
        ("capacity", |value| value.max_concurrent = Some(2), 0),
        ("queue capacity", |value| value.max_queue_depth = Some(2), 0),
    ];
    let base = backend("backend-0", "http://localhost/v1".into());
    for (name, edit, revision) in edits {
        let mut changed = base.clone();
        edit(&mut changed);
        let mut schedule = ProbeSchedule::default();
        schedule.select(std::slice::from_ref(&base), true);
        for known_revision in [0, *revision] {
            let modeled = gents_lean_vocab::lean_backend_probe_schedule_cases()
                .iter()
                .find(|case| {
                    !case.periodic
                        && case.known.len() == 1
                        && case.current.len() == 1
                        && case.known[0].backend_id == 0
                        && case.known[0].revision == known_revision
                        && case.known[0].enabled
                        && case.current[0].backend_id == 0
                        && case.current[0].revision == *revision
                        && case.current[0].enabled
                })
                .expect("the model emits changed and unchanged enabled configurations");
            let expected = modeled
                .due
                .iter()
                .map(|id| format!("backend-{id}"))
                .collect::<RapidHashSet<_>>();
            assert_eq!(
                schedule.select(std::slice::from_ref(&changed), false),
                expected,
                "{name}"
            );
        }
    }
}

struct ModelsListener {
    port: u16,
    requests: Arc<AtomicUsize>,
    last_authorization: Arc<std::sync::RwLock<Option<String>>>,
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl ModelsListener {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind models listener");
        listener
            .set_nonblocking(true)
            .expect("set models listener nonblocking");
        let port = listener.local_addr().expect("local addr").port();
        let listener =
            tokio::net::TcpListener::from_std(listener).expect("build async models listener");
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let last_authorization = Arc::new(std::sync::RwLock::new(None));
        let captured = last_authorization.clone();
        let models = move |headers: axum::http::HeaderMap| {
            let observed = observed.clone();
            let captured = captured.clone();
            async move {
                observed.fetch_add(1, Ordering::Relaxed);
                if let Some(value) = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                {
                    *captured.write().expect("authorization capture lock") = Some(value.into());
                }
                Json(serde_json::json!({
                    "data": [{"id": "test-model"}],
                    "models": [{"model": "test-model", "name": "Test model"}]
                }))
            }
        };
        let app = Router::new()
            .route("/v1/models", get(models.clone()))
            .route("/v1/models-v2", get(models));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .await
        });
        Self {
            port,
            requests,
            last_authorization,
            shutdown: Some(shutdown_tx),
            handle: Some(handle),
        }
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }

    fn last_authorization(&self) -> Option<String> {
        self.last_authorization
            .read()
            .expect("authorization capture lock")
            .clone()
    }

    async fn shutdown(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(handle) = self.handle.take() {
            handle
                .await
                .expect("join models listener task")
                .expect("serve models listener");
        }
    }
}

impl Drop for ModelsListener {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn backend(backend_id: &str, endpoint: String) -> InferenceBackend {
    InferenceBackend {
        physical_doc_id: None,
        node_did: "did:key:backend-owner".to_string(),
        backend_id: backend_id.to_string(),
        name: backend_id.to_string(),
        provider_kind: crate::backends::provider::BackendProviderKind::OpenAiCompatible,
        openai_wire_api: None,
        endpoint,
        claude_platform: None,
        auth: gents_documents::BackendAuth::Unauthenticated,
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        max_concurrent: Some(1),
        max_queue_depth: Some(DEFAULT_MAX_QUEUE_DEPTH),
        enabled: true,
        tags: Vec::new(),
    }
}

fn probe_options() -> BackendProberOptions {
    BackendProberOptions {
        probe_interval: Duration::from_millis(50),
        probe_timeout: Duration::from_secs(2),
        failure_threshold_k: 3,
    }
}

fn assert_modeled_probe(
    previous: Option<&BackendHealthSnapshot>,
    current: &BackendHealthSnapshot,
    event: &str,
) {
    let threshold_k = probe_options().failure_threshold_k;
    let mut model = previous
        .map(|snapshot| (snapshot.state, snapshot.failure_count))
        .unwrap_or((BackendHealthState::Unknown, 0));
    if previous.is_some_and(|snapshot| {
        snapshot.configuration_fingerprint != current.configuration_fingerprint
    }) {
        model = step_backend(model, ProbeEvent::ConfigurationChanged, threshold_k);
    }
    let (state, count) = step_backend(model, probe_event(event), threshold_k);
    assert_eq!(current.state, state);
    assert_eq!(current.failure_count, count);
}
