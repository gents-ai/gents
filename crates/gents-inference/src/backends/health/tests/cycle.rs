use super::*;
use crate::test_support::scripted_backend::ScriptedBackend;
use rig::completion::CompletionModel as _;

struct CycleFixture {
    db: gents_store::test_support::TestStore,
    binding: gents_protocol::agent_identity::AgentIdentityBinding,
    _user_identity: gents_store::identity::KeyIdentity,
    user: gents_store::config_client::ConfigAccess,
    runtime: gents_store::config_client::ConfigAccess,
}

impl CycleFixture {
    async fn new() -> Self {
        let signed_home = gents_documents::test_support::SignedHome::start(
            "health-cycle",
            gents_store::node_extension::Extensions::none(),
        )
        .await;
        let binding = crate::test_support::default_agent(&signed_home).await;
        let (db, user_identity, user) = signed_home.into_parts();
        let runtime =
            crate::auth::credential::runtime_credential_access(&db.node, db.node_identity.did())
                .unwrap();
        Self {
            db,
            binding,
            _user_identity: user_identity,
            user,
            runtime,
        }
    }

    fn backend(&self, id: &str, endpoint: String) -> InferenceBackend {
        let mut backend = super::backend(id, endpoint);
        backend.node_did = self.db.node_identity.did().to_owned();
        backend
    }

    async fn write(&self, backend: &InferenceBackend) {
        gents_documents::write_inference_backend_document(
            &self.user,
            backend,
            &crate::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
    }

    async fn seed(&self, backend: &InferenceBackend, status: &str) {
        self.write(backend).await;
        crate::backends::registry::set_backend_probe_status_as(
            &self.runtime,
            &backend.node_did,
            &backend.backend_id,
            status,
        )
        .await
        .unwrap();
    }

    async fn observation(
        &self,
        backend: &InferenceBackend,
    ) -> gents_documents::InferenceBackendObservation {
        self.user
            .transact_readonly("test.health.observation", |txn| {
                Box::pin(async move {
                    crate::backends::registry::lookup_backend_observation_in_txn(
                        txn,
                        &backend.node_did,
                        &backend.backend_id,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap()
    }

    async fn wait_for_promotion(&self, backend: &InferenceBackend) {
        tokio::time::timeout(
            Duration::from_secs(10),
            gents_store::test_support::read_when_changed(
                &self.db.node,
                "InferenceBackendObservation",
                "backend promotion",
                async || {
                    let observation = self.observation(backend).await;
                    if observation.probe_status.as_deref() == Some("healthy")
                        && observation.last_probe.is_some()
                    {
                        Ok(())
                    } else {
                        Err(format!("{observation:?}"))
                    }
                },
            ),
        )
        .await
        .expect("the backend was not probed before the periodic interval");
    }

    async fn grant_removal(&self) {
        let admin = gents_store::config_client::ConfigAccess::local_as(
            self.db.node.clone(),
            self.db.node_identity.did().parse().unwrap(),
        );
        let user = self.user.actor_did().unwrap().parse().unwrap();
        for permission in [
            gents_store::config_client::NodePermission::DocumentDelete,
            gents_store::config_client::NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&user, permission)
                .await
                .unwrap();
        }
    }

    async fn remove(&self, backend: &InferenceBackend) {
        let plan = gents_documents::DesiredStateApplyPlan::new(Vec::new())
            .unwrap()
            .with_removals(vec![(
                gents_store::collection::Collection::InferenceBackend,
                backend.node_did.clone(),
                backend.backend_id.clone(),
            )])
            .unwrap();
        self.user
            .transact_write("test.backend.remove", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan(
                        txn,
                        plan,
                        &crate::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn spawned_prober_observes_a_new_backend_before_the_next_periodic_tick() {
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();
    let listener = ScriptedBackend::start("test-model", Vec::new()).unwrap();
    let initial = fixture.backend("initial", listener.endpoint().to_owned());
    fixture.seed(&initial, "unknown").await;
    let health = BackendHealthMap::new();
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let (events, _received) = mpsc::channel(1);
    let prober = tokio_util::task::AbortOnDropHandle::new(spawn_backend_prober(
        node.clone(),
        health.clone(),
        BackendProberOptions {
            probe_interval: Duration::from_secs(3600),
            ..probe_options()
        },
        events,
        cancel.clone(),
        initial.node_did.clone(),
        Arc::new(crate::admission::AdmissionRegistry::new(node.clone())),
    ));
    fixture.wait_for_promotion(&initial).await;
    let initial_health = health.get(&initial.backend_id).await.unwrap();
    assert_modeled_probe(None, &initial_health, "probeSuccess");

    let added = fixture.backend("added", listener.endpoint().to_owned());
    fixture.write(&added).await;
    fixture.wait_for_promotion(&added).await;
    assert_modeled_probe(
        None,
        &health.get(&added.backend_id).await.unwrap(),
        "probeSuccess",
    );
    assert_eq!(
        health.get(&initial.backend_id).await.unwrap().last_probe_at,
        initial_health.last_probe_at
    );
    assert_eq!(listener.recorded_requests().len(), 2);

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(10), prober)
        .await
        .expect("the prober did not cancel")
        .unwrap();
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn cycle_reproduces_fleet_evidence_dead_backend_demotes_and_recovers() {
    let options = probe_options();
    let client = reqwest::Client::builder()
        .timeout(options.probe_timeout)
        .build()
        .unwrap();
    let health_map = BackendHealthMap::new();
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();

    let listener = ScriptedBackend::start("test-model", Vec::new()).unwrap();
    let endpoint = listener.endpoint().to_owned();
    let backends = vec![fixture.backend("spark", endpoint.clone())];
    fixture.write(&backends[0]).await;
    let now = Utc::now();
    let outcome =
        probe_backends_cycle(&node, &client, &backends, now, &health_map, &options, None).await;
    assert!(outcome.flipped.is_empty());
    let snap = health_map.get("spark").await.expect("entry after probe");
    assert_eq!(snap.state, BackendHealthState::Healthy);
    assert_eq!(snap.last_probe_at, now);
    assert!(!health_map.measured_blocks_routing("spark").await);

    listener.shutdown().await;
    for cycle in 1..=3u32 {
        let cycle_now = Utc::now();
        let outcome = probe_backends_cycle(
            &node,
            &client,
            &backends,
            cycle_now,
            &health_map,
            &options,
            None,
        )
        .await;
        let snap = health_map.get("spark").await.expect("entry");
        assert_eq!(snap.failure_count, cycle, "consecutive failures accumulate");
        assert_eq!(
            snap.last_probe_at, cycle_now,
            "every attempt stamps last_probe"
        );
        if cycle < 3 {
            assert_eq!(snap.state, BackendHealthState::Degraded);
            assert!(outcome.flipped.is_empty(), "no veto below the K threshold");
            assert!(!health_map.measured_blocks_routing("spark").await);
        } else {
            assert_eq!(snap.state, BackendHealthState::Unhealthy);
            assert_eq!(outcome.flipped, vec!["spark".to_string()]);
            assert!(health_map.measured_blocks_routing("spark").await);
            assert!(snap.last_error.is_some(), "failure detail retained");
        }
    }

    let recovered = ScriptedBackend::start("test-model", Vec::new()).unwrap();
    let backends = vec![fixture.backend("spark", recovered.endpoint().to_owned())];
    fixture.write(&backends[0]).await;
    let outcome = probe_backends_cycle(
        &node,
        &client,
        &backends,
        Utc::now(),
        &health_map,
        &options,
        None,
    )
    .await;
    assert_eq!(
        outcome.flipped,
        vec!["spark".to_string()],
        "recovery flips the veto back"
    );
    let snap = health_map.get("spark").await.expect("entry");
    assert_eq!(snap.state, BackendHealthState::Healthy);
    assert_eq!(snap.failure_count, 0);
    assert!(snap.last_error.is_none());
    assert!(!health_map.measured_blocks_routing("spark").await);
}

#[tokio::test]
async fn cycle_probes_anthropic_key_backends_independently() {
    let options = probe_options();
    let client = reqwest::Client::builder()
        .timeout(options.probe_timeout)
        .build()
        .unwrap();
    let health_map = BackendHealthMap::new();
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();
    let listener = ScriptedBackend::start("test-model", Vec::new()).unwrap();
    // The Anthropic API-key adapter only reaches its fixed public endpoint, so a
    // local listener cannot stand in for it. The keyed probe path under test is
    // provider-independent, so the backends use a keyed OpenAI-compatible adapter.
    let key_backend = |backend_id: &str, endpoint: String| {
        let mut backend = fixture.backend(backend_id, endpoint);
        backend.provider_kind = crate::backends::provider::BackendProviderKind::OpenAiCompatible;
        backend.auth = gents_documents::BackendAuth::ApiKey {
            key: "placeholder-key".to_string(),
        };
        backend
    };
    let backends = vec![
        key_backend("a", listener.endpoint().to_owned()),
        key_backend("b", "http://127.0.0.1:1/v1".to_string()),
    ];
    for backend in &backends {
        fixture.write(backend).await;
    }
    let backends = crate::backends::registry::list_enabled_backends_for_runtime(
        &node,
        fixture.db.node_identity.did(),
    )
    .await
    .unwrap()
    .into_iter()
    .filter(|backend| ["a", "b"].contains(&backend.backend_id.as_str()))
    .collect::<Vec<_>>();
    assert_eq!(backends.len(), 2);
    assert!(backends.iter().all(|backend| matches!(
        &backend.auth,
        gents_documents::BackendAuth::Credential { .. }
    )));

    probe_backends_cycle(
        &node,
        &client,
        &backends,
        Utc::now(),
        &health_map,
        &options,
        None,
    )
    .await;

    let a = health_map.get("a").await.expect("a probed");
    assert_eq!(a.state, BackendHealthState::Healthy);
    assert_eq!(a.failure_count, 0);
    let b = health_map.get("b").await.expect("b probed");
    assert_eq!(b.failure_count, 1);
    assert_ne!(b.state, BackendHealthState::Healthy);
    listener.shutdown().await;
}

#[tokio::test]
async fn cycle_marks_reachable_unknown_backends_promotable() {
    let options = probe_options();
    let client = reqwest::Client::new();
    let health_map = BackendHealthMap::new();
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();
    let listener = ScriptedBackend::start("test-model", Vec::new()).unwrap();

    let backends = vec![fixture.backend("late-arrival", listener.endpoint().to_owned())];
    fixture.seed(&backends[0], "unknown").await;
    let outcome = probe_backends_cycle(
        &node,
        &client,
        &backends,
        Utc::now(),
        &health_map,
        &options,
        None,
    )
    .await;
    assert_eq!(outcome.promotable, vec!["late-arrival".to_string()]);

    let backends = vec![fixture.backend("late-arrival", listener.endpoint().to_owned())];
    fixture.seed(&backends[0], "healthy").await;
    let outcome = probe_backends_cycle(
        &node,
        &client,
        &backends,
        Utc::now(),
        &health_map,
        &options,
        None,
    )
    .await;
    assert!(outcome.promotable.is_empty());

    fixture.seed(&backends[0], "unknown").await;
    let outcome = run_backend_probe_cycle(
        node.clone(),
        &client,
        &health_map,
        &options,
        fixture.db.node_identity.did(),
    )
    .await;
    assert_eq!(outcome.promotable, vec!["late-arrival".to_string()]);

    let observation = fixture.observation(&backends[0]).await;
    assert_eq!(
        observation.probe_status.as_deref(),
        Some("healthy"),
        "the probe owner must persist its promotion"
    );
    assert!(
        observation.last_probe.is_some(),
        "promotion must stamp last_probe: {observation:?}"
    );
}

#[tokio::test]
async fn runtime_probe_cycle_scopes_same_named_backends_to_its_node() {
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();
    let listener = ScriptedBackend::start("test-model", Vec::new()).unwrap();
    let local = fixture.backend("same", listener.endpoint().to_owned());
    let mut foreign = fixture.backend("same", "http://127.0.0.1:1/v1".into());
    let foreign_db = gents_store::test_support::TestStore::start(
        "health-foreign-node",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    foreign.node_did = foreign_db.node_identity.did().to_owned();
    gents_documents::test_support::bootstrap_signed_test_node(&fixture.user, &foreign.node_did)
        .await;
    fixture.seed(&local, "unknown").await;
    fixture.write(&foreign).await;
    let admin = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        fixture.db.node_identity.did().parse().unwrap(),
    );
    let foreign_actor = foreign.node_did.parse().unwrap();
    for permission in [
        gents_store::config_client::NodePermission::CollectionGet,
        gents_store::config_client::NodePermission::DocumentRead,
        gents_store::config_client::NodePermission::DocumentUpdate,
    ] {
        admin
            .grant_node_permission(&foreign_actor, permission)
            .await
            .unwrap();
    }
    let foreign_runtime = fixture.db.client_access(foreign_db.node_identity.as_ref());
    crate::backends::registry::set_backend_probe_status_as(
        &foreign_runtime,
        &foreign.node_did,
        &foreign.backend_id,
        "unknown",
    )
    .await
    .unwrap();
    let health = BackendHealthMap::new();
    let outcome = run_backend_probe_cycle(
        node.clone(),
        &reqwest::Client::new(),
        &health,
        &probe_options(),
        &local.node_did,
    )
    .await;
    assert_eq!(outcome.promotable, vec!["same"]);
    assert_eq!(
        health.get("same").await.unwrap().state,
        BackendHealthState::Healthy
    );
    let foreign_observation = fixture.observation(&foreign).await;
    assert_eq!(foreign_observation.probe_status.as_deref(), Some("unknown"));
    assert!(foreign_observation.catalogs.is_empty());
}

#[tokio::test]
async fn cycle_never_probes_or_demotes_chatgpt_codex_backends() {
    let options = probe_options();
    let client = reqwest::Client::new();
    let health_map = BackendHealthMap::new();
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();

    let mut codex = fixture.backend("codex", "http://127.0.0.1:1/v1".to_string());
    codex.provider_kind = crate::backends::provider::BackendProviderKind::ChatGptCodex;
    codex.auth = gents_documents::BackendAuth::NodeOAuth { account_ref: None };
    let outcome = probe_backends_cycle(
        &node,
        &client,
        std::slice::from_ref(&codex),
        Utc::now(),
        &health_map,
        &options,
        None,
    )
    .await;
    assert!(outcome.flipped.is_empty());
    assert!(health_map.get("codex").await.is_none(), "no measured entry");
    assert!(!health_map.measured_blocks_routing("codex").await);
}

#[tokio::test]
async fn probe_shutdown_cancels_pending_backend_without_pruning_or_failure() {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let started = Arc::new(tokio::sync::Notify::new());
    let seen = started.clone();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 1024];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        seen.notify_one();
        std::future::pending::<()>().await;
    });
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();
    let health = BackendHealthMap::new();
    health
        .set_for_test("pending", BackendHealthState::Degraded, 1)
        .await;
    health
        .set_for_test("unobserved", BackendHealthState::Healthy, 0)
        .await;
    let before_pending = health.get("pending").await.unwrap();
    let before_unobserved = health.get("unobserved").await.unwrap();
    let cancel = CancellationToken::new();
    let mut pending = fixture.backend("pending", endpoint);
    pending.discovery_timeout_secs = Some(60);
    fixture.write(&pending).await;
    let (events, mut received) = mpsc::channel(1);
    let handle = spawn_backend_prober(
        node.clone(),
        health.clone(),
        BackendProberOptions {
            probe_timeout: Duration::from_secs(60),
            ..probe_options()
        },
        events,
        cancel.clone(),
        pending.node_did.clone(),
        Arc::new(crate::admission::AdmissionRegistry::new(node.clone())),
    );
    tokio::time::timeout(Duration::from_secs(2), started.notified())
        .await
        .unwrap();
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("pending backend observation must stop on shutdown")
        .unwrap();
    assert!(
        received.try_recv().is_err(),
        "cancelled discovery has no health event"
    );
    for previous in [before_pending, before_unobserved] {
        let current = health.get(&previous.backend_id).await.unwrap();
        assert_eq!(current.state, previous.state);
        assert_eq!(current.failure_count, previous.failure_count);
        assert_eq!(current.last_probe_at, previous.last_probe_at);
        assert_eq!(current.last_error, previous.last_error);
        assert_eq!(
            current.configuration_fingerprint,
            previous.configuration_fingerprint
        );
    }
    assert!(health.get("later").await.is_none());
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    node.shutdown().await.expect("node shutdown");
}

fn backend_index(backend_id: &str) -> u64 {
    backend_id
        .strip_prefix("backend-")
        .and_then(|index| index.parse().ok())
        .unwrap_or_else(|| panic!("unexpected backend id {backend_id}"))
}

fn backend_indexes(backend_ids: impl IntoIterator<Item = String>) -> Vec<u64> {
    backend_ids
        .into_iter()
        .map(|backend_id| backend_index(&backend_id))
        .collect()
}

async fn measured_entries(health: &BackendHealthMap) -> Vec<(u64, BackendHealthState, u64)> {
    let mut entries = health
        .snapshot()
        .await
        .into_values()
        .map(|snapshot| {
            (
                backend_index(&snapshot.backend_id),
                snapshot.state,
                u64::from(snapshot.failure_count),
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.0);
    entries
}

#[tokio::test]
async fn cycle_retention_matches_the_lean_model() {
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();
    let listener = ScriptedBackend::start("test-model", Vec::new()).unwrap();
    for case in gents_lean_vocab::families::backend_health_retention_cycle::cases::<
        BackendHealthState,
        String,
    >() {
        let health = BackendHealthMap::new();
        for entry in &case.input.entries {
            health
                .set_for_test(
                    format!("backend-{}", entry.id),
                    entry.state,
                    u32::try_from(entry.failure_count).unwrap(),
                )
                .await;
        }
        let mut backends = Vec::new();
        for wire in &case.input.backends {
            let endpoint = match wire.target.as_str() {
                "unreachable" => format!("{}/missing", listener.endpoint().to_owned()),
                _ => listener.endpoint().to_owned(),
            };
            let mut backend = fixture.backend(&format!("backend-{}", wire.id), endpoint);
            if wire.target == "node_oauth" {
                backend.provider_kind =
                    crate::backends::provider::BackendProviderKind::ClaudeCliSubscription;
                backend.auth = gents_documents::BackendAuth::NodeOAuth { account_ref: None };
            } else {
                fixture
                    .seed(
                        &backend,
                        if wire.status_unknown {
                            "unknown"
                        } else {
                            "healthy"
                        },
                    )
                    .await;
            }
            backends.push(backend);
        }
        let cancel = CancellationToken::new();
        if case.input.cancelled {
            cancel.cancel();
        }
        let options = BackendProberOptions {
            failure_threshold_k: u32::try_from(case.input.threshold_k).unwrap(),
            ..probe_options()
        };
        let outcome = probe_selected_backends(
            &node,
            &reqwest::Client::new(),
            &backends,
            ProbeEnv {
                now: Utc::now(),
                health_map: &health,
                options: &options,
                cancel: Some(&cancel),
                progress: None,
            },
            None,
            None,
        )
        .await;
        case.check(
            gents_lean_vocab::families::backend_health_retention_cycle::CycleOutput {
                entries: measured_entries(&health)
                    .await
                    .into_iter()
                    .map(|(id, state, failure_count)| {
                        gents_lean_vocab::families::backend_health_retention_cycle::EntryWire {
                            id,
                            state,
                            failure_count,
                        }
                    })
                    .collect(),
                flipped: backend_indexes(outcome.flipped),
                promotable: backend_indexes(outcome.promotable),
            },
        );
    }
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn scheduled_cycles_match_the_lean_trace() {
    for case in
        gents_lean_vocab::families::backend_health_retention_trace::cases::<BackendHealthState>()
    {
        let fixture = CycleFixture::new().await;
        fixture.grant_removal().await;
        let node = fixture.db.node.clone();
        for bootstrap in crate::backends::registry::list_enabled_backends_for_runtime(
            &node,
            fixture.db.node_identity.did(),
        )
        .await
        .unwrap()
        {
            fixture
                .write(&InferenceBackend {
                    enabled: false,
                    ..bootstrap
                })
                .await;
        }
        // A revision names the endpoint alone, so a revision change is an endpoint change.
        let listeners = [
            ScriptedBackend::start("test-model", Vec::new()).unwrap(),
            ScriptedBackend::start("test-model", Vec::new()).unwrap(),
        ];
        let health = BackendHealthMap::new();
        let mut schedule = ProbeSchedule::default();
        let options = BackendProberOptions {
            failure_threshold_k: u32::try_from(case.input.threshold_k).unwrap(),
            ..probe_options()
        };
        let mut present: Vec<InferenceBackend> = Vec::new();
        let mut observations = Vec::new();
        for (index, step) in case.input.steps.iter().enumerate() {
            let mut current = Vec::new();
            for wire in &step.backends {
                let listener = &listeners[usize::try_from(wire.revision).unwrap()];
                let endpoint = if wire.reachable {
                    listener.endpoint().to_owned()
                } else {
                    format!("{}/missing", listener.endpoint().to_owned())
                };
                let mut backend = fixture.backend(&format!("backend-{}", wire.id), endpoint);
                backend.enabled = wire.enabled;
                backend.name = format!("step-{index}");
                backend.tags = vec![format!("step-{index}")];
                backend.max_concurrent = Some(1 + i64::try_from(index).unwrap());
                fixture.write(&backend).await;
                current.push(backend);
            }
            for removed in present.iter().filter(|backend| {
                !current
                    .iter()
                    .any(|now| now.backend_id == backend.backend_id)
            }) {
                fixture.remove(removed).await;
            }
            present = current;
            let before = health.snapshot().await;
            let outcome = run_scheduled_backend_probe_cycle(
                node.clone(),
                &reqwest::Client::new(),
                &health,
                &options,
                fixture.db.node_identity.did(),
                super::super::prober::CycleControl {
                    schedule: Some((&mut schedule, step.periodic)),
                    ..Default::default()
                },
            )
            .await;
            let mut probed = health
                .snapshot()
                .await
                .into_values()
                .filter(|after| {
                    before
                        .get(&after.backend_id)
                        .is_none_or(|previous| previous.last_probe_at != after.last_probe_at)
                })
                .map(|after| backend_index(&after.backend_id))
                .collect::<Vec<_>>();
            probed.sort_unstable();
            let mut known = backend_indexes(schedule.known.keys().cloned());
            known.sort_unstable();
            let mut documents = Vec::new();
            for backend in &present {
                let observation = fixture.observation(backend).await;
                documents.push(gents_lean_vocab::families::backend_health_retention_trace::DocumentObservation {
                    id: backend_index(&backend.backend_id),
                    healthy: observation.probe_status.as_deref() == Some("healthy"),
                    catalogued: !observation.catalogs.is_empty(),
                });
            }
            observations.push(
                gents_lean_vocab::families::backend_health_retention_trace::StepOutput {
                    entries: measured_entries(&health)
                        .await
                        .into_iter()
                        .map(|(id, state, failure_count)| {
                            gents_lean_vocab::families::backend_health_retention_trace::EntryWire {
                                id,
                                state,
                                failure_count,
                            }
                        })
                        .collect(),
                    probed,
                    known,
                    flipped: backend_indexes(outcome.flipped),
                    promotable: backend_indexes(outcome.promotable),
                    documents,
                },
            );
        }
        case.check(observations);
        node.shutdown().await.expect("node shutdown");
    }
}

impl CycleFixture {
    async fn agent_signer(&self) -> gents_store::identity::ManagedAgentSigner {
        let signer = gents_store::identity::resolve_agent_identity(
            self.db.node_identity.as_ref(),
            &self.binding,
        )
        .unwrap()
        .managed_agent_signer()
        .unwrap();
        crate::admission::tests::grant_test_agent(&self.db, &signer).await;
        signer
    }

    /// Opens or closes `backend`'s admission as the runtime does from its
    /// measured health.
    async fn reconcile_admission(
        &self,
        registry: &crate::admission::AdmissionRegistry,
        backend: &InferenceBackend,
        health: &BackendHealthMap,
    ) {
        let config = crate::admission::BackendAdmissionConfig::from_backend(
            backend,
            &self.observation(backend).await,
        )
        .unwrap()
        .with_measured_unhealthy(health.measured_blocks_routing(&backend.backend_id).await);
        registry.reconcile(
            1,
            &rapidhash::RapidHashMap::from_iter([(backend.backend_id.clone(), config)]),
        );
    }
}

/// A model whose calls for `request_id` are admitted to `backend` and sent to
/// the scripted backend through the production provider client.
async fn admitted_model(
    fixture: &CycleFixture,
    signer: &gents_store::identity::ManagedAgentSigner,
    registry: &crate::admission::AdmissionRegistry,
    backend: &InferenceBackend,
    request_id: &str,
) -> impl rig::completion::CompletionModel + Send + Sync + 'static {
    use gents_loop::provider_call::ForCall;
    use rig::client::CompletionClient;
    let request =
        crate::admission::tests::seeded_request(&fixture.db.node, signer, request_id).await;
    let provider = crate::llm::rig_compat::providers::build_openai_chat_completions_client(
        "key",
        &backend.endpoint,
        crate::clients::provider_http::ProviderHttpClient::new(
            crate::clients::provider_http::ProviderHttpClient::credential_client().unwrap(),
        ),
    )
    .unwrap()
    .completion_model("test-model");
    let mut model = crate::admission::AdmittedCompletionModel::new(
        provider,
        registry.clone(),
        "test-slot-connection",
        gents_loop::provider_limit::LONG_RATE_LIMIT_WAIT,
    );
    model.agent_signer = Some(signer.clone());
    model.for_call(&gents_loop::provider_call::ProviderCall {
        admission: Some(
            gents_loop::provider_call::admission::AdmissionCallContext::for_request(
                &request,
                signer.did(),
                &backend.backend_id,
            ),
        ),
        ..Default::default()
    })
}

async fn stream_to_end<M>(model: M, prompt: &str) -> Result<(), rig::completion::CompletionError>
where
    M: rig::completion::CompletionModel,
{
    use futures::StreamExt;
    let mut stream = model.completion_request(prompt).stream().await?;
    while let Some(item) = stream.next().await {
        item?;
    }
    Ok(())
}

/// One probe cycle whose models request the backend holds: once the `nth`
/// held probe has arrived, `during` runs inside the probe's window, and the
/// paused clock then carries the probe to its deadline.
async fn probe_held_models<F>(
    node: &Arc<defra_node::Node>,
    backends: &[InferenceBackend],
    env: ProbeEnv<'_>,
    models: &crate::test_support::scripted_backend::HeldCall<'_>,
    nth: usize,
    during: F,
) where
    F: std::future::Future<Output = ()>,
{
    let client = reqwest::Client::new();
    let cycle = probe_selected_backends(node, &client, backends, env, None, None);
    tokio::pin!(cycle);
    tokio::select! {
        biased;
        () = models.entered_times(nth) => {}
        _ = &mut cycle => panic!("probe {nth} ended before its models request arrived"),
    }
    during.await;
    tokio::time::pause();
    cycle.await;
    tokio::time::resume();
}

fn probe_env<'a>(
    health: &'a BackendHealthMap,
    options: &'a BackendProberOptions,
    registry: &'a crate::admission::AdmissionRegistry,
) -> ProbeEnv<'a> {
    ProbeEnv {
        now: Utc::now(),
        health_map: health,
        options,
        cancel: None,
        progress: Some(registry),
    }
}

/// A backend busy streaming a call admitted to it answers its models endpoint
/// slower than the probe waits; probes that time out while that call receives
/// response items leave it healthy, and the call queued behind it is admitted.
#[tokio::test]
async fn a_busy_backend_whose_models_probes_time_out_stays_routable() {
    use crate::test_support::scripted_backend::StreamScript;
    use futures::StreamExt;
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();
    let scripted = ScriptedBackend::start(
        "test-model",
        vec![StreamScript::paused(
            "busy-call",
            ["one", "two", "three", "four"],
        )],
    )
    .unwrap();
    let backend = fixture.backend("busy", scripted.endpoint().to_owned());
    fixture.seed(&backend, "healthy").await;
    let backends = vec![backend.clone()];
    let (health, options) = (BackendHealthMap::new(), BackendProberOptions::default());
    let registry = crate::admission::AdmissionRegistry::new(node.clone());
    let signer = fixture.agent_signer().await;
    probe_selected_backends(
        &node,
        &reqwest::Client::new(),
        &backends,
        probe_env(&health, &options, &registry),
        None,
        None,
    )
    .await;
    fixture
        .reconcile_admission(&registry, &backend, &health)
        .await;

    let busy = admitted_model(&fixture, &signer, &registry, &backend, "req-busy").await;
    let mut stream = busy.completion_request("busy-call").stream().await.unwrap();
    let queued_model = admitted_model(&fixture, &signer, &registry, &backend, "req-queued").await;
    let queued = tokio::spawn(stream_to_end(queued_model, "queued call"));
    crate::admission::tests::wait_for_request_call_state(&node, &signer, "req-queued", "queued")
        .await;

    let models = scripted.hold_models();
    for nth in 1..=3 {
        probe_held_models(
            &node,
            &backends,
            probe_env(&health, &options, &registry),
            &models,
            nth,
            async {
                stream.next().await.unwrap().unwrap();
            },
        )
        .await;
    }
    let snapshot = health.get("busy").await.unwrap();
    assert_eq!(
        (snapshot.state, snapshot.failure_count),
        (BackendHealthState::Healthy, 0)
    );

    fixture
        .reconcile_admission(&registry, &backend, &health)
        .await;
    scripted.release("busy-call");
    while let Some(item) = stream.next().await {
        item.unwrap();
    }
    queued.await.unwrap().unwrap();
}

/// With no call admitted to it, a backend whose models probes time out is
/// demoted at the third, as failed probes demote it.
#[tokio::test]
async fn models_probes_timing_out_with_no_call_in_flight_demote_at_the_threshold() {
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();
    let scripted = ScriptedBackend::start("test-model", Vec::new()).unwrap();
    let backend = fixture.backend("idle", scripted.endpoint().to_owned());
    fixture.seed(&backend, "healthy").await;
    let backends = vec![backend.clone()];
    let (health, options) = (BackendHealthMap::new(), BackendProberOptions::default());
    let registry = crate::admission::AdmissionRegistry::new(node.clone());
    probe_selected_backends(
        &node,
        &reqwest::Client::new(),
        &backends,
        probe_env(&health, &options, &registry),
        None,
        None,
    )
    .await;
    fixture
        .reconcile_admission(&registry, &backend, &health)
        .await;

    let models = scripted.hold_models();
    for (nth, state) in [
        (1, BackendHealthState::Degraded),
        (2, BackendHealthState::Degraded),
        (3, BackendHealthState::Unhealthy),
    ] {
        probe_held_models(
            &node,
            &backends,
            probe_env(&health, &options, &registry),
            &models,
            nth,
            async {},
        )
        .await;
        let snapshot = health.get("idle").await.unwrap();
        assert_eq!(
            (snapshot.state, snapshot.failure_count),
            (state, u32::try_from(nth).unwrap())
        );
    }
}

/// A call admitted before an outage stalls with the backend's models
/// endpoint; the probes demote the backend, the call queued behind it ends as
/// `BackendGone`, and the stalled call still finishes by its own response.
#[tokio::test]
async fn a_call_in_flight_across_an_outage_finishes_after_its_backend_is_demoted() {
    use crate::test_support::scripted_backend::{StreamChunk, StreamScript};
    use futures::StreamExt;
    let fixture = CycleFixture::new().await;
    let node = fixture.db.node.clone();
    let scripted = ScriptedBackend::start(
        "test-model",
        vec![StreamScript::paused_before(
            "stalled-call",
            vec![StreamChunk::text("late answer")],
        )],
    )
    .unwrap();
    let backend = fixture.backend("outage", scripted.endpoint().to_owned());
    fixture.seed(&backend, "healthy").await;
    let backends = vec![backend.clone()];
    let (health, options) = (BackendHealthMap::new(), BackendProberOptions::default());
    let registry = crate::admission::AdmissionRegistry::new(node.clone());
    let signer = fixture.agent_signer().await;
    probe_selected_backends(
        &node,
        &reqwest::Client::new(),
        &backends,
        probe_env(&health, &options, &registry),
        None,
        None,
    )
    .await;
    fixture
        .reconcile_admission(&registry, &backend, &health)
        .await;

    let stalled = admitted_model(&fixture, &signer, &registry, &backend, "req-stalled").await;
    let mut stream = stalled
        .completion_request("stalled-call")
        .stream()
        .await
        .unwrap();
    let queued_model = admitted_model(&fixture, &signer, &registry, &backend, "req-behind").await;
    let queued = tokio::spawn(stream_to_end(queued_model, "queued call"));
    crate::admission::tests::wait_for_request_call_state(&node, &signer, "req-behind", "queued")
        .await;

    let models = scripted.hold_models();
    for nth in 1..=3 {
        probe_held_models(
            &node,
            &backends,
            probe_env(&health, &options, &registry),
            &models,
            nth,
            async {},
        )
        .await;
    }
    assert!(health.measured_blocks_routing("outage").await);

    fixture
        .reconcile_admission(&registry, &backend, &health)
        .await;
    let gone = queued
        .await
        .unwrap()
        .expect_err("a demoted backend admits no queued call");
    assert!(gone.to_string().contains("BackendGone"), "{gone}");

    scripted.release("stalled-call");
    let mut text = String::new();
    while let Some(item) = stream.next().await {
        if let rig::streaming::StreamedAssistantContent::Text(chunk) = item.unwrap() {
            text.push_str(&chunk.text);
        }
    }
    assert_eq!(text, "late answer");
    crate::admission::tests::wait_for_request_call_state(
        &node,
        &signer,
        "req-stalled",
        "completed",
    )
    .await;
}
