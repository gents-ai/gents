//! Subscription factory for the three event-driven sources.

use std::sync::Arc;

use defra_node::EmbeddedNode;
use events::{DocumentChangeSubscription, EventName, Subscription};

pub trait UpdateSubscriptionSource: Send + Sync {
    fn subscribe_updates(&self) -> Subscription;
    fn subscribe_document_changes(&self) -> DocumentChangeSubscription;
}

impl UpdateSubscriptionSource for EmbeddedNode {
    fn subscribe_updates(&self) -> Subscription {
        self.subscribe(&[EventName::Update])
    }

    fn subscribe_document_changes(&self) -> DocumentChangeSubscription {
        EmbeddedNode::subscribe_document_changes(self)
    }
}

impl UpdateSubscriptionSource for Arc<EmbeddedNode> {
    fn subscribe_updates(&self) -> Subscription {
        self.as_ref().subscribe(&[EventName::Update])
    }

    fn subscribe_document_changes(&self) -> DocumentChangeSubscription {
        self.as_ref().subscribe_document_changes()
    }
}
