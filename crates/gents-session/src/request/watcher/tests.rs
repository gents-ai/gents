use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use super::cooldown::ProcessedMark;
use super::*;

fn publish_watcher_update(bus: &events::ChannelBus, collection_id: &str, doc_id: &str) {
    use events::Bus;
    let block = format!("{collection_id}:{doc_id}").into_bytes();
    let cid = defra_core::block::generate_cid_from_bytes(&block).unwrap();
    bus.publish(events::Message::update(events::Update::new(
        doc_id.to_owned(),
        cid,
        collection_id.to_owned(),
        block,
        false,
        true,
    )));
}

#[tokio::test(start_paused = true)]
async fn unrelated_changes_do_not_wake_or_postpone_the_request_scan() {
    use events::Bus;
    let bus = events::ChannelBus::new();
    let mut subscription = bus.subscribe_collection_changes(vec!["requests".into()]);
    let wait = wait_for_request_change(&mut subscription);
    tokio::pin!(wait);
    for _ in 0..30 {
        publish_watcher_update(&bus, "output", "streaming-output");
        tokio::select! {
            biased;
            result = &mut wait => panic!("unrelated output woke the queue scan: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    assert!(
        wait.await.is_err(),
        "output traffic must not postpone fallback polling"
    );
}

#[tokio::test(start_paused = true)]
async fn request_changes_wake_without_waiting_for_the_fallback() {
    use events::Bus;
    let bus = events::ChannelBus::new();
    let mut subscription = bus.subscribe_collection_changes(vec!["requests".into()]);
    publish_watcher_update(&bus, "output", "first");
    publish_watcher_update(&bus, "requests", "second");
    let started = tokio::time::Instant::now();
    let batch = wait_for_request_change(&mut subscription)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tokio::time::Instant::now(), started);
    assert_eq!(batch.changes.len(), 1);
    assert_eq!(batch.changes[0].doc_id, "second");
}

#[tokio::test]
async fn a_closed_request_change_feed_stops_the_watcher_wait() {
    use events::Bus;
    let bus = events::ChannelBus::new();
    let mut subscription = bus.subscribe_collection_changes(vec!["requests".into()]);
    bus.close();
    assert!(wait_for_request_change(&mut subscription)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test(start_paused = true)]
async fn closed_feed_reopens_only_at_fallback_and_wait_is_cancellable() {
    use events::Bus;
    use std::cell::Cell;

    let bus = events::ChannelBus::new();
    let mut subscription = bus.subscribe_collection_changes(vec!["requests".into()]);
    bus.close();
    let attempts = Cell::new(0);
    let mut first = Box::pin(wait_for_change_or_resubscribe(&mut subscription, || {
        attempts.set(attempts.get() + 1);
        Err(anyhow::anyhow!("transient subscription failure"))
    }));
    assert!(futures::poll!(first.as_mut()).is_pending());
    tokio::time::advance(GOSSIP_FALLBACK_POLL - Duration::from_millis(1)).await;
    assert!(futures::poll!(first.as_mut()).is_pending());
    assert_eq!(attempts.get(), 0);
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(matches!(
        futures::poll!(first.as_mut()),
        std::task::Poll::Ready(Err(_))
    ));
    drop(first);
    assert_eq!(attempts.get(), 1, "observer may re-read after the fallback");

    let mut second = Box::pin(wait_for_change_or_resubscribe(&mut subscription, || {
        attempts.set(attempts.get() + 1);
        Ok(bus.subscribe_collection_changes(vec!["requests".into()]))
    }));
    assert!(futures::poll!(second.as_mut()).is_pending());
    tokio::time::advance(GOSSIP_FALLBACK_POLL - Duration::from_millis(1)).await;
    assert!(futures::poll!(second.as_mut()).is_pending());
    assert_eq!(attempts.get(), 1, "a failed reopen cannot spin");
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(matches!(
        futures::poll!(second.as_mut()),
        std::task::Poll::Ready(Ok(()))
    ));
    drop(second);
    assert_eq!(
        attempts.get(),
        2,
        "observer re-reads after the next fallback"
    );

    let mut cancelled = Box::pin(wait_for_change_or_resubscribe(&mut subscription, || {
        attempts.set(attempts.get() + 1);
        Ok(bus.subscribe_collection_changes(vec!["requests".into()]))
    }));
    assert!(futures::poll!(cancelled.as_mut()).is_pending());
    drop(cancelled);
    tokio::time::advance(GOSSIP_FALLBACK_POLL).await;
    assert_eq!(attempts.get(), 2, "dropping the wait cancels the reopen");
}

