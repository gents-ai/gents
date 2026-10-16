#[tokio::test]
async fn install_updates_applied_after_success() {
    let store = MockStore::with_desired(Some(PairingDesired {
        collections: set(&["c1"]),
        replicator_addresses: set(&["addr1"]),
        ..Default::default()
    }));
    let admin = MockAdmin::default();

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("tick result");

    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::InstallCollection("c1".into()),
            DiffOp::InstallReplicator("addr1".into())
        ]
    );
    assert_eq!(*admin.connects.lock().unwrap(), vec![vec!["addr1"]]);
    assert_eq!(
        *store.applied.lock().unwrap(),
        PairingApplied {
            collections: set(&["c1"]),
            replicator_addresses: set(&["addr1"]),
            ..Default::default()
        }
    );
}

#[tokio::test]
async fn active_peer_skips_redial_and_upgrades_data_plane_replicator() {
    let conversation_filter = one_filter("AgentRequest", "requester_did", "did:key:requester");
    let store = MockStore::with_desired(Some(PairingDesired {
        collections: set(&["Network", "AgentRequest"]),
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_filter: conversation_filter.clone(),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        collections: set(&["Network"]),
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A]),
        replicator_filter: PairingFilters::new(),
    };
    let admin = MockAdmin {
        active: Mutex::new(vec![TEST_TRANSPORT_PEER_ID.into()]),
        fail_connect: true,
        ..Default::default()
    };
    *admin.collections.lock().unwrap() = set(&[&mock_collection_id("Network")]);
    admin.replicators.lock().unwrap().insert(
        TEST_TRANSPORT_ADDRESS_A.into(),
        RemoteReplicator {
            id: Some("id-current-endpoint".into()),
            collections: vec!["Network".into()],
            address: Some(TEST_TRANSPORT_ADDRESS_A.into()),
            filters: Some(Default::default()),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("tick must reconcile without dialing");

    assert!(
        admin.connects.lock().unwrap().is_empty(),
        "already-active peer must not be redialed"
    );
    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::InstallCollection("AgentRequest".into()),
            DiffOp::TeardownReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
            DiffOp::InstallReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
        ]
    );
    assert_eq!(
        store.applied.lock().unwrap().replicator_filter,
        conversation_filter
    );
    let recorded = admin.recorded_filters.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].1, conversation_filter);
}

#[tokio::test]
async fn changed_endpoint_replaces_same_peer_replicator_teardown_first() {
    let old_address = "stable-peer@127.0.0.1:4100";
    let fresh_address = "stable-peer@127.0.0.1:4200";
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&[fresh_address]),
        replicator_collections: set(&["AgentRequest"]),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&[old_address]),
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec!["peer-a".into()]),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        old_address.into(),
        RemoteReplicator {
            id: Some("stable-peer".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: None,
            filters: Some(Default::default()),
        },
    );
    admin.replicators.lock().unwrap().insert(
        "other-peer@127.0.0.1:4300".into(),
        RemoteReplicator {
            id: Some("other-peer".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("other-peer@127.0.0.1:4300".into()),
            filters: Some(Default::default()),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("changed endpoint reconcile");

    assert_eq!(
        *admin.connects.lock().unwrap(),
        vec![vec![fresh_address.to_string()]]
    );
    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::TeardownReplicator(old_address.to_string()),
            DiffOp::InstallReplicator(fresh_address.to_string()),
        ]
    );
    assert_eq!(
        store.applied.lock().unwrap().replicator_addresses,
        set(&[fresh_address])
    );
}

#[tokio::test]
async fn matching_applied_repairs_restored_unfiltered_stale_address() {
    let peer_id = "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb";
    let stale_address = format!("127.0.0.1:56091/p2p/{peer_id}");
    let desired_address = format!("127.0.0.1:56092/p2p/{peer_id}");
    let filter = one_filter("AgentRequest", "node_did", "did:key:mandrake");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&[&desired_address]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&[&desired_address]),
        replicator_filter: filter.clone(),
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec![peer_id.to_string()]),
        ..Default::default()
    };
    admin.replicators.lock().unwrap().insert(
        stale_address.clone(),
        RemoteReplicator {
            id: Some(peer_id.to_string()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some(stale_address),
            filters: Some(Default::default()),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("repair restored route");

    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::TeardownReplicator(desired_address.clone()),
            DiffOp::InstallReplicator(desired_address.clone()),
        ]
    );
    assert!(outcome.live_route_matches);
    let live = admin.replicators.lock().unwrap();
    let repaired = live.get(&desired_address).expect("fresh route installed");
    assert_eq!(repaired.address.as_deref(), Some(desired_address.as_str()));
    assert_eq!(repaired.filters.as_ref(), Some(&mock_live_filters(&filter)));
}

#[tokio::test]
async fn omitted_live_filters_force_current_protocol_repair() {
    let filter = one_filter("AgentRequest", "node_did", "did:key:mandrake");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&["addr1"]),
        replicator_filter: filter,
        ..Default::default()
    };
    let admin = MockAdmin::default();
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: None,
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("current-protocol reconcile tick");
    assert!(!outcome.ops_applied.is_empty());
    assert!(outcome.live_route_matches);
    assert!(!admin.emitted.lock().unwrap().is_empty());
    assert!(!admin.recorded_filters.lock().unwrap().is_empty());
}

#[tokio::test]
async fn known_empty_live_filters_repair_a_scoped_route() {
    let filter = one_filter("AgentRequest", "node_did", "did:key:mandrake");
    let store = MockStore::with_desired(Some(PairingDesired {
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: filter.clone(),
        ..Default::default()
    }));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&["addr1"]),
        replicator_filter: filter,
        ..Default::default()
    };
    let admin = MockAdmin::default();
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec![mock_collection_id("AgentRequest")],
            address: Some("addr1".into()),
            filters: Some(Default::default()),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("repair current-wire empty filter drift");

    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::TeardownReplicator("addr1".into()),
            DiffOp::InstallReplicator("addr1".into()),
        ]
    );
    assert!(outcome.live_route_matches);
}

#[test]
fn addressless_replicator_is_not_aliased_across_multiple_applied_routes() {
    let replicator = RemoteReplicator {
        id: Some("stable-peer".to_string()),
        collections: Vec::new(),
        address: None,
        filters: Some(Default::default()),
    };
    let applied = set(&["stable-peer@127.0.0.1:4100", "stable-peer@127.0.0.1:4200"]);

    assert_eq!(
        canonical_replicator_address(&replicator, &applied),
        Some("stable-peer".to_string())
    );
}

#[tokio::test]
async fn duplicate_remote_routes_merge_their_collection_sets() {
    let admin = MockAdmin::default();
    for (key, id, collection) in [
        ("row-a", "id-a", "AgentRequest"),
        ("row-b", "id-b", "AgentResponse"),
    ] {
        admin.replicators.lock().unwrap().insert(
            key.to_string(),
            RemoteReplicator {
                id: Some(id.to_string()),
                collections: vec![mock_collection_id(collection)],
                address: Some("shared-route".to_string()),
                filters: Some(Default::default()),
            },
        );
    }

    let actual = read_actual(&admin, &BTreeSet::new())
        .await
        .expect("read actual routes");

    assert_eq!(
        actual.state.replicator_collections["shared-route"],
        set(&["AgentRequest", "AgentResponse"])
    );
    assert_eq!(actual.replicator_ids_by_addr["shared-route"], "id-a");
}
