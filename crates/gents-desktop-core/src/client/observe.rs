use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use gents::store::config_client::{ConfigAccess, TransactionFailure};
use tokio::sync::{mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};
use tokio::time::Instant;

use super::core::sync_state::ClientSyncStateOwner;
use super::query::load_node_snapshot_on;
use super::store::ClientStore;

mod delivery;
mod projection_store;
mod remote;
use projection_store::RuntimeConnectionEvent;
pub use projection_store::{
    ObservedStore, ObserverMetrics, ObserverMetricsSnapshot, RuntimeConnectionStatus,
    StoreProjectionRevision, StoreUpdateNotice,
};

/// The transport back-off before a failed node is loaded again.
const RESYNC_RETRY_DELAY: Duration = Duration::from_millis(250);

/// `ObserverResync.Settlement` of a finished snapshot load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settlement {
    Published,
    /// A publication into the node since the capture rejected the reload; the
    /// store already shows newer rows, so the next load needs no back-off.
    Superseded,
    TransportFailed,
    Denied,
}

impl Settlement {
    /// The connection event the settlement reports, if any.
    fn connection_event(self) -> Option<RuntimeConnectionEvent> {
        match self {
            Self::Published => Some(RuntimeConnectionEvent::SnapshotSucceeded),
            Self::Superseded => None,
            Self::TransportFailed => Some(RuntimeConnectionEvent::TransportFailed),
            Self::Denied => Some(RuntimeConnectionEvent::PermissionDenied),
        }
    }
}

/// The loop's per-node `ObserverResync.Slot` fields other than `loading`
/// (`Loads`) and `status` (the store's runtime connection).
#[derive(Default)]
struct Queue {
    pending: BTreeSet<String>,
    retry_at: BTreeMap<String, Instant>,
    /// The source generation whose every root is armed, per node. A replaced
    /// source has a new generation, so it is unarmed until its own notice.
    armed: BTreeMap<String, u64>,
}

/// A connection notice from the node's current source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceNotice {
    /// `ObserverResync.armed`.
    Armed,
    /// `ObserverResync.streamFailed`.
    StreamFailed,
    /// `ObserverResync.streamDenied`.
    Denied,
}

impl SourceNotice {
    fn connection_event(self) -> RuntimeConnectionEvent {
        match self {
            Self::Armed => RuntimeConnectionEvent::SubscriptionsReady,
            Self::StreamFailed => RuntimeConnectionEvent::TransportFailed,
            Self::Denied => RuntimeConnectionEvent::PermissionDenied,
        }
    }

    /// Whether the node's in-flight load is aborted.
    fn aborts_load(self) -> bool {
        !matches!(self, Self::Armed)
    }
}

impl Queue {
    /// Applies `notice` from `node`'s source at `generation`. A denial also
    /// clears the node's rows, as a denied load does.
    fn notice(&mut self, store: &ObservedStore, node: &str, generation: u64, notice: SourceNotice) {
        match notice {
            SourceNotice::Armed => {
                self.pending.insert(node.to_owned());
                self.armed.insert(node.to_owned(), generation);
            }
            SourceNotice::StreamFailed => {
                self.armed.remove(node);
            }
            SourceNotice::Denied => {
                clear_node(store, node);
                self.settle(node, Settlement::Denied, Instant::now());
            }
        }
    }

    /// `ObserverResync.startable` for `node`'s source at `generation`.
    fn startable(&self, node: &str, generation: u64, loading: bool) -> bool {
        self.pending.contains(node)
            && !self.retry_at.contains_key(node)
            && !loading
            && self.armed.get(node) == Some(&generation)
    }

    /// `ObserverResync.settle`.
    fn settle(&mut self, node: &str, settlement: Settlement, now: Instant) {
        match settlement {
            Settlement::Published => {}
            Settlement::Superseded => {
                self.pending.insert(node.to_owned());
            }
            Settlement::TransportFailed => {
                self.pending.insert(node.to_owned());
                self.retry_at.insert(node.to_owned(), now + RESYNC_RETRY_DELAY);
            }
            Settlement::Denied => {
                self.pending.remove(node);
                self.retry_at.remove(node);
                self.armed.remove(node);
            }
        }
    }

    /// A node no longer configured.
    fn forget(&mut self, node: &str) {
        self.pending.remove(node);
        self.retry_at.remove(node);
        self.armed.remove(node);
    }
}

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
    result: anyhow::Result<ClientStore>,
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
        let mut queue = Queue::default();
        sources.reconcile(
            &configured_peers.records(),
            &capability,
            &notices,
            &store,
            &metrics_for_task,
        );
        loop {
            let wake = queue
                .retry_at
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
                        queue.forget(removed);
                        store.set_runtime_connection(removed, None);
                    }
                    loads.cancel_stale(&sources);
                }
                notice = notice_rx.recv() => {
                    let Some(notice) = notice else { break; };
                    if let Some(generation) = sources.entries.get(&notice.node).filter(|source| source.generation == notice.generation && !source.denied).map(|source| source.generation) {
                        let kind = if notice.denied {
                            SourceNotice::Denied
                        } else if notice.ready {
                            SourceNotice::Armed
                        } else {
                            SourceNotice::StreamFailed
                        };
                        if kind.aborts_load() {
                            loads.cancel(&notice.node);
                        }
                        queue.notice(&store, &notice.node, generation, kind);
                        sources.observe(&notice.node, kind.connection_event(), &store, Instant::now());
                    }
                }
                changed = resync_requests.changed() => {
                    if changed.is_err() { break; }
                    let (full, scopes) = store.take_resync_requests();
                    if full { queue.pending.extend(sources.entries.keys().cloned()); }
                    queue.pending.extend(scopes);
                }
                Some(joined) = loads.tasks.join_next(), if !loads.tasks.is_empty() => {
                    let load = match joined {
                        Ok(load) => load,
                        Err(error) => {
                            if let Some(node) = loads.forget(error.id()) {
                                tracing::warn!(node_did = %node, error = %error, "runtime snapshot task failed");
                                queue.settle(&node, Settlement::TransportFailed, Instant::now());
                            }
                            continue;
                        }
                    };
                    if !loads.accept(&load) {
                        continue;
                    }
                    let Load { node, captured, result, .. } = load;
                    let settlement = match result {
                        Ok(snapshot) => {
                            let rows = snapshot.row_count();
                            if store.replace_reloaded_snapshot_at_read(&captured, Some(&node), snapshot) {
                                metrics_for_task.scope_reloads.fetch_add(1, Ordering::Relaxed);
                                metrics_for_task.docs_fetched.fetch_add(rows as u64, Ordering::Relaxed);
                                Settlement::Published
                            } else {
                                Settlement::Superseded
                            }
                        }
                        Err(error) => {
                            tracing::warn!(node_did = %node, error = %error, "runtime snapshot failed");
                            metrics_for_task.fetch_failures.fetch_add(1, Ordering::Relaxed);
                            if error.permission_denied() {
                                clear_node(&store, &node);
                                Settlement::Denied
                            } else {
                                Settlement::TransportFailed
                            }
                        }
                    };
                    if let Some(event) = settlement.connection_event() {
                        sources.observe(&node, event, &store, Instant::now());
                    }
                    queue.settle(&node, settlement, Instant::now());
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
            queue
                .retry_at
                .retain(|_, due| !resync_retry_ready(Some(*due), now));
            let runnable: Vec<_> = queue
                .pending
                .iter()
                .filter(|node| {
                    sources.entries.get(*node).is_some_and(|source| {
                        queue.startable(node, source.generation, loads.in_flight.contains_key(*node))
                    })
                })
                .cloned()
                .collect();
            for node in runnable {
                queue.pending.remove(&node);
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
