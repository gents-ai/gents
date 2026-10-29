use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;
use events::DocumentChangeSubscription;
use gents_protocol::schemas::AGENT_TOOL_CALL_NAME;
use tokio::sync::watch;
use tokio::time::Instant;

use super::collection_resolver::CollectionResolver;
use super::core::sync_state::ClientSyncStateOwner;
use super::query::{
    fetch_doc_patch, is_transcript_content_collection, load_full_snapshot_with_peer_records,
    load_node_scoped_snapshot_with_peer_records, supports_doc_patch_collection,
};
use gents_protocol::schemas::AGENT_REQUEST_NAME;

mod projection_store;
pub use projection_store::{
    ObservedStore, ObserverMetrics, ObserverMetricsSnapshot, StoreProjectionRevision,
    StoreUpdateNotice,
};

const FETCH_RETRY_DELAY: Duration = Duration::from_millis(10);
const RESYNC_RETRY_DELAY: Duration = Duration::from_millis(250);
/// Replace repeated patch failures with a scoped resync rather than keeping
/// the same documents dirty indefinitely.
const FETCH_RETRY_LIMIT: u32 = 3;
const SESSION_HYDRATION_REQUEST: &str = "SessionHydrationRequest";

fn resync_retry_ready(retry_not_before: Option<Instant>, now: Instant) -> bool {
    retry_not_before.is_none_or(|deadline| now >= deadline)
}

enum ObserverWake {
    Changes(events::DocumentChangeBatch),
    Retry,
    Resync,
}

pub struct ObserverHandle {
    stop_tx: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    metrics: Arc<ObserverMetrics>,
}

impl ObserverHandle {
    pub async fn shutdown(self) {
        let _ = self.stop_tx.send(true);
        let _ = self.task.await;
    }

    pub fn metrics_snapshot(&self) -> ObserverMetricsSnapshot {
        self.metrics.snapshot()
    }
}

pub fn spawn_observer_with_selection(
    node: Arc<EmbeddedNode>,
    store: Arc<ObservedStore>,
    configured_peers: ClientSyncStateOwner,
    requester_did: String,
    subscription: DocumentChangeSubscription,
    selected_node_did_rx: watch::Receiver<Option<String>>,
    identity_interaction: crate::local_runtime::OperatorIdentityInteraction,
) -> ObserverHandle {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let metrics = Arc::new(ObserverMetrics::default());
    let metrics_for_task = Arc::clone(&metrics);
    let resolver = Arc::new(CollectionResolver::new());
    let mut resync_requests = store.subscribe_resync_requests();

    let task = tokio::spawn(async move {
        let mut subscription = subscription;
        let mut dirty: RapidHashMap<&'static str, RapidHashSet<String>> = RapidHashMap::new();
        let mut redundant_fetches_pending: RapidHashMap<(String, String), u32> =
            RapidHashMap::new();
        let mut resync_pending = false;
        let mut resync_retry_not_before = None;

        loop {
            let wake = tokio::select! {
                changed = stop_rx.changed() => match changed {
                    Ok(()) if *stop_rx.borrow() => {
                        tracing::debug!("desktop observation requested shutdown");
                        break;
                    }
                    Ok(()) => continue,
                    Err(_) => break,
                },
                msg = subscription.recv() => match msg {
                    Some(msg) => ObserverWake::Changes(msg),
                    None => {
                        tracing::debug!("desktop observation subscription closed");
                        break;
                    }
                },
                changed = resync_requests.changed() => match changed {
                    Ok(()) => ObserverWake::Resync,
                    Err(_) => break,
                },
                _ = async {
                    match resync_retry_not_before {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => tokio::time::sleep(FETCH_RETRY_DELAY).await,
                    }
                }, if resync_pending || !dirty.is_empty() => {
                    ObserverWake::Retry
                }
            };
            if matches!(&wake, ObserverWake::Resync) {
                resync_pending = true;
            }
            if let ObserverWake::Changes(msg) = wake {
                metrics_for_task
                    .events_received
                    .fetch_add(msg.updates, Ordering::Relaxed);
                metrics_for_task
                    .document_change_batches
                    .fetch_add(1, Ordering::Relaxed);
                if !msg.resync_required {
                    metrics_for_task.coalesced_updates.fetch_add(
                        msg.updates.saturating_sub(msg.changes.len() as u64),
                        Ordering::Relaxed,
                    );
                }

                for update in &msg.changes {
                    accumulate_dirty(
                        &mut dirty,
                        resolver.as_ref(),
                        node.as_ref(),
                        &update.collection_id,
                        &update.doc_id,
                        !update.has_local_write,
                        metrics_for_task.as_ref(),
                        store.as_ref(),
                    )
                    .await;
                }

                if msg.resync_required {
                    tracing::warn!(
                        updates = msg.updates,
                        "desktop observation distinct-document capacity exceeded; performing scoped reload"
                    );
                    metrics_for_task
                        .drop_recoveries
                        .fetch_add(1, Ordering::Relaxed);
                    // Fence in-flight request reads: overflow may omit an
                    // AgentRequest event from this batch.
                    store.observe_request_event(None);
                    store.enqueue_resync_request(None);
                    resync_pending = true;
                }
            }
            if resync_pending && resync_retry_ready(resync_retry_not_before, Instant::now()) {
                resync_retry_not_before = None;
                let peers = configured_peers.records();
                let (reload_full, requested_scopes) = store.take_resync_requests();
                let scopes = if reload_full {
                    vec![None]
                } else if !requested_scopes.is_empty() {
                    requested_scopes.into_iter().map(Some).collect()
                } else {
                    Vec::new()
                };
                if reload_full {
                    dirty.clear();
                    redundant_fetches_pending.clear();
                }
                resync_pending = false;
                for (scope_index, scope) in scopes.iter().enumerate() {
                    let captured = store.capture_read();
                    let result = match scope.as_deref() {
                        Some(did) => {
                            load_node_scoped_snapshot_with_peer_records(
                                node.as_ref(),
                                did,
                                &peers,
                                &requester_did,
                                identity_interaction,
                            )
                            .await
                        }
                        None => {
                            load_full_snapshot_with_peer_records(
                                node.as_ref(),
                                &peers,
                                &requester_did,
                                identity_interaction,
                            )
                            .await
                        }
                    };
                    match result {
                        Ok(snapshot) => {
                            if !store.replace_reloaded_snapshot_at_read(
                                &captured,
                                scope.as_deref(),
                                snapshot,
                            ) {
                                for pending_scope in &scopes[scope_index..] {
                                    store.enqueue_resync_request(pending_scope.as_deref());
                                }
                                resync_pending = true;
                                resync_retry_not_before = Some(Instant::now() + RESYNC_RETRY_DELAY);
                                break;
                            }
                            metrics_for_task
                                .scope_reloads
                                .fetch_add(1, Ordering::Relaxed);
                            tracing::debug!(
                                node_did = ?scope,
                                "drop-recovery snapshot merged"
                            );
                        }
                        Err(err) => {
                            for pending_scope in &scopes[scope_index..] {
                                store.enqueue_resync_request(pending_scope.as_deref());
                            }
                            resync_pending = true;
                            resync_retry_not_before = Some(Instant::now() + RESYNC_RETRY_DELAY);
                            tracing::error!(error = %err, "drop-recovery snapshot failed");
                            break;
                        }
                    }
                }
                continue;
            }
            // While waiting for a failed authoritative reload's deadline, do
            // not start patch or reload I/O.
            if resync_pending {
                continue;
            }

            if dirty.is_empty() {
                continue;
            }
            metrics_for_task
                .debounce_flushes
                .fetch_add(1, Ordering::Relaxed);

            let mut flushed: RapidHashMap<&'static str, RapidHashSet<String>> = RapidHashMap::new();
            std::mem::swap(&mut flushed, &mut dirty);

            let transcript_collection_count = flushed
                .keys()
                .filter(|name| is_transcript_content_collection(name))
                .count();
            let transcript_changed_docs = flushed
                .iter()
                .filter(|(name, _)| is_transcript_content_collection(name))
                .map(|(_, doc_ids)| doc_ids.len())
                .sum::<usize>();
            let hydration_control_changed_docs = flushed
                .get(SESSION_HYDRATION_REQUEST)
                .map_or(0, RapidHashSet::len);
            if transcript_changed_docs > 0 || hydration_control_changed_docs > 0 {
                let store_version =
                    store.invalidate_projection(flushed.contains_key(AGENT_TOOL_CALL_NAME));
                if transcript_changed_docs > 0 {
                    metrics_for_task
                        .transcript_invalidations
                        .fetch_add(1, Ordering::Relaxed);
                }
                tracing::trace!(
                    changed_collections = transcript_collection_count,
                    transcript_changed_docs,
                    hydration_control_changed_docs,
                    store_version,
                    "published coalesced session projection invalidation"
                );
            }

            for (collection_name, doc_ids) in flushed {
                if is_transcript_content_collection(collection_name)
                    || collection_name == SESSION_HYDRATION_REQUEST
                {
                    continue;
                }
                let id_refs: Vec<&str> = doc_ids.iter().map(|s| s.as_str()).collect();
                let captured_snapshot = store.capture_read();
                match fetch_doc_patch(node.as_ref(), collection_name, &id_refs).await {
                    Ok(patch) => {
                        let row_count = patch.observed_documents;
                        // A scoped deletion reload cannot recover surviving rows
                        // for other nodes in this batch.
                        if patch.store.row_count() > 0
                            && store
                                .merge_observer_patch_at_read(&captured_snapshot, patch.store)
                                .is_none()
                        {
                            store.enqueue_resync_request(None);
                            resync_pending = true;
                            continue;
                        }
                        let reload_scopes = match patch.missing_doc_ids.as_deref() {
                            Some(missing_doc_ids) if !missing_doc_ids.is_empty() => {
                                let snapshot = store.snapshot();
                                let missing: RapidHashSet<&str> =
                                    missing_doc_ids.iter().map(String::as_str).collect();
                                let mut scopes = RapidHashSet::new();
                                let mut require_full = false;
                                for request in &snapshot.requests {
                                    if request
                                        .doc_id
                                        .as_deref()
                                        .is_some_and(|doc_id| missing.contains(doc_id))
                                    {
                                        if let Some(node_did) = request.node_did.as_ref() {
                                            scopes.insert(node_did.clone());
                                        } else {
                                            require_full = true;
                                        }
                                    }
                                }
                                if require_full {
                                    vec![None]
                                } else {
                                    let mut scopes: Vec<_> = scopes.into_iter().map(Some).collect();
                                    scopes.sort();
                                    scopes
                                }
                            }
                            Some(_) => Vec::new(),
                            None if row_count < id_refs.len() => {
                                vec![selected_node_did_rx.borrow().clone()]
                            }
                            None => Vec::new(),
                        };
                        for scope in reload_scopes {
                            store.request_resync(scope.as_deref());
                            resync_pending = true;
                        }
                        metrics_for_task
                            .docs_fetched
                            .fetch_add(row_count as u64, Ordering::Relaxed);
                        for id in &doc_ids {
                            redundant_fetches_pending
                                .remove(&(collection_name.to_string(), id.clone()));
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            collection = collection_name,
                            error = %err,
                            "fetch_doc_patch failed; will retry"
                        );
                        metrics_for_task
                            .fetch_failures
                            .fetch_add(1, Ordering::Relaxed);
                        for id in &doc_ids {
                            let key = (collection_name.to_string(), id.clone());
                            let count = redundant_fetches_pending.entry(key.clone()).or_insert(0);
                            *count += 1;
                            if *count >= FETCH_RETRY_LIMIT {
                                tracing::warn!(
                                    collection = collection_name,
                                    doc_id = %id,
                                    limit = FETCH_RETRY_LIMIT,
                                    "fetch_doc_patch retry limit reached; scheduling scoped resync"
                                );
                                redundant_fetches_pending.remove(&key);
                                store.enqueue_resync_request(None);
                                resync_pending = true;
                            } else {
                                dirty.entry(collection_name).or_default().insert(id.clone());
                            }
                        }
                    }
                }
            }
        }
    });

    ObserverHandle {
        stop_tx,
        task,
        metrics,
    }
}

/// Branchable collections emit a collection-head update with an empty document
/// ID; only the document update identifies a row to read.
#[expect(
    clippy::too_many_arguments,
    reason = "observer loop state borrowed from separate owners"
)]
async fn accumulate_dirty(
    dirty: &mut RapidHashMap<&'static str, RapidHashSet<String>>,
    resolver: &CollectionResolver,
    node: &EmbeddedNode,
    collection_id: &str,
    doc_id: &str,
    is_relay: bool,
    metrics: &ObserverMetrics,
    store: &ObservedStore,
) {
    if doc_id.is_empty() {
        return;
    }
    match resolver.resolve(node, collection_id).await {
        Ok(Some(name))
            if supports_doc_patch_collection(name) || name == SESSION_HYDRATION_REQUEST =>
        {
            // Fence prior request reads before this event can be delayed
            // behind the observer's retry timer.
            if name == AGENT_REQUEST_NAME {
                store.observe_request_event(Some(doc_id));
            }
            if !is_relay && supports_doc_patch_collection(name) {
                metrics
                    .local_write_redundant_fetches
                    .fetch_add(1, Ordering::Relaxed);
            }
            dirty.entry(name).or_default().insert(doc_id.to_string());
        }
        Ok(Some(name)) => {
            tracing::trace!(
                collection = name,
                "ignoring update outside the desktop snapshot store"
            );
        }
        Ok(None) => {
            tracing::trace!(collection_id, "ignoring update for unknown collection");
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                collection_id,
                "collection resolver failed"
            );
        }
    }
}
#[cfg(test)]
mod tests;
