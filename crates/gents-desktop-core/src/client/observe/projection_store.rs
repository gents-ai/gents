use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use tokio::sync::watch;

use crate::client::store::{ClientStore, SharedClientStore};

#[derive(Debug, Default)]
pub struct ObserverMetrics {
    pub events_received: AtomicU64,
    pub docs_fetched: AtomicU64,
    pub debounce_flushes: AtomicU64,
    pub scope_reloads: AtomicU64,
    pub drop_recoveries: AtomicU64,
    pub local_write_redundant_fetches: AtomicU64,
    pub fetch_failures: AtomicU64,
    pub response_in_place_merges: AtomicU64,
    pub response_copy_on_write_merges: AtomicU64,
    pub transcript_invalidations: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct ObserverMetricsSnapshot {
    pub events_received: u64,
    pub docs_fetched: u64,
    pub debounce_flushes: u64,
    pub scope_reloads: u64,
    pub drop_recoveries: u64,
    pub local_write_redundant_fetches: u64,
    pub fetch_failures: u64,
    pub response_in_place_merges: u64,
    pub response_copy_on_write_merges: u64,
    pub transcript_invalidations: u64,
}

impl ObserverMetrics {
    pub fn snapshot(&self) -> ObserverMetricsSnapshot {
        ObserverMetricsSnapshot {
            events_received: self.events_received.load(Ordering::Relaxed),
            docs_fetched: self.docs_fetched.load(Ordering::Relaxed),
            debounce_flushes: self.debounce_flushes.load(Ordering::Relaxed),
            scope_reloads: self.scope_reloads.load(Ordering::Relaxed),
            drop_recoveries: self.drop_recoveries.load(Ordering::Relaxed),
            local_write_redundant_fetches: self
                .local_write_redundant_fetches
                .load(Ordering::Relaxed),
            fetch_failures: self.fetch_failures.load(Ordering::Relaxed),
            response_in_place_merges: self.response_in_place_merges.load(Ordering::Relaxed),
            response_copy_on_write_merges: self
                .response_copy_on_write_merges
                .load(Ordering::Relaxed),
            transcript_invalidations: self.transcript_invalidations.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreProjectionRevision {
    pub store_version: u64,
    pub reconcile_version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreUpdateNotice {
    pub revision: StoreProjectionRevision,
    /// True only when every database row merged by this publication belongs to
    /// AgentResponse. Consumers may use the live-tail projection in that case;
    /// every other publication requires an authoritative snapshot reconcile.
    pub response_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorePatchMergeOutcome {
    pub store_version: u64,
    pub response_only: bool,
    /// True when a reader still held the prior immutable snapshot and the hot
    /// response merge therefore had to preserve it with copy-on-write.
    pub copied_snapshot: bool,
}

struct ObservedState {
    snapshot: SharedClientStore,
    revision: StoreProjectionRevision,
}

pub struct ObservedStore {
    state: RwLock<ObservedState>,
    focused_request_id: RwLock<Option<String>>,
    version_tx: watch::Sender<u64>,
    change_tx: watch::Sender<StoreUpdateNotice>,
}

impl ObservedStore {
    pub fn new(initial_snapshot: ClientStore) -> (Arc<Self>, watch::Receiver<u64>) {
        let (version_tx, version_rx) = watch::channel(1_u64);
        let revision = StoreProjectionRevision {
            store_version: 1,
            reconcile_version: 1,
        };
        let (change_tx, _change_rx) = watch::channel(StoreUpdateNotice {
            revision,
            response_only: false,
        });
        let store = Arc::new(Self {
            state: RwLock::new(ObservedState {
                snapshot: Arc::new(initial_snapshot.into_observer_projection()),
                revision,
            }),
            focused_request_id: RwLock::new(None),
            version_tx,
            change_tx,
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
        let snapshot = snapshot.into_observer_projection();
        self.update(false, |_| snapshot)
    }

    pub fn merge_chat_patch(&self, patch: ClientStore) -> u64 {
        let patch = patch.into_observer_projection();
        self.update(false, |snapshot| snapshot.merge_chat_patch(patch))
    }

    pub fn merge_snapshot(&self, incoming: ClientStore) -> u64 {
        let incoming = incoming.into_observer_projection();
        self.merge_observer_patch(incoming, false)
    }

    pub fn merge_observer_patch(&self, incoming: ClientStore, response_only: bool) -> u64 {
        self.merge_observer_patch_with_outcome(incoming, response_only)
            .store_version
    }

    pub fn merge_observer_patch_with_outcome(
        &self,
        incoming: ClientStore,
        response_only: bool,
    ) -> StorePatchMergeOutcome {
        // Decide this before stripping transcript rows. A mislabeled message
        // patch becomes empty in the observer projection, but it is still a
        // structural invalidation and must advance the reconcile fence. A
        // genuinely empty response-only patch remains response-only.
        let is_response_only_patch = incoming.is_response_only_patch();
        let incoming = incoming.into_observer_projection();
        if response_only && is_response_only_patch {
            return self.update_in_place(true, |snapshot| {
                snapshot.merge_response_patch_in_place(incoming);
            });
        }
        StorePatchMergeOutcome {
            store_version: self.update(false, |snapshot| snapshot.merge_snapshot(incoming)),
            response_only: false,
            copied_snapshot: false,
        }
    }

    pub fn replace_agent_snapshot(&self, agent_did: &str, incoming: ClientStore) -> u64 {
        let incoming = incoming.into_observer_projection();
        self.update(false, |snapshot| {
            snapshot.replace_agent_scope(agent_did, incoming)
        })
    }

    /// Publish a structural database change without retaining its transcript
    /// payload in the process-wide observer. Consumers reconcile by issuing a
    /// bounded DefraDB projection for the selected session.
    pub fn invalidate_projection(&self) -> u64 {
        let notice = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            state.revision = StoreProjectionRevision {
                store_version: state.revision.store_version.saturating_add(1),
                reconcile_version: state.revision.reconcile_version.saturating_add(1),
            };
            StoreUpdateNotice {
                revision: state.revision,
                response_only: false,
            }
        };
        self.version_tx.send_replace(notice.revision.store_version);
        self.change_tx.send_replace(notice);
        notice.revision.store_version
    }

    fn update(
        &self,
        response_only: bool,
        transform: impl FnOnce(&ClientStore) -> ClientStore,
    ) -> u64 {
        let notice = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            let store_version = state.revision.store_version.saturating_add(1);
            let reconcile_version = if response_only {
                state.revision.reconcile_version
            } else {
                state.revision.reconcile_version.saturating_add(1)
            };
            state.snapshot = Arc::new(transform(state.snapshot.as_ref()));
            state.revision = StoreProjectionRevision {
                store_version,
                reconcile_version,
            };
            StoreUpdateNotice {
                revision: state.revision,
                response_only,
            }
        };
        self.version_tx.send_replace(notice.revision.store_version);
        self.change_tx.send_replace(notice);
        notice.revision.store_version
    }

    fn update_in_place(
        &self,
        response_only: bool,
        transform: impl FnOnce(&mut ClientStore),
    ) -> StorePatchMergeOutcome {
        let (notice, copied_snapshot) = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            let copied_snapshot = Arc::strong_count(&state.snapshot) > 1;
            transform(Arc::make_mut(&mut state.snapshot));
            let store_version = state.revision.store_version.saturating_add(1);
            let reconcile_version = if response_only {
                state.revision.reconcile_version
            } else {
                state.revision.reconcile_version.saturating_add(1)
            };
            state.revision = StoreProjectionRevision {
                store_version,
                reconcile_version,
            };
            (
                StoreUpdateNotice {
                    revision: state.revision,
                    response_only,
                },
                copied_snapshot,
            )
        };
        self.version_tx.send_replace(notice.revision.store_version);
        self.change_tx.send_replace(notice);
        StorePatchMergeOutcome {
            store_version: notice.revision.store_version,
            response_only,
            copied_snapshot,
        }
    }
}
