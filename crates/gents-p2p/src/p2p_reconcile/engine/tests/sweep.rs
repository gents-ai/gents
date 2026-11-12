#[tokio::test]
async fn pairing_sweep_races_shutdown_as_the_lean_supervisor_model() {
    for case in gents_lean_vocab::families::pairing_reconcile_supervisor_shutdown_race::cases() {
        let store = MockStore::with_desired(Some(PairingDesired {
            replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
            ..Default::default()
        }));
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let admin = MockAdmin {
            active_peers_started: Some(started.clone()),
            active_peers_release: Some(release.clone()),
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
            result = &mut sweep => panic!("{}: sweep returned before admin barrier: {result:?}", case.input.name),
        }

        if case.input.cancelled_during_admin_wait {
            cancel.cancel();
        }
        let before_release = tokio::time::timeout(Duration::from_millis(100), &mut sweep).await;
        let resolved_inside = before_release.is_ok();
        let completed = match before_release {
            Ok(completed) => Some(completed),
            Err(_) => {
                release.notify_one();
                tokio::time::timeout(Duration::from_millis(100), &mut sweep)
                    .await
                    .ok()
            }
        };
        case.check(gents_lean_vocab::families::pairing_reconcile_supervisor_shutdown_race::ShutdownOutput {
            per_admin_call_timeout_ms: crate::p2p_reconcile::embedded_impl::DEFAULT_EMBEDDED_ADMIN_TIMEOUT
                .as_millis() as u64,
            cancellation_observed_inside_sweep: resolved_inside,
            current_admin_future_dropped: resolved_inside,
            remaining_peers_skipped: completed == Some(false),
            shutdown_join_bounded: completed.is_some(),
        });
    }
}

#[tokio::test(start_paused = true)]
async fn pairing_reconciler_retries_sweeps_as_the_lean_supervisor_model() {
    for case in gents_lean_vocab::families::pairing_reconcile_supervisor_sweep_retry::cases() {
        let enumeration_started = Arc::new(tokio::sync::Notify::new());
        let enumeration_release = Arc::new(tokio::sync::Notify::new());
        let convergence_completed = Arc::new(tokio::sync::Notify::new());
        let store = MockStore {
            desired: Mutex::new(Ok(Some(PairingDesired {
                replicator_addresses: set(&["addr1"]),
                ..Default::default()
            }))),
            list_peer_ids_failures: Mutex::new(case.input.initial_failures as usize),
            list_peer_ids_retry_started: Some(enumeration_started.clone()),
            list_peer_ids_retry_release: Some(enumeration_release.clone()),
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
        let started_at = tokio::time::Instant::now();

        let terminated_before_enumeration = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                _ = enumeration_started.notified() => false,
                _ = &mut reconciler => true,
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{}: the enumeration never succeeded", case.input.name));
        let attempts = *store.list_peer_ids_calls.lock().unwrap() as u64;
        let retry_trigger = (attempts > 1).then(|| {
            if tokio::time::Instant::now() == started_at {
                "immediateFirstIntervalTick".to_string()
            } else {
                "laterIntervalTick".to_string()
            }
        });
        let emitted_before_convergence = admin.emitted.lock().unwrap().clone();

        let converged = convergence_completed.notified();
        tokio::pin!(converged);
        enumeration_release.notify_one();
        let terminated_before_convergence = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                _ = &mut converged => false,
                _ = &mut reconciler => true,
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{}: the sweep never converged", case.input.name));
        let converged_ops = admin.emitted.lock().unwrap().clone();
        assert!(emitted_before_convergence.is_empty(), "{}", case.input.name);
        assert_eq!(
            converged_ops,
            vec![DiffOp::InstallReplicator("addr1".to_string())],
            "{}",
            case.input.name
        );

        cancel.cancel();
        let cancellation_prioritized = tokio::time::timeout(Duration::from_millis(100), &mut reconciler)
            .await
            .is_ok();
        case.check(
            gents_lean_vocab::families::pairing_reconcile_supervisor_sweep_retry::RetryOutput {
                failure_terminal: terminated_before_enumeration || terminated_before_convergence,
                retry_trigger,
                cancellation_prioritized,
                convergence_retried: attempts > 1,
                enumeration_attempts: attempts,
            },
        );
    }
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
async fn peer_reconnect_between_sweeps_replays_on_connection_event() {
    let filter = one_filter("AgentRequest", "requester_did", "did:key:peer");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["agent-target-host"]),
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_filter: filter.clone(),
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

#[test]
fn pairing_reconcile_sweep_does_not_head_of_line_block_ready_peer() {
    let cases = gents_lean_vocab::lean_pairing_reconcile_sweep_scheduling_cases();
    assert_eq!(cases.len(), 1);
    let case = &cases[0];
    assert_eq!(case.name, "stale_peer_dial_does_not_block_ready_peer");
    assert_eq!(case.supervisor, "pairingReconciler");
    assert_eq!(case.work_class, "p2pReconcileSweep");
    assert_eq!(case.boundary, "pairingReconcilePeerPreparationBoundary");
    assert_eq!(
        case.max_concurrent_peer_preparations,
        MAX_CONCURRENT_PEER_PREPARATIONS
    );
    assert!(case.peer_preparation_bounded);
    assert!(case.topology_mutation_serialized);
    assert!(!case.stale_peer_blocks_ready_peer);
    assert!(case.every_peer_result_accounted);
}
