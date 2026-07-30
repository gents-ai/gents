use std::time::Duration;

use gents::interrupt::{spawn_request_interrupt_observer, InterruptIntent};
use tokio::sync::watch;

use crate::support::{create_request, set_interrupt_requested_at, test_db};

#[tokio::test]
async fn observer_sends_intent_when_field_becomes_non_null() {
    let db = test_db("observer-sends-intent").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let (interrupt_tx, interrupt_rx) = watch::channel::<Option<InterruptIntent>>(None);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let observer = spawn_request_interrupt_observer(
        db.node.clone(),
        doc_id.clone(),
        interrupt_tx,
        shutdown_rx,
    );

    assert!(interrupt_rx.borrow().is_none());

    let at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &at).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if interrupt_rx.borrow().is_some() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("observer did not signal within 5s of interrupt_requested_at being set");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let intent = interrupt_rx.borrow().clone().unwrap();
    let expected = chrono::DateTime::parse_from_rfc3339(&at)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(intent.at, expected);

    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        observer.is_finished(),
        "observer must self-exit after latching"
    );
}

#[tokio::test]
async fn observer_exits_on_shutdown_signal() {
    let db = test_db("observer-shutdown").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let (interrupt_tx, _interrupt_rx) = watch::channel::<Option<InterruptIntent>>(None);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let observer = spawn_request_interrupt_observer(
        db.node.clone(),
        doc_id.clone(),
        interrupt_tx,
        shutdown_rx,
    );

    assert!(!observer.is_finished());

    shutdown_tx.send(true).unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if observer.is_finished() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("observer did not exit within 5s of shutdown signal");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn observer_survives_malformed_interrupt_timestamp() {
    let db = test_db("observer-malformed").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let (interrupt_tx, interrupt_rx) = watch::channel::<Option<InterruptIntent>>(None);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let observer = spawn_request_interrupt_observer(
        db.node.clone(),
        doc_id.clone(),
        interrupt_tx,
        shutdown_rx,
    );

    set_interrupt_requested_at(&db.node, &doc_id, "not-a-timestamp").await;

    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !observer.is_finished(),
        "observer must not exit on parse error"
    );
    assert!(
        interrupt_rx.borrow().is_none(),
        "observer must not signal on parse error"
    );

    let at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &at).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if interrupt_rx.borrow().is_some() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("observer did not recover and signal after a valid timestamp landed");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Regression test for the "2-second blind spot" bug: the observer used to
/// skip its first tick, so any interrupt latched before observer spawn (or in
/// the first ~`OBSERVER_POLL_INTERVAL` after spawn) was invisible until the
/// second tick — short requests could even finish inside that window and never
/// observe the interrupt at all.
///
/// After the fix, `tokio::time::interval`'s first-tick-is-immediate behavior
/// gives us a prompt first poll, and a pre-set latch is caught well under
/// the poll-interval window.
#[tokio::test]
async fn observer_picks_up_already_set_interrupt_on_first_poll() {
    let db = test_db("observer-initial-poll").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let at = chrono::Utc::now().to_rfc3339();
    set_interrupt_requested_at(&db.node, &doc_id, &at).await;

    let (interrupt_tx, interrupt_rx) = watch::channel::<Option<InterruptIntent>>(None);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let start = tokio::time::Instant::now();
    let _observer = spawn_request_interrupt_observer(
        db.node.clone(),
        doc_id.clone(),
        interrupt_tx,
        shutdown_rx,
    );

    let deadline = start + Duration::from_secs(1);
    loop {
        if interrupt_rx.borrow().is_some() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "observer did not catch pre-set interrupt within 1s (the 2s blind spot regression)"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn observer_exits_on_abort() {
    let db = test_db("observer-abort").await;
    let request_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let created_at = chrono::Utc::now().to_rfc3339();
    let doc_id = create_request(&db.node, &request_id, &session_id, "pending", &created_at).await;

    let (interrupt_tx, _interrupt_rx) = watch::channel::<Option<InterruptIntent>>(None);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let observer = spawn_request_interrupt_observer(
        db.node.clone(),
        doc_id.clone(),
        interrupt_tx,
        shutdown_rx,
    );

    observer.abort();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if observer.is_finished() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("observer did not terminate within 3s of abort");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
