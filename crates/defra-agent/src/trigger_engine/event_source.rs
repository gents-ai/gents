//! Event-trigger `TriggerSource`.
//!
//! Subscribes to DefraDB document events and emits `FireIntent`s whenever an
//! event from a `source_collection` referenced by the active runtime
//! snapshot's `active_event_triggers` lands. The subscription set is kept in
//! sync with the snapshot generation — see `reconcile_subscriptions` (Task 19).
//!
//! This file lands in staged tasks:
//! - Task 18 (this file): skeleton only — struct, constructor, no-op
//!   `next_fire` stub.
//! - Task 19: `reconcile_subscriptions` drives the desired-collections set from
//!   the snapshot at each generation bump.
//! - Task 20: full `next_fire` loop (poll subscription, filter by desired
//!   collections, build `FireIntent`).
//! - Task 21: filter probe + doc-var hydration.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use defra_node::{EmbeddedNode, EventName};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::runtime_snapshot::ActiveRuntimeSnapshot;

use super::{FireIntent, TriggerSource};

/// `TriggerSource` that fans DefraDB document events out to
/// `active_event_triggers`.
///
/// Holds a single global `events::Subscription` (the `defra-node` API exposes
/// only `subscribe(&[EventName])` — there is no per-collection subscription)
/// and filters incoming events by `desired_collections` on the dispatch hot
/// path. That set is recomputed from the snapshot whenever
/// `snapshot.generation` bumps.
pub(crate) struct EventSource {
    #[allow(dead_code)]
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    #[allow(dead_code)]
    node: Arc<EmbeddedNode>,
    /// Single global subscription. Task 19 populates this on first
    /// reconciliation; Task 20's `next_fire` consumes from it.
    #[allow(dead_code)]
    subscription: Option<events::Subscription>,
    /// Source-collection names that any `active_event_triggers` entry
    /// currently references. Task 20 consults this set as the client-side
    /// filter before dispatching an event to matching triggers.
    #[allow(dead_code)]
    desired_collections: HashSet<String>,
    /// The snapshot generation whose `active_event_triggers` produced the
    /// current `desired_collections`. Task 19 compares against
    /// `snapshot.generation` at tick boundaries to decide whether to
    /// reconcile.
    #[allow(dead_code)]
    reconciled_generation: u64,
    /// Debounce window for snapshot-publish-driven reconciliation. Reserved
    /// for Tasks 19-20 — reconciliation is currently driven by the tick loop
    /// rather than a timer.
    #[allow(dead_code)]
    reconcile_debounce: Duration,
    #[allow(dead_code)]
    cancel: CancellationToken,
    /// Reserved for Task 21: per-source-collection schema cache used by
    /// doc-var hydration. Stays empty until Task 21 wires the lookup path.
    #[allow(dead_code)]
    source_schema_cache: SourceSchemaCache,
}

/// Per-source-collection schema cache. Task 21 will populate this to avoid
/// re-querying collection schemas for every dispatched event; stays empty
/// until then.
#[derive(Default)]
pub(crate) struct SourceSchemaCache {
    // by_collection: tokio::sync::Mutex<HashMap<String, Vec<String>>>,  // Task 21
}

impl EventSource {
    /// Build an event source wired to the given snapshot receiver and
    /// embedded node.
    ///
    /// The subscription itself is not created here — Task 19's
    /// `reconcile_subscriptions` opens it on the first tick once the
    /// snapshot's `active_event_triggers` have been read.
    pub(crate) fn new(
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            snapshot_rx,
            node,
            subscription: None,
            desired_collections: HashSet::new(),
            reconciled_generation: 0,
            reconcile_debounce: Duration::from_millis(250),
            cancel,
            source_schema_cache: SourceSchemaCache::default(),
        }
    }

    /// Override the reconciliation debounce. Test-only hook mirroring
    /// `ScheduleSource::with_tick_every`.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_reconcile_debounce(mut self, debounce: Duration) -> Self {
        self.reconcile_debounce = debounce;
        self
    }

    /// Snapshot of the source-collection names the event source is currently
    /// filtering on. Test-only accessor.
    #[cfg(test)]
    pub(crate) fn subscribed_collections(&self) -> Vec<String> {
        let mut v: Vec<String> = self.desired_collections.iter().cloned().collect();
        v.sort();
        v
    }

    /// Refresh `desired_collections` from the supplied snapshot's
    /// `active_event_triggers` and ensure the global `events::Subscription`
    /// exists.
    ///
    /// `defra-node` only exposes `subscribe(&[EventName])` — a single
    /// process-wide stream of `Update` events, with the collection carried
    /// in the event payload. So "reconciliation" here is twofold:
    ///
    /// 1. Recompute the filter set (`desired_collections`) that Task 20's
    ///    `next_fire` will consult before dispatching an event to matching
    ///    triggers. Collections whose last `active_event_trigger` was
    ///    removed drop out; newly referenced collections appear.
    /// 2. Lazily open the global subscription the first time we have at
    ///    least one desired collection. If the desired set later shrinks to
    ///    empty we keep the subscription open — reopening is cheap only in
    ///    principle, and events.Bus has no "pause" API; Task 20 short-
    ///    circuits on an empty filter set instead.
    ///
    /// Finally, stamp `reconciled_generation = snapshot.generation` so the
    /// `next_fire` tick loop can detect further snapshot bumps.
    #[allow(dead_code)]
    pub(crate) async fn reconcile_subscriptions(&mut self, snapshot: &ActiveRuntimeSnapshot) {
        let desired: HashSet<String> = snapshot
            .active_event_triggers()
            .values()
            .map(|t| t.source_collection.clone())
            .collect();

        // Trace added / removed collections so operators can correlate a
        // config change with the subscription-set delta. Keeping this at
        // `info!` matches `ScheduleSource::next_fire`'s first-seen logs.
        for added in desired.difference(&self.desired_collections) {
            tracing::info!(
                source_collection = %added,
                generation = snapshot.generation,
                "event source now observing source collection",
            );
        }
        for removed in self.desired_collections.difference(&desired) {
            tracing::info!(
                source_collection = %removed,
                generation = snapshot.generation,
                "event source no longer observing source collection",
            );
        }

        self.desired_collections = desired;

        // Lazily open the global subscription. We defer opening until the
        // first non-empty desired set so a runtime with no event triggers
        // never materializes an unused subscription.
        if self.subscription.is_none() && !self.desired_collections.is_empty() {
            let subscription = self.node.subscribe(&[EventName::Update]);
            tracing::info!(
                collections = self.desired_collections.len(),
                generation = snapshot.generation,
                "event source opened global Update subscription",
            );
            self.subscription = Some(subscription);
        }

        self.reconciled_generation = snapshot.generation;
    }
}

impl TriggerSource for EventSource {
    fn next_fire(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FireIntent>> + Send + '_>>
    {
        Box::pin(async move {
            // Task 20 implements the full loop: reconcile-on-generation-bump,
            // poll subscription, filter by desired collections, build intent.
            // Until then `next_fire` is a no-op stub — the engine will simply
            // never pull a fire intent from this source.
            let _ = &self;
            None
        })
    }
}
