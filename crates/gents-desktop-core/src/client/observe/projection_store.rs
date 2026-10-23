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
    fn generated_notice_delivery_never_regresses_watchers() {
        let contract = gents_lean_contract::load_contract_snapshot::<serde_json::Value>()
            .expect("executed Lean publication contract");
        let fixture = &contract["client_data_publication_cases"];
        let case = &fixture["notice_delivery"];
        let decode = |value: &serde_json::Value| StoreUpdateNotice {
            revision: StoreProjectionRevision {
                store_version: value["version"].as_u64().expect("store version"),
                provenance_version: value["provenance"].as_u64().expect("provenance version"),
                request_event_version: value["request_event"]
                    .as_u64()
                    .expect("request event version"),
            },
        };
        let (store, versions) = ObservedStore::new(ClientStore::default());
        let changes = store.change_tx.subscribe();
        store.publish_notice(decode(&case["initial"]));
        for delivery in case["deliveries"].as_array().expect("notice deliveries") {
            store.publish_notice(decode(&delivery["incoming"]));
            let expected = decode(&delivery["expected"]);
            assert_eq!(*versions.borrow(), expected.revision.store_version);
            assert_eq!(*changes.borrow(), expected);
        }
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
        let captured = store.capture_read();
        store.invalidate_projection(true);
        let current = store.projection_revision();
        assert!(!store.replace_reloaded_snapshot_at_read(&captured, None, requests(&["parent"]),));
        assert_eq!(store.projection_revision(), current);
        let current_snapshot = store.capture_read();
        assert!(store.replace_reloaded_snapshot_at_read(
            &current_snapshot,
            None,
            requests(&["parent"]),
        ));
        assert_eq!(
            store.projection_revision().provenance_version,
            current.provenance_version + 1
        );
    }

    #[test]
    fn late_reload_cannot_erase_a_request_refreshed_after_capture() {
        for scope in [None, Some("did:test:reload")] {
            let (store, _) = ObservedStore::new(requests(&["accounted"]));
            let captured = store.capture_read();
            let old_database_read = requests(&["accounted"]);
            store.merge_chat_patch(requests(&["pending"]));
            let refreshed = store.projection_revision();
            assert!(!store.replace_reloaded_snapshot_at_read(&captured, scope, old_database_read,));
            assert_eq!(store.projection_revision(), refreshed);
            assert!(store
                .snapshot()
                .requests
                .iter()
                .any(|row| row.request_id == "pending"));
            let refreshed_snapshot = store.capture_read();
            assert!(store.replace_reloaded_snapshot_at_read(
                &refreshed_snapshot,
                scope,
                requests(&["pending"]),
            ));
            assert_eq!(store.snapshot().requests.len(), 1);
            assert_eq!(store.snapshot().requests[0].request_id, "pending");
        }
    }

    #[test]
    fn physical_request_owner_replacement_fences_both_scopes() {
        let rows = |owner: &str| {
            ClientStore::from_rows(ClientStoreRows {
                requests: vec![AgentRequestRow {
                    doc_id: Some("physical".into()),
                    request_id: "request".into(),
                    node_did: Some(owner.into()),
                    purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                    ..Default::default()
                }],
                ..Default::default()
            })
        };
        let (store, _) = ObservedStore::new(rows("did:old"));
        let read = store.capture_read();
        store.merge_observer_patch(rows("did:new"));
        assert!(!store.replace_reloaded_snapshot_at_read(&read, Some("did:old"), rows("did:old")));
        assert_eq!(
            store.snapshot().requests[0].node_did.as_deref(),
            Some("did:new")
        );
    }

    #[test]
    fn captured_patch_rejects_newer_data_and_request_event_but_not_transcript_notice() {
        let (store, _) = ObservedStore::new(requests(&["old"]));
        let captured = store.capture_read();
        store.invalidate_projection(false);
        assert!(store
            .merge_chat_patch_at_read(&captured, requests(&["valid"]))
            .is_some());

        let stale_snapshot = store.capture_read();
        store.replace_snapshot(requests(&[]));
        assert!(store
            .merge_chat_patch_at_read(&stale_snapshot, requests(&["old"]),)
            .is_none());
        assert!(!store
            .snapshot()
            .requests
            .iter()
            .any(|row| row.request_id == "old"));

        let uncached_snapshot = store.capture_read();
        store.observe_request_event(None);
        assert!(store
            .merge_chat_patch_at_read(&uncached_snapshot, requests(&["deleted-before-read"]),)
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
            let captured = store.capture_read();
            for action in case["actions"].as_array().expect("modeled actions") {
                match action.as_str().expect("publication action") {
                    "request_event" => {
                        store.observe_request_event(None);
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
                .merge_chat_patch_at_read(&captured, requests(&publish_refs))
                .is_some();
            assert_eq!(published, expected, "modeled case {name}");
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
            let initial = ClientStore::from_rows(crate::client::store::ClientStoreRows {
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
                .collect::<rapidhash::RapidHashSet<_>>();
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
    async fn generated_scoped_publications_preserve_independent_nodes_during_a_blocked_read() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../gents/tests/fixtures/client_data_publication.json"
        ))
        .expect("generated ClientDataPublication cases");
        let did = |owner: u64| format!("did:scope:{owner}");
        let sessions = |values: &[serde_json::Value]| {
            ClientStore::from_rows(ClientStoreRows {
                sessions: values
                    .iter()
                    .map(|row| {
                        serde_json::from_value(serde_json::json!({
                            "node_did": did(row["owner"].as_u64().unwrap()),
                            "session_id": row["key"],
                            "agent_id": "agent",
                            "created_at": "2026-01-01T00:00:00Z",
                            "title": {"text": row["value"], "source": "user"}
                        }))
                        .expect("modeled session")
                    })
                    .collect(),
                ..Default::default()
            })
        };
        for case in fixture["scoped_publication"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let mut initial = sessions(case["initial"].as_array().unwrap()).to_rows();
            initial.requests = case["initial"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    let owner = row["owner"].as_u64().unwrap();
                    AgentRequestRow {
                        doc_id: Some(format!("document-{owner}")),
                        request_id: format!("request-{owner}"),
                        node_did: Some(did(owner)),
                        session_id: row["key"].as_str().map(str::to_owned),
                        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                        ..Default::default()
                    }
                })
                .collect();
            let (store, _) = ObservedStore::new(ClientStore::from_rows(initial));
            let read = store.capture_read();
            let incoming = sessions(case["incoming"].as_array().unwrap());
            let merge = case["merge"].as_bool().unwrap();
            let scope = if merge {
                None
            } else {
                case["owners"].as_array().map(|owners| {
                    assert_eq!(owners.len(), 1, "a replacement has one modeled owner");
                    did(owners[0].as_u64().unwrap())
                })
            };
            let (finish_read, blocked_read) = tokio::sync::oneshot::channel::<()>();
            let publisher = Arc::clone(&store);
            let publication = tokio::spawn(async move {
                blocked_read.await.expect("release modeled database read");
                if merge {
                    let read_accepted = publisher.accepts_chat_patch_read(&read, &incoming);
                    let published = publisher
                        .merge_observer_patch_at_read(&read, incoming)
                        .is_some();
                    assert_eq!(read_accepted, published);
                    published
                } else {
                    publisher.replace_reloaded_snapshot_at_read(&read, scope.as_deref(), incoming)
                }
            });
            for action in case["actions"].as_array().unwrap() {
                match action["op"].as_str().unwrap() {
                    "merge" => {
                        store.merge_observer_patch(sessions(action["rows"].as_array().unwrap()));
                    }
                    "request" => {
                        let doc = action["owner"]
                            .as_u64()
                            .map(|owner| format!("document-{owner}"));
                        store.observe_request_event(doc.as_deref());
                    }
                    "unknown_observation" => {
                        store.merge_observer_patch(ClientStore::from_rows(ClientStoreRows {
                            trigger_observations: vec![serde_json::from_value(serde_json::json!({
                                "trigger_id": "unknown-source",
                                "last_attempt_at": null,
                                "last_fired_source_doc_id": null,
                                "last_status": null,
                                "last_error": null,
                                "fire_count": 0
                            }))
                            .unwrap()],
                            ..Default::default()
                        }));
                    }
                    "replace_all" => {
                        store.replace_snapshot(sessions(action["rows"].as_array().unwrap()));
                    }
                    "transcript" => {
                        store.invalidate_projection(false);
                    }
                    other => panic!("unknown modeled publication action {other}"),
                }
            }
            finish_read
                .send(())
                .expect("complete modeled database read");
            let accepted = publication.await.expect("publication task");
            assert_eq!(accepted, case["accepted"].as_bool().unwrap(), "{name}");
            let actual = store.snapshot();
            let mut actual: Vec<_> = actual
                .sessions
                .iter()
                .map(|row| {
                    (
                        row.node_did.clone(),
                        row.session_id.clone(),
                        row.title.as_ref().unwrap().text.clone(),
                    )
                })
                .collect();
            let mut expected: Vec<_> = case["expected"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    (
                        did(row["owner"].as_u64().unwrap()),
                        row["key"].as_str().unwrap().to_owned(),
                        row["value"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            actual.sort();
            expected.sort();
            assert_eq!(actual, expected, "{name}: retained current unrelated facts");
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
