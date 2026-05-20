use crate::bridge::cascade::{interrupt_request, latch_root_interrupt};
use crate::bridge::tests::support::{fetch_request_row, seed_standalone_fixture};
use crate::bridge::types::DesktopInterruptRequest;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn latch_writes_interrupt_requested_at_when_absent() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let before = fetch_request_row(&core, "req_solo").await;
    assert!(before.interrupt_requested_at.is_none());

    let latched = latch_root_interrupt(&core, "req_solo")
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
    let _ = latch_root_interrupt(&core, "req_solo")
        .await
        .expect("first latch");
    let second = latch_root_interrupt(&core, "req_solo")
        .await
        .expect("second latch");
    assert!(!second.was_first);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_no_cascade_returns_accepted() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let result = interrupt_request(&core, &DesktopInterruptRequest {
        request_id: "req_solo".into(),
        cause: "userCancelled".into(),
        cascade: false,
        expected_preview_signature: None,
    })
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
    let _ = interrupt_request(&core, &DesktopInterruptRequest {
        request_id: "req_solo".into(),
        cause: "userCancelled".into(),
        cascade: false,
        expected_preview_signature: None,
    })
    .await
    .expect("first");
    let second = interrupt_request(&core, &DesktopInterruptRequest {
        request_id: "req_solo".into(),
        cause: "userCancelled".into(),
        cascade: false,
        expected_preview_signature: None,
    })
    .await
    .expect("second");
    assert!(!second.accepted);
    assert!(second.already_interrupted);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupt_request_rejects_non_user_cancelled_cause() {
    let (core, _tmp) = seed_standalone_fixture().await;
    let err = interrupt_request(&core, &DesktopInterruptRequest {
        request_id: "req_solo".into(),
        cause: "deadline".into(), // not operator-authentic
        cascade: false,
        expected_preview_signature: None,
    })
    .await
    .unwrap_err();
    assert!(err.contains("userCancelled"));
}
