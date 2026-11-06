use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use defra_node::Node as EmbeddedNode;
use events::DocumentChangeSubscription;
use gents::store::config_client::ConfigAccess;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::core::sync_state::ClientSyncStateOwner;
use super::query::load_node_snapshot_on;

mod projection_store;
mod remote;
use projection_store::RuntimeConnectionEvent;
pub use projection_store::{
    ObservedStore, ObserverMetrics, ObserverMetricsSnapshot, RuntimeConnectionStatus,
    StoreProjectionRevision, StoreUpdateNotice,
};

const RESYNC_RETRY_DELAY: Duration = Duration::from_millis(250);

fn resync_retry_ready(retry_not_before: Option<Instant>, now: Instant) -> bool {
    retry_not_before.is_none_or(|deadline| now >= deadline)
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
    _node: Arc<EmbeddedNode>,
    store: Arc<ObservedStore>,
    configured_peers: ClientSyncStateOwner,
    capability: gents::store::config_client::ClientSigningCapability,
    _subscription: DocumentChangeSubscription,
    _selected_node_did_rx: watch::Receiver<Option<String>>,
    identity_interaction: crate::local_runtime::OperatorIdentityInteraction,
) -> ObserverHandle {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let metrics = Arc::new(ObserverMetrics::default());
    let metrics_for_task = metrics.clone();
    let mut resync_requests = store.subscribe_resync_requests();
    let mut peer_updates = configured_peers.subscribe();
    let (notices, mut notice_rx) = mpsc::unbounded_channel();

    let task = tokio::spawn(async move {
        let mut sources = remote::Sources::default();
        let mut pending = BTreeSet::<String>::new();
        let mut retry_not_before = None;
        sources.reconcile(
            &configured_peers.records(),
            &capability,
            identity_interaction,
            &notices,
            &store,
            &metrics_for_task,
        );
        loop {
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() { break; }
                }
                changed = peer_updates.changed() => {
                    if changed.is_err() { break; }
                    let previous: Vec<_> = sources.entries.keys().cloned().collect();
                    sources.reconcile(&configured_peers.records(), &capability,
                        identity_interaction, &notices, &store, &metrics_for_task);
                    for removed in previous.iter().filter(|node| !sources.entries.contains_key(*node)) {
                        let captured = store.capture_read();
                        store.replace_reloaded_snapshot_at_read(&captured, Some(removed), Default::default());
                        pending.remove(removed);
                        store.set_runtime_connection(removed, None);
                    }
                }
                notice = notice_rx.recv() => {
                    let Some(notice) = notice else { break; };
                    if let Some(source) = sources.entries.get_mut(&notice.node).filter(|source| source.generation == notice.generation && !source.denied) {
                        source.ready = notice.ready;
                        source.denied = notice.denied;
                        let event = if notice.denied {
                            RuntimeConnectionEvent::PermissionDenied
                        } else if notice.ready {
                            RuntimeConnectionEvent::SubscriptionsReady
                        } else {
                            RuntimeConnectionEvent::TransportFailed
                        };
                        store.observe_runtime_connection(&notice.node, event);
                        if notice.denied {
                            let captured = store.capture_read();
                            store.replace_reloaded_snapshot_at_read(&captured, Some(&notice.node), Default::default());
                            pending.remove(&notice.node);
                        } else if notice.ready { pending.insert(notice.node); }
                    }
                }
                changed = resync_requests.changed() => {
                    if changed.is_err() { break; }
                    for node in store.take_permission_denials() {
                        if let Some(source) = sources.entries.get_mut(&node) {
                            source.stop();
                        }
                        store.observe_runtime_connection(&node, RuntimeConnectionEvent::PermissionDenied);
                        pending.remove(&node);
                    }
                    let (full, scopes) = store.take_resync_requests();
                    if full { pending.extend(sources.entries.keys().cloned()); }
                    pending.extend(scopes);
                }
                _ = async {
                    if let Some(deadline) = retry_not_before {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if retry_not_before.is_some() => {}
            }
            if !resync_retry_ready(retry_not_before, Instant::now()) {
                continue;
            }
            retry_not_before = None;
            let runnable: Vec<_> = pending
                .iter()
                .filter(|node| {
                    sources
                        .entries
                        .get(*node)
                        .is_some_and(|source| source.ready)
                        && store.runtime_connection(node) != Some(RuntimeConnectionStatus::Denied)
                })
                .cloned()
                .collect();
            for node in runnable {
                let Some(source) = sources.entries.get(&node) else {
                    continue;
                };
                let access = ConfigAccess::Graphql(source.endpoint.clone());
                let captured = store.capture_read();
                match load_node_snapshot_on(&access, &node).await {
                    Ok(snapshot) => {
                        let rows = snapshot.row_count();
                        if store.replace_reloaded_snapshot_at_read(&captured, Some(&node), snapshot)
                        {
                            pending.remove(&node);
                            store.observe_runtime_connection(
                                &node,
                                RuntimeConnectionEvent::SnapshotSucceeded,
                            );
                            metrics_for_task
                                .scope_reloads
                                .fetch_add(1, Ordering::Relaxed);
                            metrics_for_task
                                .docs_fetched
                                .fetch_add(rows as u64, Ordering::Relaxed);
                        } else {
                            retry_not_before = Some(Instant::now() + RESYNC_RETRY_DELAY);
                        }
                    }
                    Err(error) => {
                        tracing::warn!(node_did = %node, error = %error, "runtime snapshot failed");
                        metrics_for_task
                            .fetch_failures
                            .fetch_add(1, Ordering::Relaxed);
                        let denied = remote::permission_denied(&error);
                        store.observe_runtime_connection(
                            &node,
                            if denied {
                                RuntimeConnectionEvent::PermissionDenied
                            } else {
                                RuntimeConnectionEvent::TransportFailed
                            },
                        );
                        if denied {
                            if let Some(source) = sources.entries.get_mut(&node) {
                                source.stop();
                            }
                            let captured = store.capture_read();
                            store.replace_reloaded_snapshot_at_read(
                                &captured,
                                Some(&node),
                                Default::default(),
                            );
                            pending.remove(&node);
                        } else {
                            retry_not_before = Some(Instant::now() + RESYNC_RETRY_DELAY);
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

#[cfg(test)]
mod tests;
