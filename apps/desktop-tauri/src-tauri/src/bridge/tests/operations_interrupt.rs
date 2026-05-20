use crate::bridge::cascade::latch_root_interrupt;
use crate::bridge::tests::support::{fetch_request_row, seed_standalone_fixture};

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
