use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use tokio::sync::watch;

use crate::client::store::{ClientStore, SharedClientStore};

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
    /// Advances for observed request, session and tool-call lineage inputs, not
    /// transcript invalidations. It is a refresh cue, never an authority check.
    pub provenance_version: u64,
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
            provenance_version: 1,
        };
        let (change_tx, _change_rx) = watch::channel(StoreUpdateNotice { revision });
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
        self.update(ProvenanceUpdate::Invalidate, |_| snapshot)
    }

    pub fn merge_chat_patch(&self, patch: ClientStore) -> u64 {
        let patch = patch.into_observer_projection();
        self.update(provenance_update(&patch), |snapshot| {
            snapshot.merge_chat_patch(patch)
        })
    }

    pub fn merge_snapshot(&self, incoming: ClientStore) -> u64 {
        let incoming = incoming.into_observer_projection();
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
        let incoming = incoming.into_observer_projection();
        StorePatchMergeOutcome {
            store_version: self.update(provenance_update(&incoming), |snapshot| {
                snapshot.merge_snapshot(incoming)
            }),
        }
    }

    pub fn replace_node_snapshot(&self, node_did: &str, incoming: ClientStore) -> u64 {
        let incoming = incoming.into_observer_projection();
        self.update(ProvenanceUpdate::Invalidate, |snapshot| {
            snapshot.replace_node_scope(node_did, incoming)
        })
    }

    /// A reload may await database reads while an explicit request refresh
    /// publishes newer facts. Check its captured revision under the same lock
    /// as replacement so that older reads cannot erase those facts. An accepted
    /// reload also invalidates unretained tool-call lineage after lost events.
    pub(crate) fn replace_reloaded_snapshot(
        &self,
        captured: StoreProjectionRevision,
        node_did: Option<&str>,
        incoming: ClientStore,
    ) -> bool {
        let incoming = incoming.into_observer_projection();
        self.update_at_revision(Some(captured), ProvenanceUpdate::Invalidate, |snapshot| {
            match node_did {
                Some(node_did) => snapshot.replace_node_scope(node_did, incoming),
                None => incoming,
            }
        })
        .is_some()
    }

    /// Publish a structural database change without retaining its transcript
    /// payload in the process-wide observer. Consumers reconcile by issuing a
    /// bounded DefraDB projection for the selected session. Tool-call changes
    /// also invalidate lineage, although their payloads are never retained.
    pub fn invalidate_projection(&self, provenance_changed: bool) -> u64 {
        let notice = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            state.revision = StoreProjectionRevision {
                store_version: state.revision.store_version.saturating_add(1),
                provenance_version: state
                    .revision
                    .provenance_version
                    .saturating_add(u64::from(provenance_changed)),
            };
            StoreUpdateNotice {
                revision: state.revision,
            }
        };
        self.version_tx.send_replace(notice.revision.store_version);
        self.change_tx.send_replace(notice);
        notice.revision.store_version
    }

    fn update(
        &self,
        provenance: ProvenanceUpdate,
        transform: impl FnOnce(&ClientStore) -> ClientStore,
    ) -> u64 {
        self.update_at_revision(None, provenance, transform)
            .expect("unconditional store update")
    }

    fn update_at_revision(
        &self,
        captured: Option<StoreProjectionRevision>,
        provenance: ProvenanceUpdate,
        transform: impl FnOnce(&ClientStore) -> ClientStore,
    ) -> Option<u64> {
        let notice = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            if captured.is_some_and(|revision| revision != state.revision) {
                return None;
            }
            let store_version = state.revision.store_version.saturating_add(1);
            let next = transform(state.snapshot.as_ref());
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
            state.snapshot = Arc::new(next);
            state.revision = StoreProjectionRevision {
                store_version,
                provenance_version,
            };
            StoreUpdateNotice {
                revision: state.revision,
            }
        };
        self.version_tx.send_replace(notice.revision.store_version);
        self.change_tx.send_replace(notice);
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

/// These are observations used by the lineage read, including the remote
/// request's lifecycle. Lease renewal and streamed text cannot change them.
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
mod reload_tests {
    use super::*;
    use crate::client::store::ClientStoreRows;
    use gents_protocol::row::AgentRequestRow;

    fn requests(ids: &[&str]) -> ClientStore {
        ClientStore::from_rows(ClientStoreRows {
            requests: ids
                .iter()
                .map(|id| AgentRequestRow {
                    request_id: (*id).into(),
                    node_did: Some("did:test:reload".into()),
                    session_id: Some("session".into()),
                    purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        })
    }

    #[test]
    fn provenance_revision_ignores_transcript_and_lease_but_tracks_remote_completion() {
        use gents_protocol::request_lifecycle::RequestLifecycleState;

        let mut initial = requests(&["parent", "remote-child"]);
        initial.requests[0].doc_id = Some("parent-doc".into());
        initial.requests[1].node_did = Some("did:test:remote".into());
        initial.requests[1].caused_by_parent_request_doc_id = Some("parent-doc".into());
        initial.requests[1].lifecycle_state = Some(RequestLifecycleState::Processing);
        let (store, _) = ObservedStore::new(initial);
        let original = store.projection_revision();
        for _ in 0..50 {
            store.invalidate_projection(false);
        }
        assert_eq!(
            store.projection_revision().provenance_version,
            original.provenance_version
        );
        assert_eq!(
            store.projection_revision().store_version,
            original.store_version + 50
        );

        let mut renewed = store.snapshot().as_ref().clone();
        renewed.requests[1].execution_lease_expires_at = Some("later".into());
        store.merge_observer_patch(renewed);
        assert_eq!(
            store.projection_revision().provenance_version,
            original.provenance_version
        );

        let mut completed = store.snapshot().as_ref().clone();
        completed.requests[1].lifecycle_state = Some(RequestLifecycleState::Completed);
        store.merge_observer_patch(completed);
        assert_eq!(
            store.projection_revision().provenance_version,
            original.provenance_version + 1
        );
        let settled = store.projection_revision();
        store.merge_observer_patch(store.snapshot().as_ref().clone());
        assert_eq!(
            store.projection_revision().provenance_version,
            settled.provenance_version
        );
    }

    #[test]
    fn accepted_reload_refreshes_unretained_lineage_but_rejected_reload_does_not() {
        let (store, _) = ObservedStore::new(requests(&["parent"]));
        let captured = store.projection_revision();
        store.invalidate_projection(false);
        let current = store.projection_revision();
        assert!(!store.replace_reloaded_snapshot(captured, None, requests(&["parent"])));
        assert_eq!(store.projection_revision(), current);
        assert!(store.replace_reloaded_snapshot(current, None, requests(&["parent"])));
        assert_eq!(
            store.projection_revision().provenance_version,
            current.provenance_version + 1
        );
    }

    #[test]
    fn late_reload_cannot_erase_a_request_refreshed_after_capture() {
        for scope in [None, Some("did:test:reload")] {
            let (store, _) = ObservedStore::new(requests(&["accounted"]));
            let captured = store.projection_revision();
            let old_database_read = requests(&["accounted"]);
            store.merge_chat_patch(requests(&["pending"]));
            let refreshed = store.projection_revision();
            assert!(!store.replace_reloaded_snapshot(captured, scope, old_database_read));
            assert_eq!(store.projection_revision(), refreshed);
            assert!(store
                .snapshot()
                .requests
                .iter()
                .any(|row| row.request_id == "pending"));
            assert!(store.replace_reloaded_snapshot(refreshed, scope, requests(&["pending"])));
            assert_eq!(store.snapshot().requests.len(), 1);
            assert_eq!(store.snapshot().requests[0].request_id, "pending");
        }
    }
}
