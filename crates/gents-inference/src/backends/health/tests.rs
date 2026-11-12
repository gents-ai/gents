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
use gents_lean_vocab::lean_backend_health_cases;

fn state_from_lean(name: &str, state: &str) -> BackendHealthState {
    match state {
        "unknown" => BackendHealthState::Unknown,
        "healthy" => BackendHealthState::Healthy,
        "degraded" => BackendHealthState::Degraded,
        "unhealthy" => BackendHealthState::Unhealthy,
        "credentials_required" => BackendHealthState::CredentialsRequired,
        "subscription_not_entitled" => BackendHealthState::SubscriptionNotEntitled,
        other => panic!("Lean backend health case {name} produced unknown state {other:?}"),
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
fn generated_backend_health_cases_match_prober_transitions() {
    let cases = lean_backend_health_cases();
    assert!(
        !cases.is_empty(),
        "Lean must emit at least one backend health case"
    );

    for case in cases {
        let start = (
            state_from_lean(&case.name, &case.start_state),
            u32::try_from(case.start_count).unwrap(),
        );
        let event = match case.event.as_str() {
            "probeSuccess" => ProbeEvent::ProbeSuccess,
            "probeFail" => ProbeEvent::ProbeFail,
            "credentialsRejected" => ProbeEvent::CredentialsRejected,
            "subscriptionNotEntitled" => ProbeEvent::SubscriptionNotEntitled,
            "configurationChanged" => ProbeEvent::ConfigurationChanged,
            other => panic!(
                "Lean backend health case {} produced unknown event {other:?}",
                case.name
            ),
        };

        let (next_state, next_count) =
            step_backend(start, event, u32::try_from(case.threshold_k).unwrap());

        assert_eq!(
            next_state,
            state_from_lean(&case.name, &case.next_state),
            "Lean backend health case {} must match next_state",
            case.name
        );
        assert_eq!(
            next_count as usize, case.next_count,
            "Lean backend health case {} must match next_count",
            case.name
        );
        assert_eq!(
            next_state.blocks_routing(),
            case.blocks_routing,
            "Lean backend health case {} must match the blocksRouting projection",
            case.name
        );
        assert_eq!(
            next_state
                .readiness_reason()
                .map(|reason| serde_json::to_value(reason)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()),
            case.readiness_reason,
            "Lean backend health case {} must match the readiness reason",
            case.name
        );
    }
}

#[test]
fn generated_backend_probe_schedule_cases_match_runtime_selection() {
    use gents_lean_vocab::families::backend_probe_schedule_select as wire;

    fn configuration(value: &wire::ProbeConfiguration) -> InferenceBackend {
        let mut backend = backend(
            &format!("backend-{}", value.backend_id),
            format!("http://localhost/revision-{}", value.revision),
        );
        backend.enabled = value.enabled;
        backend
    }

    fn backend_number(backend_id: &str) -> u64 {
        backend_id
            .strip_prefix("backend-")
            .and_then(|id| id.parse().ok())
            .unwrap_or_else(|| panic!("schedule fixture backend id {backend_id:?}"))
    }

    gents_lean_vocab::families::backend_probe_schedule_select::replay(|input: &wire::SelectInput| {
        let mut schedule = ProbeSchedule::default();
        schedule.select(
            &input.known.iter().map(configuration).collect::<Vec<_>>(),
            true,
        );
        let selected = schedule.select(
            &input.current.iter().map(configuration).collect::<Vec<_>>(),
            input.periodic,
        );
        let mut due = selected
            .iter()
            .map(|id| backend_number(id))
            .collect::<Vec<_>>();
        due.sort_unstable();
        let mut observed = schedule
            .known
            .iter()
            .map(|(backend_id, backend)| {
                assert_eq!(backend_id, &backend.backend_id);
                wire::ProbeConfiguration {
                    backend_id: backend_number(&backend.backend_id),
                    revision: backend
                        .endpoint
                        .strip_prefix("http://localhost/revision-")
                        .and_then(|revision| revision.parse().ok())
                        .unwrap_or_else(|| panic!("schedule fixture endpoint {}", backend.endpoint)),
                    enabled: backend.enabled,
                }
            })
            .collect::<Vec<_>>();
        observed.sort_unstable_by_key(|configuration| configuration.backend_id);
        wire::SelectOutput { due, observed }
    });
}

#[test]
fn backend_fields_refine_the_modeled_probe_revision() {
    use gents_lean_vocab::families::backend_probe_schedule_field_edit as wire;

    gents_lean_vocab::families::backend_probe_schedule_field_edit::replay::<String>(|input: &wire::FieldEditInput<String>| {
        let base = backend("backend-0", "http://localhost/v1".into());
        let mut changed = base.clone();
        match input.field.as_str() {
            "provider" => {
                changed.provider_kind = crate::backends::provider::BackendProviderKind::OpenRouter
            }
            "wire_api" => {
                changed.openai_wire_api = Some(gents_loop::openai_wire::OpenAiWireApi::Responses)
            }
            "endpoint" => changed.endpoint.push_str("/changed"),
            "auth" => {
                changed.auth = gents_documents::BackendAuth::ApiKey {
                    key: "test-key".into(),
                }
            }
            "connect_timeout" => changed.connect_timeout_secs = Some(20),
            "discovery_timeout" => changed.discovery_timeout_secs = Some(20),
            "name" => changed.name = "Renamed".into(),
            "tags" => changed.tags = vec!["changed".into()],
            "capacity" => changed.max_concurrent = Some(2),
            "queue_capacity" => changed.max_queue_depth = Some(2),
            other => panic!("unknown modeled backend field {other:?}"),
        }
        let mut schedule = ProbeSchedule::default();
        schedule.select(std::slice::from_ref(&base), true);
        let mut selected = schedule.select(std::slice::from_ref(&changed), false);
        if input.repeated {
            selected = schedule.select(std::slice::from_ref(&changed), false);
        }
        let mut due = selected
            .iter()
            .map(|id| {
                id.strip_prefix("backend-")
                    .and_then(|id| id.parse().ok())
                    .unwrap_or_else(|| panic!("schedule fixture backend id {id:?}"))
            })
            .collect::<Vec<u64>>();
        due.sort_unstable();
        due
    });
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
    let (mut state, mut count) = previous
        .map(|snapshot| (snapshot.state, snapshot.failure_count))
        .unwrap_or((BackendHealthState::Unknown, 0));
    if previous.is_some_and(|snapshot| {
        snapshot.configuration_fingerprint != current.configuration_fingerprint
    }) {
        let reset = lean_backend_health_cases()
            .iter()
            .find(|case| {
                case.start_state == state.as_str()
                    && case.start_count == count as usize
                    && case.event == "configurationChanged"
                    && case.threshold_k == probe_options().failure_threshold_k as usize
            })
            .expect("the model emits this connection change");
        state = state_from_lean(&reset.name, &reset.next_state);
        count = reset.next_count.try_into().unwrap();
    }
    let modeled = lean_backend_health_cases()
        .iter()
        .find(|case| {
            case.start_state == state.as_str()
                && case.start_count == count as usize
                && case.event == event
                && case.threshold_k == probe_options().failure_threshold_k as usize
        })
        .expect("the model emits this probe transition");
    assert_eq!(current.state.as_str(), modeled.next_state);
    assert_eq!(current.failure_count as usize, modeled.next_count);
}
