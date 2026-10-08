use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::event_delivery::EventConsumer;
use serde_json::Value;
use tokio::time::MissedTickBehavior;

use crate::config_client::{event_source_cursor, ConfigAccess};
use crate::graphql::{document_composite_version, escape_graphql_string};
use crate::UpdateSubscriptionSource;

use super::claim::invocation_is_claimable;
use super::documents::{
    create_pending_invocation, idempotency_key, list_enabled_bindings, load_binding, load_callback,
    load_event_source, strip_secret_fields, validate_callback_binding, CallbackBindingDoc,
    CallbackInvocationDoc,
};
use super::run::run_owned_invocation;
use super::{CallbackEngine, LIFECYCLE_PENDING};

const SEEN_DOCS_SEED_LIMIT: usize = 10_000;
const CALLBACK_RESCAN_INTERVAL: Duration = Duration::from_secs(5);

use crate::trigger_engine::event_delivery::{self, Delivery, GroupOutcome};
pub(super) use crate::trigger_engine::event_source::SourceSchemaCache;

pub(super) fn rescan_tick() -> tokio::time::Interval {
    let mut tick = tokio::time::interval(CALLBACK_RESCAN_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    tick
}

impl CallbackEngine {
    pub(super) fn new(
        node: Arc<EmbeddedNode>,
        agent_did: String,
        ceiling: Option<std::path::PathBuf>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self::with_subscription_source(node.clone(), node, agent_did, ceiling, cancel)
    }

    pub(super) fn with_subscription_source(
        subs: Arc<dyn UpdateSubscriptionSource>,
        node: Arc<EmbeddedNode>,
        agent_did: String,
        ceiling: Option<std::path::PathBuf>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            node,
            agent_did,
            ceiling,
            plugins: Arc::default(),
            subscription_source: subs,
            subscription: None,
            desired_collections: HashSet::new(),
            seen_docs: HashMap::new(),
            cursor_bindings: HashMap::new(),
            group_collections: HashSet::new(),
            collection_id_to_name: HashMap::new(),
            group_page_cursors: HashMap::new(),
            group_recovery_cursor: 0,
            rescan_tick: rescan_tick(),
            cancel,
        }
    }

    pub(super) async fn reconcile_bindings(&mut self) {
        let bindings = match list_enabled_bindings(self.node.as_ref(), &self.agent_did).await {
            Ok(bindings) => bindings,
            Err(error) => {
                tracing::warn!(%error, "callback engine failed to load CallbackBinding rows");
                return;
            }
        };
        let mut desired = HashSet::new();
        let mut cursor_bindings = HashMap::new();
        let mut group_collections = HashSet::new();
        // Per grouped collection, each binding and the source field that correlates it.
        let mut consumers: HashMap<String, Vec<(String, Option<String>)>> = HashMap::new();
        for binding in &bindings {
            match load_event_source(
                self.node.as_ref(),
                &binding.event_source_id,
                &binding.agent_did,
            )
            .await
            {
                Ok(Some(source)) => {
                    if source.group.is_none() {
                        // Configuration applies seed this cursor when they
                        // register the binding; this covers bindings written
                        // outside them.
                        if !self.cursor_bindings.contains_key(&binding.binding_id) {
                            let _ = self.load_cursor(binding, &source).await;
                        }
                        cursor_bindings
                            .insert(binding.binding_id.clone(), source.source_collection.clone());
                    } else {
                        consumers
                            .entry(source.source_collection.clone())
                            .or_default()
                            .push((binding.binding_id.clone(), source.correlation_field.clone()));
                        group_collections.insert(source.source_collection.clone());
                    }
                    desired.insert(source.source_collection);
                }
                Ok(None) => {
                    tracing::warn!(binding_id = %binding.binding_id, "callback EventSource missing for owner")
                }
                Err(error) => {
                    tracing::warn!(%error, binding_id = %binding.binding_id, "callback EventSource load failed")
                }
            }
        }
        let added: Vec<String> = group_collections
            .difference(&self.group_collections)
            .cloned()
            .collect();
        for collection in &added {
            let collection_consumers = consumers.get(collection).map_or(&[][..], Vec::as_slice);
            if let Err(error) = self.seed_seen_docs(collection, collection_consumers).await {
                tracing::warn!(
                    source_collection = %collection,
                    %error,
                    "callback engine seed_seen_docs failed; forward-only semantics may be weaker"
                );
            }
        }
        self.desired_collections = desired;
        self.cursor_bindings = cursor_bindings;
        self.seen_docs
            .retain(|collection, _| group_collections.contains(collection));
        self.group_collections = group_collections;
        if self.subscription.is_none() && !self.desired_collections.is_empty() {
            self.subscription = Some(self.subscription_source.subscribe_updates());
        }
    }

    /// Marks the documents already in `collection` as history for grouped
    /// bindings, except those of a graph run already underway on one of
    /// `consumers`' own revisions: those are live work the new consumer must
    /// still deliver.
    async fn seed_seen_docs(
        &mut self,
        collection: &str,
        consumers: &[(String, Option<String>)],
    ) -> Result<()> {
        let mut ids: HashSet<String> = load_doc_ids(self.node.as_ref(), collection)
            .await?
            .into_iter()
            .collect();
        let live = crate::graph_pipeline::live_run_correlations(
            self.node.as_ref(),
            &self.agent_did,
            consumers.iter().map(|(id, _)| id.as_str()),
        )
        .await?;
        if !live.is_empty() {
            for (consumer, field) in consumers {
                let Some(field) = field else { continue };
                for (doc_id, correlation) in
                    load_doc_field(self.node.as_ref(), collection, field).await?
                {
                    if crate::graph_pipeline::is_live_for(&live, consumer, &correlation) {
                        ids.remove(&doc_id);
                    }
                }
            }
        }
        self.seen_docs
            .entry(collection.to_string())
            .or_default()
            .extend(ids);
        Ok(())
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
    }

    pub(super) async fn rescan_created_docs(&mut self) {
        self.recover_group_page().await;
        self.deliver_arrivals(None).await;
        let collections: Vec<String> = self.group_collections.iter().cloned().collect();
        for collection in collections {
            let ids = match load_doc_ids(self.node.as_ref(), &collection).await {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::warn!(
                        source_collection = %collection,
                        %error,
                        "callback engine rescan query failed"
                    );
                    continue;
                }
            };
            for doc_id in ids {
                if self.cancel.is_cancelled() {
                    return;
                }
                if self.has_seen(&collection, &doc_id) {
                    continue;
                }
                self.handle_created_doc(&collection, &doc_id).await;
            }
        }
    }

    pub(super) async fn handle_update(&mut self, collection_id: &str, doc_id: &str) {
        let Some(collection) = self.resolve_collection_name(collection_id).await else {
            return;
        };
        if self
            .cursor_bindings
            .values()
            .any(|source| source == &collection)
        {
            self.deliver_arrivals(Some(&collection)).await;
        }
        if !self.group_collections.contains(&collection) || self.has_seen(&collection, doc_id) {
            return;
        }
        self.handle_created_doc(&collection, doc_id).await;
    }

    async fn load_cursor(
        &self,
        binding: &CallbackBindingDoc,
        source: &crate::document_config::EventSource,
    ) -> Result<String> {
        let consumer = EventConsumer::CallbackBinding {
            binding_id: binding.binding_id.clone(),
        };
        let record =
            ConfigAccess::transact_local(&self.node, None, "callback.seed_arrival_cursor", |txn| {
                let consumer = &consumer;
                Box::pin(async move {
                    event_source_cursor::load_or_seed_for_source(
                        txn,
                        &binding.agent_did,
                        consumer,
                        &source.source_collection,
                    )
                    .await
                })
            })
            .await;
        match record {
            Ok(record) => Ok(record.cursor.after),
            Err(error) => {
                tracing::warn!(
                    binding_id = %binding.binding_id,
                    %error,
                    "callback arrival cursor unavailable; delivery remains pending"
                );
                Err(error)
            }
        }
    }

    /// Advances `binding`'s cursor through `position` when every arrival up
    /// to it is admitted as an invocation or excluded by the cursor owner.
    async fn checkpoint(
        &self,
        binding: &CallbackBindingDoc,
        collection: &str,
        position: &str,
    ) -> Result<bool> {
        let consumer = EventConsumer::CallbackBinding {
            binding_id: binding.binding_id.clone(),
        };
        ConfigAccess::transact_local(&self.node, None, "callback.advance_arrival", |txn| {
            let consumer = &consumer;
            Box::pin(async move {
                event_source_cursor::checkpoint_prefix(
                    txn,
                    &binding.agent_did,
                    consumer,
                    collection,
                    position,
                    false,
                )
                .await
            })
        })
        .await
    }

    /// Per-document bindings deliver from their receiving-node arrival cursor,
    /// the owner task triggers use: an arrival after a binding's registration
    /// stays pending until admitted as an invocation or excluded, however late
    /// this engine first observes the source.
    pub(super) async fn deliver_arrivals(&mut self, collection: Option<&str>) {
        let bindings = match list_enabled_bindings(self.node.as_ref(), &self.agent_did).await {
            Ok(bindings) => bindings,
            Err(error) => {
                tracing::warn!(%error, "callback engine failed to load CallbackBinding rows");
                return;
            }
        };
        for binding in bindings {
            if self.cancel.is_cancelled() {
                return;
            }
            let Some(source_collection) = self.cursor_bindings.get(&binding.binding_id) else {
                continue;
            };
            if collection.is_some_and(|collection| collection != source_collection) {
                continue;
            }
            let source = match load_event_source(
                self.node.as_ref(),
                &binding.event_source_id,
                &binding.agent_did,
            )
            .await
            {
                Ok(Some(source)) => source,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(%error, binding_id = %binding.binding_id, "callback EventSource load failed");
                    continue;
                }
            };
            if source.group.is_some()
                || collection.is_some_and(|collection| collection != source.source_collection)
            {
                continue;
            }
            if let Err(error) = self.deliver_binding_arrivals(&binding, &source).await {
                tracing::warn!(
                    binding_id = %binding.binding_id,
                    source_collection = %source.source_collection,
                    %error,
                    "callback arrival delivery remains pending"
                );
            }
        }
    }

    async fn deliver_binding_arrivals(
        &mut self,
        binding: &CallbackBindingDoc,
        source: &crate::document_config::EventSource,
    ) -> Result<()> {
        let collection = source.source_collection.as_str();
        let mut after = self.load_cursor(binding, source).await?;
        loop {
            let page_start = after.clone();
            let response = crate::graphql::graphql_with_transaction_retry(
                &self.node,
                &format!(
                    "{{ _documentArrivals(collection: \"{}\", after: \"{}\", limit: 128) {{ next entries {{ cursor docID }} }} }}",
                    escape_graphql_string(collection),
                    escape_graphql_string(&after),
                ),
                "callback.read_arrivals",
            )
            .await?;
            let data = response.data.context("arrival query omitted data")?;
            let page = &data["_documentArrivals"];
            let entries = page["entries"]
                .as_array()
                .context("arrival query omitted entries")?;
            for entry in entries {
                if self.cancel.is_cancelled() {
                    return Ok(());
                }
                let doc_id = entry["docID"]
                    .as_str()
                    .context("arrival lacks document ID")?;
                let position = entry["cursor"].as_str().context("arrival lacks cursor")?;
                // Admission is idempotent per binding and document, so an
                // arrival admitted before a crash finds its invocation again.
                self.materialize_for_binding(binding, collection, doc_id, false)
                    .await?;
                if !self.checkpoint(binding, collection, position).await? {
                    return Ok(());
                }
                after = position.to_owned();
            }
            // A page ends at `min(head, after + limit)`; the journal is drained
            // only when a page makes no progress.
            let next = page["next"]
                .as_str()
                .context("arrival query omitted next cursor")?;
            if next != after && !self.checkpoint(binding, collection, next).await? {
                return Ok(());
            }
            if next == page_start {
                return Ok(());
            }
            after = next.to_owned();
        }
    }

    /// Grouped bindings admit their sealed groups; per-document bindings
    /// deliver from their arrival cursor instead.
    pub(super) async fn handle_created_doc(&mut self, collection: &str, doc_id: &str) {
        let bindings = match list_enabled_bindings(self.node.as_ref(), &self.agent_did).await {
            Ok(bindings) => bindings,
            Err(error) => {
                tracing::warn!(%error, "callback engine reload bindings failed");
                return;
            }
        };
        let mut all_settled = true;
        for binding in bindings {
            match self
                .materialize_for_binding(&binding, collection, doc_id, true)
                .await
            {
                Ok(_) => {}
                Err(error) => {
                    all_settled = false;
                    tracing::warn!(
                        binding_id = %binding.binding_id,
                        source_collection = %collection,
                        source_doc_id = %doc_id,
                        %error,
                        "callback invocation materialize failed"
                    );
                }
            }
        }
        if all_settled {
            self.mark_seen(collection, doc_id);
        }
    }

    /// Admits `doc_id` for `binding` when the binding's persisted source is
    /// `grouped` (or per-document) and both binding and callback are still
    /// enabled; the engine's binding snapshot may be stale.
    pub(super) async fn materialize_for_binding(
        &mut self,
        binding: &CallbackBindingDoc,
        collection: &str,
        doc_id: &str,
        grouped: bool,
    ) -> Result<bool> {
        let Some(binding) =
            load_binding(self.node.as_ref(), &binding.binding_id, &binding.agent_did).await?
        else {
            return Ok(false);
        };
        let binding = &binding;
        if !binding.enabled {
            return Ok(false);
        }
        if let Err(error) = validate_callback_binding(binding) {
            tracing::warn!(
                binding_id = %binding.binding_id,
                %error,
                "callback binding invalid at scan"
            );
            return Ok(false);
        }
        let event = load_event_source(
            self.node.as_ref(),
            &binding.event_source_id,
            &binding.agent_did,
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "CallbackBinding {} EventSource missing for owner",
                binding.binding_id
            )
        })?;
        if event.source_collection != collection || event.group.is_some() != grouped {
            return Ok(false);
        }
        anyhow::ensure!(
            event.event_kind.as_deref().unwrap_or("created") == "created",
            "callback EventSource requires supported created event kind"
        );

        super::documents::reject_secret_bearing_callback_fields(
            &binding.binding_id,
            event.filter.as_deref(),
            None,
        )?;
        let callback = load_callback(self.node.as_ref(), &binding.callback_id, &binding.agent_did)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Callback {} missing for owner", binding.callback_id))?;
        if !callback.enabled {
            return Ok(false);
        }
        let source_version = match probe_filter(
            self.node.as_ref(),
            collection,
            doc_id,
            event.filter.as_deref(),
        )
        .await
        {
            Ok(Some(version)) => version,
            Ok(None) => return Ok(false),
            Err(error) => return Err(error),
        };
        let delivery = Delivery::Callback {
            binding,
            source: &event,
        };
        let correlation = match delivery.correlation_field() {
            Some(field) => self.source_field(collection, doc_id, field).await?,
            None => None,
        };
        let (key, input, origin) = if event.group.is_some() {
            delivery.validate_group()?;
            let correlation =
                correlation.ok_or_else(|| anyhow::anyhow!("callback group correlation missing"))?;
            return self.materialize_group(binding, &event, &correlation).await;
        } else {
            let input = fetch_source_doc(
                self.node.as_ref(),
                &mut SourceSchemaCache::default(),
                collection,
                doc_id,
                Some(&source_version),
                binding,
            )
            .await?;
            (
                idempotency_key(&binding.binding_id, collection, doc_id),
                input,
                crate::document_config::CallbackInvocationOrigin::Event {
                    binding_id: binding.binding_id.clone(),
                    source_collection: collection.into(),
                    source_doc_id: doc_id.into(),
                    source_version: Some(source_version),
                },
            )
        };
        let invocation = CallbackInvocationDoc {
            invocation_id: uuid::Uuid::new_v4().to_string(),
            owner_agent_did: binding.agent_did.clone(),
            callback_id: binding.callback_id.clone(),
            input,
            origin,
            idempotency_key: key,
            caused_by_correlation: correlation,
            lifecycle_state: LIFECYCLE_PENDING.to_string(),
            attempts: Some(0),
            action_plan: None,
            action_journal: None,
            error: None,
            claimed_at: None,
            created_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        };
        self.publish_invocation(invocation, &callback).await
    }

    /// One string field of a source document.
    async fn source_field(
        &self,
        collection: &str,
        doc_id: &str,
        field: &str,
    ) -> Result<Option<String>> {
        crate::graphql::validate_collection_identifier(collection)?;
        crate::graphql::validate_graphql_name(field)?;
        let query = format!(
            "{{{collection}(filter:{{_docID:{{_eq:\"{}\"}}}},limit:1){{{field}}}}}",
            escape_graphql_string(doc_id)
        );
        let response = self.node.execute(&query).await;
        Ok(crate::graphql::rows::<Value>(&response, collection)?
            .first()
            .and_then(|row| row.get(field))
            .and_then(Value::as_str)
            .map(str::to_owned))
    }

    async fn publish_invocation(
        &self,
        invocation: CallbackInvocationDoc,
        callback: &crate::document_config::Callback,
    ) -> Result<bool> {
        let stored = create_pending_invocation(self.node.as_ref(), &invocation).await?;
        if invocation_is_claimable(&self.agent_did, &stored) {
            if let Err(error) = run_owned_invocation(
                self.node.as_ref(),
                &stored,
                callback,
                self.ceiling.as_deref(),
                &self.plugins,
            )
            .await
            {
                tracing::warn!(
                    invocation_id = %stored.invocation_id,
                    %error,
                    "callback invocation run failed"
                );
            }
        }
        Ok(true)
    }

    async fn materialize_group(
        &self,
        binding: &CallbackBindingDoc,
        source: &crate::document_config::EventSource,
        correlation: &str,
    ) -> Result<bool> {
        validate_callback_binding(binding)?;
        super::documents::reject_secret_bearing_callback_fields(
            &binding.binding_id,
            source.filter.as_deref(),
            None,
        )?;
        let callback = load_callback(&self.node, &binding.callback_id, &binding.agent_did)
            .await?
            .ok_or_else(|| anyhow::anyhow!("callback missing for owner"))?;
        if !binding.enabled || !callback.enabled {
            return Ok(false);
        }
        let delivery = Delivery::Callback { binding, source };
        let GroupOutcome::Ready { docs, .. } = event_delivery::evaluate_group(
            &self.node,
            &SourceSchemaCache::default(),
            delivery,
            correlation,
        )
        .await?
        else {
            return Ok(false);
        };
        let fields = binding.projected_fields()?;
        let available = SourceSchemaCache::default()
            .fields_for(&source.source_collection, &self.node)
            .await?;
        anyhow::ensure!(
            fields
                .iter()
                .all(|field| field == "_docID" || available.contains(field)),
            "callback input field is not a scalar source field"
        );
        let input = Value::Array(
            docs.into_iter()
                .map(|row| {
                    strip_secret_fields(Value::Object(
                        fields
                            .iter()
                            .filter_map(|field| {
                                row.get(field).cloned().map(|value| (field.clone(), value))
                            })
                            .collect(),
                    ))
                })
                .collect(),
        );
        let group_key = delivery.group_key(correlation);
        self.publish_invocation(
            CallbackInvocationDoc {
                invocation_id: uuid::Uuid::new_v4().to_string(),
                owner_agent_did: binding.agent_did.clone(),
                callback_id: binding.callback_id.clone(),
                input,
                origin: crate::document_config::CallbackInvocationOrigin::EventGroup {
                    binding_id: binding.binding_id.clone(),
                    group_key: group_key.clone(),
                },
                idempotency_key: group_key,
                caused_by_correlation: Some(correlation.to_owned()),
                lifecycle_state: LIFECYCLE_PENDING.into(),
                attempts: Some(0),
                action_plan: None,
                action_journal: None,
                error: None,
                claimed_at: None,
                created_at: Some(
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ),
            },
            &callback,
        )
        .await
    }

    async fn recover_group_page(&mut self) {
        let Ok(bindings) = list_enabled_bindings(&self.node, &self.agent_did).await else {
            return;
        };
        let mut grouped = Vec::new();
        for binding in bindings {
            match load_event_source(&self.node, &binding.event_source_id, &binding.agent_did).await
            {
                Ok(Some(source)) if source.group.is_some() => grouped.push((binding, source)),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error,"callback group recovery configuration unavailable")
                }
            }
        }
        grouped.sort_by(|(a, _), (b, _)| a.binding_id.cmp(&b.binding_id));
        let active = grouped
            .iter()
            .map(|(binding, source)| Delivery::Callback { binding, source }.consumer_key())
            .collect::<HashSet<_>>();
        self.group_page_cursors
            .retain(|key, _| active.contains(key));
        if grouped.is_empty() {
            return;
        }
        let index = self.group_recovery_cursor % grouped.len();
        self.group_recovery_cursor = (index + 1) % grouped.len();
        let (binding, source) = &grouped[index];
        let delivery = Delivery::Callback { binding, source };
        let key = delivery.consumer_key();
        match event_delivery::group_correlation_page(
            &self.node,
            delivery,
            self.group_page_cursors.get(&key).map(String::as_str),
        )
        .await
        {
            Ok((correlations, next, complete)) => {
                if complete {
                    self.group_page_cursors.remove(&key);
                } else if let Some(next) = next {
                    self.group_page_cursors.insert(key, next);
                }
                for correlation in correlations {
                    if self.cancel.is_cancelled() {
                        return;
                    }
                    if let Err(error) = self.materialize_group(binding, source, &correlation).await
                    {
                        tracing::warn!(%error,binding_id=%binding.binding_id,"callback group recovery failed");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error,binding_id=%binding.binding_id,"callback group recovery page failed")
            }
        }
    }

    async fn resolve_collection_name(&mut self, collection_id: &str) -> Option<String> {
        if let Some(name) = self.collection_id_to_name.get(collection_id) {
            return Some(name.clone());
        }
        let names = match self.node.list_collections() {
            Ok(names) => names,
            Err(error) => {
                tracing::warn!(%error, "callback engine failed to list collections");
                return None;
            }
        };
        for name in names {
            let def = match self.node.get_collection(&name) {
                Ok(Some(def)) => def,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(name = %name, %error, "callback engine collection lookup failed");
                    continue;
                }
            };
            self.collection_id_to_name
                .insert(def.collection_id.clone(), def.name.clone());
        }
        self.collection_id_to_name.get(collection_id).cloned()
    }
}

/// Each document's id and string `field`, for the same bounded page
/// [`load_doc_ids`] reads.
async fn load_doc_field(
    node: &EmbeddedNode,
    collection: &str,
    field: &str,
) -> Result<Vec<(String, String)>> {
    crate::graphql::validate_collection_identifier(collection)?;
    crate::graphql::validate_graphql_name(field)?;
    let query = format!(
        r#"query {{ {collection}(limit: {limit}) {{ _docID {field} }} }}"#,
        limit = SEEN_DOCS_SEED_LIMIT,
    );
    let response =
        crate::graphql::graphql_with_transaction_retry(node, &query, "callback.seed_correlations")
            .await?;
    Ok(crate::graphql::rows::<Value>(&response, collection)?
        .into_iter()
        .filter_map(|row| {
            Some((
                row.get("_docID")?.as_str()?.to_owned(),
                row.get(field)?.as_str()?.to_owned(),
            ))
        })
        .collect())
}

async fn load_doc_ids(node: &EmbeddedNode, collection: &str) -> Result<Vec<String>> {
    crate::graphql::validate_collection_identifier(collection)?;
    let query = format!(
        r#"query {{ {collection}(limit: {limit}) {{ _docID }} }}"#,
        limit = SEEN_DOCS_SEED_LIMIT,
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "callback source scan for {collection} failed: {:?}",
            response.errors
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            row.get("_docID")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect())
}

pub(super) async fn probe_filter(
    node: &EmbeddedNode,
    collection: &str,
    source_doc_id: &str,
    filter: Option<&str>,
) -> Result<Option<String>> {
    event_delivery::probe_document(node, collection, source_doc_id, filter).await
}

pub(super) async fn fetch_source_doc(
    node: &EmbeddedNode,
    cache: &mut SourceSchemaCache,
    collection: &str,
    source_doc_id: &str,
    expected_source_version: Option<&str>,
    binding: &CallbackBindingDoc,
) -> Result<Value> {
    let projected = binding.projected_fields()?;
    let available = cache.fields_for(collection, node).await?;
    for field in &projected {
        anyhow::ensure!(
            available.contains(field) || field == "_docID",
            "callback input field {field} is not a safe scalar source field"
        );
    }
    let fields = projected;
    let projection = fields.join("\n                    ");
    let query = format!(
        r#"query {{
            {collection}(filter: {{ _docID: {{ _eq: "{id}" }} }}, limit: 1) {{
                _docID
                _version {{ cid height fieldName }}
                {projection}
            }}
        }}"#,
        id = escape_graphql_string(source_doc_id),
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!("fetch callback source doc errors: {:?}", response.errors);
    }
    let Some(row) = response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
    else {
        anyhow::bail!("source doc {source_doc_id} not found in {collection}");
    };
    let actual = document_composite_version(&row, "fetch callback source")?.ok_or_else(|| {
        anyhow::anyhow!("callback source {source_doc_id} has no composite version")
    })?;
    if let Some(expected) = expected_source_version {
        if actual.cid != expected {
            anyhow::bail!(
                "callback source {source_doc_id} changed after invocation: expected version {expected}, found {}",
                actual.cid
            );
        }
    }
    let object = row
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("callback source is not an object"))?;
    let projected = fields
        .into_iter()
        .filter_map(|field| object.get(&field).cloned().map(|value| (field, value)))
        .collect();
    Ok(strip_secret_fields(Value::Object(projected)))
}

#[cfg(test)]
mod grouped_tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn group_invocation_freezes_projected_array_once_and_surfaces_planner_error() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        for sdl in [
            gents_protocol::schemas::CALLBACK,
            gents_protocol::schemas::CALLBACK_INVOCATION,
            gents_protocol::schemas::EVENT_GROUP_STATE,
        ] {
            node.add_schema(sdl).await.unwrap();
        }
        node.add_schema("type CallbackMember { batch:String value:String hidden:String }")
            .await
            .unwrap();
        let response=node.execute(r#"mutation {create_Callback(input:{agent_did:"owner",callback_id:"callback",enabled:true,handler:{kind:"built_in",emitter:"create_workspace"}}){_docID}}"#).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        for value in ["one", "two"] {
            let response=node.execute(&format!("mutation{{create_CallbackMember(input:{{batch:\"batch\",value:\"{value}\",hidden:\"not-selected\"}}){{_docID}}}}" )).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
        let binding:CallbackBindingDoc=serde_json::from_value(json!({"agent_did":"owner","binding_id":"binding","event_source_id":"source","callback_id":"callback","input_fields":["value"]})).unwrap();
        let source:crate::document_config::EventSource=serde_json::from_value(json!({"agent_did":"owner","event_source_id":"source","source_collection":"CallbackMember","correlation_field":"batch","group":{"expected_count":2}})).unwrap();
        let engine = CallbackEngine::new(
            node.clone(),
            "owner".into(),
            None,
            tokio_util::sync::CancellationToken::new(),
        );
        assert!(engine
            .materialize_group(&binding, &source, "batch")
            .await
            .unwrap());
        let query = "{CallbackInvocation {input origin idempotency_key lifecycle_state error}}";
        let response = node.execute(query).await;
        let rows = crate::graphql::rows::<Value>(&response, "CallbackInvocation").unwrap();
        assert_eq!(rows.len(), 1);
        let frozen =
            crate::callback::documents::callback_input_from_storage(rows[0]["input"].clone());
        let members = node
            .execute("{CallbackMember(order:{_docID:ASC}){value}}")
            .await;
        let ordered = crate::graphql::rows::<Value>(&members, "CallbackMember").unwrap();
        assert_eq!(
            frozen,
            Value::Array(ordered),
            "group projection must retain canonical member order"
        );
        assert_eq!(frozen.as_array().unwrap().len(), 2);
        assert!(frozen
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row.as_object().unwrap().len() == 1 && row.get("value").is_some()));
        assert_eq!(rows[0]["origin"]["kind"], "event_group");
        assert_eq!(rows[0]["lifecycle_state"], "denied");
        assert!(!rows[0]["error"].as_str().unwrap().is_empty());
        let response=node.execute(r#"mutation {update_CallbackMember(filter:{batch:{_eq:"batch"}},input:{value:"changed"}){_docID}}"#).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        assert!(engine
            .materialize_group(&binding, &source, "batch")
            .await
            .unwrap());
        let response = node.execute(query).await;
        let rows = crate::graphql::rows::<Value>(&response, "CallbackInvocation").unwrap();
        assert_eq!(
            rows.len(),
            1,
            "same group must not materialize a second invocation"
        );
        assert_eq!(
            crate::callback::documents::callback_input_from_storage(rows[0]["input"].clone()),
            frozen,
            "replay must preserve original projected input"
        );
        node.shutdown().await;
    }
}
