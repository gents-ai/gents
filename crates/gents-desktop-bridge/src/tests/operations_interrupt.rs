use crate::cascade::{
    build_cascade_preview, interrupt_request, latch_cascade_descendant_interrupt,
    latch_root_interrupt,
};
use crate::tests::support::{fetch_request_row, seed_cascade_fixture, seed_standalone_fixture};
use crate::types::{DesktopInterruptRequest, DesktopPreviewInterruptCascadeRequest};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latch_writes_interrupt_requested_at_when_absent() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let before = fetch_request_row(&core, "req_solo").await;
    assert!(before.interrupt_requested_at.is_none());

    let latched = latch_root_interrupt(&core, "req_solo", None)
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
    let _ = latch_root_interrupt(&core, "req_solo", None)
        .await
        .expect("first latch");
    let second = latch_root_interrupt(&core, "req_solo", None)
        .await
        .expect("second latch");
    assert!(!second.was_first);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn descendant_latch_is_idempotent_and_preserves_first_timestamp() {
    let (core, _tmp) = seed_cascade_fixture().await;
    let first = latch_cascade_descendant_interrupt(&core, "req_b91")
        .await
        .expect("first descendant latch")
        .expect("active descendant");
    let second = latch_cascade_descendant_interrupt(&core, "req_b91")
        .await
        .expect("second descendant latch")
        .expect("latched descendant");

    assert!(first.was_first);
    assert!(!second.was_first);
    assert_eq!(second.interrupt_requested_at, first.interrupt_requested_at);
    let stored = fetch_request_row(&core, "req_b91").await;
    assert_eq!(
        stored.interrupt_requested_at.as_deref(),
        Some(first.interrupt_requested_at.as_str())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn descendant_that_terminalized_after_preview_is_not_latched() {
    let (core, _tmp) = seed_cascade_fixture().await;
    let terminalize = r#"mutation {
        update_AgentRequest(
            filter: { request_id: { _eq: "req_b91" } },
            input: { lifecycle_state: "completed" }
        ) { _docID }
    }"#;
    let response = core.node().execute(terminalize).await;
    assert!(
        !response.has_errors(),
        "terminalize child fixture failed: {:?}",
        response.errors
    );

    let latched = latch_cascade_descendant_interrupt(&core, "req_b91")
        .await
        .expect("terminal descendant is a stale-preview no-op");
    assert!(latched.is_none());
    assert!(
        fetch_request_row(&core, "req_b91")
            .await
            .interrupt_requested_at
            .is_none(),
        "terminal descendant must not receive a stale interrupt timestamp"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_no_cascade_returns_accepted() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let result = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_solo".into(),
            agent_did: None,
            cause: "userCancelled".into(),
            cascade: false,
            expected_preview_signature: None,
        },
    )
    .await
    .expect("ok");
    assert!(result.accepted);
    assert!(!result.already_interrupted);
    assert!(!result.stale_preview);
    assert!(result.interrupt_requested_at.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_returns_already_interrupted_for_second_call() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let _ = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_solo".into(),
            agent_did: None,
            cause: "userCancelled".into(),
            cascade: false,
            expected_preview_signature: None,
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
            cascade: false,
            expected_preview_signature: None,
        },
    )
    .await
    .expect("second");
    assert!(second.accepted);
    assert!(second.already_interrupted);
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
            cascade: false,
            expected_preview_signature: None,
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
            cascade: false,
            expected_preview_signature: None,
        },
    )
    .await
    .unwrap_err();
    assert!(err.contains("userCancelled"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_cascade_returns_accepted_when_signature_matches() {
    let (core, _tmp) = seed_cascade_fixture().await;
    let preview = build_cascade_preview(
        &core,
        &DesktopPreviewInterruptCascadeRequest {
            request_id: "req_root".into(),
            agent_did: Some("did:test:operator".into()),
            include_terminal: Some(true),
        },
    )
    .await
    .unwrap();

    let result = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_root".into(),
            agent_did: Some("did:test:operator".into()),
            cause: "userCancelled".into(),
            cascade: true,
            expected_preview_signature: Some(preview.preview_signature.clone()),
        },
    )
    .await
    .expect("ok");

    assert!(result.accepted);
    assert!(!result.stale_preview);
    assert!(result.preview.is_none());
    assert!(result.interrupt_requested_at.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_cascade_latches_only_cascade_descendants() {
    let (core, _tmp) = seed_cascade_fixture().await;
    let preview = build_cascade_preview(
        &core,
        &DesktopPreviewInterruptCascadeRequest {
            request_id: "req_root".into(),
            agent_did: Some("did:test:operator".into()),
            include_terminal: Some(true),
        },
    )
    .await
    .unwrap();

    let result = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_root".into(),
            agent_did: Some("did:test:operator".into()),
            cause: "userCancelled".into(),
            cascade: true,
            expected_preview_signature: Some(preview.preview_signature.clone()),
        },
    )
    .await
    .expect("cascade interrupt ok");

    assert!(result.accepted);
    for request_id in ["req_root", "req_b91", "req_b92", "req_c01"] {
        let row = fetch_request_row(&core, request_id).await;
        assert!(
            row.interrupt_requested_at.is_some(),
            "{request_id} should be latched by cascade interrupt"
        );
    }
    for request_id in ["req_b93", "req_c02", "req_a17_old"] {
        let row = fetch_request_row(&core, request_id).await;
        assert!(
            row.interrupt_requested_at.is_none(),
            "{request_id} should not be latched by cascade interrupt"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_cascade_returns_stale_preview_when_signature_drifts() {
    let (core, _tmp) = seed_cascade_fixture().await;
    let result = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_root".into(),
            agent_did: Some("did:test:operator".into()),
            cause: "userCancelled".into(),
            cascade: true,
            expected_preview_signature: Some("00".repeat(32)),
        },
    )
    .await
    .expect("ok");

    assert!(!result.accepted);
    assert!(result.stale_preview);
    assert!(result.interrupt_requested_at.is_none());
    let fresh = result.preview.expect("fresh preview attached");
    assert_eq!(fresh.root_request_id, "req_root");
    assert_eq!(fresh.preview_signature.len(), 64);
    assert_ne!(fresh.preview_signature, "00".repeat(32));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_cascade_rejects_when_expected_signature_missing() {
    let (core, _tmp) = seed_cascade_fixture().await;
    let err = interrupt_request(
        &core,
        &DesktopInterruptRequest {
            request_id: "req_root".into(),
            agent_did: Some("did:test:operator".into()),
            cause: "userCancelled".into(),
            cascade: true,
            expected_preview_signature: None,
        },
    )
    .await
    .unwrap_err();
    assert!(err.contains("expectedPreviewSignature") || err.contains("expected_preview_signature"));
}
