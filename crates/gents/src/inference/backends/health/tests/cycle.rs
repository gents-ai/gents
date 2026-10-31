use super::*;

#[tokio::test]
async fn spawned_prober_observes_a_new_backend_before_the_next_periodic_tick() {
    let node = test_node().await;
    let listener = ModelsListener::start();
    let initial = backend("initial", listener.endpoint());
    seed_backend_observation(&node, &initial, "unknown").await;
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
    wait_for_promotion(&node, &initial).await;
    let initial_health = health.get(&initial.backend_id).await.unwrap();
    assert_modeled_probe(None, &initial_health, "probeSuccess");

    let added = backend("added", listener.endpoint());
    upsert_backend_configuration(&node, &added).await;
    wait_for_promotion(&node, &added).await;
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
    node.shutdown().await;
}

#[tokio::test]
async fn scheduled_cycles_preserve_health_across_observations_and_unrelated_edits() {
    let node = test_node().await;
    let listener = ModelsListener::start();
    let mut reachable = backend("reachable", listener.endpoint());
    let failing = backend("failing", format!("{}/missing", listener.endpoint()));
    seed_backend_observation(&node, &reachable, "unknown").await;
    seed_backend_observation(&node, &failing, "unknown").await;
    let health = BackendHealthMap::new();
    let mut schedule = ProbeSchedule::default();
    scheduled_cycle(&node, &health, &mut schedule, true).await;
    let reachable_before = health.get("reachable").await.unwrap();
    let failing_before = health.get("failing").await.unwrap();
    assert_modeled_probe(None, &reachable_before, "probeSuccess");
    assert_modeled_probe(None, &failing_before, "probeFail");
    let observation = crate::inference::backends::registry::lookup_backend_observation(
        &node,
        &reachable.node_did,
        &reachable.backend_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(observation.probe_status.as_deref(), Some("healthy"));
    assert_eq!(observation.catalogs.len(), 1);

    scheduled_cycle(&node, &health, &mut schedule, false).await;
    crate::inference::backends::registry::record_model_catalog(
        &node,
        &reachable,
        observation.catalogs[0].clone(),
    )
    .await
    .unwrap();
    crate::inference::backends::registry::set_backend_probe_status(
        &node,
        &failing.node_did,
        &failing.backend_id,
        "healthy",
    )
    .await
    .unwrap();
    reachable.name = "Renamed".into();
    reachable.tags = vec!["tagged".into()];
    reachable.max_concurrent = Some(2);
    seed_backend_observation(&node, &reachable, "healthy").await;
    scheduled_cycle(&node, &health, &mut schedule, false).await;
    assert_eq!(listener.requests(), 1);
    assert_eq!(
        health.get("reachable").await.unwrap().last_probe_at,
        reachable_before.last_probe_at
    );
    let unchanged = health.get("failing").await.unwrap();
    assert_eq!(unchanged.failure_count, failing_before.failure_count);
    assert_eq!(unchanged.last_probe_at, failing_before.last_probe_at);

    let replacement = ModelsListener::start();
    reachable.endpoint = replacement.endpoint();
    seed_backend_observation(&node, &reachable, "healthy").await;
    scheduled_cycle(&node, &health, &mut schedule, false).await;
    assert_eq!(replacement.requests(), 1);
    assert_eq!(
        health.get("failing").await.unwrap().last_probe_at,
        failing_before.last_probe_at
    );
    scheduled_cycle(&node, &health, &mut schedule, true).await;
    assert_eq!(replacement.requests(), 2);
    assert_modeled_probe(
        Some(&failing_before),
        &health.get("failing").await.unwrap(),
        "probeFail",
    );
    node.shutdown().await;
}

#[tokio::test]
async fn disabled_and_removed_backends_are_probed_again_when_observed_enabled() {
    let node = test_node().await;
    let listener = ModelsListener::start();
    let mut value = backend("returning", format!("{}/missing", listener.endpoint()));
    let health = BackendHealthMap::new();
    let mut schedule = ProbeSchedule::default();
    seed_backend_observation(&node, &value, "unknown").await;
    scheduled_cycle(&node, &health, &mut schedule, true).await;
    assert_modeled_probe(
        None,
        &health.get(&value.backend_id).await.unwrap(),
        "probeFail",
    );

    value.enabled = false;
    seed_backend_observation(&node, &value, "unknown").await;
    scheduled_cycle(&node, &health, &mut schedule, false).await;
    assert!(health.get(&value.backend_id).await.is_none());
    assert!(!schedule.known.contains_key(&value.backend_id));
    value.enabled = true;
    seed_backend_observation(&node, &value, "unknown").await;
    scheduled_cycle(&node, &health, &mut schedule, false).await;
    assert_modeled_probe(
        None,
        &health.get(&value.backend_id).await.unwrap(),
        "probeFail",
    );

    let plan = crate::store::config_client::DesiredStateApplyPlan::new(Vec::new())
        .unwrap()
        .with_removals(vec![(
            crate::Collection::InferenceBackend,
            value.node_did.clone(),
            value.backend_id.clone(),
        )])
        .unwrap();
    crate::store::config_client::ConfigAccess::transact_local_write(
        &node,
        None,
        "test.backend.remove",
        |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        },
    )
    .await
    .unwrap();
    scheduled_cycle(&node, &health, &mut schedule, false).await;
    assert!(health.get(&value.backend_id).await.is_none());
    assert!(!schedule.known.contains_key(&value.backend_id));
    seed_backend_observation(&node, &value, "unknown").await;
    scheduled_cycle(&node, &health, &mut schedule, false).await;
    assert_modeled_probe(
        None,
        &health.get(&value.backend_id).await.unwrap(),
        "probeFail",
    );
    node.shutdown().await;
}

#[tokio::test]
async fn cycle_reproduces_fleet_evidence_dead_backend_demotes_and_recovers() {
    let options = probe_options();
    let client = reqwest::Client::builder()
        .timeout(options.probe_timeout)
        .build()
        .unwrap();
    let health_map = BackendHealthMap::new();
    let node = test_node().await;

    let listener = ModelsListener::start();
    let endpoint = listener.endpoint();
    let backends = vec![backend("spark", endpoint.clone())];
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
    let backends = vec![backend("spark", recovered.endpoint())];
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
    let node = test_node().await;
    let listener = ModelsListener::start();
    let key_backend = |backend_id: &str, endpoint: String| {
        let mut backend = backend(backend_id, endpoint);
        backend.provider_kind =
            crate::inference::backends::provider::BackendProviderKind::AnthropicApiKey;
        backend.auth = crate::documents::BackendAuth::ApiKey {
            key: "placeholder-key".to_string(),
        };
        backend
    };
    let backends = vec![
        key_backend("a", listener.endpoint()),
        key_backend("b", "http://127.0.0.1:1/v1".to_string()),
    ];

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
    let node = test_node().await;
    let listener = ModelsListener::start();

    let backends = vec![backend("late-arrival", listener.endpoint())];
    seed_backend_observation(&node, &backends[0], "unknown").await;
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

    let backends = vec![backend("late-arrival", listener.endpoint())];
    seed_backend_observation(&node, &backends[0], "healthy").await;
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

    seed_backend_observation(&node, &backends[0], "unknown").await;
    let outcome = run_backend_probe_cycle(
        node.clone(),
        &client,
        &health_map,
        &options,
        "did:key:backend-owner",
    )
    .await;
    assert_eq!(outcome.promotable, vec!["late-arrival".to_string()]);

    let document = node
        .execute(
            r#"{ InferenceBackend(filter: { backend_id: { _eq: "late-arrival" } }) { probe_status last_probe } }"#,
        )
        .await;
    assert!(!document.has_errors(), "{:?}", document.errors);
    let rows = document
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceBackend"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0]["probe_status"], "healthy",
        "the probe owner must persist its promotion"
    );
    assert!(
        !rows[0]["last_probe"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "promotion must stamp last_probe: {rows:?}"
    );
}

#[tokio::test]
async fn runtime_probe_cycle_scopes_same_named_backends_to_its_node() {
    let node = test_node().await;
    let listener = ModelsListener::start();
    let local = backend("same", listener.endpoint());
    let mut foreign = backend("same", "http://127.0.0.1:1/v1".into());
    foreign.node_did = "did:key:foreign-owner".into();
    seed_backend_observation(&node, &local, "unknown").await;
    seed_backend_observation(&node, &foreign, "unknown").await;
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
    let foreign_observation = crate::inference::backends::registry::lookup_backend_observation(
        &node,
        &foreign.node_did,
        &foreign.backend_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(foreign_observation.probe_status.as_deref(), Some("unknown"));
    assert!(foreign_observation.catalogs.is_empty());
}

#[tokio::test]
async fn cycle_never_probes_or_demotes_chatgpt_codex_backends() {
    let options = probe_options();
    let client = reqwest::Client::new();
    let health_map = BackendHealthMap::new();
    let node = test_node().await;

    let mut codex = backend("codex", "http://127.0.0.1:1/v1".to_string());
    codex.provider_kind = crate::inference::backends::provider::BackendProviderKind::ChatGptCodex;
    codex.auth = crate::documents::BackendAuth::NodeOAuth { account_ref: None };
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
async fn cycle_drops_entries_for_backends_no_longer_enabled() {
    let options = probe_options();
    let client = reqwest::Client::new();
    let health_map = BackendHealthMap::new();
    health_map
        .set_for_test("retired", BackendHealthState::Unhealthy, 5)
        .await;

    let node = test_node().await;
    let listener = ModelsListener::start();
    let backends = vec![backend("current", listener.endpoint())];
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

    assert!(health_map.get("retired").await.is_none());
    assert!(health_map.get("current").await.is_some());
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
    let node = test_node().await;
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
    let mut pending = backend("pending", endpoint);
    pending.discovery_timeout_secs = Some(60);
    upsert_backend_configuration(&node, &pending).await;
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
    node.shutdown().await;
}

#[tokio::test]
async fn probe_shutdown_prevents_new_backend_probes_and_pruning() {
    let node = test_node().await;
    let health = BackendHealthMap::new();
    health
        .set_for_test("unobserved", BackendHealthState::Healthy, 0)
        .await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let outcome = probe_selected_backends(
        &node,
        &reqwest::Client::new(),
        &[backend("pending", "http://127.0.0.1:1/v1".into())],
        ProbeEnv {
            now: Utc::now(),
            health_map: &health,
            options: &probe_options(),
            cancel: Some(&cancel),
        },
        None,
        None,
    )
    .await;
    assert!(outcome.flipped.is_empty() && outcome.promotable.is_empty());
    assert!(health.get("pending").await.is_none());
    assert!(health.get("unobserved").await.is_some());
    node.shutdown().await;
}
