use std::time::Duration;

use events::{Bus, ChannelBus, ChannelBusConfig, DocumentChangeBatch, Message, Update};
use tokio::time::{advance, Instant};

use super::DocumentChangeWakeup;

const FALLBACK: Duration = Duration::from_secs(2);

fn observer(bus: &ChannelBus) -> DocumentChangeWakeup {
    DocumentChangeWakeup::with_subscription(
        bus.subscribe_document_changes(),
        Some(
            [("requests".into(), true), ("unrelated".into(), false)]
                .into_iter()
                .collect(),
        ),
        FALLBACK,
    )
}

fn publish(bus: &ChannelBus, collection: &str, document: &str) {
    let block = format!("{collection}:{document}").into_bytes();
    let cid = defra_core::block::generate_cid_from_bytes(&block).unwrap();
    bus.publish(Message::update(Update::new(
        document.into(),
        cid,
        collection.into(),
        block,
        false,
        true,
    )));
}

async fn assert_waiting(wakeup: &mut DocumentChangeWakeup) {
    tokio::select! {
        biased;
        _ = wakeup.wait() => panic!("durable read woke without a change or recovery deadline"),
        _ = tokio::task::yield_now() => {},
    }
}

#[tokio::test(start_paused = true)]
async fn idle_observers_only_read_at_the_recovery_deadline() {
    let bus = ChannelBus::new();
    let mut wakeup = observer(&bus);
    for _ in 0..19 {
        advance(Duration::from_millis(100)).await;
        assert_waiting(&mut wakeup).await;
    }
    let start = Instant::now();
    wakeup.wait().await;
    assert_eq!(start.elapsed(), Duration::from_millis(100));
    assert_waiting(&mut wakeup).await;
}

#[tokio::test(start_paused = true)]
async fn unrelated_streaming_does_not_postpone_recovery() {
    let bus = ChannelBus::new();
    let mut wakeup = observer(&bus);
    let start = Instant::now();
    for _ in 0..19 {
        publish(&bus, "unrelated", "stream");
        assert_waiting(&mut wakeup).await;
        advance(Duration::from_millis(100)).await;
    }
    wakeup.wait().await;
    assert_eq!(start.elapsed(), FALLBACK);
}

#[tokio::test(start_paused = true)]
async fn a_coalesced_burst_wakes_once_and_keeps_the_original_recovery_deadline() {
    let bus = ChannelBus::new();
    let mut wakeup = observer(&bus);
    advance(Duration::from_millis(1900)).await;
    for _ in 0..64 {
        publish(&bus, "requests", "request");
    }
    let start = Instant::now();
    wakeup.wait().await;
    assert_eq!(start.elapsed(), Duration::ZERO);
    assert_waiting(&mut wakeup).await;
    wakeup.wait().await;
    assert_eq!(start.elapsed(), Duration::from_millis(100));
}

#[tokio::test(start_paused = true)]
async fn spaced_updates_cannot_raise_the_durable_read_rate_above_its_existing_cadence() {
    let bus = ChannelBus::new();
    let mut wakeup = observer(&bus).with_minimum_interval(Duration::from_millis(200));
    publish(&bus, "requests", "first");
    wakeup.wait().await;
    let started = Instant::now();
    publish(&bus, "requests", "second");
    let wait = wakeup.wait();
    tokio::pin!(wait);
    for _ in 0..4 {
        publish(&bus, "requests", "streaming");
        tokio::select! {
            biased;
            _ = &mut wait => panic!("streaming changes exceeded the existing durable read cadence"),
            _ = tokio::task::yield_now() => {},
        }
        advance(Duration::from_millis(50)).await;
    }
    wait.await;
    assert_eq!(started.elapsed(), Duration::from_millis(200));
}

#[tokio::test(start_paused = true)]
async fn overflow_and_new_collection_ids_wake_a_durable_read() {
    for overflow in [false, true] {
        let bus = ChannelBus::with_config(ChannelBusConfig::default().with_event_buffer_size(1));
        let mut wakeup = observer(&bus);
        publish(&bus, if overflow { "unrelated" } else { "new" }, "first");
        if overflow {
            publish(&bus, "unrelated", "second");
        }
        let start = Instant::now();
        wakeup.wait().await;
        assert_eq!(start.elapsed(), Duration::ZERO);
    }
}

#[tokio::test(start_paused = true)]
async fn a_closed_feed_falls_back_without_spinning() {
    let bus = ChannelBus::new();
    let mut wakeup = observer(&bus);
    bus.close();
    let start = Instant::now();
    wakeup.wait().await;
    assert_eq!(start.elapsed(), Duration::ZERO);
    assert_waiting(&mut wakeup).await;
    wakeup.wait().await;
    assert_eq!(start.elapsed(), FALLBACK);
}

#[tokio::test(start_paused = true)]
async fn waiting_can_be_cancelled_without_losing_the_fallback_deadline() {
    let bus = ChannelBus::new();
    let mut wakeup = observer(&bus);
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let wait = async {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {},
                _ = wakeup.wait() => panic!("idle observer woke before cancellation"),
            }
        };
        tokio::pin!(wait);
        publish(&bus, "unrelated", "stream");
        tokio::select! {
            biased;
            _ = &mut wait => panic!("unrelated changes ended the wait"),
            _ = tokio::task::yield_now() => {},
        }
        advance(Duration::from_millis(500)).await;
        cancel.cancel();
        wait.await;
    }
    advance(Duration::from_millis(1500)).await;
    let start = Instant::now();
    wakeup.wait().await;
    assert_eq!(start.elapsed(), Duration::ZERO);
}

#[tokio::test]
async fn a_batch_without_document_metadata_still_requires_a_read() {
    let bus = ChannelBus::new();
    let wakeup = observer(&bus);
    assert!(wakeup.requires_read(&DocumentChangeBatch {
        changes: Vec::new(),
        resync_required: false,
        updates: 1,
    }));
}

#[tokio::test]
async fn committed_changes_are_observed_after_subscribing_before_the_initial_read() {
    let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    let access = crate::config_client::ConfigAccess::local(node.clone());
    access
        .add_schema("type WakeupRequest { value: String } type WakeupUnrelated { value: String }")
        .await
        .unwrap();
    let mut wakeup =
        DocumentChangeWakeup::new(&node, Some(&["WakeupRequest"]), Duration::from_secs(60));
    access
        .write(
            "wakeup_test.unrelated",
            r#"mutation { create_WakeupUnrelated(input: { value: "irrelevant" }) { _docID } }"#,
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(25), wakeup.wait())
            .await
            .is_err()
    );
    access
        .write(
            "wakeup_test.request",
            r#"mutation { create_WakeupRequest(input: { value: "committed" }) { _docID } }"#,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), wakeup.wait())
        .await
        .expect("the committed change must wake before the recovery interval");
    let durable = access.execute("{ WakeupRequest { value } }").await.unwrap();
    assert_eq!(durable["data"]["WakeupRequest"][0]["value"], "committed");
    access
        .write(
            "wakeup_test.queued",
            r#"mutation { create_WakeupUnrelated(input: { value: "already queued" }) { _docID } }"#,
        )
        .await
        .unwrap();
    wakeup.set_collections(&node, None);
    tokio::time::timeout(Duration::from_secs(5), wakeup.wait())
        .await
        .expect("expanding observation must retain changes queued during the durable read");
    wakeup.set_collections(&node, Some(&["WakeupRequest"]));
    access
        .write(
            "wakeup_test.idle_again",
            r#"mutation { create_WakeupUnrelated(input: { value: "irrelevant again" }) { _docID } }"#,
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(25), wakeup.wait())
            .await
            .is_err()
    );
    node.shutdown().await;
}
