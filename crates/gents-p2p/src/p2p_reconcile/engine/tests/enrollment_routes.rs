#[tokio::test]
async fn committed_revocation_between_desired_load_and_apply_tears_down_stale_route() {
    let store = enrolled_live_route_store([Ok(true), Ok(false)]);
    let admin = admin_with_live_enrolled_route();

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("revoked route reconciles fail closed");

    assert_eq!(
        outcome.ops_applied,
        vec![DiffOp::TeardownReplicator("addr1".into())]
    );
    assert!(admin.connects.lock().unwrap().is_empty());
    assert!(admin.replicators.lock().unwrap().is_empty());
    assert!(store.applied.lock().unwrap().is_empty());
    let saved = store.saved.lock().unwrap();
    assert!(!saved[0].state.is_empty());
    assert_eq!(saved[0].enrollment, EnrollmentMaterialization::default());
}

#[tokio::test]
async fn enrollment_projection_read_failure_tears_down_instead_of_preserving_live_route() {
    let store = enrolled_live_route_store([Err("projection unavailable".into())]);
    let admin = admin_with_live_enrolled_route();

    let outcome = reconcile_peer_tick(&admin, &store, "peer-a")
        .await
        .expect("unknown enrollment authority closes the route");

    assert!(!outcome.desired_read_failed);
    assert_eq!(
        outcome.ops_applied,
        vec![DiffOp::TeardownReplicator("addr1".into())]
    );
    assert!(admin.connects.lock().unwrap().is_empty());
    assert!(admin.replicators.lock().unwrap().is_empty());
    assert!(store.applied.lock().unwrap().is_empty());
    let saved = store.saved.lock().unwrap();
    assert!(!saved[0].state.is_empty());
    assert_eq!(saved[0].enrollment, EnrollmentMaterialization::default());
}

#[tokio::test]
async fn converged_route_records_current_enrollment_without_reinstalling() {
    let store = enrolled_live_route_store([Ok(true), Ok(true), Ok(true), Ok(true)]);
    *store.materialization.lock().unwrap() = Default::default();
    let admin = admin_with_live_enrolled_route();
    let outcome = reconcile_peer_tick(&admin, &store, "peer-a").await.unwrap();
    assert!(outcome.ops_applied.is_empty());
    assert_eq!(
        *store.materialization.lock().unwrap(),
        EnrollmentMaterialization::from(&store.generation)
    );
    assert_eq!(store.saved.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn converged_route_cannot_record_an_authorization_lost_before_publication() {
    let store = enrolled_live_route_store([Ok(true), Ok(true), Ok(true), Ok(false)]);
    let admin = admin_with_live_enrolled_route();
    let outcome = reconcile_peer_tick(&admin, &store, "peer-a").await.unwrap();
    assert!(outcome.ops_applied.is_empty());
    assert_eq!(
        *store.materialization.lock().unwrap(),
        EnrollmentMaterialization::default()
    );
}
