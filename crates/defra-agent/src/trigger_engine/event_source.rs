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
use tokio::sync::watch;
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

/// `TriggerSource` that fans DefraDB document events out to
/// `active_event_triggers`.
///
/// Holds a single global `events::Subscription` (the `defra-node` API exposes
/// only `subscribe(&[EventName])` — there is no per-collection subscription)
/// and filters incoming events by `desired_collections` on the dispatch hot
/// path. That set is recomputed from the snapshot whenever
/// `snapshot.generation` bumps.
pub struct EventSource {
    snapshot_rx: watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
    node: Arc<EmbeddedNode>,
    subscription_source: Arc<dyn UpdateSubscriptionSource>,
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
    /// Per-source-collection schema cache used by `fetch_source_doc` to
    /// build a field-projection query. Entries are populated lazily on first
    /// hydration against a given source collection and reused for the
    /// lifetime of the source (collection schemas are stable across a
    /// deployment's runtime; no invalidation is needed here).
    source_schema_cache: SourceSchemaCache,
    /// Cache of `collection_id -> collection name` mappings. The Update event
    /// carries only the stable `collection_id` string (see `events::Update`),
    /// but `source_collection` on an `EventTrigger` is the human-readable
    /// collection name. We resolve lazily on first-encountered event, then
    /// reuse the cached mapping for subsequent events in the same collection.
    /// Entries are never invalidated — collection IDs are stable for the
    /// lifetime of a collection's existence.
    collection_id_to_name: HashMap<String, String>,
    /// Tracks `(collection, doc_id)` pairs already observed by this source
    /// this process lifetime. The DefraDB event bus fires a single
    /// `EventName::Update` for creates, updates, and deletes, so the source
    /// can't distinguish create from update at the event level. We enforce
    /// the v1 `event_kind = "created"` contract structurally: only the FIRST
    /// observation of a given `(collection, doc_id)` pair is treated as a
    /// creation. Seeded at subscription-open via a one-shot existing-docs
    /// query (see `seed_seen_docs_for_collection`) to enforce spec's
    /// forward-only semantic for pre-existing docs.
    seen_docs: HashMap<String, HashSet<String>>,
    /// Queued `FireIntent`s from a single event that matched multiple
    /// `EventTrigger`s with the same `source_collection` + `event_kind`.
    /// `next_fire` drains the queue one intent per call before polling the
    /// subscription again, so fan-out across N matching triggers yields N
    /// fires (rather than silently dropping N-1 as `first_matching_trigger`
    /// did previously). Wrapped in `std::sync::Mutex` so that
    /// `EventSource: Sync` (the `TriggerSource` trait requires it and the
    /// `Box<dyn FnOnce + Send>` inside each `FireIntent` is not itself
    /// `Sync`). The mutex is held for trivially-short critical sections
    /// (`pop_front` / `push_back`) with no `.await` inside, so it will never
    /// actually contend.
    pending_intents: Mutex<VecDeque<FireIntent>>,
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
    /// Return the projectable scalar-ish field names for `collection`,
    /// querying the node's GraphQL schema on first access and caching the
    /// result. Errors from the introspection query (or a missing
    /// `__type.fields` array) bubble up so the caller can skip the fire
    /// rather than materialize against a half-populated doc.
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
            // Skip GraphQL meta fields and DefraDB's `_count`-style
            // aggregate wrappers — they can't be projected as plain
            // scalars.
            .filter(|name| !name.starts_with('_'))
            // Skip DefraDB's upper-case aggregate/search verbs — they take
            // required arguments and can't appear in a bare projection.
            .filter(|name| !is_defradb_aggregate_field(name))
            .collect();
        guard.insert(collection.to_string(), fields.clone());
        Ok(fields)
    }
}

/// DefraDB generates per-collection GraphQL fields for aggregates and
/// full-text search that share the collection's scalar namespace but
/// require arguments to project. Include them in the `__type.fields`
/// response; reject them here so `fetch_source_doc`'s projection stays
/// syntactically valid. Mirrors the CLI's `is_aggregate_field` list in
/// `defradb.rs`'s `cli/src/commands/client/collection/introspection.rs`.
fn is_defradb_aggregate_field(name: &str) -> bool {
    matches!(
        name,
        "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "GROUP" | "SIMILARITY" | "BM25"
    )
}

impl EventSource {
    /// Build an event source wired to the given snapshot receiver and
    /// embedded node.
    ///
    /// The subscription itself is not created here — Task 19's
    /// `reconcile_subscriptions` opens it on the first tick once the
    /// snapshot's `active_event_triggers` have been read.
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
        // Enumerate newly-added collections so we can seed `seen_docs`
        // BEFORE the subscription starts delivering events for them. Without
        // seeding, any pre-existing doc whose first observation happens on
        // an update would be (incorrectly) treated as a create.
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
            // Intentionally DO NOT clear seen_docs for removed collections:
            // keeping the history means that if an operator re-adds the same
            // collection later in this process, we still know which doc_ids
            // we'd already observed. Clearing would be defensible too but
            // would risk a create-on-re-add storm for docs that predate the
            // original seed.
        }

        self.desired_collections = desired;

        // Seed seen_docs for each newly-added collection. Runs AFTER
        // `desired_collections` is updated so a concurrently-delivered event
        // landing mid-reconcile sees the set, but BEFORE `reconciled_generation`
        // is stamped so we don't advance the ticker past a partial seed on
        // error. The seed itself is best-effort — a collection that can't be
        // introspected (e.g. transiently missing from the schema) logs and
        // proceeds with an empty seen set.
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

        // Lazily open the global subscription. We defer opening until the
        // first non-empty desired set so a runtime with no event triggers
        // never materializes an unused subscription.
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

    /// Seed `seen_docs` for `collection` with every `_docID` currently
    /// persisted in that collection (up to `SEEN_DOCS_SEED_LIMIT`). Called
    /// from `reconcile_subscriptions` the first time a source collection
    /// appears in the desired set. This enforces the spec's forward-only
    /// semantic: pre-existing docs in the source collection do NOT fire as
    /// "created" when their first Update event arrives.
    ///
    /// A missing / unintrospectable collection is recoverable — we log a
    /// warning and proceed with an empty seen set, which is equivalent to
    /// treating every first-observed doc_id as a create (same behavior as
    /// the pre-fix code).
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

    /// Record that `(collection, doc_id)` has been observed and return
    /// whether this was the FIRST observation — i.e. whether the event
    /// should be treated as a "created" fire under v1 semantics. Subsequent
    /// observations (updates / deletes / replays) return `false`.
    fn is_first_seen(&mut self, collection: &str, doc_id: &str) -> bool {
        let set = self.seen_docs.entry(collection.to_string()).or_default();
        set.insert(doc_id.to_string())
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

    /// Project the source doc's scalar-ish fields into a JSON object that
    /// populates the FireIntent's `doc_vars`. Looks up (and caches) the
    /// collection's projectable field list via introspection, then runs a
    /// `{collection}(filter: { _docID: _eq }, limit: 1) { _docID <fields> }`
    /// query. Returns an error if the doc can't be found or the query
    /// itself fails — the caller treats errors as "skip this fire".
    async fn fetch_source_doc(
        &self,
        collection: &str,
        source_doc_id: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let fields = self
            .source_schema_cache
            .fields_for(collection, &self.node)
            .await?;
        // Even an empty `fields` list is valid: `_docID` alone still
        // round-trips the doc's identity so downstream render scopes can
        // reference `doc._docID`.
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

    /// Spawn a background task that writes the runtime-owned bookkeeping
    /// fields on the `EventTrigger` document referenced by `trigger_id`.
    ///
    /// Invoked from the `on_result` callback on a `FireIntent` emitted by
    /// `next_fire`. Runs off the engine's dispatch path so the inner loop
    /// isn't blocked on DefraDB I/O. Mirrors `ScheduleSource`'s callback:
    ///
    /// - `Fired`: `last_status = "fired"`, `fire_count += 1`, stamp the
    ///   source doc id that caused the fire, and clear `last_error`.
    /// - `Skipped`: `last_status = "skipped"`, record the skip reason in
    ///   `last_error` (for operator visibility into concurrency/latest-only
    ///   collapse), leave `fire_count` untouched.
    /// - `Errored`: `last_status = "error"`, record the failure string in
    ///   `last_error`, leave `fire_count` untouched.
    ///
    /// Writes are best-effort: a failing DefraDB update is logged at `warn`
    /// so an operator can correlate missing runtime fields with a backing
    /// mutation error, but it does not propagate (the event has already
    /// fired into the materializer).
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
                Ok(true) => { /* matched; fall through to hydrate */ }
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

            // Hydrate `doc_vars` per trigger. The projection is cached per
            // source collection, so N-trigger fan-out for a single doc runs
            // one introspection + N cheap filter-by-_docID queries.
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

            // Values captured for the result-writeback closure (see
            // single-trigger path for rationale).
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
}

impl EventDeliveryRuntimeContract for EventSource {
    const EVENT_DELIVERY_CONTRACT: EventDeliverySourceContract = EventDeliverySourceContract {
        name: "EventSource",
        dedupe_policy: "monotone_once",
        rescan_bounded_by: 0,
        deviation: Some("event_source_lacks_periodic_rescan"),
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
                // Step 0: drain any FireIntents queued from a prior event
                // whose fan-out matched multiple triggers. Returning the
                // queued intent here (without touching the subscription) is
                // what turns a single Update event into N sequential fires
                // across all N matching triggers.
                if let Some(intent) = self
                    .pending_intents
                    .lock()
                    .expect("pending_intents mutex poisoned")
                    .pop_front()
                {
                    return Some(intent);
                }

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

                // Step 7: gate on first-observation. DefraDB's event bus
                // fires a single `EventName::Update` for creates, updates,
                // AND deletes, so we can't distinguish them at the event
                // layer. v1 ships `event_kind = "created"` only, so we treat
                // the FIRST observation of a `(collection, doc_id)` pair as
                // the create and skip every subsequent event. Combined with
                // the existing-docs seed in `reconcile_subscriptions`, this
                // enforces the spec's forward-only semantic end-to-end.
                if !self.is_first_seen(&collection_name, &doc_id) {
                    tracing::debug!(
                        source_collection = %collection_name,
                        source_doc_id = %doc_id,
                        "event source treating non-first-seen event as update; skipping",
                    );
                    continue;
                }

                // Step 8: fan out to every matching trigger in the latest
                // snapshot. Re-borrow the snapshot so we're always checking
                // against the latest published view, not the copy we
                // captured for the generation-bump check (those might
                // diverge if a snapshot published while we were awaiting
                // `subscription.recv()`). `build_intents_for_all_matching`
                // probes each candidate's filter independently so a miss on
                // one trigger does not silently drop the event for the
                // other matching triggers.
                let snapshot = self.snapshot_rx.borrow().clone();
                // v1 spec: event_kind is always "created". If that widens,
                // map the event variant (Update carries no kind field today
                // — all writes go through Update, distinguished only by
                // block contents) to the right string.
                let event_kind = "created";
                let mut intents = self
                    .build_intents_for_all_matching(
                        snapshot.as_ref(),
                        &collection_name,
                        &doc_id,
                        event_kind,
                    )
                    .await;
                if intents.is_empty() {
                    // Either no triggers in the snapshot key on this
                    // collection+kind (can happen briefly after a reconcile
                    // removes the last matching trigger), or every
                    // candidate's filter missed / probe errored. Drop and
                    // park on the next event.
                    continue;
                }

                // Step 9: return the first intent now; queue the rest so
                // subsequent `next_fire` calls yield them one-at-a-time
                // before reading another event off the subscription. Order
                // is deterministic (sorted by trigger_id in
                // `build_intents_for_all_matching`).
                let first = intents.remove(0);
                {
                    let mut queue = self
                        .pending_intents
                        .lock()
                        .expect("pending_intents mutex poisoned");
                    for intent in intents {
                        queue.push_back(intent);
                    }
                }
                return Some(first);
            }
        })
    }
}
