//! In-memory `UpdateSubscriptionSource` for conformance tests.
//!
//! Backed by an `events::ChannelBus`. Tests push `events::Message`s via
//! `publish_update(collection_id, doc_id)`; the source receives them through
//! the real `events::Subscription` returned by `subscribe_updates()`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gents::UpdateSubscriptionSource;
use events::{Bus, ChannelBus, EventName, Message, Subscription, Update};
use tokio::sync::Notify;

#[derive(Clone)]
pub struct MockUpdateSubscriptionSource {
    bus: Arc<ChannelBus>,
    subscriber_count: Arc<AtomicUsize>,
    subscriber_notify: Arc<Notify>,
}

impl MockUpdateSubscriptionSource {
    pub fn new() -> Self {
        Self {
            bus: Arc::new(ChannelBus::new()),
            subscriber_count: Arc::new(AtomicUsize::new(0)),
            subscriber_notify: Arc::new(Notify::new()),
        }
    }

    /// Push a synthetic Update event into the in-memory bus.
    ///
    /// Production update messages carry a CID, block bytes, retry flag, and
    /// relay flag. The conformance driver only consumes collection/doc IDs and
    /// relay-ness today, so the CID and block are deterministic synthetic
    /// placeholders while `is_relay` defaults to true.
    pub fn publish_update(&self, collection_id: impl Into<String>, doc_id: impl Into<String>) {
        let collection_id = collection_id.into();
        let doc_id = doc_id.into();
        let block = format!("{collection_id}:{doc_id}").into_bytes();
        let cid = defra_core::block::generate_cid_from_bytes(&block)
            .expect("synthetic update block bytes must produce a CID");
        let update = Update::new(doc_id, cid, collection_id, block, false, true);
        self.bus.publish(Message::update(update));
    }

    pub async fn wait_for_subscribers(&self, expected: usize, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.subscriber_count.load(Ordering::SeqCst) >= expected {
                return true;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return false;
            }
            if tokio::time::timeout_at(deadline, self.subscriber_notify.notified())
                .await
                .is_err()
            {
                return false;
            }
        }
    }
}

impl Default for MockUpdateSubscriptionSource {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateSubscriptionSource for MockUpdateSubscriptionSource {
    fn subscribe_updates(&self) -> Subscription {
        let subscription = self.bus.subscribe(&[EventName::Update]);
        self.subscriber_count.fetch_add(1, Ordering::SeqCst);
        self.subscriber_notify.notify_waiters();
        subscription
    }
}
