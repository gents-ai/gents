use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};

use tokio::sync::watch;

use crate::client::store::{ClientStore, SharedClientStore, StoreScope};

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
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorePatchMergeOutcome {
    pub store_version: u64,
}

enum ProvenanceUpdate {
    Preserve,
    Compare,
    Invalidate,
}

/// Captures publication ordering: unknown ownership invalidates every scope;
/// classified changes invalidate only their actual owners.
#[derive(Clone, Default)]
pub(crate) struct StoreReadFence {
    all: Arc<()>,
    shared: Arc<()>,
    nodes: BTreeMap<String, Arc<()>>,
}

impl StoreReadFence {
    fn accepts(&self, captured: &Self, scope: &StoreScope) -> bool {
        match scope {
            StoreScope::All => Arc::ptr_eq(&self.all, &captured.all),
            StoreScope::Nodes(nodes) => {
                Arc::ptr_eq(&self.shared, &captured.shared)
                    && nodes.iter().all(|node| {
                        match (self.nodes.get(node), captured.nodes.get(node)) {
                            (Some(current), Some(previous)) => Arc::ptr_eq(current, previous),
                            (None, None) => true,
                            _ => false,
                        }
                    })
            }
        }
    }

    fn touch(&mut self, scope: &StoreScope) {
        self.all = Arc::new(());
        match scope {
            StoreScope::All => self.shared = Arc::new(()),
            StoreScope::Nodes(nodes) => {
                for node in nodes {
                    self.nodes.insert(node.clone(), Arc::new(()));
                }
            }
        }
    }
}

enum StoreMutation {
    Replace(ClientStore),
    ReplaceNode(String, ClientStore),
    Chat(ClientStore),
    Observer(ClientStore),
}

impl StoreMutation {
    fn scope(&self, current: &ClientStore) -> StoreScope {
        match self {
            Self::Replace(_) => StoreScope::All,
            Self::ReplaceNode(node, incoming) => current.replacement_scope(node, incoming),
            Self::Chat(incoming) | Self::Observer(incoming) => current.merge_scope(incoming),
        }
    }

    fn provenance(&self) -> ProvenanceUpdate {
        match self {
            Self::Replace(_) | Self::ReplaceNode(_, _) => ProvenanceUpdate::Invalidate,
            Self::Chat(incoming) | Self::Observer(incoming) => provenance_update(incoming),
        }
    }

    fn apply(self, current: &ClientStore) -> ClientStore {
        match self {
            Self::Replace(incoming) => incoming,
            Self::ReplaceNode(node, incoming) => current.replace_node_scope(&node, incoming),
            Self::Chat(incoming) => current.merge_chat_patch(incoming),
            Self::Observer(incoming) => current.merge_snapshot(incoming),
        }
    }
}

struct ObservedState {
    snapshot: SharedClientStore,
    revision: StoreProjectionRevision,
    fences: StoreReadFence,
}

#[derive(Default)]
struct PendingResync {
    full: bool,
    node_dids: rapidhash::RapidHashSet<String>,
}

pub struct ObservedStore {
    state: RwLock<ObservedState>,
    focused_request_id: RwLock<Option<String>>,
    version_tx: watch::Sender<u64>,
    change_tx: watch::Sender<StoreUpdateNotice>,
    resync_tx: watch::Sender<u64>,
    pending_resync: StdMutex<PendingResync>,
}

impl ObservedStore {
    pub fn new(initial_snapshot: ClientStore) -> (Arc<Self>, watch::Receiver<u64>) {
        let (version_tx, version_rx) = watch::channel(1_u64);
        let revision = StoreProjectionRevision {
            store_version: 1,
            provenance_version: 1,
            request_event_version: 1,
        };
        let (change_tx, _change_rx) = watch::channel(StoreUpdateNotice { revision });
        let (resync_tx, _resync_rx) = watch::channel(0_u64);
        let store = Arc::new(Self {
            state: RwLock::new(ObservedState {
                snapshot: Arc::new(initial_snapshot.into_observer_projection()),
                revision,
                fences: StoreReadFence::default(),
            }),
            focused_request_id: RwLock::new(None),
            version_tx,
            change_tx,
            resync_tx,
            pending_resync: StdMutex::new(PendingResync::default()),
        });
        (store, version_rx)
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

    pub(crate) fn capture_read(&self) -> StoreReadFence {
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

    pub(crate) fn subscribe_resync_requests(&self) -> watch::Receiver<u64> {
        self.resync_tx.subscribe()
    }

    /// Coalesces stale async publishers into the observer's bounded
    /// authoritative reload loop.
    pub(crate) fn request_resync(&self, node_did: Option<&str>) {
        self.enqueue_resync_request(node_did);
        self.resync_tx.send_modify(|generation| {
            *generation = generation.saturating_add(1);
        });
    }

    pub(crate) fn enqueue_resync_request(&self, node_did: Option<&str>) {
        const MAX_PENDING_SCOPES: usize = 16;
        let mut pending = self
            .pending_resync
            .lock()
            .expect("pending resync lock poisoned");
        if pending.full {
        } else if let Some(node_did) = node_did {
            pending.node_dids.insert(node_did.to_owned());
            if pending.node_dids.len() > MAX_PENDING_SCOPES {
                pending.full = true;
                pending.node_dids.clear();
            }
        } else {
            pending.full = true;
            pending.node_dids.clear();
        }
    }

    pub(crate) fn take_resync_requests(&self) -> (bool, Vec<String>) {
        let mut pending = self
            .pending_resync
            .lock()
            .expect("pending resync lock poisoned");
        let full = std::mem::take(&mut pending.full);
        let mut node_dids: Vec<_> = std::mem::take(&mut pending.node_dids).into_iter().collect();
        node_dids.sort();
        (full, node_dids)
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

    pub(crate) fn merge_chat_patch_at_read(
        &self,
        captured: &StoreReadFence,
        patch: ClientStore,
    ) -> Option<u64> {
        self.update_if(
            Some(captured),
            StoreMutation::Chat(patch.into_observer_projection()),
        )
    }

    pub fn merge_snapshot(&self, incoming: ClientStore) -> u64 {
        self.merge_observer_patch(incoming)
    }

    pub fn merge_observer_patch(&self, incoming: ClientStore) -> u64 {
        self.merge_observer_patch_with_outcome(incoming)
            .store_version
    }

    pub fn merge_observer_patch_with_outcome(
        &self,
        incoming: ClientStore,
    ) -> StorePatchMergeOutcome {
        StorePatchMergeOutcome {
            store_version: self
                .update_if(
                    None,
                    StoreMutation::Observer(incoming.into_observer_projection()),
                )
                .expect("unconditional store update"),
        }
    }

    pub(crate) fn merge_observer_patch_at_read(
        &self,
        captured: &StoreReadFence,
        incoming: ClientStore,
    ) -> Option<StorePatchMergeOutcome> {
        self.update_if(
            Some(captured),
            StoreMutation::Observer(incoming.into_observer_projection()),
        )
        .map(|store_version| StorePatchMergeOutcome { store_version })
    }

    pub(crate) fn replace_reloaded_snapshot_at_read(
        &self,
        captured: &StoreReadFence,
        node_did: Option<&str>,
        incoming: ClientStore,
    ) -> bool {
        let incoming = incoming.into_observer_projection();
        let mutation = match node_did {
            Some(node) => StoreMutation::ReplaceNode(node.to_owned(), incoming),
            None => StoreMutation::Replace(incoming),
        };
        self.update_if(Some(captured), mutation).is_some()
    }

    /// Publish a structural database change without retaining its transcript
    /// payload; consumers reconcile with a bounded DefraDB projection for the
    /// selected session. Tool-call changes also invalidate lineage.
    pub fn invalidate_projection(&self, provenance_changed: bool) -> u64 {
        let notice = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            if provenance_changed {
                state.fences.touch(&StoreScope::All);
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

    pub(crate) fn observe_request_event(&self, doc_id: Option<&str>) -> u64 {
        let mut state = self.state.write().expect("store snapshot lock poisoned");
        let scope = state.snapshot.request_event_scope(doc_id);
        state.fences.touch(&scope);
        state.revision.request_event_version =
            state.revision.request_event_version.saturating_add(1);
        state.revision.request_event_version
    }

    fn publish_notice(&self, notice: StoreUpdateNotice) {
        self.version_tx.send_if_modified(|current| {
            if *current >= notice.revision.store_version {
                return false;
            }
            *current = notice.revision.store_version;
            true
        });
        self.change_tx.send_if_modified(|current| {
            if current.revision.store_version >= notice.revision.store_version {
                return false;
            }
            *current = notice;
            true
        });
    }

    #[cfg(test)]
    pub(crate) fn accepts_chat_patch_read(
        &self,
        captured: &StoreReadFence,
        incoming: &ClientStore,
    ) -> bool {
        let state = self.state.read().expect("store snapshot lock poisoned");
        state
            .fences
            .accepts(captured, &state.snapshot.merge_scope(incoming))
    }

    /// Checks the publication fence and cached request evidence against the
    /// physical rows while holding the same store read lock. A cache hit is
    /// useful only if every request document it would return is still present
    /// with the exact row that was originally published.
    pub(crate) fn current_request_patch_read_status(
        &self,
        captured: &StoreReadFence,
        incoming: &ClientStore,
    ) -> (bool, bool) {
        let state = self.state.read().expect("store snapshot lock poisoned");
        let scope = state.snapshot.merge_scope(incoming);
        let fence_accepted = state.fences.accepts(captured, &scope);
        let rows_current = !incoming.requests.is_empty()
            && incoming.requests.iter().all(|expected| {
                let Some(doc_id) = expected.doc_id.as_deref() else {
                    return false;
                };
                state.snapshot.request_row_by_doc_id(doc_id) == Some(expected)
            });
        (fence_accepted, rows_current)
    }

    fn update_if(&self, captured: Option<&StoreReadFence>, mutation: StoreMutation) -> Option<u64> {
        let notice = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            let scope = mutation.scope(&state.snapshot);
            if captured.is_some_and(|captured| !state.fences.accepts(captured, &scope)) {
                return None;
            }
            let store_version = state.revision.store_version.saturating_add(1);
            let provenance = mutation.provenance();
            let next = mutation.apply(state.snapshot.as_ref());
            let provenance_version =
                state
                    .revision
                    .provenance_version
                    .saturating_add(u64::from(match provenance {
                        ProvenanceUpdate::Preserve => false,
                        ProvenanceUpdate::Compare => {
                            !same_provenance_inputs(state.snapshot.as_ref(), &next)
                        }
                        ProvenanceUpdate::Invalidate => true,
                    }));
            state.fences.touch(&scope);
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
        Some(notice.revision.store_version)
    }
}

fn provenance_update(patch: &ClientStore) -> ProvenanceUpdate {
    if patch.requests.is_empty() && patch.sessions.is_empty() {
        ProvenanceUpdate::Preserve
    } else {
        ProvenanceUpdate::Compare
    }
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
