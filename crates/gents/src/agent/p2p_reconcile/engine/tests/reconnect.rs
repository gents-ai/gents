#[tokio::test]
async fn degraded_first_sweep_preserves_startup_replay_without_repeats() {
    let filter = one_filter("AgentRequest", "node_did", "did:key:local-owner");
    let desired = PairingDesired {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["agent-target-host"]),
        ..Default::default()
    };
    let store = MockStore {
        desired: Mutex::new(Err("transient desired read".into())),
        applied: Mutex::new(PairingApplied {
            replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
            replicator_filter: filter,
            ..Default::default()
        }),
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec![TEST_TRANSPORT_PEER_ID.into()]),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        TEST_TRANSPORT_ADDRESS_A.into(),
        RemoteReplicator {
            id: Some("id-current-endpoint".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some(TEST_TRANSPORT_ADDRESS_A.into()),
            filters: Some(Default::default()),
        },
    );
    let mut replay_connections = BTreeMap::new();

    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("degraded desired-read sweep");
    assert_eq!(replay_connections.get("peer-a"), Some(&false));

    *store.desired.lock().unwrap() = Ok(Some(desired));
    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("healthy follow-up sweep");

    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![
            DiffOp::TeardownReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
            DiffOp::InstallReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
        ],
        "the deferred startup replay must fire on the first healthy sweep"
    );
    assert_eq!(replay_connections.get("peer-a"), Some(&true));

    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("steady-state sweep");
    assert_eq!(
        admin.emitted.lock().unwrap().len(),
        2,
        "a steady-state sweep without a connection edge must not replay again"
    );
}

#[tokio::test]
async fn desired_read_failure_during_reconnect_keeps_replay_pending() {
    let filter = one_filter("AgentRequest", "node_did", "did:key:local-owner");
    let desired = PairingDesired {
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["agent-target-host"]),
        ..Default::default()
    };
    let store = MockStore {
        desired: Mutex::new(Err("transient desired read".into())),
        applied: Mutex::new(PairingApplied {
            replicator_addresses: set(&["addr1"]),
            replicator_filter: filter,
            ..Default::default()
        }),
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec!["peer-a".into()]),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(Default::default()),
        },
    );
    let mut replay_connections = BTreeMap::from([("peer-a".to_string(), false)]);

    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("degraded reconnect sweep");
    assert_eq!(replay_connections.get("peer-a"), Some(&false));

    *store.desired.lock().unwrap() = Ok(Some(desired));
    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("healthy follow-up replays pending reconnect");

    assert_eq!(replay_connections.get("peer-a"), Some(&true));
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![
            DiffOp::TeardownReplicator("addr1".into()),
            DiffOp::InstallReplicator("addr1".into()),
        ]
    );
}

#[tokio::test]
async fn failed_reconnect_replay_is_healed_by_next_tick_diff() {
    let filter = one_filter("AgentRequest", "node_did", "did:key:local-owner");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["agent-target-host"]),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&["addr1"]),
        replicator_filter: filter,
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec!["peer-a".into()]),
        fail_add_replicator_attempts: Mutex::new(1),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(Default::default()),
        },
    );
    let mut replay_connections = BTreeMap::new();

    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("sweep contains per-peer replay failure");
    assert_eq!(replay_connections.get("peer-a"), Some(&false));
    assert!(admin.replicators.lock().unwrap().is_empty());
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![DiffOp::TeardownReplicator("addr1".into())]
    );

    sweep_pairings(
        &admin,
        &store,
        &mut replay_connections,
        &mut BTreeSet::<String>::new(),
    )
    .await
    .expect("next sweep heals torn replay");
    assert_eq!(replay_connections.get("peer-a"), Some(&true));
    assert!(admin.replicators.lock().unwrap().contains_key("addr1"));
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![
            DiffOp::TeardownReplicator("addr1".into()),
            DiffOp::InstallReplicator("addr1".into()),
        ]
    );
}

#[tokio::test]
async fn reconnect_force_replays_converged_agent_target_replicator() {
    let filter = one_filter("AgentRequest", "node_did", "did:key:local-owner");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["agent-target-host"]),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&["addr1"]),
        replicator_filter: filter.clone(),
        ..Default::default()
    };
    let admin = MockAdmin::default();
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(mock_live_filters(&filter)),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("reconnect replay tick");

    assert!(
        outcome.ops_applied.is_empty(),
        "topology was already converged"
    );
    assert_eq!(outcome.replayed_replicators, vec!["addr1"]);
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![
            DiffOp::TeardownReplicator("addr1".into()),
            DiffOp::InstallReplicator("addr1".into()),
        ],
        "reconnect must force one bounded full replay"
    );
}

#[tokio::test]
async fn inbound_reconnect_force_replays_without_owner_redial() {
    let filter = one_filter("AgentRequest", "node_did", "did:key:local-owner");
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
    let admin = MockAdmin::default();
    admin
        .active
        .lock()
        .unwrap()
        .push(TEST_TRANSPORT_PEER_ID.into());
    admin.replicators.lock().unwrap().insert(
        TEST_TRANSPORT_ADDRESS_A.into(),
        RemoteReplicator {
            id: Some("id-current-endpoint".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some(TEST_TRANSPORT_ADDRESS_A.into()),
            filters: Some(mock_live_filters(&filter)),
        },
    );

    let outcome = reconcile_peer_tick_with_replay(&admin, &store, "peer-a", true)
        .await
        .expect("inbound reconnect replay tick");

    assert!(admin.connects.lock().unwrap().is_empty());
    assert_eq!(outcome.replayed_replicators, vec![TEST_TRANSPORT_ADDRESS_A]);
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![
            DiffOp::TeardownReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
            DiffOp::InstallReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
        ]
    );
}

#[tokio::test]
async fn reconnect_replay_reinstalls_when_delete_reports_remote_not_found() {
    let filter = one_filter("AgentRequest", "node_did", "did:key:local-owner");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["agent-target-host"]),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&["addr1"]),
        replicator_filter: filter.clone(),
        ..Default::default()
    };
    let admin = MockAdmin::default();
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(mock_live_filters(&filter)),
        },
    );
    *admin.delete_replicator_not_found.lock().unwrap() = true;

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("missing old replicator must not prevent reconnect replay");

    assert_eq!(outcome.replayed_replicators, vec!["addr1"]);
    assert_eq!(
        *admin.emitted.lock().unwrap(),
        vec![DiffOp::InstallReplicator("addr1".into())],
        "replay must reinstall after the concurrent removal"
    );
    assert!(admin.replicators.lock().unwrap().contains_key("addr1"));
}

#[tokio::test]
async fn reconnect_skips_replay_when_peer_acknowledged_every_document() {
    let filter = one_filter("AgentRequest", "requester_did", "did:key:peer");
    let desired = PairingDesired {
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest", "AgentSession"]),
        replicator_filter: filter.clone(),
        template_ids: set(&["agent-target-host"]),
        ..Default::default()
    };
    let store = MockStore::with_desired(Some(desired));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&["addr1"]),
        replicator_filter: filter.clone(),
        ..Default::default()
    };
    let admin = MockAdmin {
        acknowledged_peers: set(&["id-addr1"]),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec![
                mock_collection_id("AgentRequest"),
                mock_collection_id("AgentSession"),
            ],
            address: Some("addr1".into()),
            filters: Some(mock_live_filters(&filter)),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("acknowledged reconnect tick");

    assert!(outcome.replayed_replicators.is_empty());
    assert!(
        admin.emitted.lock().unwrap().is_empty(),
        "an acknowledged replicator keeps its retry markers and acknowledgement records"
    );
    assert_eq!(
        *admin.acknowledgement_queries.lock().unwrap(),
        vec![
            ("id-addr1".to_string(), "AgentRequest".to_string()),
            ("id-addr1".to_string(), "AgentSession".to_string()),
        ]
    );
}
