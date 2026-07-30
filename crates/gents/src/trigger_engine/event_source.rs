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
//! - Task 20: full `next_fire` loop (poll subscription, filter by desired
//!   collections, build `FireIntent`).
//! - Task 21 (this file): filter probe + doc-var hydration via an
//!   introspected source-doc projection cached per source collection.
//! - Task 22: `on_result` callback body for bookkeeping writes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::event_delivery_contract::{EventDeliveryRuntimeContract, EventDeliverySourceContract};
use crate::runtime_snapshot::ActiveRuntimeSnapshot;
use crate::UpdateSubscriptionSource;

use super::{FireIntent, TriggerKind, TriggerSource};

/// Cap for the one-shot existing-docs seed query run when a collection is
/// newly admitted to `desired_collections`. The goal of the seed is to
/// enforce spec's forward-only semantic: pre-existing docs in the source
/// collection must not fire as "created" when the first event arrives.
/// Collections larger than the cap are still safe (we just log a warning
/// and accept that docs beyond the cap may appear as "first-seen" on their
/// next event); v1 doesn't target catalog-scale source collections, so a
/// conservative limit is fine.
const SEEN_DOCS_SEED_LIMIT: usize = 10_000;
const EVENT_SOURCE_RESCAN_INTERVAL: Duration = Duration::from_secs(5);

pub struct EventSource {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    subscription_source: Arc<dyn UpdateSubscriptionSource>,
    subscription: Option<events::Subscription>,
    desired_collections: HashSet<String>,
    reconciled_generation: u64,
    #[allow(dead_code)]
    reconcile_debounce: Duration,
    cancel: CancellationToken,
    source_schema_cache: SourceSchemaCache,
    collection_id_to_name: HashMap<String, String>,
    seen_docs: HashMap<String, HashSet<String>>,
    pending_intents: Mutex<VecDeque<FireIntent>>,
    /// Periodic live rescan that closes the lossy-subscription gap. The
    /// interval is stored on the source so a busy stream of `next_fire()` calls
    /// does not reset the cadence.
    rescan_tick: tokio::time::Interval,
}

#[derive(Debug, Deserialize)]
struct SourceDocIdRow {
    #[serde(rename = "_docID")]
    doc_id: String,
}

/// Per-source-collection schema cache.
///
/// `fields_for(collection, node)` runs a one-shot GraphQL introspection
/// (`__type(name: "<collection>") { fields { name } }`) the first time a
/// given source collection is seen, then memoizes the resulting projectable
/// field list. Subsequent hydrations for the same collection are a pure
/// cache hit. Entries are never invalidated — the active schema for a
/// collection is stable across the runtime's lifetime, and any schema
/// migration produces a new collection version whose identity the fire
/// path treats as a distinct source.
///
/// Filtering: DefraDB's GraphQL introspection exposes several auto-
/// generated fields on every collection (aggregates like `_count`,
/// `_sum`, and per-field wrappers). These are not direct scalars and
/// cannot be included in a plain projection — selecting them without
/// required arguments produces a parse error. We filter aggressively:
/// drop anything starting with `_` (GraphQL meta / DefraDB aggregate) and
/// anything whose name is an upper-case aggregate keyword.
#[derive(Default)]
pub(crate) struct SourceSchemaCache {
    by_collection: tokio::sync::Mutex<HashMap<String, Vec<String>>>,
}

impl SourceSchemaCache {
    async fn fields_for(
        &self,
        collection: &str,
        node: &EmbeddedNode,
    ) -> anyhow::Result<Vec<String>> {
        let mut guard = self.by_collection.lock().await;
        if let Some(fields) = guard.get(collection) {
            return Ok(fields.clone());
        }
        let query = format!(
            r#"query {{
                __type(name: "{name}") {{
                    fields {{ name }}
                }}
            }}"#,
            name = collection,
        );
        let response = node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("introspect {} failed: {:?}", collection, response.errors);
        }
        let Some(fields_arr) = response
            .data
            .as_ref()
            .and_then(|d| d.get("__type"))
            .and_then(|t| t.get("fields"))
            .and_then(serde_json::Value::as_array)
        else {
            anyhow::bail!("introspection returned no fields for {}", collection);
        };
        let fields: Vec<String> = fields_arr
            .iter()
            .filter_map(|f| f.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .filter(|name| !name.starts_with('_'))
            .filter(|name| !is_defradb_aggregate_field(name))
            .collect();
        guard.insert(collection.to_string(), fields.clone());
        Ok(fields)
    }
}

fn is_defradb_aggregate_field(name: &str) -> bool {
    matches!(
        name,
        "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "GROUP" | "SIMILARITY" | "BM25"
    )
}

fn event_source_rescan_tick(interval: Duration) -> tokio::time::Interval {
    let interval = if interval.is_zero() {
        EVENT_SOURCE_RESCAN_INTERVAL
    } else {
        interval
    };
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tick
}

impl EventSource {
    pub fn new(
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        cancel: CancellationToken,
    ) -> Self {
        Self::with_subscription_source(node.clone(), snapshot_rx, node, cancel)
    }

    pub fn with_subscription_source(
        subs: Arc<dyn UpdateSubscriptionSource>,
        snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
        node: Arc<EmbeddedNode>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            snapshot_rx,
            node,
            subscription_source: subs,
            subscription: None,
            desired_collections: HashSet::new(),
            reconciled_generation: 0,
            reconcile_debounce: Duration::from_millis(250),
            cancel,
            source_schema_cache: SourceSchemaCache::default(),
            collection_id_to_name: HashMap::new(),
            seen_docs: HashMap::new(),
            pending_intents: Mutex::new(VecDeque::new()),
            rescan_tick: event_source_rescan_tick(EVENT_SOURCE_RESCAN_INTERVAL),
        }
    }

    #[doc(hidden)]
    pub fn with_rescan_interval(mut self, interval: Duration) -> Self {
        self.rescan_tick = event_source_rescan_tick(interval);
        self
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

    pub(crate) async fn reconcile_subscriptions(&mut self, snapshot: &ActiveRuntimeSnapshot) {
        let desired: HashSet<String> = snapshot
            .active_event_triggers()
            .values()
            .map(|t| t.source_collection.clone())
            .collect();

        let added: Vec<String> = desired
            .difference(&self.desired_collections)
            .cloned()
            .collect();
        for added_collection in &added {
            tracing::info!(
                source_collection = %added_collection,
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

        for added_collection in &added {
            if let Err(err) = self.seed_seen_docs_for_collection(added_collection).await {
                tracing::warn!(
                    source_collection = %added_collection,
                    %err,
                    "event source seed_seen_docs_for_collection failed; forward-only \
                     semantics may be weaker for pre-existing docs in this collection",
                );
            }
        }

        if self.subscription.is_none() && !self.desired_collections.is_empty() {
            let subscription = self.subscription_source.subscribe_updates();
            tracing::info!(
                collections = self.desired_collections.len(),
                generation = snapshot.generation,
                "event source opened global Update subscription",
            );
            self.subscription = Some(subscription);
        }

        self.reconciled_generation = snapshot.generation;
    }

    async fn seed_seen_docs_for_collection(&mut self, collection: &str) -> anyhow::Result<()> {
        let query = format!(
            r#"query {{ {collection}(limit: {limit}) {{ _docID }} }}"#,
            collection = collection,
            limit = SEEN_DOCS_SEED_LIMIT,
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            tracing::warn!(
                source_collection = %collection,
                errors = ?response.errors,
                "event source could not seed seen_docs (introspection errors); \
                 forward-only semantics may be weaker for pre-existing docs",
            );
            return Ok(());
        }
        let rows = response
            .data
            .as_ref()
            .and_then(|d| d.get(collection))
            .and_then(serde_json::Value::as_array);
        let Some(rows) = rows else {
            return Ok(());
        };
        let doc_ids: HashSet<String> = rows
            .iter()
            .filter_map(|r| r.get("_docID").and_then(|v| v.as_str()).map(str::to_owned))
            .collect();
        let count = doc_ids.len();
        if count >= SEEN_DOCS_SEED_LIMIT {
            tracing::warn!(
                source_collection = %collection,
                seed_count = %count,
                limit = %SEEN_DOCS_SEED_LIMIT,
                "event source seeded seen_docs at limit; older pre-existing docs \
                 beyond the cap may fire as created on their first observed event",
            );
        }
        self.seen_docs
            .entry(collection.to_string())
            .or_default()
            .extend(doc_ids);
        Ok(())
    }

    async fn load_doc_ids_for_collection(&self, collection: &str) -> anyhow::Result<Vec<String>> {
        let query = format!(
            r#"query {{ {collection}(limit: {limit}) {{ _docID }} }}"#,
            collection = collection,
            limit = SEEN_DOCS_SEED_LIMIT,
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!(
                "event source rescan query for {} failed: {:?}",
                collection,
                response.errors
            );
        }
        let rows: Vec<SourceDocIdRow> = response
            .data
            .as_ref()
            .and_then(|data| data.get(collection))
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        if rows.len() >= SEEN_DOCS_SEED_LIMIT {
            tracing::warn!(
                source_collection = %collection,
                limit = %SEEN_DOCS_SEED_LIMIT,
                "event source rescan hit limit; older unseen docs may wait for a later event"
            );
        }
        Ok(rows.into_iter().map(|row| row.doc_id).collect())
    }

    fn is_first_seen(&mut self, collection: &str, doc_id: &str) -> bool {
        let set = self.seen_docs.entry(collection.to_string()).or_default();
        set.insert(doc_id.to_string())
    }

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
            self.collection_id_to_name
                .insert(def.collection_id.clone(), def.name.clone());
        }

        self.collection_id_to_name.get(collection_id).cloned()
    }

    /// Run the trigger's `filter` against the source doc, narrowed by
    /// `_docID`, via a `limit: 1` probe. Returns `Ok(true)` when the doc
    /// matches (so the fire should proceed), `Ok(false)` when it doesn't
    /// (so the dispatch loop should skip), and `Err` when the probe query
    /// itself errored — the caller treats errors as "skip this fire" so a
    /// transient GraphQL failure doesn't brick the source.
    ///
    /// Trust boundary: `trigger.filter` is operator-authored and validated
    /// at apply time (the apply path rejects ill-formed filter objects
    /// before the trigger ever lands in `active_event_triggers`). It's
    /// interpolated directly as a filter-object fragment — we do NOT run
    /// it through `escape_graphql_string`, because that helper escapes
    /// scalar string literals and would break the object syntax. The
    /// `_docID` value, which comes from the event payload (external
    /// input), IS escaped.
    async fn probe_filter(
        &self,
        source_doc_id: &str,
        trigger: &crate::runtime_snapshot::ResolvedEventTrigger,
    ) -> anyhow::Result<bool> {
        let user_filter = trigger
            .filter
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty());
        let filter_literal = match user_filter {
            Some(f) => format!(
                r#"{{ _docID: {{ _eq: "{id}" }}, _and: [ {user_filter} ] }}"#,
                id = crate::graphql::escape_graphql_string(source_doc_id),
                user_filter = f,
            ),
            None => format!(
                r#"{{ _docID: {{ _eq: "{id}" }} }}"#,
                id = crate::graphql::escape_graphql_string(source_doc_id),
            ),
        };
        let query = format!(
            r#"query {{
                {collection}(filter: {filter_literal}, limit: 1) {{
                    _docID
                }}
            }}"#,
            collection = trigger.source_collection,
            filter_literal = filter_literal,
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("filter probe errors: {:?}", response.errors);
        }
        let rows = response
            .data
            .as_ref()
            .and_then(|d| d.get(&trigger.source_collection))
            .and_then(serde_json::Value::as_array);
        Ok(rows.is_some_and(|rs| !rs.is_empty()))
    }

    async fn fetch_source_doc(
        &self,
        collection: &str,
        source_doc_id: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let fields = self
            .source_schema_cache
            .fields_for(collection, &self.node)
            .await?;
        let projection = fields.join("\n                    ");
        let query = format!(
            r#"query {{
                {collection}(filter: {{ _docID: {{ _eq: "{id}" }} }}, limit: 1) {{
                    _docID
                    {projection}
                }}
            }}"#,
            collection = collection,
            id = crate::graphql::escape_graphql_string(source_doc_id),
            projection = projection,
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            anyhow::bail!("fetch source doc errors: {:?}", response.errors);
        }
        let Some(rows) = response
            .data
            .as_ref()
            .and_then(|d| d.get(collection))
            .and_then(serde_json::Value::as_array)
        else {
            anyhow::bail!(
                "source doc {} not found in {} (no rows in response)",
                source_doc_id,
                collection
            );
        };
        let Some(row) = rows.first() else {
            anyhow::bail!(
                "source doc {} not found in {} (empty rows)",
                source_doc_id,
                collection
            );
        };
        Ok(row.clone())
    }

    pub(super) fn spawn_runtime_field_write(
        node: Arc<EmbeddedNode>,
        trigger_id: String,
        source_doc_id: String,
        result: crate::trigger_engine::FireResult,
    ) {
        tokio::spawn(async move {
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let (status, error_value, fire_delta) = match &result {
                crate::trigger_engine::FireResult::Fired { request_id } => {
                    tracing::debug!(
                        trigger_id = %trigger_id,
                        request_id = %request_id,
                        "event trigger fire materialized request"
                    );
                    ("fired", None, Some(1))
                }
                crate::trigger_engine::FireResult::Skipped { reason } => {
                    ("skipped", Some(reason.clone()), None)
                }
                crate::trigger_engine::FireResult::Errored { error } => {
                    ("error", Some(error.clone()), None)
                }
            };
            let update = crate::document_config::EventTriggerRuntimeUpdate {
                last_attempt_at: Some(now),
                last_fired_source_doc_id: Some(source_doc_id),
                last_status: Some(status.to_string()),
                last_error: error_value,
                fire_count_delta: fire_delta,
            };
            if let Err(error) = crate::document_config::update_event_trigger_runtime_fields(
                &node,
                &trigger_id,
                update,
            )
            .await
            {
                tracing::warn!(
                    trigger_id = %trigger_id,
                    %error,
                    "event trigger runtime-field update failed"
                );
            }
        });
    }

    /// Build a `FireIntent` for every active `EventTrigger` whose
    /// `source_collection` matches `collection_name` AND `event_kind` matches
    /// `kind`. Each candidate's operator-authored filter is probed against
    /// `source_doc_id`; candidates that miss the filter or whose probe errors
    /// are skipped (those failures are isolated to the one candidate — they
    /// must not prevent the other matching triggers from firing). A
    /// successful candidate is hydrated via `fetch_source_doc` and wrapped in
    /// a `FireIntent` with a bookkeeping `on_result` callback identical to
    /// the single-trigger path.
    ///
    /// Candidates are ordered by `trigger_id` for determinism so tests and
    /// dispatch order are stable across ticks.
    ///
    /// Replaces the former `first_matching_trigger` helper, which silently
    /// dropped all but one matching trigger per event (and, worse, dropped
    /// the whole event when that one trigger's filter missed).
    async fn build_intents_for_all_matching(
        &self,
        snapshot: &ActiveRuntimeSnapshot,
        collection_name: &str,
        source_doc_id: &str,
        kind: &str,
    ) -> Vec<FireIntent> {
        let mut candidates: Vec<crate::runtime_snapshot::ResolvedEventTrigger> = snapshot
            .active_event_triggers()
            .values()
            .filter(|t| t.source_collection == collection_name && t.event_kind == kind)
            .cloned()
            .collect();
        candidates.sort_by(|a, b| a.trigger_id.cmp(&b.trigger_id));

        let mut intents = Vec::with_capacity(candidates.len());
        for trigger in candidates {
            match self.probe_filter(source_doc_id, &trigger).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::trace!(
                        trigger_id = %trigger.trigger_id,
                        source_collection = %collection_name,
                        %source_doc_id,
                        "event source: filter miss, skipping this trigger",
                    );
                    continue;
                }
                Err(err) => {
                    tracing::warn!(
                        trigger_id = %trigger.trigger_id,
                        source_collection = %collection_name,
                        %source_doc_id,
                        %err,
                        "event source: filter probe failed; skipping this trigger",
                    );
                    continue;
                }
            }

            let doc_vars = match self
                .fetch_source_doc(&trigger.source_collection, source_doc_id)
                .await
            {
                Ok(value) => Some(value),
                Err(err) => {
                    tracing::warn!(
                        trigger_id = %trigger.trigger_id,
                        source_collection = %collection_name,
                        %source_doc_id,
                        %err,
                        "event source: source-doc fetch failed; skipping this trigger",
                    );
                    continue;
                }
            };

            let fired_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
            let event_vars = serde_json::json!({
                "fired_at": fired_at,
                "trigger_id": trigger.trigger_id,
                "trigger_kind": TriggerKind::Event.as_str(),
                "source_collection": collection_name,
                "source_doc_id": source_doc_id,
            });

            tracing::info!(
                trigger_id = %trigger.trigger_id,
                source_collection = %collection_name,
                %source_doc_id,
                "event source matched event to trigger; emitting fire intent",
            );

            let trigger_id_for_callback = trigger.trigger_id.clone();
            let source_doc_id_for_callback = source_doc_id.to_string();
            let node_for_callback = self.node.clone();

            intents.push(FireIntent {
                trigger_id: Some(trigger.trigger_id.clone()),
                trigger_kind: TriggerKind::Event,
                task: trigger.task.clone(),
                concurrency: trigger.concurrency,
                event_vars,
                doc_vars,
                args_vars: None,
                pre_materialized_request_id: None,
                on_result: Box::new(move |result| {
                    EventSource::spawn_runtime_field_write(
                        node_for_callback,
                        trigger_id_for_callback,
                        source_doc_id_for_callback,
                        result,
                    );
                }),
            });
        }
        intents
    }

    fn take_first_and_queue_rest(&self, mut intents: Vec<FireIntent>) -> Option<FireIntent> {
        if intents.is_empty() {
            return None;
        }
        let first = intents.remove(0);
        let mut queue = self
            .pending_intents
            .lock()
            .expect("pending_intents mutex poisoned");
        for intent in intents {
            queue.push_back(intent);
        }
        Some(first)
    }

    async fn rescan_created_docs(&mut self) -> Option<FireIntent> {
        let mut collections: Vec<String> = self.desired_collections.iter().cloned().collect();
        collections.sort();
        let snapshot = self.snapshot_rx.borrow().clone();

        for collection in collections {
            let doc_ids = match self.load_doc_ids_for_collection(&collection).await {
                Ok(doc_ids) => doc_ids,
                Err(err) => {
                    tracing::warn!(
                        source_collection = %collection,
                        %err,
                        "event source periodic rescan failed for collection",
                    );
                    continue;
                }
            };

            for doc_id in doc_ids {
                if !self.is_first_seen(&collection, &doc_id) {
                    continue;
                }
                let intents = self
                    .build_intents_for_all_matching(
                        snapshot.as_ref(),
                        &collection,
                        &doc_id,
                        "created",
                    )
                    .await;
                if let Some(first) = self.take_first_and_queue_rest(intents) {
                    tracing::info!(
                        source_collection = %collection,
                        source_doc_id = %doc_id,
                        "event source periodic rescan emitted fire intent",
                    );
                    return Some(first);
                }
            }
        }
        None
    }
}

impl EventDeliveryRuntimeContract for EventSource {
    const EVENT_DELIVERY_CONTRACT: EventDeliverySourceContract = EventDeliverySourceContract {
        name: "EventSource",
        dedupe_policy: "monotone_once",
        rescan_bounded_by: 1,
        deviation: None,
    };
}

impl TriggerSource for EventSource {
    fn next_fire(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<FireIntent>> + Send + '_>> {
        Box::pin(async move {
            // Outer loop: reconcile-on-generation-bump, then race subscription
            // vs. snapshot-change vs. cancel. `None` here means "source is
            // permanently done, drop it" — an idle tick or an unmatched event
            // must not exit. Return `None` only on cancel or subscription
            // channel closure; keep looping otherwise so the engine's outer
            // driver doesn't teardown the source on the first miss.
            loop {
                if let Some(intent) = self
                    .pending_intents
                    .lock()
                    .expect("pending_intents mutex poisoned")
                    .pop_front()
                {
                    return Some(intent);
                }

                let snapshot = self.snapshot_rx.borrow().clone();
                if snapshot.generation > self.reconciled_generation {
                    self.reconcile_subscriptions(snapshot.as_ref()).await;
                }

                if self.subscription.is_none() || self.desired_collections.is_empty() {
                    tokio::select! {
                        biased;
                        _ = self.cancel.cancelled() => return None,
                        res = self.snapshot_rx.changed() => {
                            if res.is_err() {
                                return None;
                            }
                            continue;
                        }
                    }
                }

                // Step 3: race the subscription against snapshot changes and
                // cancel. Subscription is guaranteed Some here by the check
                // above, so we can take a &mut borrow for the recv poll.
                let mut message = None;
                let mut dropped = 0;
                let rescan_due = {
                    let subscription = self
                        .subscription
                        .as_mut()
                        .expect("subscription is Some when desired_collections is non-empty");
                    let rescan_due = tokio::select! {
                        biased;
                        _ = self.cancel.cancelled() => return None,
                        res = self.snapshot_rx.changed() => {
                            if res.is_err() {
                                return None;
                            }
                            continue;
                        }
                        _ = self.rescan_tick.tick() => true,
                        msg = subscription.recv() => {
                            match msg {
                                Some(m) => {
                                    message = Some(m);
                                    false
                                }
                                None => {
                                    tracing::warn!(
                                        "event source subscription channel closed; \
                                         source exiting",
                                    );
                                    return None;
                                }
                            }
                        }
                    };
                    if !rescan_due {
                        dropped = subscription.check_and_reset_dropped();
                    }
                    rescan_due
                };
                if rescan_due {
                    if let Some(intent) = self.rescan_created_docs().await {
                        return Some(intent);
                    }
                    continue;
                }
                let message = message.expect("subscription recv branch sets message");

                if dropped > 0 {
                    // Dropped events are a correctness hazard. The periodic
                    // rescan closes the gap for created docs, and this log
                    // keeps the lossy event visible operationally.
                    tracing::warn!(
                        dropped = dropped,
                        "event source dropped messages; periodic rescan will recover created docs",
                    );
                }

                let Some(update) = message.as_update() else {
                    continue;
                };

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

                if !self.desired_collections.contains(&collection_name) {
                    continue;
                }

                if !self.is_first_seen(&collection_name, &doc_id) {
                    tracing::debug!(
                        source_collection = %collection_name,
                        source_doc_id = %doc_id,
                        "event source treating non-first-seen event as update; skipping",
                    );
                    continue;
                }

                let snapshot = self.snapshot_rx.borrow().clone();
                let event_kind = "created";
                let intents = self
                    .build_intents_for_all_matching(
                        snapshot.as_ref(),
                        &collection_name,
                        &doc_id,
                        event_kind,
                    )
                    .await;
                if intents.is_empty() {
                    continue;
                }

                return self.take_first_and_queue_rest(intents);
            }
        })
    }
}
