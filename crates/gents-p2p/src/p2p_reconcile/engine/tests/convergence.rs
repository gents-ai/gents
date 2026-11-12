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

