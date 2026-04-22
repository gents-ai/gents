//! Event-trigger `TriggerSource`.
//!
//! Subscribes to DefraDB document events and emits `FireIntent`s whenever an
//! event from a `source_collection` referenced by the active runtime
//! snapshot's `active_event_triggers` lands. The subscription set is kept in
//! sync with the snapshot generation — see `reconcile_subscriptions` (Task 19).
//!
//! This file lands in staged tasks:
//! - Task 18: skeleton only — struct, constructor, no-op `next_fire` stub.
//! - Task 19: `reconcile_subscriptions` drives the desired-collections set
//!   from the snapshot at each generation bump.
//! - Task 20 (this file): full `next_fire` loop (poll subscription, filter by
//!   desired collections, build `FireIntent`).
//! - Task 21: filter probe + doc-var hydration.
//! - Task 22: `on_result` callback body for bookkeeping writes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use defra_node::{EmbeddedNode, EventName};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::runtime_snapshot::ActiveRuntimeSnapshot;

use super::{FireIntent, TriggerKind, TriggerSource};

/// `TriggerSource` that fans DefraDB document events out to
/// `active_event_triggers`.
///
/// Holds a single global `events::Subscription` (the `defra-node` API exposes
/// only `subscribe(&[EventName])` — there is no per-collection subscription)
/// and filters incoming events by `desired_collections` on the dispatch hot
/// path. That set is recomputed from the snapshot whenever
/// `snapshot.generation` bumps.
pub(crate) struct EventSource {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    /// Single global subscription. Task 19 populates this on first
    /// reconciliation; Task 20's `next_fire` consumes from it.
    subscription: Option<events::Subscription>,
    /// Source-collection names that any `active_event_triggers` entry
    /// currently references. Task 20 consults this set as the client-side
    /// filter before dispatching an event to matching triggers.
    desired_collections: HashSet<String>,
    /// The snapshot generation whose `active_event_triggers` produced the
    /// current `desired_collections`. Task 19 compares against
    /// `snapshot.generation` at tick boundaries to decide whether to
    /// reconcile.
    reconciled_generation: u64,
    /// Debounce window for snapshot-publish-driven reconciliation. Reserved
    /// for Tasks 19-20 — reconciliation is currently driven by the tick loop
    /// rather than a timer.
    #[allow(dead_code)]
    reconcile_debounce: Duration,
    cancel: CancellationToken,
    /// Reserved for Task 21: per-source-collection schema cache used by
    /// doc-var hydration. Stays empty until Task 21 wires the lookup path.
    #[allow(dead_code)]
    source_schema_cache: SourceSchemaCache,
    /// Cache of `collection_id -> collection name` mappings. The Update event
    /// carries only the stable `collection_id` string (see `events::Update`),
    /// but `source_collection` on an `EventTrigger` is the human-readable
    /// collection name. We resolve lazily on first-encountered event, then
    /// reuse the cached mapping for subsequent events in the same collection.
    /// Entries are never invalidated — collection IDs are stable for the
    /// lifetime of a collection's existence.
    collection_id_to_name: HashMap<String, String>,
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
            collection_id_to_name: HashMap::new(),
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

    /// Resolve an event's `collection_id` (the stable hash-like ID carried
    /// in the `Update` event payload) to the human-readable collection name
    /// used by `EventTrigger.source_collection`.
    ///
    /// Caches results in `collection_id_to_name`. On cache miss walks every
    /// active collection known to the node — this is a one-shot cost per
    /// collection-id (entries never invalidate because a collection's
    /// `collection_id` is stable for its lifetime).
    ///
    /// Returns `None` on query failure or when the id doesn't correspond to
    /// any active collection, which the caller treats as "no matching
    /// trigger; ignore this event".
    async fn resolve_collection_name(&mut self, collection_id: &str) -> Option<String> {
        if let Some(name) = self.collection_id_to_name.get(collection_id) {
            return Some(name.clone());
        }

        let names = match self.node.list_collections() {
            Ok(names) => names,
            Err(e) => {
                tracing::warn!(
                    collection_id = %collection_id,
                    error = %e,
                    "event source failed to list collections; dropping event",
                );
                return None;
            }
        };

        for name in names {
            let def = match self.node.get_collection(&name) {
                Ok(Some(def)) => def,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(
                        name = %name,
                        error = %e,
                        "event source failed to fetch collection definition while resolving id",
                    );
                    continue;
                }
            };
            // Populate the cache eagerly for every collection we touched
            // during the scan so the next event on any of those collections
            // is a pure cache hit.
            self.collection_id_to_name
                .insert(def.collection_id.clone(), def.name.clone());
        }

        self.collection_id_to_name.get(collection_id).cloned()
    }

    /// Find the first active event trigger whose `source_collection` matches
    /// `collection_name` AND whose `event_kind` matches `kind` (currently
    /// always `"created"` per the v1 spec; `kind` is validated at resolve
    /// time). Triggers are ordered by `trigger_id` for determinism so the
    /// "first" match doesn't shift across ticks when multiple triggers key on
    /// the same collection.
    ///
    /// Returns a clone so the caller drops its snapshot borrow before
    /// building the `FireIntent`.
    fn first_matching_trigger(
        snapshot: &ActiveRuntimeSnapshot,
        collection_name: &str,
        kind: &str,
    ) -> Option<crate::runtime_snapshot::ResolvedEventTrigger> {
        let mut matches: Vec<_> = snapshot
            .active_event_triggers()
            .values()
            .filter(|t| t.source_collection == collection_name && t.event_kind == kind)
            .collect();
        matches.sort_by(|a, b| a.trigger_id.cmp(&b.trigger_id));
        matches.first().map(|t| (*t).clone())
    }
}

impl TriggerSource for EventSource {
    fn next_fire(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FireIntent>> + Send + '_>>
    {
        Box::pin(async move {
            // Outer loop: reconcile-on-generation-bump, then race subscription
            // vs. snapshot-change vs. cancel. `None` here means "source is
            // permanently done, drop it" — an idle tick or an unmatched event
            // must not exit. Return `None` only on cancel or subscription
            // channel closure; keep looping otherwise so the engine's outer
            // driver doesn't teardown the source on the first miss.
            loop {
                // Step 1: snapshot-read; reconcile if the generation moved.
                // Reconciliation might open the subscription on first non-
                // empty desired set.
                let snapshot = self.snapshot_rx.borrow().clone();
                if snapshot.generation > self.reconciled_generation {
                    self.reconcile_subscriptions(snapshot.as_ref()).await;
                }

                // Step 2: empty-filter-set short-circuit. If no triggers are
                // live the subscription was never opened (or reconciled down
                // to an empty set with an already-open subscription that we
                // ignore). Either way, sit on `snapshot_rx.changed()` or
                // cancel — no events to dispatch.
                if self.subscription.is_none() || self.desired_collections.is_empty() {
                    tokio::select! {
                        biased;
                        _ = self.cancel.cancelled() => return None,
                        res = self.snapshot_rx.changed() => {
                            if res.is_err() {
                                // The snapshot publisher has hung up. Treat
                                // this as permanent source exhaustion.
                                return None;
                            }
                            continue;
                        }
                    }
                }

                // Step 3: race the subscription against snapshot changes and
                // cancel. Subscription is guaranteed Some here by the check
                // above, so we can take a &mut borrow for the recv poll.
                let subscription = self
                    .subscription
                    .as_mut()
                    .expect("subscription is Some when desired_collections is non-empty");
                let message = tokio::select! {
                    biased;
                    _ = self.cancel.cancelled() => return None,
                    res = self.snapshot_rx.changed() => {
                        if res.is_err() {
                            return None;
                        }
                        // New snapshot — loop back to reconcile. Any event
                        // in-flight on the subscription will be seen on the
                        // next tick (events::Subscription buffers behind the
                        // mpsc receiver, so no loss).
                        continue;
                    }
                    msg = subscription.recv() => {
                        match msg {
                            Some(m) => m,
                            None => {
                                // The subscription channel closed. This is
                                // effectively source death; returning None so
                                // the engine drops us.
                                tracing::warn!(
                                    "event source subscription channel closed; \
                                     source exiting",
                                );
                                return None;
                            }
                        }
                    }
                };

                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    // Dropped events are a correctness hazard for this
                    // source — without a full-resync path we can't know what
                    // we missed. Log loudly; Task 22+ may add a resync hook.
                    tracing::warn!(
                        dropped = dropped,
                        "event source dropped messages; may have missed event triggers",
                    );
                }

                // Step 4: decode the Update payload. Non-Update messages are
                // filtered by the subscription mask, but `as_update()` is the
                // idiomatic way to narrow and we get a defensive check for
                // free.
                let Some(update) = message.as_update() else {
                    continue;
                };

                // Step 5: resolve collection_id -> collection name. The
                // Update event carries a stable `collection_id` hash; our
                // trigger docs key on the human-readable name. Unknown ids
                // (e.g. a transiently-dropped collection) drop the event.
                let collection_id = update.collection_id.clone();
                let doc_id = update.doc_id.clone();
                let Some(collection_name) = self.resolve_collection_name(&collection_id).await
                else {
                    tracing::trace!(
                        collection_id = %collection_id,
                        doc_id = %doc_id,
                        "event source could not resolve collection_id to name; skipping event",
                    );
                    continue;
                };

                // Step 6: client-side filter against desired_collections.
                // This is the fast path for events in collections no trigger
                // cares about.
                if !self.desired_collections.contains(&collection_name) {
                    continue;
                }

                // Step 7: find the first matching trigger in the snapshot we
                // read at the top of the loop. Re-borrow the snapshot so
                // we're always checking against the latest published view,
                // not the copy we captured for the generation-bump check
                // (those might diverge if a snapshot published while we were
                // awaiting `subscription.recv()`).
                let snapshot = self.snapshot_rx.borrow().clone();
                // v1 spec: event_kind is always "created". If that widens,
                // map the event variant (Update carries no kind field today
                // — all writes go through Update, distinguished only by
                // block contents) to the right string.
                let event_kind = "created";
                let Some(trigger) =
                    Self::first_matching_trigger(snapshot.as_ref(), &collection_name, event_kind)
                else {
                    // We filter-pass matched at the collection level but no
                    // trigger in the snapshot keys on this collection+kind.
                    // That can happen briefly after a reconcile removes a
                    // trigger or after a snapshot bump raced with this event;
                    // drop silently.
                    continue;
                };

                // Step 8: build the FireIntent. Task 21 will hydrate
                // `doc_vars` via a per-collection schema read + filter probe;
                // for now we leave `doc_vars = None` and populate `event_vars`
                // with the source identity so the dispatcher / materializer
                // still have a minimally-useful scope. Task 22 fills in the
                // `on_result` body; for now it's a no-op.
                let fired_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
                let event_vars = serde_json::json!({
                    "fired_at": fired_at,
                    "trigger_id": trigger.trigger_id,
                    "trigger_kind": TriggerKind::Event.as_str(),
                    "source_collection": collection_name,
                    "source_doc_id": doc_id,
                });

                tracing::info!(
                    trigger_id = %trigger.trigger_id,
                    source_collection = %collection_name,
                    source_doc_id = %doc_id,
                    "event source matched event to trigger; emitting fire intent",
                );

                return Some(FireIntent {
                    trigger_id: Some(trigger.trigger_id.clone()),
                    trigger_kind: TriggerKind::Event,
                    task: trigger.task.clone(),
                    concurrency: trigger.concurrency,
                    event_vars,
                    // Task 21 owns doc-var hydration (filter probe + field
                    // projection). Task 22 owns the on_result callback body.
                    doc_vars: None,
                    args_vars: None,
                    on_result: Box::new(move |_result| {}),
                });
            }
        })
    }
}
