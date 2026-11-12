use super::*;

struct CycleFixture {
    db: crate::test_support::TestDb,
    _user_identity: gents_store::identity::KeyIdentity,
    user: gents_store::config_client::ConfigAccess,
    runtime: gents_store::config_client::ConfigAccess,
}

impl CycleFixture {
    async fn new() -> Self {
        let (db, user_identity, user, _binding) = crate::test_support::signed_test_node(
            "health-cycle",
            gents_store::node_extension::Extensions::none(),
        )
        .await;
        let runtime =
            crate::auth::credential::runtime_credential_access(&db.node, db.node_identity.did())
                .unwrap();
        Self {
            db,
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
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let observation = self.observation(backend).await;
                if observation.probe_status.as_deref() == Some("healthy")
                    && observation.last_probe.is_some()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
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
    let listener = ModelsListener::start();
    let initial = fixture.backend("initial", listener.endpoint());
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
    ));
    fixture.wait_for_promotion(&initial).await;
    let initial_health = health.get(&initial.backend_id).await.unwrap();
    assert_modeled_probe(None, &initial_health, "probeSuccess");

    let added = fixture.backend("added", listener.endpoint());
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
    assert_eq!(listener.requests(), 2);

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

    let listener = ModelsListener::start();
    let endpoint = listener.endpoint();
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

    let recovered = ModelsListener::start();
    let backends = vec![fixture.backend("spark", recovered.endpoint())];
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
    let listener = ModelsListener::start();
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
        key_backend("a", listener.endpoint()),
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
    let listener = ModelsListener::start();

    let backends = vec![fixture.backend("late-arrival", listener.endpoint())];
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

    let backends = vec![fixture.backend("late-arrival", listener.endpoint())];
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
    let listener = ModelsListener::start();
    let local = fixture.backend("same", listener.endpoint());
    let mut foreign = fixture.backend("same", "http://127.0.0.1:1/v1".into());
    let foreign_db = crate::test_support::test_db(
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
    let listener = ModelsListener::start();
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
                "unreachable" => format!("{}/missing", listener.endpoint()),
                _ => listener.endpoint(),
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
        let listener = ModelsListener::start();
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
                let endpoint = if wire.reachable {
                    listener.endpoint()
                } else {
                    format!("{}/missing", listener.endpoint())
                };
                let mut backend = fixture.backend(&format!("backend-{}", wire.id), endpoint);
                backend.enabled = wire.enabled;
                backend.discovery_timeout_secs = Some(5 + i64::try_from(wire.revision).unwrap());
                backend.name = format!("step-{index}");
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
                Some((&mut schedule, step.periodic)),
                None,
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
