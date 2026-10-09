use crate::interrupt::interrupt_request;
use crate::tests::support::{fetch_request_row, seed_provenance_fixture, seed_standalone_fixture};
use crate::types::DesktopInterruptRequest;

fn user_cancel(request_id: &str) -> DesktopInterruptRequest {
    DesktopInterruptRequest {
        request_id: request_id.into(),
        node_did: None,
        cause: "userCancelled".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_writes_the_owner_latch_and_reports_its_timestamp() {
    let (core, _tmp) = seed_standalone_fixture().await;
    assert!(fetch_request_row(&core, "req_solo")
        .await
        .interrupt_requested_at
        .is_none());

    let result = interrupt_request(&core, &user_cancel("req_solo"))
        .await
        .expect("interrupt");
    let stored = fetch_request_row(&core, "req_solo").await;
    assert!(stored.interrupt_requested_at.is_some());
    assert_eq!(result.interrupt_requested_at, stored.interrupt_requested_at);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_refuses_a_logical_id_carried_by_two_physical_requests() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let duplicate = r#"mutation {
        create_AgentRequest(input: { purpose: "normal",
            request_id: "req_solo",
            node_did: "did:test:operator",
            agent_id: "test-agent",
            session_id: "sess_other",
            content: "duplicate logical root",
            lifecycle_state: "processing",
            backend_id: "",
            created_at: "2026-05-20T00:00:01Z",
            retry_count: 0
        }) { _docID }
    }"#;
    let response = core.node().execute(duplicate).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let error = interrupt_request(&core, &user_cancel("req_solo"))
        .await
        .expect_err("ambiguous logical root");
    assert!(error.contains("ambiguous"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_returns_accepted() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let result = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_solo".into(),
            node_did: None,
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
            node_did: None,
            cause: "userCancelled".into(),
        },
    )
    .await
    .expect("first");
    let second = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_solo".into(),
            node_did: None,
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
            node_did: None,
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
            node_did: None,
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
            node_did: Some("did:test:operator".into()),
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
async fn interrupt_request_stops_a_caused_request_on_its_own_node() {
    let (core, _tmp, _) = seed_provenance_fixture().await;
    let result = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_peer".into(),
            node_did: Some("did:test:other".into()),
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
