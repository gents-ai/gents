use crate::interrupt::{interrupt_request, latch_request_interrupt};
use crate::tests::support::{fetch_request_row, seed_provenance_fixture, seed_standalone_fixture};
use crate::types::DesktopInterruptRequest;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latch_writes_interrupt_requested_at_when_absent() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let before = fetch_request_row(&core, "req_solo").await;
    assert!(before.interrupt_requested_at.is_none());

    let latched = latch_request_interrupt(&core, "req_solo", None)
        .await
        .expect("latch ok");
    assert!(latched.was_first);
    assert!(!latched.interrupt_requested_at.is_empty());

    let after = fetch_request_row(&core, "req_solo").await;
    assert_eq!(
        after.interrupt_requested_at.as_deref(),
        Some(latched.interrupt_requested_at.as_str())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latch_is_noop_when_already_interrupted() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let first = latch_request_interrupt(&core, "req_solo", None)
        .await
        .expect("first latch");
    let second = latch_request_interrupt(&core, "req_solo", None)
        .await
        .expect("second latch");
    assert!(!second.was_first);
    assert_eq!(second.interrupt_requested_at, first.interrupt_requested_at);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_returns_accepted() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let result = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_solo".into(),
            agent_did: None,
            cause: "userCancelled".into(),
        },
    )
    .await
    .expect("ok");
    assert!(result.accepted);
    assert!(!result.already_interrupted);
    assert!(result.interrupt_requested_at.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_returns_already_interrupted_for_second_call() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let first = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_solo".into(),
            agent_did: None,
            cause: "userCancelled".into(),
        },
    )
    .await
    .expect("first");
    let second = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_solo".into(),
            agent_did: None,
            cause: "userCancelled".into(),
        },
    )
    .await
    .expect("second");
    assert!(second.accepted);
    assert!(second.already_interrupted);
    assert_eq!(second.interrupt_requested_at, first.interrupt_requested_at);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_rejects_terminal_request_without_latching_it() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let terminalize = r#"mutation {
        update_AgentRequest(
            filter: { request_id: { _eq: "req_solo" } },
            input: { lifecycle_state: "completed" }
        ) { _docID }
    }"#;
    let response = core.node().execute(terminalize).await;
    assert!(
        !response.has_errors(),
        "terminalize fixture failed: {:?}",
        response.errors
    );

    let error = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_solo".into(),
            agent_did: None,
            cause: "userCancelled".into(),
        },
    )
    .await
    .expect_err("terminal request must reject stale interrupt");

    assert!(error.contains("already terminal"));
    let after = fetch_request_row(&core, "req_solo").await;
    assert!(
        after.interrupt_requested_at.is_none(),
        "rejected terminal interrupt must not corrupt the audit latch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_rejects_non_user_cancelled_cause() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let err = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_solo".into(),
            agent_did: None,
            cause: "deadline".into(),
        },
    )
    .await
    .unwrap_err();
    assert!(err.contains("userCancelled"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_reaches_only_the_named_request() {
    let (core, _tmp, _) = seed_provenance_fixture().await;
    let result = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_parent".into(),
            agent_did: Some("did:test:operator".into()),
            cause: "userCancelled".into(),
        },
    )
    .await
    .expect("interrupt parent");
    assert!(result.accepted);

    assert!(fetch_request_row(&core, "req_parent")
        .await
        .interrupt_requested_at
        .is_some());
    for caused in ["req_child_2", "req_peer"] {
        assert!(
            fetch_request_row(&core, caused)
                .await
                .interrupt_requested_at
                .is_none(),
            "{caused} was caused by the interrupted request and must keep running"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_stops_a_caused_request_on_its_own_agent() {
    let (core, _tmp, _) = seed_provenance_fixture().await;
    let result = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_peer".into(),
            agent_did: Some("did:test:other".into()),
            cause: "userCancelled".into(),
        },
    )
    .await
    .expect("interrupt caused request");
    assert!(result.accepted);
    assert!(fetch_request_row(&core, "req_parent")
        .await
        .interrupt_requested_at
        .is_none());
}
