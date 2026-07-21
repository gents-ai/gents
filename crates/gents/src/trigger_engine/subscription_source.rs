//! Subscription factory for the three event-driven sources.
//!
//! Production wraps `Arc<EmbeddedNode>`; the integration test harness wraps
//! an `Arc<events::ChannelBus>`. Both yield a real `events::Subscription`
//! so the sources' `recv()` / `check_and_reset_dropped()` loops are
//! unchanged.

use std::sync::Arc;

use defra_node::EmbeddedNode;
use events::{EventName, Subscription};

/// Source of `events::Update` subscriptions for `DefraWatcher`,
/// `EventSource`, and `SubagentSource`.
///
/// Implementations are responsible for filtering to `EventName::Update`
/// only; callers do not pass an event mask. Keeping the surface to one
/// method matches the only call site the three sources need today and
/// avoids exposing the full `events::Bus` surface.
pub trait UpdateSubscriptionSource: Send + Sync {
    /// Open a fresh subscription to `EventName::Update`.
    ///
    /// Each call returns a distinct subscription; the caller owns the
    /// receiver. Mirrors `EmbeddedNode::subscribe`'s ownership semantics.
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
