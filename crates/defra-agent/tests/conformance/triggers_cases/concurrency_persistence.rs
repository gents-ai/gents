use super::*;

/// Serial concurrency: when an active runtime request already exists for the
/// event-kind lineage tuple, the engine's gating query sees it → `FireResult::Skipped`.
/// No second `AgentRequest` materializes for the same tuple. Asserted at the
/// persistence-layer contract (PR 1 pattern).
#[tokio::test]
async fn serial_skips_when_prior_active_runtime() {
    let db = test_db("trigger-conformance-event-serial-skip").await;

    let lineage = TriggerLineage {
        trigger_id: Some("trigger-event-serial".into()),
        trigger_kind: Some("event".into()),
    };
    RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "seed event in-flight",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();

    assert!(
        has_active_runtime_request_for_trigger(
            db.node.as_ref(),
            AGENT_DID,
            "trigger-event-serial",
            "event"
        )
        .await,
        "gating query must see the in-flight event-kind request"
    );
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-event-serial", "event",).await,
        1,
        "seeded count should be 1"
    );

    // The engine's FireResult::Skipped decision: no materialize call, so no
    // additional AgentRequest for the lineage tuple.
    let after =
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-event-serial", "event").await;
    assert_eq!(
        after, 1,
        "serial skip must not produce a second AgentRequest for the event-kind tuple"
    );
}

/// LatestOnly concurrency: the engine's supersede mutation transitions the
/// in-flight event-kind request to `superseded`; a new materialize lands
/// with the same lineage tuple. Asserted at the persistence-layer contract.
#[tokio::test]
async fn latest_only_supersedes_prior_fire() {
    let db = test_db("trigger-conformance-event-latest-only").await;

    let lineage = TriggerLineage {
        trigger_id: Some("trigger-event-latest".into()),
        trigger_kind: Some("event".into()),
    };
    let prior = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "seed prior",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        lineage,
    )
    .await
    .unwrap();
    let prior_request_id = prior.request().request_id.clone();

    let superseded = supersede_active_runtime_requests_for_trigger(
        db.node.as_ref(),
        AGENT_DID,
        "trigger-event-latest",
        "event",
    )
    .await;
    assert_eq!(
        superseded, 1,
        "supersede mutation must transition exactly the one in-flight request"
    );
    let prior_state = fetch_request_state(db.node.as_ref(), &prior_request_id)
        .await
        .expect("prior request still present");
    assert_eq!(
        prior_state,
        ("superseded".into(), "superseded".into()),
        "prior event-kind AgentRequest must be (lifecycle_state=superseded, status=superseded)"
    );

    // Materialize the new fire with the same trigger lineage.
    let new_lineage = TriggerLineage {
        trigger_id: Some("trigger-event-latest".into()),
        trigger_kind: Some("event".into()),
    };
    let new_fire = RequestLifecycle::materialize_claimed_with_execution_binding(
        db.node.clone(),
        AGENT_NAME,
        AGENT_DID,
        "latest event fire",
        DEADLINE_SECS,
        ExecutionOrigin::Scheduled,
        BACKEND_ID,
        new_lineage,
    )
    .await
    .unwrap();
    assert_ne!(
        new_fire.request().request_id,
        prior_request_id,
        "new fire must have a fresh request_id"
    );
    assert_eq!(
        count_agent_requests_for_trigger(db.node.as_ref(), "trigger-event-latest", "event",).await,
        2
    );
    assert!(
        has_active_runtime_request_for_trigger(
            db.node.as_ref(),
            AGENT_DID,
            "trigger-event-latest",
            "event"
        )
        .await,
        "after materialize, the new claimed request must be visible to the gating query"
    );
}
