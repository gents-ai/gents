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

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use defra_node::EmbeddedNode;
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
const GROUP_STARTUP_PAGE_BUDGET: usize = 1;
const GROUP_DUE_RECONCILE_BUDGET: usize = 16;
const MAX_ACTIVE_GROUP_TIMERS: usize = 4096;
const MAX_DORMANT_GROUP_TIMERS: usize = 4096;

fn captured_source_field(doc: &serde_json::Value, field: &str) -> anyhow::Result<Option<String>> {
    let Some(value) = doc.get(field) else {
        if field == "requester_did" {
            return Ok(None);
        }
        anyhow::bail!("source document is missing runtime-filled field `{field}`");
    };
    let canonical = match value {
        serde_json::Value::Null if field == "requester_did" => return Ok(None),
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(value)
            if value.as_i64().is_some() || value.as_u64().is_some() =>
        {
            value.to_string()
        }
        _ => anyhow::bail!(
            "runtime-filled source field `{field}` must be a string or integral number"
        ),
    };
    if field == "requester_did" && canonical.trim().is_empty() {
        anyhow::bail!("runtime-filled source field `requester_did` must be non-empty");
    }
    Ok(Some(canonical))
}

#[cfg(test)]
pub(super) use super::event_delivery::group_candidate_eligible;
use super::event_delivery::{self, Delivery, GroupOutcome, GROUP_RECOVERY_PAGE_SIZE};

fn take_due_group_batch(
    mut due: Vec<GroupTrackingKey>,
    cursor: &mut usize,
) -> Vec<GroupTrackingKey> {
    due.sort_by(|left, right| {
        left.consumer_key
            .cmp(&right.consumer_key)
            .then_with(|| left.correlation.cmp(&right.correlation))
    });
    if due.is_empty() {
        return Vec::new();
    }
    let start = *cursor % due.len();
    let count = due.len().min(GROUP_DUE_RECONCILE_BUDGET);
    let batch = (0..count)
        .map(|offset| due[(start + offset) % due.len()].clone())
        .collect();
    *cursor = (start + count) % due.len();
    batch
}

#[cfg(test)]
mod due_group_batch_tests {
    use super::*;

    #[test]
    fn due_group_batches_are_bounded_and_rotate_fairly() {
        let due = (0..(GROUP_DUE_RECONCILE_BUDGET + 3))
            .map(|index| GroupTrackingKey {
                consumer_key: "consumer-key".to_string(),
                correlation: format!("run-{index:03}"),
            })
            .collect::<Vec<_>>();
        let mut cursor = 0;

        let first = take_due_group_batch(due.clone(), &mut cursor);
        let second = take_due_group_batch(due, &mut cursor);

        assert_eq!(first.len(), GROUP_DUE_RECONCILE_BUDGET);
        assert_eq!(second.len(), GROUP_DUE_RECONCILE_BUDGET);
        assert_eq!(first[0].correlation, "run-000");
        assert_eq!(second[0].correlation, "run-016");
        assert!(second.iter().any(|key| key.correlation == "run-018"));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GroupTrackingKey {
    consumer_key: String,
    correlation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SourceDocumentKey {
    source_collection: String,
    source_doc_id: String,
}

#[derive(Debug, Clone)]
struct GroupTimer {
    first_seen: DateTime<Utc>,
    last_touched: Instant,
    dormant: bool,
    quiesced: bool,
}

#[derive(Default)]
struct DeliveryBuild {
    intents: Vec<FireIntent>,
    correlation_pending: bool,
    settled_trigger_ids: Vec<String>,
}

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
    // Fast path for documents whose matching trigger deliveries have all
    // settled. Incomplete correlation keeps a document out of this set.
    seen_docs: HashMap<String, HashSet<String>>,
    // Settled delivery identities for a document that still has at least one
    // correlation-incomplete sibling. This prevents a ready sibling from
    // firing again when a follow-up update supplies the missing correlation.
    partially_seen_triggers: HashMap<SourceDocumentKey, HashSet<String>>,
    pending_intents: Mutex<VecDeque<FireIntent>>,
    group_timers: Arc<Mutex<HashMap<GroupTrackingKey, GroupTimer>>>,
    group_due_cursor: usize,
    group_recovery_cursor: usize,
    group_page_cursors: HashMap<String, String>,
    group_trigger_fingerprints: HashMap<String, String>,
    #[cfg(test)]
    group_recovery_page_queries: AtomicUsize,
    #[cfg(test)]
    group_membership_queries: AtomicUsize,
    /// Periodic live rescan that closes the lossy-subscription gap. The
    /// interval is stored on the source so a busy stream of `next_fire()` calls
    /// does not reset the cadence.
    rescan_tick: tokio::time::Interval,
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
    pub(crate) async fn fields_for(
        &self,
        collection: &str,
        node: &EmbeddedNode,
    ) -> anyhow::Result<Vec<String>> {
        crate::graphql::validate_collection_identifier(collection)?;
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
            partially_seen_triggers: HashMap::new(),
            pending_intents: Mutex::new(VecDeque::new()),
            group_timers: Arc::new(Mutex::new(HashMap::new())),
            group_due_cursor: 0,
            group_recovery_cursor: 0,
            group_page_cursors: HashMap::new(),
            group_trigger_fingerprints: HashMap::new(),
            #[cfg(test)]
            group_recovery_page_queries: AtomicUsize::new(0),
            #[cfg(test)]
            group_membership_queries: AtomicUsize::new(0),
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

    #[cfg(test)]
    pub(crate) fn group_recovery_page_query_count(&self) -> usize {
        self.group_recovery_page_queries.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn group_membership_query_count(&self) -> usize {
        self.group_membership_queries.load(Ordering::Relaxed)
    }

    pub(crate) async fn reconcile_subscriptions(&mut self, snapshot: &ActiveRuntimeSnapshot) {
        // Resolve-time quarantine keeps identifier-invalid source
        // collections out of active_event_triggers, but this source must
        // not trust that another code path assembled the snapshot the same
        // way: names that fail the collection-identifier check never enter
        // the desired set, so no seed/rescan/probe query is built from them.
        let desired: HashSet<String> = snapshot
            .active_event_triggers()
            .values()
            .map(|t| t.source_collection.clone())
            .filter(
                |collection| match crate::graphql::validate_collection_identifier(collection) {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!(
                            source_collection = %collection,
                            generation = snapshot.generation,
                            %error,
                            "event source refusing to observe source collection: \
                             not a valid GraphQL collection identifier",
                        );
                        false
                    }
                },
            )
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

        let mut group_triggers = snapshot
            .active_event_triggers()
            .values()
            .filter(|trigger| {
                trigger.fire_mode == crate::runtime_snapshot::EventTriggerFireMode::PerGroup
            })
            .cloned()
            .collect::<Vec<_>>();
        group_triggers.sort_by(|left, right| left.trigger_id.cmp(&right.trigger_id));
        let active_group_trigger_ids = group_triggers
            .iter()
            .map(|trigger| trigger.trigger_id.as_str())
            .collect::<HashSet<_>>();
        let changed_group_trigger_ids = group_triggers
            .iter()
            .filter_map(|trigger| {
                let fingerprint = Self::delivery(snapshot, trigger).ok()?.config_key();
                (self.group_trigger_fingerprints.get(&trigger.trigger_id) != Some(&fingerprint))
                    .then(|| trigger.trigger_id.clone())
            })
            .collect::<HashSet<_>>();
        self.group_timers
            .lock()
            .expect("group_timers mutex poisoned")
            .retain(|key, _| {
                group_triggers.iter().any(|trigger| {
                    Self::delivery(snapshot, trigger)
                        .is_ok_and(|delivery| delivery.consumer_key() == key.consumer_key)
                })
            });
        self.group_page_cursors.retain(|trigger_id, _| {
            active_group_trigger_ids.contains(trigger_id.as_str())
                && !changed_group_trigger_ids.contains(trigger_id)
        });
        self.group_trigger_fingerprints = group_triggers
            .iter()
            .filter_map(|trigger| {
                Self::delivery(snapshot, trigger)
                    .ok()
                    .map(|delivery| (trigger.trigger_id.clone(), delivery.config_key()))
            })
            .collect();

        for added_collection in &added {
            if let Err(err) = self
                .seed_seen_docs_for_collection(added_collection, snapshot)
                .await
            {
                tracing::warn!(
                    source_collection = %added_collection,
                    %err,
                    "event source seed_seen_docs_for_collection failed; forward-only \
                     semantics may be weaker for pre-existing docs in this collection",
                );
            }
        }

        // A snapshot generation bump is not permission to full-scan every
        // source collection. Admit at most one page of changed group
        // membership here; the existing rotating sweep eventually covers the
        // remaining pages and triggers. Unrelated task/config updates do no
        // startup recovery I/O at all.
        for trigger in group_triggers
            .iter()
            .filter(|trigger| changed_group_trigger_ids.contains(&trigger.trigger_id))
            .take(GROUP_STARTUP_PAGE_BUDGET)
        {
            let mut seen = HashSet::new();
            let (intents, next_cursor, complete) = self
                .recover_trigger_group_page(snapshot, trigger, None, &mut seen)
                .await;
            if complete {
                self.group_page_cursors.remove(&trigger.trigger_id);
            } else if let Some(next_cursor) = next_cursor {
                self.group_page_cursors
                    .insert(trigger.trigger_id.clone(), next_cursor);
            }
            self.pending_intents
                .lock()
                .expect("pending_intents mutex poisoned")
                .extend(intents);
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

    async fn seed_seen_docs_for_collection(
        &mut self,
        collection: &str,
        snapshot: &ActiveRuntimeSnapshot,
    ) -> anyhow::Result<()> {
        crate::graphql::validate_collection_identifier(collection)?;
        let trigger_ids = snapshot
            .active_event_triggers()
            .values()
            .filter(|trigger| trigger.source_collection == collection)
            .map(|trigger| trigger.trigger_id.clone())
            .collect::<HashSet<_>>();
        let mut correlation_probes: BTreeMap<(Option<String>, String), Vec<String>> =
            BTreeMap::new();
        for trigger in snapshot
            .active_event_triggers()
            .values()
            .filter(|trigger| trigger.source_collection == collection)
        {
            let Some(field) = trigger
                .correlation_field
                .as_deref()
                .map(str::trim)
                .filter(|field| !field.is_empty())
            else {
                continue;
            };
            correlation_probes
                .entry((trigger.filter.clone(), field.to_string()))
                .or_default()
                .push(trigger.trigger_id.clone());
        }
        for (_, field) in correlation_probes.keys() {
            crate::graphql::validate_graphql_name(field)?;
        }
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
        let mut doc_ids: HashSet<String> = rows
            .iter()
            .filter_map(|row| {
                row.get("_docID")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        let mut deferred_by_doc: HashMap<String, HashSet<String>> = HashMap::new();
        for ((filter, field), correlated_trigger_ids) in correlation_probes {
            let filter = filter
                .as_deref()
                .map(str::trim)
                .filter(|filter| !filter.is_empty())
                .unwrap_or("{}");
            crate::graphql::validate_graphql_filter_fragment(filter)?;
            let query = format!(
                r#"query {{
                    {collection}(filter: {filter}, limit: {limit}) {{ _docID {field} }}
                }}"#,
                limit = SEEN_DOCS_SEED_LIMIT,
            );
            let response = self.node.execute(&query).await;
            if response.has_errors() {
                anyhow::bail!(
                    "correlation readiness seed for {}.{} failed: {:?}",
                    collection,
                    field,
                    response.errors
                );
            }
            for row in response
                .data
                .as_ref()
                .and_then(|data| data.get(collection))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
            {
                let ready = row
                    .get(&field)
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .is_some_and(|value| !value.is_empty());
                if !ready {
                    if let Some(doc_id) = row.get("_docID").and_then(serde_json::Value::as_str) {
                        deferred_by_doc
                            .entry(doc_id.to_string())
                            .or_default()
                            .extend(correlated_trigger_ids.iter().cloned());
                    }
                }
            }
        }
        for (doc_id, pending_trigger_ids) in &deferred_by_doc {
            doc_ids.remove(doc_id);
            self.mark_triggers_seen(
                collection,
                doc_id,
                trigger_ids
                    .difference(pending_trigger_ids)
                    .cloned()
                    .collect::<Vec<_>>(),
            );
        }
        let count = doc_ids.len();
        let deferred = deferred_by_doc.len();
        if deferred > 0 {
            tracing::debug!(
                source_collection = %collection,
                deferred_docs = deferred,
                "event source left pre-existing docs with incomplete correlation eligible for a follow-up update",
            );
        }
        if rows.len() >= SEEN_DOCS_SEED_LIMIT {
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
        crate::graphql::validate_collection_identifier(collection)?;
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
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get(collection))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        if rows.len() >= SEEN_DOCS_SEED_LIMIT {
            tracing::warn!(
                source_collection = %collection,
                limit = %SEEN_DOCS_SEED_LIMIT,
                "event source rescan hit limit; older unseen docs may wait for a later event"
            );
        }
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                row.get("_docID")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect())
    }

    fn has_seen(&self, collection: &str, doc_id: &str) -> bool {
        self.seen_docs
            .get(collection)
            .is_some_and(|docs| docs.contains(doc_id))
    }

    fn mark_seen(&mut self, collection: &str, doc_id: &str) {
        self.seen_docs
            .entry(collection.to_string())
            .or_default()
            .insert(doc_id.to_string());
        self.partially_seen_triggers.remove(&SourceDocumentKey {
            source_collection: collection.to_string(),
            source_doc_id: doc_id.to_string(),
        });
    }

    fn has_seen_trigger(&self, collection: &str, doc_id: &str, trigger_id: &str) -> bool {
        self.has_seen(collection, doc_id)
            || self
                .partially_seen_triggers
                .get(&SourceDocumentKey {
                    source_collection: collection.to_string(),
                    source_doc_id: doc_id.to_string(),
                })
                .is_some_and(|trigger_ids| trigger_ids.contains(trigger_id))
    }

    fn mark_triggers_seen(
        &mut self,
        collection: &str,
        doc_id: &str,
        trigger_ids: impl IntoIterator<Item = String>,
    ) {
        let trigger_ids = trigger_ids.into_iter().collect::<Vec<_>>();
        if trigger_ids.is_empty() {
            return;
        }
        self.partially_seen_triggers
            .entry(SourceDocumentKey {
                source_collection: collection.to_string(),
                source_doc_id: doc_id.to_string(),
            })
            .or_default()
            .extend(trigger_ids);
    }

    fn commit_delivery_seen_state(
        &mut self,
        collection: &str,
        doc_id: &str,
        build: &DeliveryBuild,
    ) {
        if build.correlation_pending {
            self.mark_triggers_seen(collection, doc_id, build.settled_trigger_ids.clone());
        } else {
            self.mark_seen(collection, doc_id);
        }
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
    /// Trust boundary, one defense per interpolation position:
    /// `source_collection` is validated as a collection identifier, the
    /// `_docID` from the event payload is escaped, and `trigger.filter` —
    /// spliced in whole as an object fragment, where escaping would break
    /// the syntax — is checked as a balanced filter object. A break-out has
    /// to close an `]`, `}` or `)` it never opened, which is what that
    /// check rejects.
    ///
    /// Note the CLI apply probe is not a substitute: it executes the filter
    /// rather than validating it, in a different embedding than this one.
    async fn probe_filter(
        &self,
        source_doc_id: &str,
        trigger: &crate::runtime_snapshot::ResolvedEventTrigger,
    ) -> anyhow::Result<bool> {
        Ok(event_delivery::probe_document(
            &self.node,
            &trigger.source_collection,
            source_doc_id,
            trigger.filter.as_deref(),
        )
        .await?
        .is_some())
    }

    async fn fetch_source_doc(
        &self,
        collection: &str,
        source_doc_id: &str,
    ) -> anyhow::Result<serde_json::Value> {
        // `fields_for` validates this same binding and `?`-propagates before
        // the query below is built, so it is the one gate for both sites.
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

    fn trigger_context_for_doc(
        snapshot: &ActiveRuntimeSnapshot,
        trigger: &crate::runtime_snapshot::ResolvedEventTrigger,
        doc: &serde_json::Value,
    ) -> anyhow::Result<Option<String>> {
        let fields = snapshot
            .tool_surface(&trigger.task.behavior_id)
            .map(|surface| surface.source_fill_fields())
            .unwrap_or_default();
        let mut source_fields = std::collections::BTreeMap::new();
        for field in fields {
            // Mailbox-capable stages capture an owner when the source has
            // one. Ownerless automation still runs; the stamped mailbox tool
            // itself fails closed if that stage tries to file without owner
            // lineage.
            if let Some(canonical) = captured_source_field(doc, &field)? {
                source_fields.insert(field, canonical);
            }
        }
        crate::lifecycle::snapshot_workspace_lineage_source_fields(
            doc,
            &mut source_fields,
            trigger.workspace_authority.as_deref(),
        );
        if source_fields.is_empty() {
            return Ok(None);
        }
        let encoded = serde_json::to_string(&crate::lifecycle::TriggerExecutionContext {
            version: 1,
            source_fields,
        })?;
        if encoded.len() > crate::lifecycle::MAX_TRIGGER_CONTEXT_BYTES {
            anyhow::bail!(
                "trigger execution context exceeds {} bytes",
                crate::lifecycle::MAX_TRIGGER_CONTEXT_BYTES
            );
        }
        Ok(Some(encoded))
    }

    fn delivery<'a>(
        snapshot: &'a ActiveRuntimeSnapshot,
        trigger: &'a crate::runtime_snapshot::ResolvedEventTrigger,
    ) -> anyhow::Result<Delivery<'a>> {
        let behavior = snapshot
            .behavior(&trigger.task.behavior_id)
            .ok_or_else(|| anyhow::anyhow!("group trigger behavior is unavailable"))?;
        Ok(Delivery::Trigger {
            agent_did: behavior.agent_did(),
            trigger,
        })
    }

    #[cfg(test)]
    pub(super) fn group_state_keys(
        owner: &str,
        trigger: &crate::runtime_snapshot::ResolvedEventTrigger,
        correlation: &str,
    ) -> (String, String) {
        let delivery = Delivery::Trigger {
            agent_did: owner,
            trigger,
        };
        (delivery.group_key(correlation), delivery.config_key())
    }

    fn cache_group_clock(
        &self,
        key: &GroupTrackingKey,
        first_seen: DateTime<Utc>,
        reactivate: bool,
    ) {
        let mut timers = self
            .group_timers
            .lock()
            .expect("group_timers mutex poisoned");
        if let Some(timer) = timers.get_mut(key) {
            timer.last_touched = Instant::now();
            if reactivate {
                timer.dormant = false;
            }
        } else if timers
            .iter()
            .filter(|(existing, timer)| existing.consumer_key == key.consumer_key && !timer.dormant)
            .count()
            < MAX_ACTIVE_GROUP_TIMERS
        {
            timers.insert(
                key.clone(),
                GroupTimer {
                    first_seen,
                    last_touched: Instant::now(),
                    dormant: false,
                    quiesced: false,
                },
            );
        }
    }

    fn mark_group_dormant(&self, key: &GroupTrackingKey) {
        let mut timers = self
            .group_timers
            .lock()
            .expect("group_timers mutex poisoned");
        if let Some(timer) = timers.get_mut(key) {
            timer.dormant = true;
            timer.last_touched = Instant::now();
        }
        let dormant_count = timers
            .iter()
            .filter(|(existing, timer)| existing.consumer_key == key.consumer_key && timer.dormant)
            .count();
        if dormant_count > MAX_DORMANT_GROUP_TIMERS {
            if let Some(oldest) = timers
                .iter()
                .filter(|(existing, timer)| {
                    existing.consumer_key == key.consumer_key && timer.dormant
                })
                .min_by_key(|(_, timer)| timer.last_touched)
                .map(|(key, _)| key.clone())
            {
                timers.remove(&oldest);
            }
        }
    }

    async fn reconcile_group(
        &self,
        snapshot: &ActiveRuntimeSnapshot,
        trigger: &crate::runtime_snapshot::ResolvedEventTrigger,
        correlation: &str,
        reactivate: bool,
    ) -> Option<FireIntent> {
        if correlation.trim().is_empty() {
            tracing::warn!(
                trigger_id = %trigger.trigger_id,
                "event-trigger group has an empty correlation; failing closed",
            );
            return None;
        }
        let delivery = match Self::delivery(snapshot, trigger) {
            Ok(delivery) => delivery,
            Err(error) => {
                tracing::warn!(%error,"event group owner unavailable");
                return None;
            }
        };
        let key = GroupTrackingKey {
            consumer_key: delivery.consumer_key(),
            correlation: correlation.to_string(),
        };
        if self
            .group_timers
            .lock()
            .expect("group_timers mutex poisoned")
            .get(&key)
            .is_some_and(|timer| timer.quiesced)
        {
            return None;
        }
        #[cfg(test)]
        self.group_membership_queries
            .fetch_add(1, Ordering::Relaxed);
        let (docs, complete) = match event_delivery::evaluate_group(
            &self.node,
            &self.source_schema_cache,
            delivery,
            correlation,
        )
        .await
        {
            Ok(GroupOutcome::Ready {
                first_seen,
                docs,
                complete,
            }) => {
                self.cache_group_clock(&key, first_seen, reactivate);
                (docs, complete)
            }
            Ok(GroupOutcome::Pending {
                first_seen,
                dormant,
            }) => {
                self.cache_group_clock(&key, first_seen, reactivate);
                if dormant {
                    self.mark_group_dormant(&key);
                }
                return None;
            }
            Ok(GroupOutcome::Empty | GroupOutcome::Quiesced) => return None,
            Err(error) => {
                tracing::warn!(trigger_id=%trigger.trigger_id,%correlation,%error,"event group reconciliation failed");
                return None;
            }
        };

        let representative = docs
            .first()
            .expect("non-empty group has representative")
            .clone();
        let source_doc_id = representative
            .get("_docID")
            .and_then(serde_json::Value::as_str)
            .expect("group projection always selects _docID")
            .to_string();
        let trigger_context =
            match Self::trigger_context_for_doc(snapshot, trigger, &representative) {
                Ok(context) => context,
                Err(error) => {
                    tracing::error!(
                        trigger_id = %trigger.trigger_id,
                        %correlation,
                        %error,
                        "event-trigger source-field snapshot failed; failing closed",
                    );
                    return None;
                }
            };
        let fired_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let event_vars = serde_json::json!({
            "fired_at": fired_at,
            "trigger_id": trigger.trigger_id,
            "trigger_kind": TriggerKind::Event.as_str(),
            "source_collection": trigger.source_collection,
            "source_doc_id": source_doc_id,
            "correlation": correlation,
        });
        let group_vars = serde_json::json!({
            "correlation_value": correlation,
            "count": docs.len(),
            "docs": docs,
            "complete": complete,
        });
        let owner_for_callback = Self::delivery(snapshot, &trigger).ok()?.owner().to_owned();
        let trigger_id_for_callback = trigger.trigger_id.clone();
        let source_doc_id_for_callback = source_doc_id.clone();
        let node_for_callback = self.node.clone();
        let timers = self.group_timers.clone();
        let key_for_callback = key.clone();

        Some(FireIntent {
            trigger_id: Some(trigger.trigger_id.clone()),
            trigger_kind: TriggerKind::Event,
            task: trigger.task.clone(),
            concurrency: trigger.concurrency,
            event_vars,
            doc_vars: Some(representative),
            correlation: Some(correlation.to_string()),
            group_vars: Some(group_vars),
            trigger_context,
            args_vars: None,
            durable_fire_key: crate::trigger_engine::durable_fire_key(
                "event-group",
                &[&delivery.group_key(correlation)],
            ),
            pre_materialized_request_id: None,
            on_result: Box::new(move |result| {
                if matches!(
                    result,
                    super::FireResult::Fired { .. } | super::FireResult::Skipped { .. }
                ) {
                    timers
                        .lock()
                        .expect("group_timers mutex poisoned")
                        .remove(&key_for_callback);
                }
                EventSource::spawn_runtime_field_write(
                    node_for_callback,
                    owner_for_callback,
                    trigger_id_for_callback,
                    source_doc_id_for_callback,
                    result,
                );
            }),
        })
    }

    async fn marked_group_correlations(
        &self,
        snapshot: &ActiveRuntimeSnapshot,
        trigger: &crate::runtime_snapshot::ResolvedEventTrigger,
        correlations: &[String],
    ) -> HashSet<String> {
        if correlations.is_empty() {
            return HashSet::new();
        }
        let Some(agent_did) = snapshot
            .behavior(&trigger.task.behavior_id)
            .map(|behavior| behavior.agent_did())
        else {
            return HashSet::new();
        };
        let delivery = Delivery::Trigger { agent_did, trigger };
        let keys = correlations
            .iter()
            .map(|correlation| {
                (
                    correlation.clone(),
                    super::durable_fire_key("event-group", &[&delivery.group_key(correlation)]),
                )
            })
            .collect::<HashMap<_, _>>();
        let correlations = correlations
            .iter()
            .map(|correlation| {
                format!("\"{}\"", crate::graphql::escape_graphql_string(correlation))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            r#"query {{
                AgentRequest(
                    filter: {{
                        agent_did: {{ _eq: "{agent_did}" }},
                        caused_by_trigger_id: {{ _eq: "{trigger_id}" }},
                        caused_by_correlation: {{ _in: [{correlations}] }}
                    }},
                    limit: {limit}
                ) {{ caused_by_correlation request_id }}
            }}"#,
            agent_did = crate::graphql::escape_graphql_string(agent_did),
            trigger_id = crate::graphql::escape_graphql_string(&trigger.trigger_id),
            limit = GROUP_RECOVERY_PAGE_SIZE,
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            tracing::warn!(
                trigger_id = %trigger.trigger_id,
                errors = ?response.errors,
                "event-trigger batched marker prune failed; dispatch will retain the final marker check",
            );
            return HashSet::new();
        }
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|row| {
                let correlation = row.get("caused_by_correlation")?.as_str()?;
                let key = keys.get(correlation)?;
                let request_id = row.get("request_id")?.as_str()?;
                event_delivery::request_matches_fire_key(agent_did, request_id, key)
                    .then(|| correlation.to_owned())
            })
            .collect()
    }

    async fn quiesced_group_correlations(
        &self,
        delivery: Delivery<'_>,
        correlations: &[String],
    ) -> HashSet<String> {
        if correlations.is_empty() {
            return HashSet::new();
        }
        let keys_to_correlations = correlations
            .iter()
            .map(|correlation| {
                let key = delivery.group_key(correlation);
                (key, correlation.clone())
            })
            .collect::<HashMap<_, _>>();
        let keys = keys_to_correlations
            .keys()
            .map(|key| format!("\"{}\"", crate::graphql::escape_graphql_string(key)))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            r#"query {{
                EventGroupState(
                    filter: {{ agent_did: {{ _eq: "{owner}" }}, group_key: {{ _in: [{keys}] }} }},
                    limit: {limit}
                ) {{ group_key quiesced_at }}
            }}"#,
            owner = crate::graphql::escape_graphql_string(delivery.owner()),
            limit = GROUP_RECOVERY_PAGE_SIZE,
        );
        let response = self.node.execute(&query).await;
        if response.has_errors() {
            tracing::warn!(
                consumer = ?delivery.consumer(),
                errors = ?response.errors,
                "event-trigger durable quiescence prune failed; invalid groups may be rechecked",
            );
            return HashSet::new();
        }
        response
            .data
            .as_ref()
            .and_then(|data| data.get("EventGroupState"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter(|row| row.get("quiesced_at").is_some_and(|value| !value.is_null()))
            .filter_map(|row| {
                row.get("group_key")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|key| keys_to_correlations.get(key))
                    .cloned()
            })
            .collect()
    }

    async fn recover_trigger_group_page(
        &self,
        snapshot: &ActiveRuntimeSnapshot,
        trigger: &crate::runtime_snapshot::ResolvedEventTrigger,
        cursor: Option<&str>,
        seen_correlations: &mut HashSet<String>,
    ) -> (Vec<FireIntent>, Option<String>, bool) {
        #[cfg(test)]
        self.group_recovery_page_queries
            .fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let delivery = match Self::delivery(snapshot, trigger) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%error,"group recovery owner unavailable");
                return (Vec::new(), None, true);
            }
        };
        let (correlations, next_cursor, complete) =
            match event_delivery::group_correlation_page(&self.node, delivery, cursor).await {
                Ok(page) => page,
                Err(error) => {
                    tracing::warn!(%error,"event group recovery page failed");
                    return (Vec::new(), cursor.map(str::to_owned), false);
                }
            };
        let correlations = correlations
            .into_iter()
            .filter(|value| seen_correlations.insert(value.clone()))
            .collect::<Vec<_>>();
        let marked = self
            .marked_group_correlations(snapshot, trigger, &correlations)
            .await;
        let quiesced = self
            .quiesced_group_correlations(delivery, &correlations)
            .await;
        let mut intents = Vec::new();
        for correlation in correlations
            .iter()
            .filter(|value| !marked.contains(*value) && !quiesced.contains(*value))
        {
            if self.cancel.is_cancelled() {
                break;
            }
            let tracking_key = GroupTrackingKey {
                consumer_key: delivery.consumer_key(),
                correlation: correlation.clone(),
            };
            if self
                .group_timers
                .lock()
                .expect("group_timers mutex poisoned")
                .get(&tracking_key)
                .is_some_and(|timer| timer.dormant)
            {
                continue;
            }
            if let Some(intent) = self
                .reconcile_group(snapshot, trigger, correlation, false)
                .await
            {
                intents.push(intent);
            }
        }
        tracing::debug!(
            trigger_id = %trigger.trigger_id,
            page_complete = complete,
            dirty_groups = correlations.len(),
            marker_pruned = marked.len(),
            quiesced_pruned = quiesced.len(),
            emitted_intents = intents.len(),
            sweep_millis = started.elapsed().as_millis(),
            "event-trigger group recovery page reconciled",
        );
        (intents, next_cursor, complete)
    }

    async fn reconcile_due_and_rotating_groups(&mut self) -> Option<FireIntent> {
        let snapshot = self.snapshot_rx.borrow().clone();
        let now = Utc::now();
        let due = {
            let timers = self
                .group_timers
                .lock()
                .expect("group_timers mutex poisoned");
            timers
                .iter()
                .filter(|(_, timer)| !timer.dormant && !timer.quiesced)
                .filter_map(|(key, timer)| {
                    let trigger = snapshot.active_event_triggers().values().find(|trigger| {
                        Self::delivery(&snapshot, trigger)
                            .is_ok_and(|delivery| delivery.consumer_key() == key.consumer_key)
                    })?;
                    let timeout = Duration::from_secs(trigger.group_timeout_secs?);
                    now.signed_duration_since(timer.first_seen)
                        .to_std()
                        .is_ok_and(|elapsed| elapsed >= timeout)
                        .then(|| key.clone())
                })
                .collect::<Vec<_>>()
        };
        let due = take_due_group_batch(due, &mut self.group_due_cursor);
        let mut intents = Vec::new();
        for key in due {
            if self.cancel.is_cancelled() {
                return None;
            }
            let Some(trigger) = snapshot.active_event_triggers().values().find(|trigger| {
                Self::delivery(&snapshot, trigger)
                    .is_ok_and(|delivery| delivery.consumer_key() == key.consumer_key)
            }) else {
                continue;
            };
            if let Some(intent) = self
                .reconcile_group(snapshot.as_ref(), trigger, &key.correlation, false)
                .await
            {
                intents.push(intent);
            }
        }

        let mut group_triggers = snapshot
            .active_event_triggers()
            .values()
            .filter(|trigger| {
                trigger.fire_mode == crate::runtime_snapshot::EventTriggerFireMode::PerGroup
            })
            .cloned()
            .collect::<Vec<_>>();
        group_triggers.sort_by(|left, right| left.trigger_id.cmp(&right.trigger_id));
        if self.cancel.is_cancelled() {
            return None;
        }
        if let Some(trigger) =
            group_triggers.get(self.group_recovery_cursor % group_triggers.len().max(1))
        {
            self.group_recovery_cursor = self.group_recovery_cursor.wrapping_add(1);
            let cursor = self.group_page_cursors.get(&trigger.trigger_id).cloned();
            let mut seen = HashSet::new();
            let (page_intents, next_cursor, complete) = self
                .recover_trigger_group_page(
                    snapshot.as_ref(),
                    trigger,
                    cursor.as_deref(),
                    &mut seen,
                )
                .await;
            intents.extend(page_intents);
            if complete {
                self.group_page_cursors.remove(&trigger.trigger_id);
            } else if let Some(next_cursor) = next_cursor {
                self.group_page_cursors
                    .insert(trigger.trigger_id.clone(), next_cursor);
            }
        }
        self.take_first_and_queue_rest(intents)
    }

    pub(super) fn spawn_runtime_field_write(
        node: Arc<EmbeddedNode>,
        agent_did: String,
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
            let update = crate::document_config::TriggerRuntimeUpdate {
                next_run_at: None,
                last_attempt_at: Some(now),
                last_fired_source_doc_id: Some(source_doc_id),
                last_status: Some(status.to_string()),
                last_error: error_value,
                fire_count_delta: fire_delta,
            };
            if let Err(error) = crate::document_config::update_trigger_runtime_fields(
                &node,
                &agent_did,
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

    /// Build a `FireIntent` for every active event-source trigger whose
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
    ) -> DeliveryBuild {
        let mut candidates: Vec<crate::runtime_snapshot::ResolvedEventTrigger> = snapshot
            .active_event_triggers()
            .values()
            .filter(|t| t.source_collection == collection_name && t.event_kind == kind)
            .cloned()
            .collect();
        candidates.sort_by(|a, b| a.trigger_id.cmp(&b.trigger_id));

        let mut build = DeliveryBuild {
            intents: Vec::with_capacity(candidates.len()),
            correlation_pending: false,
            settled_trigger_ids: Vec::with_capacity(candidates.len()),
        };
        for trigger in candidates {
            if self.has_seen_trigger(collection_name, source_doc_id, &trigger.trigger_id) {
                continue;
            }
            match self.probe_filter(source_doc_id, &trigger).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::trace!(
                        trigger_id = %trigger.trigger_id,
                        source_collection = %collection_name,
                        %source_doc_id,
                        "event source: filter miss, skipping this trigger",
                    );
                    build.settled_trigger_ids.push(trigger.trigger_id.clone());
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
                    build.settled_trigger_ids.push(trigger.trigger_id.clone());
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
                    build.settled_trigger_ids.push(trigger.trigger_id.clone());
                    continue;
                }
            };

            let correlation = match trigger.correlation_field.as_deref() {
                None => None,
                Some(field) => match doc_vars
                    .as_ref()
                    .and_then(|doc| doc.get(field))
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    Some(value) => Some(value.to_string()),
                    None => {
                        build.correlation_pending = true;
                        tracing::debug!(
                            trigger_id = %trigger.trigger_id,
                            %source_doc_id,
                            correlation_field = %field,
                            "event source correlation is not ready; deferring document delivery",
                        );
                        continue;
                    }
                },
            };

            if trigger.fire_mode == crate::runtime_snapshot::EventTriggerFireMode::PerGroup {
                let correlation =
                    correlation.expect("resolved per_group trigger has correlation_field");
                if let Some(intent) = self
                    .reconcile_group(snapshot, &trigger, &correlation, true)
                    .await
                {
                    build.intents.push(intent);
                }
                build.settled_trigger_ids.push(trigger.trigger_id.clone());
                continue;
            }

            let trigger_context = match Self::trigger_context_for_doc(
                snapshot,
                &trigger,
                doc_vars.as_ref().expect("source doc was hydrated"),
            ) {
                Ok(context) => context,
                Err(error) => {
                    tracing::warn!(
                        trigger_id = %trigger.trigger_id,
                        %source_doc_id,
                        %error,
                        "event source trigger-context snapshot failed; skipping fire",
                    );
                    build.settled_trigger_ids.push(trigger.trigger_id.clone());
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
                "correlation": correlation,
            });

            tracing::info!(
                trigger_id = %trigger.trigger_id,
                source_collection = %collection_name,
                %source_doc_id,
                "event source matched event to trigger; emitting fire intent",
            );

            let Ok(delivery) = Self::delivery(snapshot, &trigger) else {
                continue;
            };
            let owner_for_callback = delivery.owner().to_owned();
            let trigger_id_for_callback = trigger.trigger_id.clone();
            let source_doc_id_for_callback = source_doc_id.to_string();
            let node_for_callback = self.node.clone();

            build.intents.push(FireIntent {
                trigger_id: Some(trigger.trigger_id.clone()),
                trigger_kind: TriggerKind::Event,
                task: trigger.task.clone(),
                concurrency: trigger.concurrency,
                event_vars,
                doc_vars,
                correlation,
                group_vars: None,
                trigger_context,
                args_vars: None,
                durable_fire_key: crate::trigger_engine::durable_fire_key(
                    "event-document",
                    &[&trigger.trigger_id, &source_doc_id],
                ),
                pre_materialized_request_id: None,
                on_result: Box::new(move |result| {
                    EventSource::spawn_runtime_field_write(
                        node_for_callback,
                        owner_for_callback,
                        trigger_id_for_callback,
                        source_doc_id_for_callback,
                        result,
                    );
                }),
            });
            build.settled_trigger_ids.push(trigger.trigger_id.clone());
        }
        build
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
        if let Some(intent) = self.reconcile_due_and_rotating_groups().await {
            return Some(intent);
        }
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
                if self.has_seen(&collection, &doc_id) {
                    continue;
                }
                let build = self
                    .build_intents_for_all_matching(
                        snapshot.as_ref(),
                        &collection,
                        &doc_id,
                        "created",
                    )
                    .await;
                self.commit_delivery_seen_state(&collection, &doc_id, &build);
                if let Some(first) = self.take_first_and_queue_rest(build.intents) {
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

                if self.has_seen(&collection_name, &doc_id) {
                    tracing::debug!(
                        source_collection = %collection_name,
                        source_doc_id = %doc_id,
                        "event source treating non-first-seen event as update; skipping",
                    );
                    continue;
                }

                let snapshot = self.snapshot_rx.borrow().clone();
                let event_kind = "created";
                let build = self
                    .build_intents_for_all_matching(
                        snapshot.as_ref(),
                        &collection_name,
                        &doc_id,
                        event_kind,
                    )
                    .await;
                self.commit_delivery_seen_state(&collection_name, &doc_id, &build);
                if build.intents.is_empty() {
                    continue;
                }

                return self.take_first_and_queue_rest(build.intents);
            }
        })
    }
}

#[cfg(test)]
mod mailbox_owner_capture_tests {
    use super::captured_source_field;

    #[test]
    fn ownerless_source_is_optional_but_declared_source_fills_are_not() {
        let doc = serde_json::json!({"run_id": "run-1"});
        assert_eq!(captured_source_field(&doc, "requester_did").unwrap(), None);
        let null_owner = serde_json::json!({"requester_did": null});
        assert_eq!(
            captured_source_field(&null_owner, "requester_did").unwrap(),
            None
        );
        assert!(captured_source_field(&doc, "required_route").is_err());
        assert_eq!(
            captured_source_field(&doc, "run_id").unwrap().as_deref(),
            Some("run-1")
        );
    }

    #[test]
    fn malformed_present_owner_is_rejected() {
        let doc = serde_json::json!({"requester_did": ["did:test:owner"]});
        assert!(captured_source_field(&doc, "requester_did").is_err());
        let blank = serde_json::json!({"requester_did": "   "});
        assert!(captured_source_field(&blank, "requester_did").is_err());
    }
}
