use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use defra_node::EmbeddedNode;
use events::Subscription;
use tokio::sync::{watch, RwLock as AsyncRwLock};

use super::collection_resolver::CollectionResolver;
use super::peer_directory::PeerDirectory;
use super::query::{
    fetch_doc_patch, is_transcript_content_collection, isolate_legacy_bearer_rows,
    load_agent_scoped_snapshot_with_peer_records, load_full_snapshot_with_peer_records,
    supports_doc_patch_collection,
};
use super::store::ClientStore;

mod projection_store;
pub use projection_store::{
    ObservedStore, ObserverMetrics, ObserverMetricsSnapshot, StoreProjectionRevision,
    StoreUpdateNotice,
};

const OBSERVER_DEBOUNCE: Duration = Duration::from_millis(150);
const FETCH_RETRY_LIMIT: u32 = 3;

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
    peer_directory: Arc<AsyncRwLock<PeerDirectory>>,
    requester_did: String,
    subscription: Subscription,
    selected_agent_did_rx: watch::Receiver<Option<String>>,
) -> ObserverHandle {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let metrics = Arc::new(ObserverMetrics::default());
    let metrics_for_task = Arc::clone(&metrics);
    let resolver = Arc::new(CollectionResolver::new());

    let task = tokio::spawn(async move {
        let mut subscription = subscription;
        let mut dirty: HashMap<&'static str, HashSet<String>> = HashMap::new();
        let mut redundant_fetches_pending: HashMap<(String, String), u32> = HashMap::new();

        loop {
            let next = tokio::select! {
                changed = stop_rx.changed() => match changed {
                    Ok(()) if *stop_rx.borrow() => {
                        tracing::debug!("desktop observation requested shutdown");
                        break;
                    }
                    Ok(()) => continue,
                    Err(_) => break,
                },
                msg = subscription.recv() => msg,
            };
            let Some(msg) = next else {
                tracing::debug!("desktop observation subscription closed");
                break;
            };
            metrics_for_task
                .events_received
                .fetch_add(1, Ordering::Relaxed);

            if let Some(update) = msg.as_update() {
                accumulate_dirty(
                    &mut dirty,
                    resolver.as_ref(),
                    node.as_ref(),
                    &update.collection_id,
                    &update.doc_id,
                    update.is_relay,
                    metrics_for_task.as_ref(),
                )
                .await;
            }

            tokio::time::sleep(OBSERVER_DEBOUNCE).await;

            while let Ok(msg) = subscription.try_recv() {
                metrics_for_task
                    .events_received
                    .fetch_add(1, Ordering::Relaxed);
                if let Some(update) = msg.as_update() {
                    accumulate_dirty(
                        &mut dirty,
                        resolver.as_ref(),
                        node.as_ref(),
                        &update.collection_id,
                        &update.doc_id,
                        update.is_relay,
                        metrics_for_task.as_ref(),
                    )
                    .await;
                }
            }

            let dropped = subscription.check_and_reset_dropped();
            if dropped > 0 {
                tracing::warn!(
                    dropped,
                    "desktop observation subscription dropped messages; performing scoped reload"
                );
                metrics_for_task
                    .drop_recoveries
                    .fetch_add(1, Ordering::Relaxed);
                dirty.clear();
                redundant_fetches_pending.clear();

                let scope = selected_agent_did_rx.borrow().clone();
                let peers = peer_directory.read().await.records().to_vec();
                let selected_is_legacy_remote = scope.as_deref().is_some_and(|did| {
                    peers.iter().any(|peer| {
                        peer.agent_did == did
                            && peer
                                .graphql
                                .as_deref()
                                .is_some_and(|value| !value.trim().is_empty())
                    })
                });
                let result = match scope {
                    _ if selected_is_legacy_remote => {
                        load_full_snapshot_with_peer_records(node.as_ref(), &peers, &requester_did)
                            .await
                    }
                    Some(ref did) => {
                        load_agent_scoped_snapshot_with_peer_records(
                            node.as_ref(),
                            did,
                            &peers,
                            &requester_did,
                        )
                        .await
                    }
                    None => {
                        load_full_snapshot_with_peer_records(node.as_ref(), &peers, &requester_did)
                            .await
                    }
                };
                match result {
                    Ok(snapshot) => {
                        match (selected_is_legacy_remote, scope.as_deref()) {
                            (true, _) => store.replace_snapshot(snapshot),
                            (false, Some(did)) => store.replace_agent_snapshot(did, snapshot),
                            (false, None) => store.replace_snapshot(snapshot),
                        };
                        metrics_for_task
                            .scope_reloads
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::debug!(
                            agent_did = ?scope,
                            "drop-recovery snapshot merged"
                        );
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "drop-recovery snapshot failed");
                    }
                }
                continue;
            }

            if dirty.is_empty() {
                continue;
            }
            metrics_for_task
                .debounce_flushes
                .fetch_add(1, Ordering::Relaxed);

            let mut flushed: HashMap<&'static str, HashSet<String>> = HashMap::new();
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
            if transcript_changed_docs > 0 {
                let store_version = store.invalidate_projection();
                metrics_for_task
                    .transcript_invalidations
                    .fetch_add(1, Ordering::Relaxed);
                tracing::trace!(
                    changed_collections = transcript_collection_count,
                    changed_docs = transcript_changed_docs,
                    store_version,
                    "published coalesced transcript projection invalidation"
                );
            }

            for (collection_name, doc_ids) in flushed {
                if is_transcript_content_collection(collection_name) {
                    continue;
                }
                let id_refs: Vec<&str> = doc_ids.iter().map(|s| s.as_str()).collect();
                match fetch_doc_patch(node.as_ref(), collection_name, &id_refs).await {
                    Ok(patch) => {
                        let row_count = patch.row_count();
                        if row_count == 0 {
                            // An empty doc-id patch is authoritative delete
                            // evidence. Reload and replace the selected scope so
                            // the removed row cannot remain visible until restart.
                            let scope = selected_agent_did_rx.borrow().clone();
                            let peers = peer_directory.read().await.records().to_vec();
                            let selected_is_legacy_remote = scope.as_deref().is_some_and(|did| {
                                peers.iter().any(|peer| {
                                    peer.agent_did == did
                                        && peer
                                            .graphql
                                            .as_deref()
                                            .is_some_and(|value| !value.trim().is_empty())
                                })
                            });
                            let reload = match scope.as_deref() {
                                _ if selected_is_legacy_remote => {
                                    load_full_snapshot_with_peer_records(
                                        node.as_ref(),
                                        &peers,
                                        &requester_did,
                                    )
                                    .await
                                }
                                Some(did) => {
                                    load_agent_scoped_snapshot_with_peer_records(
                                        node.as_ref(),
                                        did,
                                        &peers,
                                        &requester_did,
                                    )
                                    .await
                                }
                                None => {
                                    load_full_snapshot_with_peer_records(
                                        node.as_ref(),
                                        &peers,
                                        &requester_did,
                                    )
                                    .await
                                }
                            };
                            match reload {
                                Ok(snapshot) => match (selected_is_legacy_remote, scope.as_deref())
                                {
                                    (true, _) => {
                                        store.replace_snapshot(snapshot);
                                    }
                                    (false, Some(did)) => {
                                        store.replace_agent_snapshot(did, snapshot);
                                    }
                                    (false, None) => {
                                        store.replace_snapshot(snapshot);
                                    }
                                },
                                Err(error) => {
                                    tracing::warn!(
                                        collection = collection_name,
                                        error = %error,
                                        "authoritative delete reload failed"
                                    );
                                }
                            }
                        } else {
                            let mut rows = patch.to_rows();
                            let peers = peer_directory.read().await.records().to_vec();
                            isolate_legacy_bearer_rows(&mut rows, &peers, &requester_did);
                            let response_only = collection_name == "AgentResponse";
                            let outcome = store.merge_observer_patch_with_outcome(
                                ClientStore::from_rows(rows),
                                response_only,
                            );
                            if outcome.response_only {
                                let counter = if outcome.copied_snapshot {
                                    &metrics_for_task.response_copy_on_write_merges
                                } else {
                                    &metrics_for_task.response_in_place_merges
                                };
                                counter.fetch_add(1, Ordering::Relaxed);
                                tracing::trace!(
                                    store_version = outcome.store_version,
                                    copied_snapshot = outcome.copied_snapshot,
                                    "merged response-only desktop observer patch"
                                );
                            }
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
                                    "fetch_doc_patch failed too many times; dropping"
                                );
                                redundant_fetches_pending.remove(&key);
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

async fn accumulate_dirty(
    dirty: &mut HashMap<&'static str, HashSet<String>>,
    resolver: &CollectionResolver,
    node: &EmbeddedNode,
    collection_id: &str,
    doc_id: &str,
    is_relay: bool,
    metrics: &ObserverMetrics,
) {
    match resolver.resolve(node, collection_id).await {
        Ok(Some(name)) if supports_doc_patch_collection(name) => {
            if !is_relay {
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
