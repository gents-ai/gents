//! Subscription factory for the three event-driven sources.

use std::sync::Arc;

use defra_node::EmbeddedNode;
use events::{EventName, Subscription};

pub trait UpdateSubscriptionSource: Send + Sync {
    fn subscribe_updates(&self) -> Subscription;
}

impl UpdateSubscriptionSource for EmbeddedNode {
    fn subscribe_updates(&self) -> Subscription {
        self.subscribe(&[EventName::Update])
    }
}

impl UpdateSubscriptionSource for Arc<EmbeddedNode> {
    fn subscribe_updates(&self) -> Subscription {
        self.as_ref().subscribe(&[EventName::Update])
    }
}
