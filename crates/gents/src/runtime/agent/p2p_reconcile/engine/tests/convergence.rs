#[tokio::test]
async fn owned_teardown_removes_endpoint_despite_collection_drift() {
    let admin = MockAdmin::default();
    let owned_address =
        "127.0.0.1:56091/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb"
            .to_string();
    let unrelated_address =
        "127.0.0.1:56092/p2p/7fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bc"
            .to_string();
    admin.replicators.lock().unwrap().insert(
        owned_address.clone(),
        RemoteReplicator {
            id: Some("drifted-owned-replicator".into()),
            collections: vec![mock_collection_id("unexpected-drifted-collection")],
            address: Some(owned_address.clone()),
            filters: Some(Default::default()),
        },
    );
    admin.replicators.lock().unwrap().insert(
        unrelated_address.clone(),
        RemoteReplicator {
            id: Some("unrelated-replicator".into()),
            collections: vec![mock_collection_id("unrelated-collection")],
            address: Some(unrelated_address.clone()),
            filters: Some(Default::default()),
        },
    );

    let removed = teardown_owned_replicators_at_endpoint(&admin, &owned_address)
        .await
        .expect("owned endpoint cleanup");

    assert_eq!(removed, 1);
    let remaining = admin.replicators.lock().unwrap();
    assert!(!remaining.contains_key(&owned_address));
    assert!(remaining.contains_key(&unrelated_address));
    assert_eq!(
        *admin.deleted_replicator_collections.lock().unwrap(),
        vec![vec![mock_collection_id("unexpected-drifted-collection")]],
        "authoritative teardown must pass the live drifted CollectionIDs"
    );
}

#[tokio::test]
async fn stale_applied_endpoint_absent_from_actual_is_torn_down_once() {
    let desired = PairingDesired {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_B]),
        replicator_collections: set(&["AgentRequest"]),
        ..Default::default()
    };
    let store = MockStore::with_desired(Some(desired));
    *store.applied.lock().unwrap() = PairingApplied {
        replicator_addresses: set(&[TEST_TRANSPORT_ADDRESS_A, TEST_TRANSPORT_ADDRESS_B]),
        ..Default::default()
    };
    let admin = MockAdmin {
        active: Mutex::new(vec![TEST_TRANSPORT_PEER_ID.into()]),
        ..Default::default()
    };
    let first = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("first reconcile");
    let second = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("second reconcile");

    assert_eq!(
        first.ops_applied,
        vec![
            DiffOp::TeardownReplicator(TEST_TRANSPORT_ADDRESS_A.into()),
            DiffOp::InstallReplicator(TEST_TRANSPORT_ADDRESS_B.into()),
        ]
    );
    assert!(second.ops_applied.is_empty());
    assert_eq!(
        *admin.connects.lock().unwrap(),
        vec![vec![TEST_TRANSPORT_ADDRESS_B.to_string()]]
    );
    assert_eq!(
        *admin.deleted_replicator_collections.lock().unwrap(),
        vec![vec!["AgentRequest".to_string()]]
    );
    assert_eq!(
        store.applied.lock().unwrap().replicator_addresses,
        set(&[TEST_TRANSPORT_ADDRESS_B])
    );
}

#[tokio::test]
async fn grown_replicator_collection_set_reinstalls_replicator() {
    let conversation_filter = one_filter("AgentRequest", "requester_did", "did:key:requester");
    let store = MockStore::with_desired(Some(PairingDesired {
        collections: BTreeSet::new(),
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["AgentRequest"]),
        replicator_filter: conversation_filter.clone(),
        ..Default::default()
    }));
    let admin = MockAdmin {
        active: Mutex::new(vec!["peer-a".into()]),
        ..Default::default()
    };

    let first = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("first tick");
    assert_eq!(
        first.ops_applied,
        vec![DiffOp::InstallReplicator("addr1".into())]
    );

    *store.desired.lock().unwrap() = Ok(Some(PairingDesired {
        collections: set(&["Network"]),
        replicator_addresses: set(&["addr1"]),
        replicator_collections: set(&["Network", "AgentRequest"]),
        replicator_filter: conversation_filter,
        ..Default::default()
    }));

    let second = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("second tick");
    assert_eq!(
        second.ops_applied,
        vec![
            DiffOp::InstallCollection("Network".into()),
            DiffOp::TeardownReplicator("addr1".into()),
            DiffOp::InstallReplicator("addr1".into()),
        ]
    );

    assert_eq!(
        admin.replicators.lock().unwrap()["addr1"].collections,
        vec![
            mock_collection_id("AgentRequest"),
            mock_collection_id("Network")
        ]
    );

    let third = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("third tick");
    assert!(
        third.ops_applied.is_empty(),
        "converged, got: {:?}",
        third.ops_applied
    );
}

#[tokio::test]
async fn active_peer_read_failure_still_dials() {
    let store = MockStore::with_desired(Some(PairingDesired {
        collections: set(&["c1"]),
        replicator_addresses: set(&["addr1"]),
        ..Default::default()
    }));
    let admin = MockAdmin {
        fail_active_peers: true,
        ..Default::default()
    };

    reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("tick result");

    assert_eq!(*admin.connects.lock().unwrap(), vec![vec!["addr1"]]);
}

#[tokio::test]
async fn other_active_peer_does_not_suppress_dial() {
    let store = MockStore::with_desired(Some(PairingDesired {
        collections: set(&["c1"]),
        replicator_addresses: set(&["addr1"]),
        ..Default::default()
    }));
    let admin = MockAdmin {
        active: Mutex::new(vec!["peer-b".into()]),
        ..Default::default()
    };

    reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("tick result");

    assert_eq!(*admin.connects.lock().unwrap(), vec![vec!["addr1"]]);
}

#[tokio::test]
async fn second_tick_converges_across_name_and_id_spaces() {
    let store = MockStore::with_desired(Some(PairingDesired {
        collections: set(&["AgentRequest"]),
        replicator_addresses: set(&["addr1"]),
        ..Default::default()
    }));
    let admin = MockAdmin::default();

    let first = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("first tick");
    assert!(
        first
            .ops_applied
            .iter()
            .any(|op| matches!(op, DiffOp::InstallCollection(_))),
        "first tick installs the collection: {:?}",
        first.ops_applied
    );

    assert_eq!(
        store.applied.lock().unwrap().collections,
        set(&["AgentRequest"]),
        "Applied persists the collection name"
    );

    let second = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("second tick");
    assert!(
        second.ops_applied.is_empty(),
        "second tick must be a no-op (converged), got: {:?}",
        second.ops_applied
    );
}

#[tokio::test]
async fn teardown_is_restricted_to_applied_extras() {
    let store = MockStore::with_desired(Some(PairingDesired::default()));
    *store.applied.lock().unwrap() = PairingApplied {
        collections: set(&["managed"]),
        replicator_addresses: set(&["managed-addr"]),
        ..Default::default()
    };
    let admin = MockAdmin::default();
    *admin.collections.lock().unwrap() = set(&[
        &mock_collection_id("managed"),
        &mock_collection_id("manual"),
    ]);
    admin.replicators.lock().unwrap().insert(
        "managed-addr".into(),
        RemoteReplicator {
            id: Some("managed-id".into()),
            collections: vec!["managed".into()],
            address: Some("managed-addr".into()),
            filters: Some(Default::default()),
        },
    );
    admin.replicators.lock().unwrap().insert(
        "manual-addr".into(),
        RemoteReplicator {
            id: Some("manual-id".into()),
            collections: vec!["manual".into()],
            address: Some("manual-addr".into()),
            filters: Some(Default::default()),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("tick result");

    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::TeardownCollection("managed".into()),
            DiffOp::TeardownReplicator("managed-addr".into())
        ]
    );
    assert_eq!(
        *admin.collections.lock().unwrap(),
        set(&[&mock_collection_id("manual")])
    );
    assert!(admin
        .replicators
        .lock()
        .unwrap()
        .contains_key("manual-addr"));
}

#[tokio::test]
async fn desired_absent_tears_down_managed_state_and_deletes_applied_row() {
    let store = MockStore::with_desired(None);
    *store.applied.lock().unwrap() = PairingApplied {
        collections: set(&["c1"]),
        replicator_addresses: set(&["addr1"]),
        ..Default::default()
    };
    let admin = MockAdmin::default();
    *admin.collections.lock().unwrap() = set(&[&mock_collection_id("c1")]);
    admin.replicators.lock().unwrap().insert(
        "addr1".into(),
        RemoteReplicator {
            id: Some("id-addr1".into()),
            collections: vec!["c1".into()],
            address: Some("addr1".into()),
            filters: Some(Default::default()),
        },
    );

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("tick result");

    assert_eq!(
        outcome.ops_applied,
        vec![
            DiffOp::TeardownCollection("c1".into()),
            DiffOp::TeardownReplicator("addr1".into())
        ]
    );
    assert_eq!(*store.deleted.lock().unwrap(), 1);
    assert!(store.applied.lock().unwrap().is_empty());
}
