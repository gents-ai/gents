use std::collections::HashMap;
use std::time::Duration;

use defra_node::EmbeddedNode;
use events::{DocumentChangeBatch, DocumentChangeSubscription};
use tokio::time::{Instant, Interval, MissedTickBehavior};

/// DefraDB subscriptions are live-only and may overflow. A wake always requires
/// a durable read; the independent fallback also covers missed notifications.
pub struct DocumentChangeWakeup {
    subscription: Option<DocumentChangeSubscription>,
    observed_collections: Option<HashMap<String, bool>>,
    fallback: Interval,
    minimum_interval: Duration,
}

impl DocumentChangeWakeup {
    pub fn new(
        node: &EmbeddedNode,
        collections: Option<&[&str]>,
        fallback_interval: Duration,
    ) -> Self {
        let subscription = node.subscribe_document_changes();
        let observed_collections = Self::collection_filter(node, collections);
        Self::with_subscription(subscription, observed_collections, fallback_interval)
    }

    pub fn set_collections(&mut self, node: &EmbeddedNode, collections: Option<&[&str]>) {
        self.observed_collections = Self::collection_filter(node, collections);
    }

    pub fn with_minimum_interval(mut self, interval: Duration) -> Self {
        self.minimum_interval = interval;
        self
    }

    fn collection_filter(
        node: &EmbeddedNode,
        collections: Option<&[&str]>,
    ) -> Option<HashMap<String, bool>> {
        collections.and_then(|observed| {
            node.list_collections()
                .ok()?
                .into_iter()
                .map(|name| {
                    let definition = node.get_collection(&name).ok()??;
                    Some((
                        definition.collection_id,
                        observed.contains(&definition.name.as_str()),
                    ))
                })
                .collect()
        })
    }

    fn with_subscription(
        subscription: DocumentChangeSubscription,
        observed_collections: Option<HashMap<String, bool>>,
        fallback_interval: Duration,
    ) -> Self {
        let mut fallback = tokio::time::interval(fallback_interval);
        fallback.set_missed_tick_behavior(MissedTickBehavior::Delay);
        fallback.reset();
        Self {
            subscription: Some(subscription),
            observed_collections,
            fallback,
            minimum_interval: Duration::ZERO,
        }
    }

    pub async fn wait(&mut self) {
        let next_read = Instant::now() + self.minimum_interval;
        loop {
            tokio::select! {
                biased;
                _ = self.fallback.tick() => break,
                batch = async {
                    match self.subscription.as_mut() {
                        Some(subscription) => subscription.recv().await,
                        None => std::future::pending().await,
                    }
                } => match batch {
                    Some(batch) if self.requires_read(&batch) => break,
                    Some(_) => tokio::task::consume_budget().await,
                    None => {
                        self.subscription = None;
                        break;
                    }
                },
            }
        }
        if next_read > Instant::now() {
            tokio::time::sleep_until(next_read).await;
        }
    }

    fn requires_read(&self, batch: &DocumentChangeBatch) -> bool {
        batch.resync_required
            || self
                .observed_collections
                .as_ref()
                .is_none_or(|collections| {
                    if batch.changes.is_empty() {
                        return batch.updates != 0;
                    }
                    batch.changes.iter().any(|change| {
                        collections
                            .get(&change.collection_id)
                            .copied()
                            .unwrap_or(true)
                    })
                })
    }
}

#[cfg(test)]
mod tests;
