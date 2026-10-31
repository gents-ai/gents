#[tokio::test]
async fn cancellation_preempts_in_flight_pairing_sweep_admin_wait() {
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        ..Default::default()
    }));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let admin = MockAdmin {
        active_peers_started: Some(started.clone()),
        active_peers_release: Some(release),
        ..Default::default()
    };
    let cancel = CancellationToken::new();
    let mut replay_connections = BTreeMap::new();
    let mut failing_peers = BTreeSet::new();
    let mut dropped = BTreeSet::new();
    let sweep = sweep_pairings_logged_until_cancelled(
        &admin,
        &store,
        &mut replay_connections,
        &mut failing_peers,
        &mut dropped,
        &cancel,
    );
    tokio::pin!(sweep);

    tokio::select! {
        _ = started.notified() => {}
        result = &mut sweep => panic!("sweep returned before admin barrier: {result:?}"),
    }

    cancel.cancel();
    let completed = tokio::time::timeout(Duration::from_millis(100), &mut sweep)
        .await
        .expect("cancellation must preempt the in-flight admin wait");
    assert!(!completed, "cancelled sweep must skip its remaining peers");
}

#[tokio::test(start_paused = true)]
async fn pairing_reconciler_retries_initial_enumeration_failure_then_cancels_cleanly() {
    let retry_started = Arc::new(tokio::sync::Notify::new());
    let retry_release = Arc::new(tokio::sync::Notify::new());
    let convergence_completed = Arc::new(tokio::sync::Notify::new());
    let store = MockStore {
        desired: Mutex::new(Ok(Some(PairingDesired {
            collections: set(&["AgentRequest"]),
            ..Default::default()
        }))),
        list_peer_ids_failures: Mutex::new(1),
        list_peer_ids_retry_started: Some(retry_started.clone()),
        list_peer_ids_retry_release: Some(retry_release.clone()),
        persist_applied_completed: Some(convergence_completed.clone()),
        ..Default::default()
    };
    let admin = MockAdmin::default();
    let event_bus = events::ChannelBus::new();
    let subscription = event_bus.subscribe_collection_changes(Vec::new());
    let cancel = CancellationToken::new();
    let connections = event_bus.subscribe(&[events::EventName::PeerConnection]);
    let reconciler =
        run_pairing_reconciler_loop(&admin, &store, subscription, connections, &cancel);
    tokio::pin!(reconciler);
    let retry_fence_time = tokio::time::Instant::now();

    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            _ = retry_started.notified() => {}
            result = &mut reconciler => {
                panic!("initial enumeration failure terminated reconciler before retry: {result:?}")
            }
        }
    })
    .await
    .expect("immediate first interval tick must start the retry");
    assert_eq!(
        tokio::time::Instant::now(),
        retry_fence_time,
        "startup retry must consume the already-ready first tick without advancing time"
    );
    assert_eq!(
        *store.list_peer_ids_calls.lock().unwrap(),
        2,
        "the interval's immediately-ready first tick must start the retry"
    );
    assert!(
        admin.emitted.lock().unwrap().is_empty(),
        "the failed initial sweep and gated retry must emit no operation"
    );

    let converged = convergence_completed.notified();
    tokio::pin!(converged);
    retry_release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::select! {
            _ = &mut converged => {}
            result = &mut reconciler => {
                panic!("reconciler terminated before retry convergence: {result:?}")
            }
        }
    })
    .await
    .expect("healthy immediate-tick retry must converge");
    assert_eq!(*store.list_peer_ids_calls.lock().unwrap(), 2);
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![DiffOp::InstallCollection("AgentRequest".to_string())]
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_millis(100), &mut reconciler)
        .await
        .expect("cancellation must stop the reconciler");
}

#[tokio::test]
async fn stale_peer_dial_does_not_head_of_line_block_ready_peer() {
    let desired_for = |address: &str| PairingDesired {
        replicator_addresses: set(&[address]),
        replicator_collections: set(&["AgentRequest"]),
        template_ids: set(&["conversation"]),
        ..Default::default()
    };
    let store = MultiPeerStore {
        desired: BTreeMap::from([
            ("peer-a-stale".into(), desired_for("stale-addr")),
            ("peer-z-ready".into(), desired_for("ready-addr")),
        ]),
        applied: Mutex::new(BTreeMap::new()),
    };
    let stale_started = Arc::new(tokio::sync::Notify::new());
    let stale_release = Arc::new(tokio::sync::Notify::new());
    let replicator_installed = Arc::new(tokio::sync::Notify::new());
    let admin = MockAdmin {
        blocked_connect_address: Some("stale-addr".into()),
        blocked_connect_started: Some(stale_started.clone()),
        blocked_connect_release: Some(stale_release.clone()),
        replicator_installed: Some(replicator_installed.clone()),
        ..Default::default()
    };
    let mut replay_connections = BTreeMap::new();
    let mut failing_peers = BTreeSet::new();
    let sweep = sweep_pairings(&admin, &store, &mut replay_connections, &mut failing_peers);
    tokio::pin!(sweep);

    tokio::select! {
        _ = stale_started.notified() => {}
        result = &mut sweep => panic!("sweep returned before stale dial barrier: {result:?}"),
    }
    tokio::time::timeout(Duration::from_millis(500), replicator_installed.notified())
        .await
        .expect("ready peer must install while stale peer dial remains blocked");
    assert!(
        admin.replicators.lock().unwrap().contains_key("ready-addr"),
        "ready peer topology must converge before stale dial is released"
    );

    stale_release.notify_one();
    tokio::time::timeout(Duration::from_secs(1), &mut sweep)
        .await
        .expect("sweep must finish after stale dial is released")
        .expect("sweep result");
    assert!(admin.replicators.lock().unwrap().contains_key("stale-addr"));
}

#[tokio::test]
async fn read_failure_noops_without_remote_reads() {
    let store = MockStore {
        desired: Mutex::new(Err("boom".into())),
        ..Default::default()
    };
    let admin = MockAdmin::default();

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("tick result");

    assert!(outcome.desired_read_failed);
    assert!(outcome.ops_applied.is_empty());
    assert!(admin.emitted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn peer_reconnect_between_sweeps_replays_on_connection_event() {
    let filter = one_filter("AgentRequest", "requester_did", "did:key:peer");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["agent-target-host"]),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_filter: filter.clone(),
        ..Default::default()
    };
    let installed = Arc::new(tokio::sync::Notify::new());
    let admin = MockAdmin {
        active: Mutex::new(vec![TEST_TRANSPORT_PEER_ID.into()]),
        replicator_installed: Some(installed.clone()),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        TEST_TRANSPORT_ADDRESS_A.into(),
        RemoteReplicator {
            id: Some("id-current-endpoint".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some(TEST_TRANSPORT_ADDRESS_A.into()),
            filters: Some(mock_live_filters(&filter)),
        },
    );
    let event_bus = events::ChannelBus::new();
    let subscription = event_bus.subscribe_collection_changes(Vec::new());
    let connections = event_bus.subscribe(&[events::EventName::PeerConnection]);
    let cancel = CancellationToken::new();
    let reconciler =
        run_pairing_reconciler_loop(&admin, &store, subscription, connections, &cancel);
    tokio::pin!(reconciler);

    let startup_replay = installed.notified();
    tokio::pin!(startup_replay);
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = &mut startup_replay => {}
            result = &mut reconciler => panic!("reconciler stopped: {result:?}"),
        }
    })
    .await
    .expect("startup sweep replays once");

    // The peer drops and reconnects long before the next periodic sweep.
    for connected in [false, true] {
        event_bus.publish(events::Message::peer_connection(
            events::PeerConnectionData {
                peer_id: TEST_TRANSPORT_PEER_ID.into(),
                connected,
            },
        ));
    }
    let reconnect_replay = installed.notified();
    tokio::pin!(reconnect_replay);
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            _ = &mut reconnect_replay => {}
            result = &mut reconciler => panic!("reconciler stopped: {result:?}"),
        }
    })
    .await
    .expect("the connection event replays without waiting for the sweep interval");
    assert_eq!(
        admin
            .emitted
            .lock()
            .unwrap()
            .iter()
            .filter(|op| matches!(op, DiffOp::InstallReplicator(_)))
            .count(),
        2
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_millis(100), &mut reconciler)
        .await
        .expect("cancellation must stop the reconciler");
}
