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

    let calls = admin.recorded_filters.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].1.is_empty(),
        "Replicate template must install an unfiltered replicator"
    );
}
