use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};

use tokio::sync::watch;

use crate::client_projection::query::{replace_partial_read, CollectionRoot, PartialRead};
use crate::client_projection::store::{ClientStore, SharedClientStore};
use gents_protocol::session::SessionScope;

#[derive(Debug, Default)]
pub struct ObserverMetrics {
    pub events_received: AtomicU64,
    pub document_change_batches: AtomicU64,
    pub coalesced_updates: AtomicU64,
    pub docs_fetched: AtomicU64,
    pub debounce_flushes: AtomicU64,
    pub scope_reloads: AtomicU64,
    pub drop_recoveries: AtomicU64,
    pub local_write_redundant_fetches: AtomicU64,
    pub fetch_failures: AtomicU64,
    pub transcript_invalidations: AtomicU64,
    pub transport_failures: AtomicU64,
    pub subscription_renewals: AtomicU64,
    /// Sources ended and scheduled to reconnect.
    pub reconnects: AtomicU64,
    changed: tokio::sync::Notify,
}

#[derive(Debug, Clone)]
pub struct ObserverMetricsSnapshot {
    pub events_received: u64,
    pub document_change_batches: u64,
    pub coalesced_updates: u64,
    pub docs_fetched: u64,
    pub debounce_flushes: u64,
    pub scope_reloads: u64,
    pub drop_recoveries: u64,
    pub local_write_redundant_fetches: u64,
    pub fetch_failures: u64,
    pub transcript_invalidations: u64,
    pub transport_failures: u64,
    pub subscription_renewals: u64,
    pub reconnects: u64,
}

impl ObserverMetrics {
    pub fn snapshot(&self) -> ObserverMetricsSnapshot {
        ObserverMetricsSnapshot {
            events_received: self.events_received.load(Ordering::Relaxed),
            document_change_batches: self.document_change_batches.load(Ordering::Relaxed),
            coalesced_updates: self.coalesced_updates.load(Ordering::Relaxed),
            docs_fetched: self.docs_fetched.load(Ordering::Relaxed),
            debounce_flushes: self.debounce_flushes.load(Ordering::Relaxed),
            scope_reloads: self.scope_reloads.load(Ordering::Relaxed),
            drop_recoveries: self.drop_recoveries.load(Ordering::Relaxed),
            local_write_redundant_fetches: self
                .local_write_redundant_fetches
                .load(Ordering::Relaxed),
            fetch_failures: self.fetch_failures.load(Ordering::Relaxed),
            transcript_invalidations: self.transcript_invalidations.load(Ordering::Relaxed),
            transport_failures: self.transport_failures.load(Ordering::Relaxed),
            subscription_renewals: self.subscription_renewals.load(Ordering::Relaxed),
            reconnects: self.reconnects.load(Ordering::Relaxed),
        }
    }

    pub(super) fn notify_changed(&self) {
        self.changed.notify_waiters();
    }

    pub(super) fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.changed.notified()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreProjectionRevision {
    pub store_version: u64,
    /// Advances for request/session/tool-call lineage inputs, not transcript
    /// invalidations; this is the UI lineage-cache revision.
    pub provenance_version: u64,
    /// Advances for request events without changing the lineage-cache revision.
    pub request_event_version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreUpdateNotice {
    pub revision: StoreProjectionRevision,
}

enum ProvenanceUpdate {
    Compare,
    Invalidate,
}

/// `ClientDataPublication` epochs of one node's store: publications move
/// `data`, request events move `events`. A snapshot reload checks `data`
/// only; a patch checks both.
#[derive(Clone, Default)]
pub struct StoreReadFence {
    data: Arc<()>,
    events: Arc<()>,
}

impl StoreReadFence {
    /// `ClientDataPublication.accepts`.
    fn accepts(&self, captured: &Self) -> bool {
        self.accepts_reload(captured) && Arc::ptr_eq(&self.events, &captured.events)
    }

    /// `ClientDataPublication.acceptsReload`.
    fn accepts_reload(&self, captured: &Self) -> bool {
        Arc::ptr_eq(&self.data, &captured.data)
    }

    fn touch(&mut self) {
        self.data = Arc::new(());
    }

    fn touch_events(&mut self) {
        self.events = Arc::new(());
    }
}

/// `SessionRequestReload.derivesAlone`.
fn derives_alone(held_triggered: bool, read_triggered: bool) -> bool {
    !held_triggered && !read_triggered
}

/// Why a read did not publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rejected {
    /// A publication since the capture moved its fence.
    Superseded,
    /// A session scope it read does not derive alone.
    Deferred,
}

/// `ObserverResync.collectionSettlement` of a partial read that returned rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartialPublication {
    Published,
    Superseded,
    Deferred,
}

enum StoreMutation {
    /// A reload of the node, replacing every row its store holds.
    Replace(ClientStore),
    /// A partial read, replacing the rows of its collections and the
    /// requests of its session scopes alone.
    Partial(PartialRead, ClientStore),
    Chat(ClientStore),
}

impl StoreMutation {
    /// Whether a read captured at `captured` may still publish this mutation:
    /// a snapshot reload checks the publication epoch, a patch also checks
    /// the request-event epoch.
    fn accepts(&self, fences: &StoreReadFence, captured: &StoreReadFence) -> bool {
        match self {
            Self::Replace(_) | Self::Partial(..) => fences.accepts_reload(captured),
            Self::Chat(_) => fences.accepts(captured),
        }
    }

    fn provenance(&self) -> Option<ProvenanceUpdate> {
        match self {
            Self::Replace(_) => Some(ProvenanceUpdate::Invalidate),
            Self::Partial(read, _) => {
                (!read.scopes.is_empty()).then_some(ProvenanceUpdate::Compare)
            }
            Self::Chat(incoming) => provenance_update(incoming),
        }
    }

    /// Whether every session scope the mutation replaces derives alone
    /// against the rows it publishes into.
    fn derivable(&self, current: &ClientStore) -> bool {
        match self {
            Self::Partial(read, incoming) if !read.scopes.is_empty() => derives_alone(
                current.holds_triggered_request(&read.scopes),
                incoming
                    .requests
                    .iter()
                    .any(|row| row.caused_by_trigger_id.is_some()),
            ),
            _ => true,
        }
    }

    fn apply(self, current: &ClientStore) -> ClientStore {
        match self {
            Self::Replace(incoming) => incoming,
            Self::Partial(read, incoming) => replace_partial_read(current, &read, incoming),
            Self::Chat(incoming) => current.merge_chat_patch(incoming),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
    /// The runtime refused this client's credentials; its source keeps
    /// reconnecting on the backoff.
    Refused,
    /// The peer has no operator GraphQL endpoint to observe.
    NoEndpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeConnectionEvent {
    SourceStarted,
    SubscriptionsReady,
    SnapshotSucceeded,
    TransportFailed,
    /// The access owner refused the source's request or read; it reconnects
    /// and the status names the refusal.
    Refused,
    Renewed,
    EndpointMissing,
}

impl RuntimeConnectionEvent {
    /// `RuntimeConnectionObservation.observe`.
    fn observe(self, status: Option<RuntimeConnectionStatus>) -> Option<RuntimeConnectionStatus> {
        Some(match self {
            Self::SourceStarted => RuntimeConnectionStatus::Connecting,
            Self::SubscriptionsReady => match status {
                Some(RuntimeConnectionStatus::Refused) => RuntimeConnectionStatus::Refused,
                _ => RuntimeConnectionStatus::Connecting,
            },
            Self::Refused => RuntimeConnectionStatus::Refused,
            Self::SnapshotSucceeded => RuntimeConnectionStatus::Connected,
            Self::TransportFailed => RuntimeConnectionStatus::Disconnected,
            Self::Renewed => return status,
            Self::EndpointMissing => RuntimeConnectionStatus::NoEndpoint,
        })
    }
}

impl RuntimeConnectionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
            Self::Refused => "refused",
            Self::NoEndpoint => "noEndpoint",
        }
    }
}

struct ObservedState {
    snapshot: SharedClientStore,
    revision: StoreProjectionRevision,
    fences: StoreReadFence,
}

/// What every node's store shares: the fleet notice that any store, status or
/// membership changed, and the observer's queues of nodes and collections to
/// read again.
struct Fleet {
    version_tx: watch::Sender<u64>,
    resync_tx: watch::Sender<u64>,
    pending_resync: StdMutex<BTreeSet<String>>,
    pending_partial: StdMutex<BTreeMap<String, PartialRead>>,
}

impl Fleet {
    fn changed(&self) {
        self.version_tx
            .send_modify(|version| *version = version.saturating_add(1));
    }
}

/// Every node's own store, keyed by node DID. A node's rows are read and
/// observed only through that node's connection and published only into its
/// store (`ClientDataPublication.Fleet`); no store holds another node's rows.
pub struct ObservedStores {
    stores: RwLock<BTreeMap<String, Arc<ObservedStore>>>,
    runtime_connections: RwLock<BTreeMap<String, (RuntimeConnectionStatus, u64)>>,
    connection_epoch: AtomicU64,
    fleet: Arc<Fleet>,
}

impl ObservedStores {
    pub fn new() -> Arc<Self> {
        let (version_tx, _) = watch::channel(1_u64);
        let (resync_tx, _) = watch::channel(0_u64);
        Arc::new(Self {
            stores: RwLock::new(BTreeMap::new()),
            runtime_connections: RwLock::new(BTreeMap::new()),
            connection_epoch: AtomicU64::new(0),
            fleet: Arc::new(Fleet {
                version_tx,
                resync_tx,
                pending_resync: StdMutex::new(BTreeSet::new()),
                pending_partial: StdMutex::new(BTreeMap::new()),
            }),
        })
    }

    /// The node's store, if it is observed.
    pub fn store(&self, node_did: &str) -> Option<Arc<ObservedStore>> {
        self.stores
            .read()
            .expect("observed stores lock poisoned")
            .get(node_did)
            .cloned()
    }

    /// The node's store, created empty for a node not yet observed.
    pub fn ensure(&self, node_did: &str) -> Arc<ObservedStore> {
        if let Some(store) = self.store(node_did) {
            return store;
        }
        let store = {
            let mut stores = self.stores.write().expect("observed stores lock poisoned");
            stores
                .entry(node_did.to_owned())
                .or_insert_with(|| {
                    ObservedStore::create(node_did, ClientStore::default(), self.fleet.clone())
                })
                .clone()
        };
        self.fleet.changed();
        store
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn insert_for_test(&self, node_did: &str, initial: ClientStore) -> Arc<ObservedStore> {
        let store = ObservedStore::create(node_did, initial, self.fleet.clone());
        self.stores
            .write()
            .expect("observed stores lock poisoned")
            .insert(node_did.to_owned(), store.clone());
        store
    }

    /// Drops a node no longer configured, with its rows.
    pub(crate) fn remove(&self, node_did: &str) {
        let removed = self
            .stores
            .write()
            .expect("observed stores lock poisoned")
            .remove(node_did)
            .is_some();
        if removed {
            self.fleet.changed();
        }
    }

    /// The rows the node's store shows; empty for a node not observed.
    pub fn snapshot(&self, node_did: &str) -> SharedClientStore {
        self.store(node_did)
            .map(|store| store.snapshot())
            .unwrap_or_default()
    }

    /// [`Self::snapshot`] with the revision of the node's store; a node not
    /// observed shows no rows at the initial revision.
    pub fn snapshot_with_revision(
        &self,
        node_did: &str,
    ) -> (SharedClientStore, StoreProjectionRevision) {
        self.store(node_did)
            .map(|store| store.snapshot_with_revision())
            .unwrap_or_else(|| {
                (
                    SharedClientStore::default(),
                    StoreProjectionRevision {
                        store_version: 1,
                        provenance_version: 1,
                        request_event_version: 1,
                    },
                )
            })
    }

    pub fn node_dids(&self) -> Vec<String> {
        self.stores
            .read()
            .expect("observed stores lock poisoned")
            .keys()
            .cloned()
            .collect()
    }

    pub fn runtime_connection(&self, node_did: &str) -> Option<RuntimeConnectionStatus> {
        self.runtime_connection_observation(node_did)
            .map(|(status, _)| status)
    }

    pub fn runtime_connection_observation(
        &self,
        node_did: &str,
    ) -> Option<(RuntimeConnectionStatus, u64)> {
        self.runtime_connections
            .read()
            .expect("runtime connections lock poisoned")
            .get(node_did)
            .copied()
    }

    pub(crate) fn observe_runtime_connection(&self, node_did: &str, event: RuntimeConnectionEvent) {
        if let Some(status) = event.observe(self.runtime_connection(node_did)) {
            self.set_runtime_connection(node_did, Some(status));
        }
    }

    pub(crate) fn set_runtime_connection(
        &self,
        node_did: &str,
        status: Option<RuntimeConnectionStatus>,
    ) {
        let changed = {
            let mut connections = self
                .runtime_connections
                .write()
                .expect("runtime connections lock poisoned");
            match status {
                Some(status) => {
                    if connections
                        .get(node_did)
                        .is_some_and(|(previous, _)| *previous == status)
                    {
                        false
                    } else {
                        let previous = self
                            .connection_epoch
                            .try_update(Ordering::Relaxed, Ordering::Relaxed, |epoch| {
                                Some(epoch.saturating_add(1))
                            })
                            .expect("connection epoch always advances");
                        connections
                            .insert(node_did.to_owned(), (status, previous.saturating_add(1)));
                        true
                    }
                }
                None => connections.remove(node_did).is_some(),
            }
        };
        if changed {
            self.fleet.changed();
        }
    }

    /// Advances on every publication into any node's store and every change
    /// of the observed nodes or their connection status.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.fleet.version_tx.subscribe()
    }

    pub fn notify_client_change(&self) {
        self.fleet.changed();
    }

    pub fn version(&self) -> u64 {
        *self.fleet.version_tx.borrow()
    }

    pub(crate) fn subscribe_resync_requests(&self) -> watch::Receiver<u64> {
        self.fleet.resync_tx.subscribe()
    }

    /// The collections and session scopes each node's store asked to read
    /// alone.
    pub(crate) fn take_partial_requests(&self) -> BTreeMap<String, PartialRead> {
        std::mem::take(
            &mut *self
                .fleet
                .pending_partial
                .lock()
                .expect("pending partial reads lock poisoned"),
        )
    }

    /// The nodes whose stores asked to be read again, each once.
    pub(crate) fn take_resync_requests(&self) -> Vec<String> {
        std::mem::take(
            &mut *self
                .fleet
                .pending_resync
                .lock()
                .expect("pending resync lock poisoned"),
        )
        .into_iter()
        .collect()
    }
}

/// One node's observed rows.
pub struct ObservedStore {
    node_did: String,
    state: RwLock<ObservedState>,
    focused_request_id: RwLock<Option<String>>,
    version_tx: watch::Sender<u64>,
    change_tx: watch::Sender<StoreUpdateNotice>,
    fleet: Arc<Fleet>,
}

impl ObservedStore {
    fn create(node_did: &str, initial_snapshot: ClientStore, fleet: Arc<Fleet>) -> Arc<Self> {
        let revision = StoreProjectionRevision {
            store_version: 1,
            provenance_version: 1,
            request_event_version: 1,
        };
        let (version_tx, _) = watch::channel(1_u64);
        let (change_tx, _) = watch::channel(StoreUpdateNotice { revision });
        Arc::new(Self {
            node_did: node_did.to_owned(),
            state: RwLock::new(ObservedState {
                snapshot: Arc::new(initial_snapshot.into_observer_projection()),
                revision,
                fences: StoreReadFence::default(),
            }),
            focused_request_id: RwLock::new(None),
            version_tx,
            change_tx,
            fleet,
        })
    }

    pub fn node_did(&self) -> &str {
        &self.node_did
    }

    pub fn snapshot(&self) -> SharedClientStore {
        self.state
            .read()
            .expect("store snapshot lock poisoned")
            .snapshot
            .clone()
    }

    pub fn snapshot_with_revision(&self) -> (SharedClientStore, StoreProjectionRevision) {
        let state = self.state.read().expect("store snapshot lock poisoned");
        (state.snapshot.clone(), state.revision)
    }

    pub fn capture_read(&self) -> StoreReadFence {
        self.state
            .read()
            .expect("store snapshot lock poisoned")
            .fences
            .clone()
    }

    pub fn projection_revision(&self) -> StoreProjectionRevision {
        self.state
            .read()
            .expect("store snapshot lock poisoned")
            .revision
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version_tx.subscribe()
    }

    pub fn subscribe_changes(&self) -> watch::Receiver<StoreUpdateNotice> {
        self.change_tx.subscribe()
    }

    /// A refused read drops the node's rows, which this client may no longer
    /// read, and asks the observer for its own authenticated read, which owns
    /// the node's connection status.
    pub fn clear_after_refused_read(&self) {
        let _ = self.update_if(None, StoreMutation::Replace(ClientStore::default()));
        self.request_resync();
    }

    /// Queues this node for the observer's authoritative reload; requests
    /// coalesce into one load.
    pub fn request_resync(&self) {
        self.enqueue_resync_request();
        self.fleet.resync_tx.send_modify(|generation| {
            *generation = generation.saturating_add(1);
        });
    }

    /// Queues `root` of this node for the observer's collection reload
    /// (`ObserverResync.queueCollection`); requests coalesce into one read.
    pub(crate) fn request_collection_reload(&self, root: CollectionRoot) {
        self.request_partial_read(PartialRead {
            roots: BTreeSet::from([root]),
            ..Default::default()
        });
    }

    /// Queues the requests of `scopes` of this node for the observer's
    /// partial read (`ObserverResync.queueCollection`).
    pub(crate) fn request_session_reload(&self, scopes: BTreeSet<SessionScope>) {
        self.request_partial_read(PartialRead {
            scopes,
            ..Default::default()
        });
    }

    fn request_partial_read(&self, read: PartialRead) {
        self.fleet
            .pending_partial
            .lock()
            .expect("pending partial reads lock poisoned")
            .entry(self.node_did.clone())
            .or_default()
            .extend(read);
        self.fleet.resync_tx.send_modify(|generation| {
            *generation = generation.saturating_add(1);
        });
    }

    pub(crate) fn enqueue_resync_request(&self) {
        self.fleet
            .pending_resync
            .lock()
            .expect("pending resync lock poisoned")
            .insert(self.node_did.clone());
    }

    pub fn focused_request_id(&self) -> Option<String> {
        self.focused_request_id
            .read()
            .expect("focused request lock poisoned")
            .clone()
    }

    pub fn set_focused_request_id(&self, request_id: Option<String>) {
        *self
            .focused_request_id
            .write()
            .expect("focused request lock poisoned") = request_id;
    }

    pub fn replace_snapshot(&self, snapshot: ClientStore) -> u64 {
        self.update_if(
            None,
            StoreMutation::Replace(snapshot.into_observer_projection()),
        )
        .expect("unconditional store update")
    }

    pub fn merge_chat_patch(&self, patch: ClientStore) -> u64 {
        self.update_if(None, StoreMutation::Chat(patch.into_observer_projection()))
            .expect("unconditional store update")
    }

    pub fn merge_chat_patch_at_read(
        &self,
        captured: &StoreReadFence,
        patch: ClientStore,
    ) -> Option<u64> {
        self.update_if(
            Some(captured),
            StoreMutation::Chat(patch.into_observer_projection()),
        )
        .ok()
    }

    pub fn replace_reloaded_snapshot_at_read(
        &self,
        captured: &StoreReadFence,
        incoming: ClientStore,
    ) -> bool {
        self.update_if(
            Some(captured),
            StoreMutation::Replace(incoming.into_observer_projection()),
        )
        .is_ok()
    }

    /// Replaces the rows of `roots` with `incoming`'s through the reload fence
    /// (`ObserverResync.collectionSettlement`); false when a publication since
    /// `captured` rejected it.
    pub fn replace_collections_at_read(
        &self,
        captured: &StoreReadFence,
        roots: BTreeSet<CollectionRoot>,
        incoming: ClientStore,
    ) -> bool {
        self.publish_partial_at_read(
            captured,
            PartialRead {
                roots,
                ..Default::default()
            },
            incoming,
        ) == PartialPublication::Published
    }

    /// Publishes a partial read's rows (`ObserverResync.collectionSettlement`):
    /// nothing when a session scope it read does not derive alone against the
    /// store's rows, else through the reload fence.
    pub(crate) fn publish_partial_at_read(
        &self,
        captured: &StoreReadFence,
        read: PartialRead,
        incoming: ClientStore,
    ) -> PartialPublication {
        match self.update_if(
            Some(captured),
            StoreMutation::Partial(read, incoming.into_observer_projection()),
        ) {
            Ok(_) => PartialPublication::Published,
            Err(Rejected::Superseded) => PartialPublication::Superseded,
            Err(Rejected::Deferred) => PartialPublication::Deferred,
        }
    }

    /// Publish a structural database change without retaining its transcript
    /// payload; consumers reconcile with a bounded DefraDB projection for the
    /// selected session (`ClientDataPublication.transcriptInvalidation`). A
    /// tool-call change also invalidates lineage and rejects every patch read
    /// before it (`ClientDataPublication.provenanceInvalidation`).
    pub fn invalidate_projection(&self, provenance_changed: bool) -> u64 {
        let notice = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            if provenance_changed {
                state.fences.touch_events();
            }
            state.revision = StoreProjectionRevision {
                store_version: state.revision.store_version.saturating_add(1),
                provenance_version: state
                    .revision
                    .provenance_version
                    .saturating_add(u64::from(provenance_changed)),
                request_event_version: state
                    .revision
                    .request_event_version
                    .saturating_add(u64::from(provenance_changed)),
            };
            StoreUpdateNotice {
                revision: state.revision,
            }
        };
        self.publish_notice(notice);
        notice.revision.store_version
    }

    /// `ClientDataPublication.requestEvent`: a change delivered on this
    /// node's subscription, or a gap in it.
    pub fn observe_request_event(&self) -> u64 {
        let mut state = self.state.write().expect("store snapshot lock poisoned");
        state.fences.touch_events();
        state.revision.request_event_version =
            state.revision.request_event_version.saturating_add(1);
        state.revision.request_event_version
    }

    fn publish_notice(&self, notice: StoreUpdateNotice) {
        let mut published = self.version_tx.send_if_modified(|current| {
            if *current >= notice.revision.store_version {
                return false;
            }
            *current = notice.revision.store_version;
            true
        });
        published |= self.change_tx.send_if_modified(|current| {
            if current.revision.store_version >= notice.revision.store_version {
                return false;
            }
            *current = notice;
            true
        });
        if published {
            self.fleet.changed();
        }
    }

    #[cfg(test)]
    pub fn accepts_chat_patch_read(&self, captured: &StoreReadFence) -> bool {
        let state = self.state.read().expect("store snapshot lock poisoned");
        state.fences.accepts(captured)
    }

    /// Checks the publication fence and cached request evidence against the
    /// physical rows while holding the same store read lock. A cache hit is
    /// useful only if every request document it would return is still present
    /// with the exact row that was originally published.
    pub fn current_request_patch_read_status(
        &self,
        captured: &StoreReadFence,
        incoming: &ClientStore,
    ) -> (bool, bool) {
        let state = self.state.read().expect("store snapshot lock poisoned");
        let fence_accepted = state.fences.accepts(captured);
        let rows_current = !incoming.requests.is_empty()
            && incoming.requests.iter().all(|expected| {
                let Some(doc_id) = expected.doc_id.as_deref() else {
                    return false;
                };
                state.snapshot.request_row_by_doc_id(doc_id) == Some(expected)
            });
        (fence_accepted, rows_current)
    }

    fn update_if(
        &self,
        captured: Option<&StoreReadFence>,
        mutation: StoreMutation,
    ) -> Result<u64, Rejected> {
        let notice = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            if !mutation.derivable(state.snapshot.as_ref()) {
                return Err(Rejected::Deferred);
            }
            if captured.is_some_and(|captured| !mutation.accepts(&state.fences, captured)) {
                return Err(Rejected::Superseded);
            }
            let store_version = state.revision.store_version.saturating_add(1);
            let provenance = mutation.provenance();
            let next = mutation.apply(state.snapshot.as_ref());
            let provenance_version =
                state
                    .revision
                    .provenance_version
                    .saturating_add(u64::from(match provenance {
                        None => false,
                        Some(ProvenanceUpdate::Compare) => {
                            !same_provenance_inputs(state.snapshot.as_ref(), &next)
                        }
                        Some(ProvenanceUpdate::Invalidate) => true,
                    }));
            state.fences.touch();
            state.snapshot = Arc::new(next);
            state.revision = StoreProjectionRevision {
                store_version,
                provenance_version,
                request_event_version: state.revision.request_event_version,
            };
            StoreUpdateNotice {
                revision: state.revision,
            }
        };
        self.publish_notice(notice);
        Ok(notice.revision.store_version)
    }
}

fn provenance_update(patch: &ClientStore) -> Option<ProvenanceUpdate> {
    (!patch.requests.is_empty() || !patch.sessions.is_empty()).then_some(ProvenanceUpdate::Compare)
}

/// Lineage-read observations including the remote request lifecycle; lease
/// renewal and streamed text cannot change them.
fn same_provenance_inputs(before: &ClientStore, after: &ClientStore) -> bool {
    before.requests.len() == after.requests.len()
        && before.requests.iter().zip(&after.requests).all(|(a, b)| {
            a.doc_id == b.doc_id
                && a.request_id == b.request_id
                && a.purpose == b.purpose
                && a.node_did == b.node_did
                && a.session_id == b.session_id
                && a.requester_did == b.requester_did
                && a.lifecycle_state == b.lifecycle_state
                && a.created_at == b.created_at
                && a.caused_by_parent_request_doc_id == b.caused_by_parent_request_doc_id
                && a.caused_by_parent_tool_call_doc_id == b.caused_by_parent_tool_call_doc_id
        })
        && before.sessions.len() == after.sessions.len()
        && before.sessions.iter().zip(&after.sessions).all(|(a, b)| {
            a.node_did == b.node_did
                && a.session_id == b.session_id
                && a.requester_did == b.requester_did
                && a.provenance == b.provenance
        })
}

#[cfg(test)]
mod reload_tests;

#[cfg(test)]
mod conformance {
    #[test]
    fn derives_alone_matches_lean() {
        gents_lean_vocab::families::session_request_reload_derives_alone::replay(|i| {
            super::derives_alone(i.held_triggered, i.read_triggered)
        });
    }
}
