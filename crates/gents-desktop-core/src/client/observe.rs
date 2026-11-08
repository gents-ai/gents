use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use gents::store::config_client::ConfigAccess;
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::Instant;

use super::core::sync_state::ClientSyncStateOwner;
use super::query::load_node_snapshot_on;
use super::store::ClientStore;

mod projection_store;
mod remote;
use projection_store::RuntimeConnectionEvent;
pub use projection_store::{
    ObservedStore, ObserverMetrics, ObserverMetricsSnapshot, RuntimeConnectionStatus,
    StoreProjectionRevision, StoreUpdateNotice,
};

/// `RuntimeConnectionObservation.snapshotRetryDelayMs`.
const SNAPSHOT_RETRY_DELAY: Duration = Duration::from_millis(250);

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

struct Load {
    node: String,
    id: u64,
    captured: projection_store::StoreReadFence,
    result: Result<ClientStore, super::query::SnapshotReadError>,
}

struct InFlight {
    id: u64,
    source_generation: u64,
    abort: AbortHandle,
}

/// Each runtime's snapshot runs as its own task so one stalled runtime delays
/// neither another runtime nor shutdown. At most one load per node is in
/// flight; a load whose source was replaced, removed or denied is aborted and
/// its result, if it already finished, is dropped by id.
#[derive(Default)]
struct Loads {
    tasks: JoinSet<Load>,
    in_flight: BTreeMap<String, InFlight>,
    next_id: u64,
}

impl Loads {
    fn start(&mut self, node: String, source: &remote::Source, store: &ObservedStore) {
        self.next_id += 1;
        let id = self.next_id;
        let access = ConfigAccess::Graphql(source.endpoint.clone());
        let captured = store.capture_read();
        let task_node = node.clone();
        let abort = self.tasks.spawn(async move {
            let result = load_node_snapshot_on(&access, &task_node).await;
            Load {
                node: task_node,
                id,
                captured,
                result,
            }
        });
        self.in_flight.insert(
            node,
            InFlight {
                id,
                source_generation: source.generation,
                abort,
            },
        );
    }

    fn cancel(&mut self, node: &str) {
        if let Some(load) = self.in_flight.remove(node) {
            load.abort.abort();
        }
    }

    fn cancel_stale(&mut self, sources: &remote::Sources) {
        let stale: Vec<_> = self
            .in_flight
            .iter()
            .filter(|(node, load)| {
                !sources.entries.get(*node).is_some_and(|source| {
                    source.generation == load.source_generation && !source.denied
                })
            })
            .map(|(node, _)| node.clone())
            .collect();
        for node in stale {
            self.cancel(&node);
        }
    }

    /// The finished load if it is still the node's current one.
    fn accept(&mut self, load: &Load) -> bool {
        if self
            .in_flight
            .get(&load.node)
            .is_some_and(|current| current.id == load.id)
        {
            self.in_flight.remove(&load.node);
            true
        } else {
            false
        }
    }

    fn forget(&mut self, task: tokio::task::Id) -> Option<String> {
        let node = self
            .in_flight
            .iter()
            .find(|(_, load)| load.abort.id() == task)
            .map(|(node, _)| node.clone())?;
        self.in_flight.remove(&node);
        Some(node)
    }
}

fn clear_node(store: &ObservedStore, node: &str) {
    let captured = store.capture_read();
    store.replace_reloaded_snapshot_at_read(&captured, Some(node), Default::default());
}

pub fn spawn_observer(
    store: Arc<ObservedStore>,
    configured_peers: ClientSyncStateOwner,
    capability: gents::store::config_client::ClientSigningCapability,
) -> ObserverHandle {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let metrics = Arc::new(ObserverMetrics::default());
    let metrics_for_task = metrics.clone();
    let mut resync_requests = store.subscribe_resync_requests();
    let mut peer_updates = configured_peers.subscribe();
    let (notices, mut notice_rx) = mpsc::unbounded_channel();

    let task = tokio::spawn(async move {
        let mut sources = remote::Sources::default();
        let mut loads = Loads::default();
        let mut pending = BTreeSet::<String>::new();
        let mut retry_at = BTreeMap::<String, Instant>::new();
        sources.reconcile(
            &configured_peers.records(),
            &capability,
            &notices,
            &store,
            &metrics_for_task,
        );
        loop {
            let wake = retry_at
                .values()
                .copied()
                .chain(sources.next_reprobe())
                .min();
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() { break; }
                }
                changed = peer_updates.changed() => {
                    if changed.is_err() { break; }
                    let previous: Vec<_> = sources.entries.keys().cloned().collect();
                    sources.reconcile(&configured_peers.records(), &capability,
                        &notices, &store, &metrics_for_task);
                    for removed in previous.iter().filter(|node| !sources.entries.contains_key(*node)) {
                        clear_node(&store, removed);
                        pending.remove(removed);
                        retry_at.remove(removed);
                    }
                    loads.cancel_stale(&sources);
                }
                notice = notice_rx.recv() => {
                    let Some(notice) = notice else { break; };
                    if let Some(source) = sources.entries.get_mut(&notice.node).filter(|source| source.generation == notice.generation && !source.denied) {
                        let event = notice.event;
                        let armed = remote::armed_after(source.ready, event);
                        source.ready = armed;
                        match event {
                            RuntimeConnectionEvent::PermissionDenied => {
                                loads.cancel(&notice.node);
                                clear_node(&store, &notice.node);
                                pending.remove(&notice.node);
                                retry_at.remove(&notice.node);
                            }
                            RuntimeConnectionEvent::TransportFailed => loads.cancel(&notice.node),
                            _ => {}
                        }
                        sources.observe(&notice.node, event, &store, Instant::now(), &metrics_for_task);
                        if armed {
                            pending.insert(notice.node);
                        }
                    }
                }
                changed = resync_requests.changed() => {
                    if changed.is_err() { break; }
                    let (full, scopes) = store.take_resync_requests();
                    if full { pending.extend(sources.entries.keys().cloned()); }
                    pending.extend(scopes);
                }
                Some(joined) = loads.tasks.join_next(), if !loads.tasks.is_empty() => {
                    let load = match joined {
                        Ok(load) => load,
                        Err(error) => {
                            if let Some(node) = loads.forget(error.id()) {
                                tracing::warn!(node_did = %node, error = %error, "runtime snapshot task failed");
                                pending.insert(node.clone());
                                retry_at.insert(node, Instant::now() + SNAPSHOT_RETRY_DELAY);
                            }
                            continue;
                        }
                    };
                    if !loads.accept(&load) {
                        continue;
                    }
                    let Load { node, captured, result, .. } = load;
                    match result {
                        Ok(snapshot) => {
                            let rows = snapshot.row_count();
                            if store.replace_reloaded_snapshot_at_read(&captured, Some(&node), snapshot) {
                                let event = remote::snapshot_event(remote::SnapshotOutcome::Succeeded);
                                sources.observe(&node, event, &store, Instant::now(), &metrics_for_task);
                                metrics_for_task.scope_reloads.fetch_add(1, Ordering::Relaxed);
                                metrics_for_task.docs_fetched.fetch_add(rows as u64, Ordering::Relaxed);
                            } else {
                                pending.insert(node.clone());
                                retry_at.insert(node, Instant::now() + SNAPSHOT_RETRY_DELAY);
                            }
                        }
                        Err(error) => {
                            tracing::warn!(node_did = %node, error = %error, "runtime snapshot failed");
                            metrics_for_task.fetch_failures.fetch_add(1, Ordering::Relaxed);
                            let event = remote::snapshot_event(remote::snapshot_outcome(&error));
                            if matches!(event, RuntimeConnectionEvent::PermissionDenied) {
                                clear_node(&store, &node);
                                sources.observe(&node, event, &store, Instant::now(), &metrics_for_task);
                                pending.remove(&node);
                                retry_at.remove(&node);
                            } else {
                                sources.observe(&node, event, &store, Instant::now(), &metrics_for_task);
                                pending.insert(node.clone());
                                retry_at.insert(node, Instant::now() + SNAPSHOT_RETRY_DELAY);
                            }
                        }
                    }
                }
                _ = async {
                    if let Some(deadline) = wake {
                        tokio::time::sleep_until(deadline).await;
                    }
                }, if wake.is_some() => {}
            }
            let now = Instant::now();
            if sources.take_due_reprobes(now) {
                sources.reconcile(
                    &configured_peers.records(),
                    &capability,
                    &notices,
                    &store,
                    &metrics_for_task,
                );
            }
            retry_at.retain(|_, due| !resync_retry_ready(Some(*due), now));
            let runnable: Vec<_> = pending
                .iter()
                .filter(|node| {
                    !retry_at.contains_key(*node)
                        && !loads.in_flight.contains_key(*node)
                        && sources
                            .entries
                            .get(*node)
                            .is_some_and(|source| source.ready)
                })
                .cloned()
                .collect();
            for node in runnable {
                pending.remove(&node);
                if let Some(source) = sources.entries.get(&node) {
                    loads.start(node, source, &store);
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
