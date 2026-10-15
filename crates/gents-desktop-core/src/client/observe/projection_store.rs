use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};

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
    /// transcript invalidations. This is the UI lineage-cache revision.
    pub provenance_version: u64,
    /// Paired with snapshot identity to fence in-flight publications. Advances
    /// for AgentRequest events and lineage-affecting invalidations without
    /// changing the UI lineage-cache revision for request-only events.
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

struct ObservedState {
    snapshot: SharedClientStore,
    revision: StoreProjectionRevision,
    global_epoch: u64,
    node_epochs: HashMap<String, u64>,
}

pub(crate) struct ReloadRead {
    snapshot: SharedClientStore,
    request_event_version: u64,
    node_did: Option<String>,
    global_epoch: u64,
    node_epoch: u64,
}

impl ObservedState {
    fn invalidate_scopes(&mut self, owners: Option<HashSet<String>>) {
        match owners {
            Some(owners) => {
                for owner in owners {
                    let epoch = self.node_epochs.entry(owner).or_default();
                    *epoch = epoch.saturating_add(1);
                }
            }
            None => self.global_epoch = self.global_epoch.saturating_add(1),
        }
    }
}

#[derive(Default)]
struct PendingResync {
    full: bool,
    node_dids: std::collections::HashSet<String>,
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
                global_epoch: 0,
                node_epochs: HashMap::new(),
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

    pub(crate) fn begin_reload(&self, node_did: Option<&str>) -> ReloadRead {
        let state = self.state.read().expect("store snapshot lock poisoned");
        ReloadRead {
            snapshot: state.snapshot.clone(),
            request_event_version: state.revision.request_event_version,
            node_did: node_did.map(str::to_owned),
            global_epoch: state.global_epoch,
            node_epoch: node_did
                .and_then(|did| state.node_epochs.get(did))
                .copied()
                .unwrap_or_default(),
        }
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

    /// Coalesces stale async publishers into the observer's existing bounded
    /// authoritative reload loop; callers never start their own retry.
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
            // A full reload already subsumes this request.
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
        let snapshot = snapshot.into_observer_projection();
        self.update(None, ProvenanceUpdate::Invalidate, |_| snapshot)
    }

    pub fn merge_chat_patch(&self, patch: ClientStore) -> u64 {
        let patch = patch.into_observer_projection();
        self.update(
            patch.publication_owners(),
            provenance_update(&patch),
            |snapshot| snapshot.merge_chat_patch(patch),
        )
    }

    /// Publishes a patch read only while the data snapshot and lineage token it
    /// observed are still current. Transcript-only invalidations retain both,
    /// so streaming text cannot starve ordinary request reads.
    pub(crate) fn merge_chat_patch_at_snapshot(
        &self,
        captured_snapshot: &SharedClientStore,
        captured_request_event_version: u64,
        patch: ClientStore,
    ) -> Option<u64> {
        let patch = patch.into_observer_projection();
        self.update_at_snapshot(
            captured_snapshot,
            captured_request_event_version,
            patch.publication_owners(),
            provenance_update(&patch),
            |snapshot| snapshot.merge_chat_patch(patch),
        )
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
            store_version: self.update(
                incoming.publication_owners(),
                provenance_update(&incoming),
                |snapshot| snapshot.merge_snapshot(incoming),
            ),
        }
    }

    pub(crate) fn merge_observer_patch_at_snapshot(
        &self,
        captured_snapshot: &SharedClientStore,
        captured_request_event_version: u64,
        incoming: ClientStore,
    ) -> Option<StorePatchMergeOutcome> {
        let incoming = incoming.into_observer_projection();
        self.update_at_snapshot(
            captured_snapshot,
            captured_request_event_version,
            incoming.publication_owners(),
            provenance_update(&incoming),
            |snapshot| snapshot.merge_snapshot(incoming),
        )
        .map(|store_version| StorePatchMergeOutcome { store_version })
    }

    /// Scoped reads depend on their node and unknown changes. Whole-store
    /// reads retain the global data and request-event fence.
    pub(crate) fn publish_reload(&self, read: &ReloadRead, incoming: ClientStore) -> bool {
        let incoming = incoming.into_observer_projection();
        self.update_if(
            |state| match read.node_did.as_deref() {
                Some(node_did) => {
                    state.global_epoch == read.global_epoch
                        && state.node_epochs.get(node_did).copied().unwrap_or_default()
                            == read.node_epoch
                }
                None => {
                    Arc::ptr_eq(&read.snapshot, &state.snapshot)
                        && read.request_event_version == state.revision.request_event_version
                }
            },
            read.node_did
                .as_ref()
                .map(|node| HashSet::from([node.clone()])),
            ProvenanceUpdate::Invalidate,
            |snapshot| match read.node_did.as_deref() {
                Some(node_did) => snapshot.replace_node_scope(node_did, incoming),
                None => incoming,
            },
        )
        .is_some()
    }

    /// Publish a structural database change without retaining its transcript
    /// payload in the process-wide observer. Consumers reconcile by issuing a
    /// bounded DefraDB projection for the selected session. Tool-call changes
    /// also invalidate lineage, although their payloads are never retained.
    pub fn invalidate_projection(&self, provenance_changed: bool) -> u64 {
        let notice = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            if provenance_changed {
                state.invalidate_scopes(None);
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
        self.version_tx.send_replace(notice.revision.store_version);
        self.change_tx.send_replace(notice);
        notice.revision.store_version
    }

    /// Advances the request-read fence without invalidating lineage caches.
    pub(crate) fn observe_request_event(&self) -> u64 {
        let mut state = self.state.write().expect("store snapshot lock poisoned");
        state.invalidate_scopes(None);
        state.revision.request_event_version =
            state.revision.request_event_version.saturating_add(1);
        state.revision.request_event_version
    }

    pub(crate) fn observe_request_document_event(&self, doc_id: &str) {
        let mut state = self.state.write().expect("store snapshot lock poisoned");
        let owner = state
            .snapshot
            .requests
            .iter()
            .find(|row| row.doc_id.as_deref() == Some(doc_id))
            .and_then(|row| row.node_did.as_deref())
            .filter(|owner| !owner.is_empty())
            .map(|owner| HashSet::from([owner.to_owned()]));
        state.invalidate_scopes(owner);
        state.revision.request_event_version =
            state.revision.request_event_version.saturating_add(1);
    }

    fn update(
        &self,
        owners: Option<HashSet<String>>,
        provenance: ProvenanceUpdate,
        transform: impl FnOnce(&ClientStore) -> ClientStore,
    ) -> u64 {
        self.update_if(|_| true, owners, provenance, transform)
            .expect("unconditional store update")
    }

    fn update_at_snapshot(
        &self,
        captured_snapshot: &SharedClientStore,
        captured_request_event_version: u64,
        owners: Option<HashSet<String>>,
        provenance: ProvenanceUpdate,
        transform: impl FnOnce(&ClientStore) -> ClientStore,
    ) -> Option<u64> {
        self.update_if(
            |state| {
                Arc::ptr_eq(captured_snapshot, &state.snapshot)
                    && captured_request_event_version == state.revision.request_event_version
            },
            owners,
            provenance,
            transform,
        )
    }

    fn update_if(
        &self,
        accepts: impl FnOnce(&ObservedState) -> bool,
        owners: Option<HashSet<String>>,
        provenance: ProvenanceUpdate,
        transform: impl FnOnce(&ClientStore) -> ClientStore,
    ) -> Option<u64> {
        let notice = {
            let mut state = self.state.write().expect("store snapshot lock poisoned");
            if !accepts(&state) {
                return None;
            }
            let store_version = state.revision.store_version.saturating_add(1);
            let next = transform(state.snapshot.as_ref());
            state.invalidate_scopes(owners);
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
                request_event_version: state.revision.request_event_version,
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
        let read = store.begin_reload(None);
        store.invalidate_projection(true);
        let current = store.projection_revision();
        assert!(!store.publish_reload(&read, requests(&["parent"]),));
        assert_eq!(store.projection_revision(), current);
        let current_read = store.begin_reload(None);
        assert!(store.publish_reload(&current_read, requests(&["parent"]),));
        assert_eq!(
            store.projection_revision().provenance_version,
            current.provenance_version + 1
        );
    }

    #[test]
    fn late_reload_cannot_erase_a_request_refreshed_after_capture() {
        for scope in [None, Some("did:test:reload")] {
            let (store, _) = ObservedStore::new(requests(&["accounted"]));
            let read = store.begin_reload(scope);
            let old_database_read = requests(&["accounted"]);
            store.merge_chat_patch(requests(&["pending"]));
            let refreshed = store.projection_revision();
            assert!(!store.publish_reload(&read, old_database_read,));
            assert_eq!(store.projection_revision(), refreshed);
            assert!(store
                .snapshot()
                .requests
                .iter()
                .any(|row| row.request_id == "pending"));
            let refreshed_read = store.begin_reload(scope);
            assert!(store.publish_reload(&refreshed_read, requests(&["pending"]),));
            assert_eq!(store.snapshot().requests.len(), 1);
            assert_eq!(store.snapshot().requests[0].request_id, "pending");
        }
    }

    #[test]
    fn captured_patch_rejects_newer_data_and_request_event_but_not_transcript_notice() {
        let (store, _) = ObservedStore::new(requests(&["old"]));
        let (captured, revision) = store.snapshot_with_revision();
        store.invalidate_projection(false);
        assert!(store
            .merge_chat_patch_at_snapshot(
                &captured,
                revision.request_event_version,
                requests(&["valid"])
            )
            .is_some());

        let (stale_snapshot, stale_revision) = store.snapshot_with_revision();
        store.replace_snapshot(requests(&[]));
        assert!(store
            .merge_chat_patch_at_snapshot(
                &stale_snapshot,
                stale_revision.request_event_version,
                requests(&["old"]),
            )
            .is_none());
        assert!(!store
            .snapshot()
            .requests
            .iter()
            .any(|row| row.request_id == "old"));

        let (uncached_snapshot, uncached_revision) = store.snapshot_with_revision();
        store.observe_request_event();
        assert!(store
            .merge_chat_patch_at_snapshot(
                &uncached_snapshot,
                uncached_revision.request_event_version,
                requests(&["deleted-before-read"]),
            )
            .is_none());
        assert!(!store
            .snapshot()
            .requests
            .iter()
            .any(|row| row.request_id == "deleted-before-read"));
    }

    #[test]
    fn generated_data_publication_cases_match_observed_store() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../gents/tests/fixtures/client_data_publication.json"
        ))
        .expect("generated ClientDataPublication cases");
        for case in fixture["publication"]
            .as_array()
            .expect("publication cases")
        {
            let name = case["name"].as_str().expect("case name");
            let expected = case["accepted"].as_bool().expect("model decision");
            let initial_ids: Vec<String> = case["initial_request_ids"]
                .as_array()
                .expect("modeled initial request IDs")
                .iter()
                .map(|id| id.as_str().expect("request ID").to_owned())
                .collect();
            let initial_refs: Vec<&str> = initial_ids.iter().map(String::as_str).collect();
            let publish_ids: Vec<String> = case["publish_request_ids"]
                .as_array()
                .expect("modeled publication request IDs")
                .iter()
                .map(|id| id.as_str().expect("request ID").to_owned())
                .collect();
            let publish_refs: Vec<&str> = publish_ids.iter().map(String::as_str).collect();
            let (store, _) = ObservedStore::new(requests(&initial_refs));
            let (captured, revision) = store.snapshot_with_revision();
            for action in case["actions"].as_array().expect("modeled actions") {
                match action.as_str().expect("publication action") {
                    "request_event" => {
                        store.observe_request_event();
                    }
                    "transcript_invalidation" => {
                        store.invalidate_projection(false);
                    }
                    "replace_requests" => {
                        store.replace_snapshot(requests(&[]));
                    }
                    other => panic!("unrecognized modeled publication action {other}"),
                }
            }
            let published = store
                .merge_chat_patch_at_snapshot(
                    &captured,
                    revision.request_event_version,
                    requests(&publish_refs),
                )
                .is_some();
            assert_eq!(published, expected, "modeled case {name}");
        }
    }

    #[tokio::test]
    async fn generated_scoped_publication_cases_match_observed_store() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../gents/tests/fixtures/client_data_publication.json"
        ))
        .expect("generated ClientDataPublication cases");
        let rows = |owner: Option<u64>, content: &str| {
            let mut patch = requests(&[&format!("request-{}", owner.unwrap_or(0))]);
            for row in &mut patch.requests {
                row.doc_id = Some(row.request_id.clone());
                row.node_did = owner.map(|id| format!("did:scope:{id}"));
                row.content = Some(content.to_owned());
            }
            patch
        };
        for case in fixture["scoped_publication"]
            .as_array()
            .expect("scoped cases")
        {
            for patch_kind in ["request", "session", "observation"] {
                let node = format!("did:scope:{}", case["node"].as_u64().unwrap());
                let initial = rows(Some(1), "original").merge_snapshot(rows(Some(2), "original"));
                let (store, _) = ObservedStore::new(initial);
                let read = store.begin_reload(Some(&node));
                let (release, loaded) = tokio::sync::oneshot::channel();
                let publishing = Arc::clone(&store);
                let publication = tokio::spawn(async move {
                    loaded.await.expect("release blocked scoped load");
                    publishing.publish_reload(&read, rows(Some(1), "recovered"))
                });
                for (i, action) in case["actions"].as_array().unwrap().iter().enumerate() {
                    let owner = action["owner"].as_u64();
                    match action["op"].as_str().unwrap() {
                        "request_event" => {
                            store.observe_request_document_event(&format!(
                                "request-{}",
                                owner.unwrap_or(0)
                            ));
                        }
                        "patch" => {
                            let mut patch = match patch_kind {
                            "request" => rows(owner, &format!("update-{i}")),
                            "session" => ClientStore::from_rows(crate::client::store::ClientStoreRows {
                                sessions: vec![serde_json::from_value(serde_json::json!({
                                    "session_id": "session",
                                    "node_did": owner.map(|id| format!("did:scope:{id}")).unwrap_or_default(),
                                    "agent_id": "agent",
                                    "created_at": "2026-01-01T00:00:00Z"
                                })).unwrap()],
                                ..Default::default()
                            }),
                            "observation" => ClientStore::from_rows(crate::client::store::ClientStoreRows {
                                trigger_observations: vec![serde_json::from_value(serde_json::json!({
                                    "trigger_id": "trigger",
                                    "fire_count": i,
                                    "last_attempt_at": null,
                                    "last_fired_source_doc_id": null,
                                    "last_status": null,
                                    "last_error": null
                                })).unwrap()],
                                trigger_observation_source_node_dids: vec![owner.map(|id| format!("did:scope:{id}"))],
                                ..Default::default()
                            }),
                            _ => unreachable!(),
                        };
                            store.merge_snapshot(std::mem::take(&mut patch));
                        }
                        "replace" => {
                            store.replace_snapshot(rows(Some(2), "replacement"));
                        }
                        other => panic!("unknown modeled action {other}"),
                    }
                    tokio::task::yield_now().await;
                }
                let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let traffic = if case["accepted"].as_bool().unwrap() {
                    let running = Arc::clone(&running);
                    let foreign = Arc::clone(&store);
                    let (started, ready) = tokio::sync::oneshot::channel();
                    let traffic = tokio::spawn(async move {
                        foreign.merge_snapshot(rows(Some(2), "ongoing foreign traffic"));
                        started.send(()).unwrap();
                        while running.load(Ordering::Relaxed) {
                            foreign.observe_request_document_event("request-2");
                            foreign.merge_snapshot(rows(Some(2), "ongoing foreign traffic"));
                            tokio::task::yield_now().await;
                        }
                    });
                    ready.await.unwrap();
                    Some(traffic)
                } else {
                    None
                };
                release.send(()).expect("blocked scoped load");
                let accepted = tokio::time::timeout(std::time::Duration::from_secs(5), publication)
                    .await
                    .expect("scoped publication must not wait for foreign traffic to stop")
                    .expect("scoped publication");
                running.store(false, Ordering::Relaxed);
                if let Some(traffic) = traffic {
                    traffic.await.unwrap();
                }
                assert_eq!(
                    accepted,
                    case["accepted"].as_bool().unwrap(),
                    "{}",
                    case["name"]
                );
                if accepted {
                    assert_eq!(
                        store
                            .snapshot()
                            .requests
                            .iter()
                            .find(|r| r.node_did.as_deref() == Some(&node))
                            .unwrap()
                            .content
                            .as_deref(),
                        Some("recovered")
                    );
                    assert!(store
                        .snapshot()
                        .requests
                        .iter()
                        .any(|r| r.node_did.as_deref() == Some("did:scope:2")));
                }
            }
        }
    }

    #[test]
    fn generated_scoped_replacement_case_matches_client_store_ownership() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../gents/tests/fixtures/client_data_publication.json"
        ))
        .expect("generated ClientDataPublication cases");
        for case in fixture["scoped_replacement"]
            .as_array()
            .expect("modeled scoped replacement cases")
        {
            let did = |owner: u64| format!("did:scope:{owner}");
            let configs = |rows: &[serde_json::Value]| {
                rows.iter()
                    .map(|row| {
                        let owner = row["owner"].as_u64().expect("config owner");
                        serde_json::from_value(serde_json::json!({
                            "session_id": row["key"],
                            "node_did": did(owner),
                            "requester_did": null,
                            "agent_id": "agent",
                            "created_at": "2026-01-01T00:00:00Z",
                            "title": {"text": row["value"], "source": "user"}
                        }))
                        .expect("modeled session row")
                    })
                    .collect::<Vec<gents_protocol::session::AgentSession>>()
            };
            let tool_calls = |rows: &[serde_json::Value]| {
                rows.iter()
                    .map(|row| {
                        let owner = row["owner"].as_u64().expect("tool-call owner");
                        serde_json::from_value(serde_json::json!({
                            "tool_call_key": row["key"],
                            "node_did": did(owner),
                            "session_id": row["key"],
                            "tool_name": row["value"]
                        }))
                        .expect("modeled tool-call row")
                    })
                    .collect::<Vec<gents_protocol::row::AgentToolCallRow>>()
            };
            let observations = |rows: &[serde_json::Value]| {
                rows.iter()
                    .map(|row| {
                        serde_json::from_value(serde_json::json!({
                            "trigger_id": row["key"],
                            "last_attempt_at": "2026-01-01T00:00:00Z",
                            "last_fired_source_doc_id": null,
                            "last_status": row["value"],
                            "last_error": null,
                            "fire_count": 1
                        }))
                        .expect("modeled trigger observation")
                    })
                    .collect::<Vec<gents::document_config::TriggerObservation>>()
            };
            let observation_sources = |rows: &[serde_json::Value]| {
                rows.iter()
                    .map(|row| row["source"].as_u64().map(did))
                    .collect::<Vec<_>>()
            };
            let mut initial = ClientStore::from_rows(crate::client::store::ClientStoreRows {
                sessions: configs(
                    case["current_configs"]
                        .as_array()
                        .expect("modeled current configs"),
                ),
                session_source_node_dids: case["current_configs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["source"].as_u64().map(did))
                    .collect(),
                tool_calls: tool_calls(case["current_configs"].as_array().unwrap()),
                tool_call_source_node_dids: case["current_configs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["source"].as_u64().map(did))
                    .collect(),
                triggers: (1..=3)
                    .map(|owner| {
                        serde_json::from_value(serde_json::json!({
                            "node_did": did(owner),
                            "trigger_id": "shared-trigger",
                            "task_id": format!("task-{owner}"),
                            "source": {"kind": "event", "event_source_id": "source"},
                            "enabled": true,
                            "tags": []
                        }))
                        .expect("modeled trigger")
                    })
                    .collect(),
                trigger_observations: observations(
                    case["current_observations"]
                        .as_array()
                        .expect("modeled current observations"),
                ),
                trigger_observation_source_node_dids: observation_sources(
                    case["current_observations"]
                        .as_array()
                        .expect("modeled current observations"),
                ),
                ..Default::default()
            });
            let incoming = ClientStore::from_rows(crate::client::store::ClientStoreRows {
                operator_config_authoritative_node_dids: case["incoming_authoritative_owners"]
                    .as_array()
                    .expect("modeled incoming authoritative owners")
                    .iter()
                    .map(|owner| did(owner.as_u64().expect("authority owner")))
                    .collect(),
                sessions: configs(
                    case["incoming_configs"]
                        .as_array()
                        .expect("modeled incoming configs"),
                ),
                session_source_node_dids: case["incoming_configs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["source"].as_u64().map(did))
                    .collect(),
                tool_calls: tool_calls(case["incoming_configs"].as_array().unwrap()),
                tool_call_source_node_dids: case["incoming_configs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["source"].as_u64().map(did))
                    .collect(),
                triggers: case["incoming_trigger_owners"]
                    .as_array()
                    .expect("modeled incoming trigger owners")
                    .iter()
                    .map(|owner| {
                        let owner = owner.as_u64().expect("trigger owner");
                        serde_json::from_value(serde_json::json!({
                            "node_did": did(owner),
                            "trigger_id": "shared-trigger",
                            "task_id": format!("task-{owner}"),
                            "source": {"kind": "event", "event_source_id": "source"},
                            "enabled": true,
                            "tags": []
                        }))
                        .expect("modeled incoming trigger")
                    })
                    .collect(),
                trigger_source_node_dids: case["incoming_trigger_owners"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|owner| Some(did(owner.as_u64().unwrap())))
                    .collect(),
                trigger_observations: observations(
                    case["incoming_observations"]
                        .as_array()
                        .expect("modeled incoming observations"),
                ),
                trigger_observation_source_node_dids: observation_sources(
                    case["incoming_observations"]
                        .as_array()
                        .expect("modeled incoming observations"),
                ),
                ..Default::default()
            });
            let merged =
                initial.replace_node_scope(&did(case["scope"].as_u64().unwrap()), incoming);
            let expected_authority = case["expected_authoritative_owners"]
                .as_array()
                .expect("modeled expected authoritative owners")
                .iter()
                .map(|owner| did(owner.as_u64().expect("authority owner")))
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(
                merged.operator_config_authoritative_node_dids,
                expected_authority
            );
            let expected_configs = case["expected_configs"].as_array().unwrap();
            for expected in expected_configs {
                let owner_did = did(expected["owner"].as_u64().unwrap());
                assert!(merged.sessions.iter().any(|row| {
                    row.node_did == owner_did
                        && row.session_id == expected["key"].as_str().unwrap()
                        && row.title.as_ref().map(|title| title.text.as_str())
                            == Some(expected["value"].as_str().unwrap())
                }));
            }
            assert_eq!(merged.sessions.len(), expected_configs.len());
            for expected in expected_configs {
                let owner_did = did(expected["owner"].as_u64().unwrap());
                assert!(merged.tool_calls.iter().any(|row| {
                    row.node_did.as_deref() == Some(owner_did.as_str())
                        && row.tool_call_key == expected["key"].as_str().unwrap()
                        && row.tool_name.as_deref() == expected["value"].as_str()
                }));
            }
            assert_eq!(merged.tool_calls.len(), expected_configs.len());

            for expected in case["expected_observations"].as_array().unwrap() {
                let source = expected["source"].as_u64().map(did);
                assert!(merged
                    .trigger_observations
                    .iter()
                    .enumerate()
                    .any(|(index, row)| {
                        row.trigger_id == expected["key"].as_str().unwrap()
                            && row.last_status.as_deref() == expected["value"].as_str()
                            && merged.trigger_observation_source_node_dids.get(index)
                                == Some(&source)
                    }));
            }
            assert_eq!(
                merged.trigger_observations.len(),
                case["expected_observations"].as_array().unwrap().len()
            );
            for lookup in case["lookups"].as_array().unwrap() {
                let owner = lookup["owner"].as_u64().unwrap();
                let observed = merged
                    .recent_runs_for_task(&format!("task-{owner}"))
                    .last_status;
                assert_eq!(
                    observed.as_deref(),
                    lookup["value"].as_str(),
                    "modeled scoped lookup for owner {owner}"
                );
            }
        }
    }

    #[tokio::test]
    async fn generated_recovery_cases_preserve_scopes_without_self_waking() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../gents/tests/fixtures/client_data_publication.json"
        ))
        .expect("generated ClientDataPublication cases");
        for case in fixture["recovery"].as_array().expect("recovery cases") {
            let name = case["name"].as_str().expect("case name");
            let expected_scopes: Vec<String> = case["pending_scopes"]
                .as_array()
                .expect("modeled pending scopes")
                .iter()
                .map(|scope| format!("scope-{}", scope.as_u64().expect("scope id")))
                .collect();
            let expected_generation = case["generation"].as_u64().expect("model generation");
            let expected_full = case["full"].as_bool().expect("modeled full-reload state");
            let retry_scheduled = case["retry_scheduled"]
                .as_bool()
                .expect("modeled retry deadline state");
            assert_eq!(case["initial"]["full"].as_bool(), Some(false));
            assert!(case["initial"]["pending_scopes"]
                .as_array()
                .expect("modeled initial scopes")
                .is_empty());
            assert_eq!(case["initial"]["generation"].as_u64(), Some(0));
            assert_eq!(case["initial"]["retry_scheduled"].as_bool(), Some(false));
            let (store, _) = ObservedStore::new(ClientStore::default());
            let mut wake = store.subscribe_resync_requests();
            assert_eq!(
                *wake.borrow(),
                case["initial"]["generation"].as_u64().unwrap()
            );
            let mut in_flight = Vec::new();
            let mut observed_generation = 0;
            let mut batch_taken = false;
            let mut retry_not_before = None;
            let now = tokio::time::Instant::now();
            for action in case["actions"]
                .as_array()
                .expect("modeled recovery actions")
            {
                match action["op"].as_str().expect("recovery action op") {
                    "request_scope" if batch_taken => {
                        let scope =
                            format!("scope-{}", action["scope"].as_u64().expect("modeled scope"));
                        store.request_resync(Some(&scope));
                        assert!(wake.has_changed().expect("watch remains open"));
                        observed_generation = *wake.borrow_and_update();
                    }
                    "request_scope" => {
                        let scope =
                            format!("scope-{}", action["scope"].as_u64().expect("modeled scope"));
                        store.request_resync(Some(&scope));
                    }
                    "take_batch" => {
                        wake.changed().await.expect("initial batch notification");
                        let (full, batch) = store.take_resync_requests();
                        assert_eq!(full, case["initial"]["full"].as_bool().unwrap());
                        in_flight = batch;
                        observed_generation = *wake.borrow_and_update();
                        batch_taken = true;
                    }
                    "fail_batch" => {
                        let failed_scopes =
                            action["scopes"].as_array().expect("modeled failed scopes");
                        for scope in failed_scopes {
                            let scope =
                                format!("scope-{}", scope.as_u64().expect("modeled failed scope"));
                            assert!(in_flight.contains(&scope), "failed scope was not in flight");
                            store.enqueue_resync_request(Some(&scope));
                        }
                        assert!(!wake.has_changed().expect("watch remains open"));
                        assert_eq!(*wake.borrow(), observed_generation);
                        retry_not_before = Some(now + std::time::Duration::from_millis(1));
                    }
                    "retry_timer" => {
                        retry_not_before = None;
                    }
                    other => panic!("unrecognized modeled recovery action {other}"),
                }
            }
            assert_eq!(
                *wake.borrow(),
                expected_generation,
                "model generation {name}"
            );
            assert_eq!(
                super::super::resync_retry_ready(retry_not_before, now),
                !retry_scheduled,
                "observer retry gate for modeled recovery actions {name}"
            );
            if retry_scheduled {
                let deadline = retry_not_before.expect("failed batch has a retry deadline");
                assert!(super::super::resync_retry_ready(Some(deadline), deadline));
            }
            let (full, pending) = store.take_resync_requests();
            assert_eq!(full, expected_full);
            assert_eq!(pending, expected_scopes, "modeled recovery case {name}");
        }
    }
}
