/// End-to-end reconcile of a `Replicate` (agent-config) template: it both
/// subscribes (`add_p2p_collections`) and installs an UNFILTERED replicator.
#[tokio::test]
async fn replicate_template_subscribes_and_replicates() {
    let store = MockStore::with_desired(Some(
        desired_from_pairing_row(
            desired_row(Some("agent-config"), Some("did:key:bob")),
            "did:key:self",
        )
        .expect("template resolves")
        .expect("some desired layer"),
    ));
    let admin = MockAdmin::default();

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("tick result");

    let emitted = admin.emitted.lock().unwrap();
    assert!(
        emitted
            .iter()
            .any(|op| matches!(op, DiffOp::InstallCollection(_))),
        "Replicate template must subscribe: {emitted:?}"
    );
    assert!(emitted
        .iter()
        .any(|op| matches!(op, DiffOp::InstallReplicator(_))));
    drop(emitted);
    assert!(outcome
        .ops_applied
        .iter()
        .any(|op| matches!(op, DiffOp::InstallReplicator(_))));

    // The installed replicator is unfiltered.
    let calls = admin.recorded_filters.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].1.is_empty(),
        "Replicate template must install an unfiltered replicator"
    );
}

// -----------------------------------------------------------------------
// T2: filters at the RemoteP2pAdmin seam
// -----------------------------------------------------------------------

/// Verifies that the `MockAdmin` recording captures `PairingFilters` passed
/// to `add_replicator`, and that an empty `PairingFilters` records as empty
/// (back-compat) while a non-empty one is faithfully recorded.
#[tokio::test]
async fn add_replicator_records_filters_at_seam() {
    let admin = MockAdmin::default();
    let addresses = vec!["addr-a".to_string()];
    let collections: Vec<String> = vec![];

    // Back-compat: empty filters record as empty.
    admin
        .add_replicator(&addresses, &collections, &PairingFilters::default())
        .await
        .expect("add_replicator empty filters");

    {
        let calls = admin.recorded_filters.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].1.is_empty(),
            "empty filters should record as empty"
        );
    }

    // Non-empty filters are faithfully recorded.
    let mut filters = PairingFilters::default();
    filters.insert(
        "AgentRequest".to_string(),
        equality_filter("node_did", "did:key:alice"),
    );
    admin
        .add_replicator(&addresses, &collections, &filters)
        .await
        .expect("add_replicator non-empty filters");

    let calls = admin.recorded_filters.lock().unwrap();
    assert_eq!(calls.len(), 2);
    let recorded = &calls[1].1;
    assert_eq!(recorded.len(), 1);
    let pred = recorded.get("AgentRequest").expect("AgentRequest filter");
    assert_eq!(single_string_eq(pred), Some(("node_did", "did:key:alice")));
}
