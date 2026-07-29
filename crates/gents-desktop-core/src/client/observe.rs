use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use defra_node::EmbeddedNode;
use events::Subscription;
use tokio::sync::{watch, RwLock as AsyncRwLock};

use super::collection_resolver::CollectionResolver;
use super::peer_directory::PeerDirectory;
use super::query::{
    fetch_doc_patch, isolate_legacy_bearer_rows, load_agent_scoped_snapshot_with_peer_records,
    load_full_snapshot_with_peer_records,
};
use super::store::{ClientStore, SharedClientStore};

const OBSERVER_DEBOUNCE: Duration = Duration::from_millis(150);
const FETCH_RETRY_LIMIT: u32 = 3;

#[derive(Debug, Default)]
pub struct ObserverMetrics {
    pub events_received: AtomicU64,
    pub docs_fetched: AtomicU64,
    pub debounce_flushes: AtomicU64,
    pub scope_reloads: AtomicU64,
    pub drop_recoveries: AtomicU64,
    pub local_write_redundant_fetches: AtomicU64,
    pub fetch_failures: AtomicU64,
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
        }
    }
}

pub struct ObservedStore {
    snapshot: RwLock<SharedClientStore>,
    focused_request_id: RwLock<Option<String>>,
    version_tx: watch::Sender<u64>,
}

impl ObservedStore {
    pub fn new(initial_snapshot: ClientStore) -> (Arc<Self>, watch::Receiver<u64>) {
        let (version_tx, version_rx) = watch::channel(1_u64);
        let store = Arc::new(Self {
            snapshot: RwLock::new(Arc::new(initial_snapshot)),
            focused_request_id: RwLock::new(None),
            version_tx,
        });
        (store, version_rx)
    }

    pub fn snapshot(&self) -> SharedClientStore {
        self.snapshot
            .read()
            .expect("store snapshot lock poisoned")
            .clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version_tx.subscribe()
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
        *self.snapshot.write().expect("store snapshot lock poisoned") = Arc::new(snapshot);

        let next_version = self.version_tx.borrow().saturating_add(1);
        self.version_tx.send_replace(next_version);
        next_version
    }

    pub fn merge_chat_patch(&self, patch: ClientStore) -> u64 {
        let mut snapshot = self.snapshot.write().expect("store snapshot lock poisoned");
        let next_snapshot = snapshot.merge_chat_patch(patch);
        *snapshot = Arc::new(next_snapshot);

        let next_version = self.version_tx.borrow().saturating_add(1);
        self.version_tx.send_replace(next_version);
        next_version
    }

    pub fn merge_snapshot(&self, incoming: ClientStore) -> u64 {
        let mut snapshot = self.snapshot.write().expect("store snapshot lock poisoned");
        let next_snapshot = snapshot.merge_snapshot(incoming);
        *snapshot = Arc::new(next_snapshot);

        let next_version = self.version_tx.borrow().saturating_add(1);
        self.version_tx.send_replace(next_version);
        next_version
    }

    pub fn replace_agent_snapshot(&self, agent_did: &str, incoming: ClientStore) -> u64 {
        let mut snapshot = self.snapshot.write().expect("store snapshot lock poisoned");
        let next_snapshot = snapshot.replace_agent_scope(agent_did, incoming);
        *snapshot = Arc::new(next_snapshot);

        let next_version = self.version_tx.borrow().saturating_add(1);
        self.version_tx.send_replace(next_version);
        next_version
    }

    pub fn replace_remote_agent_snapshot(&self, agent_did: &str, incoming: ClientStore) -> u64 {
        let mut snapshot = self.snapshot.write().expect("store snapshot lock poisoned");
        let next_snapshot = snapshot.replace_remote_agent_scope(agent_did, incoming);
        *snapshot = Arc::new(next_snapshot);

        let next_version = self.version_tx.borrow().saturating_add(1);
        self.version_tx.send_replace(next_version);
        next_version
    }
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

            for (collection_name, doc_ids) in flushed {
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
                            store.merge_snapshot(ClientStore::from_rows(rows));
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
        Ok(Some(name)) => {
            if !is_relay {
                metrics
                    .local_write_redundant_fetches
                    .fetch_add(1, Ordering::Relaxed);
            }
            dirty.entry(name).or_default().insert(doc_id.to_string());
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
mod tests {
    use super::*;
    use crate::client::schema::ensure_runtime_schemas;
    use defra_node::{EventName, NodeBuilder};
    use std::sync::Arc;
    use tokio::sync::RwLock as AsyncRwLock;

    async fn build_observer_fixture() -> (Arc<EmbeddedNode>, Arc<ObservedStore>, ObserverHandle) {
        let node = Arc::new(NodeBuilder::default().build().await.expect("node"));
        ensure_runtime_schemas(node.as_ref())
            .await
            .expect("schemas");
        let (store, _rx) = ObservedStore::new(crate::client::store::ClientStore::default());
        let peer_dir = Arc::new(AsyncRwLock::new(
            crate::client::peer_directory::PeerDirectory::load(
                "/tmp/gents-observe-test-peers-nonexistent.json",
            )
            .await
            .expect("peer_directory"),
        ));
        let subscription = node.subscribe(&[EventName::Update]);
        let (_tx, rx) = watch::channel::<Option<String>>(None);
        let handle = spawn_observer_with_selection(
            node.clone(),
            store.clone(),
            peer_dir,
            "did:test:requester".to_string(),
            subscription,
            rx,
        );
        (node, store, handle)
    }

    async fn seed_principal(node: &EmbeddedNode, did: &str) {
        let mutation = format!(
            r#"mutation {{
                create_AgentPrincipal(input: {{
                    agent_did: "{did}",
                    display_name: "{did}",
                    default_behavior_id: "default",
                    enabled: true,
                    created_at: "2026-05-07T00:00:00Z",
                    created_by: "test"
                }}) {{ _docID }}
            }}"#
        );
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }

    async fn seed_message(node: &EmbeddedNode, session_id: &str, seq: i64, content: &str) {
        let mutation = format!(
            r#"mutation {{
                create_AgentMessage(input: {{
                    message_key: "{session_id}:{seq}",
                    session_id: "{session_id}",
                    sequence: {seq},
                    role: "user",
                    content: "{content}",
                    timestamp: "2026-05-07T00:00:00Z"
                }}) {{ _docID }}
            }}"#
        );
        let response = node.execute(&mutation).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
    }

    #[tokio::test]
    async fn coalesces_burst_into_one_fetch_per_doc() {
        let (node, store, handle) = build_observer_fixture().await;

        let create = r#"mutation {
            create_AgentResponse(input: {
                response_key: "req-1",
                request_id: "req-1",
                agent_did: "did:alpha",
                behavior_id: "default",
                session_id: "sess-1",
                content: "",
                reasoning: "",
                status: "streaming",
                error_message: "",
                token_count: 0,
                progress_seq: 0,
                created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
        }"#;
        let resp = node.execute(create).await;
        assert!(!resp.has_errors(), "{:?}", resp.errors);

        let metrics_before = handle.metrics_snapshot();
        for i in 1..=50 {
            let update = format!(
                r#"mutation {{ update_AgentResponse(filter: {{ response_key: {{ _eq: "req-1" }} }}, input: {{ progress_seq: {i} }}) {{ _docID }} }}"#
            );
            let resp = node.execute(&update).await;
            assert!(!resp.has_errors(), "{:?}", resp.errors);
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let metrics_after = handle.metrics_snapshot();

        let fetches = metrics_after.docs_fetched - metrics_before.docs_fetched;
        let flushes = metrics_after.debounce_flushes - metrics_before.debounce_flushes;
        assert!(fetches <= 5, "expected <=5 fetches, got {fetches}");
        assert!(
            flushes >= 1 && flushes <= 5,
            "expected 1..=5 flushes, got {flushes}"
        );

        let snap = store.snapshot();
        let response = snap
            .responses
            .iter()
            .find(|r| r.response_key == "req-1")
            .expect("response present");
        assert_eq!(response.progress_seq, Some(50));

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn multi_collection_burst_fans_out_correctly() {
        let (node, store, handle) = build_observer_fixture().await;

        let create_resp = r#"mutation {
            create_AgentResponse(input: {
                response_key: "req-1",
                request_id: "req-1",
                agent_did: "did:alpha",
                behavior_id: "default",
                session_id: "sess-1",
                content: "",
                reasoning: "",
                status: "streaming",
                error_message: "",
                token_count: 0,
                progress_seq: 0,
                created_at: "2026-05-07T00:00:00Z"
            }) { _docID }
        }"#;
        let resp = node.execute(create_resp).await;
        assert!(!resp.has_errors(), "{:?}", resp.errors);

        for i in 1..=5 {
            let update_resp = format!(
                r#"mutation {{ update_AgentResponse(filter: {{ response_key: {{ _eq: "req-1" }} }}, input: {{ progress_seq: {i} }}) {{ _docID }} }}"#
            );
            node.execute(&update_resp).await;

            let create_msg = format!(
                r#"mutation {{
                    create_AgentMessage(input: {{
                        message_key: "sess-1:{i}",
                        session_id: "sess-1",
                        sequence: {i},
                        role: "assistant",
                        content: "msg-{i}",
                        timestamp: "2026-05-07T00:00:0{i}Z"
                    }}) {{ _docID }}
                }}"#
            );
            node.execute(&create_msg).await;
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let snap = store.snapshot();

        assert_eq!(
            snap.responses
                .iter()
                .find(|r| r.response_key == "req-1")
                .and_then(|r| r.progress_seq),
            Some(5),
            "expected progress_seq=5 in responses"
        );

        for i in 1..=5 {
            let key = format!("sess-1:{i}");
            assert!(
                snap.messages.iter().any(|m| m.message_key == key),
                "expected message {key} in store"
            );
        }

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn dropped_events_with_no_selection_falls_back_to_full() {
        let (node, store, handle) = build_observer_fixture().await;
        seed_principal(node.as_ref(), "did:zero").await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let snap = store.snapshot();
        assert!(
            snap.agent_principals
                .iter()
                .any(|p| p.agent_did == "did:zero"),
            "expected did:zero in store"
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn delete_event_reloads_authoritative_scope_and_removes_stale_row() {
        let (node, store, handle) = build_observer_fixture().await;

        seed_message(node.as_ref(), "sess-1", 1, "before-delete").await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            store
                .snapshot()
                .messages
                .iter()
                .any(|m| m.message_key == "sess-1:1"),
            "expected message in store before delete"
        );

        node.execute(
            r#"mutation { delete_AgentMessage(filter: { message_key: { _eq: "sess-1:1" } }) { _docID } }"#,
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // An empty document patch is ambiguous, so the observer reloads the
        // authoritative scope. The deleted row must disappear from the local
        // projection instead of remaining visible until restart.
        let snap = store.snapshot();
        assert!(
            !snap.messages.iter().any(|m| m.message_key == "sess-1:1"),
            "expected authoritative reload to remove the deleted row"
        );
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn fetch_failures_increment_on_unknown_collection() {
        let (node, _store, handle) = build_observer_fixture().await;

        let result =
            crate::client::query::fetch_doc_patch(node.as_ref(), "NotARealCollection", &["x"])
                .await;
        assert!(result.is_err(), "expected error for unknown collection");

        let snap = handle.metrics_snapshot();
        assert_eq!(snap.fetch_failures, 0);
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn local_write_increments_redundant_fetch_counter() {
        let (node, _store, handle) = build_observer_fixture().await;

        seed_message(node.as_ref(), "sess-2", 1, "local").await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let snap = handle.metrics_snapshot();
        assert!(
            snap.local_write_redundant_fetches >= 1,
            "expected at least 1 local-write fetch; got {}",
            snap.local_write_redundant_fetches
        );
        handle.shutdown().await;
    }
}
