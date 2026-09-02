use std::sync::Arc;
use tokio::sync::watch;
use tokio::sync::RwLock;

use gents::P2pSyncStatusSnapshot;

use super::{p2p_health_materially_changed, ClientPeerStatus, ClientSyncStateSnapshot, P2PHealth};
#[cfg(test)]
use crate::client::load_peer_directory_snapshot;
#[cfg(test)]
use crate::client::peer_directory::PersistBarrier;
use crate::client::{PeerDirectory, PeerRecord};

type LastErrorPatch = (Option<String>, Option<String>);

/// The sole in-process owner of configured-peer and transport observations.
///
/// Status-changing mutations publish a coherent snapshot. The watch channel
/// coalesces raw clock updates while still waking the bridge for product state,
/// route, pairing retry, or configured-peer changes.
#[derive(Clone)]
pub(in crate::client) struct ClientSyncStateOwner {
    tx: watch::Sender<ClientSyncStateSnapshot>,
    directory: Arc<RwLock<PeerDirectory>>,
}

impl ClientSyncStateOwner {
    #[cfg(test)]
    pub(super) async fn for_test(
        records: Vec<PeerRecord>,
        peers: Vec<ClientPeerStatus>,
    ) -> (tempfile::TempDir, Self) {
        let tempdir = tempfile::tempdir().expect("configured peer owner tempdir");
        let mut directory = PeerDirectory::open_writer(tempdir.path().join("peers.json"))
            .await
            .expect("load configured peer owner directory");
        for record in records {
            directory
                .upsert(record)
                .await
                .expect("seed configured peer owner directory");
        }
        let owner = Self::new(P2PHealth::default(), directory, peers);
        (tempdir, owner)
    }

    pub(in crate::client) fn new(
        transport: P2PHealth,
        directory: PeerDirectory,
        peers: Vec<ClientPeerStatus>,
    ) -> Self {
        let mut records = directory.records().to_vec();
        let directory = Arc::new(RwLock::new(directory));
        sort_records(&mut records);
        let mut peers = records
            .iter()
            .map(|record| {
                peers
                    .iter()
                    .find(|status| status.peer_id == record.peer_id)
                    .cloned()
                    .unwrap_or_else(|| conservative_status(record))
            })
            .collect::<Vec<_>>();
        sort_statuses(&mut peers);
        let (tx, _) = watch::channel(ClientSyncStateSnapshot {
            transport,
            database_sync: None,
            database_sync_error: None,
            directory: records,
            peers,
        });
        Self { tx, directory }
    }

    pub(super) fn snapshot(&self) -> ClientSyncStateSnapshot {
        self.tx.borrow().clone()
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<ClientSyncStateSnapshot> {
        self.tx.subscribe()
    }

    pub(super) fn peer(&self, peer_id: &str) -> Option<ClientPeerStatus> {
        self.tx
            .borrow()
            .peers
            .iter()
            .find(|status| status.peer_id == peer_id)
            .cloned()
    }

    pub(in crate::client) fn records(&self) -> Vec<PeerRecord> {
        self.tx.borrow().directory.clone()
    }

    pub(super) async fn pending_removals(&self) -> Vec<PeerRecord> {
        self.directory.read().await.pending_removals().to_vec()
    }

    pub(super) async fn clear_ephemeral_pairing_readiness(&self) -> anyhow::Result<()> {
        let mut directory = self.directory.write().await;
        directory.clear_ephemeral_pairing_readiness().await?;
        let records = directory.records().to_vec();
        self.publish_persisted_directory(records);
        Ok(())
    }

    #[cfg(test)]
    async fn set_directory_persist_barrier(&self, barrier: Option<PersistBarrier>) {
        self.directory.write().await.set_persist_barrier(barrier);
    }

    pub(super) async fn has_pending_removal(&self, expected: &PeerRecord) -> bool {
        self.directory.read().await.has_pending_removal(expected)
    }

    pub(super) async fn upsert_local_standard_peer(
        &self,
        label: &str,
        addr: &str,
        agent_did: &str,
        graphql: &str,
        agent_home: &str,
    ) -> anyhow::Result<PeerRecord> {
        let mut directory = self.directory.write().await;
        let record = directory
            .upsert_local_standard_peer(label, addr, agent_did, graphql, agent_home)
            .await?;
        let records = directory.records().to_vec();
        self.publish_persisted_directory(records);
        Ok(record)
    }

    pub(super) async fn upsert_enrollment_peer(
        &self,
        peer_id: &str,
        label: &str,
        addr: &str,
        agent_did: &str,
        network_id: &str,
        request_id: &str,
        request_digest: &str,
        admin_did: &str,
        authorization_sequence: u64,
        authorization_expires_at: &str,
    ) -> anyhow::Result<PeerRecord> {
        let mut directory = self.directory.write().await;
        let record = directory
            .upsert_enrollment_peer(
                peer_id,
                label,
                addr,
                agent_did,
                network_id,
                request_id,
                request_digest,
                admin_did,
                authorization_sequence,
                authorization_expires_at,
            )
            .await?;
        self.publish_persisted_directory(directory.records().to_vec());
        Ok(record)
    }

    pub(super) async fn replace_record(
        &self,
        expected: &PeerRecord,
        replacement: PeerRecord,
    ) -> anyhow::Result<Option<PeerRecord>> {
        let mut directory = self.directory.write().await;
        let replaced = directory.replace_if_matches(expected, replacement).await?;
        if replaced.is_some() {
            self.publish_persisted_directory(directory.records().to_vec());
        }
        Ok(replaced)
    }

    #[cfg(test)]
    pub(super) async fn upsert_for_test(&self, record: PeerRecord) -> anyhow::Result<()> {
        let mut directory = self.directory.write().await;
        directory.upsert(record).await?;
        self.publish_persisted_directory(directory.records().to_vec());
        Ok(())
    }

    #[cfg(test)]
    async fn upsert_with_publication_barrier(
        &self,
        record: PeerRecord,
        persisted: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> anyhow::Result<()> {
        let mut directory = self.directory.write().await;
        directory.upsert(record).await?;
        let records = directory.records().to_vec();
        persisted.notify_one();
        release.notified().await;
        self.publish_persisted_directory(records);
        Ok(())
    }

    pub(super) async fn set_pairing_ready(
        &self,
        expected: &PeerRecord,
        ready: bool,
    ) -> anyhow::Result<Option<PeerRecord>> {
        let mut directory = self.directory.write().await;
        if !directory.records().iter().any(|record| record == expected) {
            return Ok(None);
        }
        let record = directory
            .set_pairing_ready(&expected.peer_id, ready)
            .await?;
        if record.is_some() {
            self.publish_persisted_directory(directory.records().to_vec());
        }
        Ok(record)
    }

    #[cfg(test)]
    async fn set_pairing_ready_with_publication_barrier(
        &self,
        expected: &PeerRecord,
        ready: bool,
        persisted: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    ) -> anyhow::Result<Option<PeerRecord>> {
        let mut directory = self.directory.write().await;
        if !directory.records().iter().any(|record| record == expected) {
            return Ok(None);
        }
        let record = directory
            .set_pairing_ready(&expected.peer_id, ready)
            .await?;
        if record.is_some() {
            let records = directory.records().to_vec();
            persisted.notify_one();
            release.notified().await;
            self.publish_persisted_directory(records);
        }
        Ok(record)
    }

    pub(super) async fn queue_removal(
        &self,
        expected: &PeerRecord,
    ) -> anyhow::Result<Option<PeerRecord>> {
        let mut directory = self.directory.write().await;
        let removed = directory.queue_removal(expected).await?;
        let records = directory.records().to_vec();
        self.publish_persisted_directory(records);
        Ok(removed)
    }

    pub(super) async fn complete_removal_if_matches(
        &self,
        expected: &PeerRecord,
    ) -> anyhow::Result<bool> {
        let mut directory = self.directory.write().await;
        let completed = directory.complete_removal_if_matches(expected).await?;
        if completed {
            self.publish_persisted_directory(directory.records().to_vec());
        }
        Ok(completed)
    }

    pub(super) fn replace_database_observation(
        &self,
        transport: P2PHealth,
        database_sync: Option<P2pSyncStatusSnapshot>,
        database_sync_error: Option<String>,
    ) {
        self.tx.send_if_modified(|state| {
            let transport_changed = p2p_health_materially_changed(&state.transport, &transport);
            let previous_health = crate::client::project_sync_health(state);
            state.transport = transport;
            state.database_sync = database_sync;
            state.database_sync_error = database_sync_error;
            transport_changed || previous_health != crate::client::project_sync_health(state)
        });
    }

    pub(super) fn set_last_error_for_records(
        &self,
        expected_records: &[PeerRecord],
        message: String,
    ) -> bool {
        self.tx.send_if_modified(|state| {
            let previous = state.peers.clone();
            for expected in expected_records {
                if !state.directory.iter().any(|record| record == expected) {
                    continue;
                }
                if let Some(status) = state.peers.iter_mut().find(|status| {
                    status.peer_id == expected.peer_id && status.addr == expected.addr
                }) {
                    status.last_error = Some(message.clone());
                }
            }
            state.peers != previous
        })
    }

    /// Patch one diagnostic only while both the durable peer generation and
    /// the previously observed diagnostic still match. Delayed projection
    /// work cannot use this path to overwrite newer supervisor state.
    #[cfg(test)]
    pub(super) fn compare_and_set_last_error(
        &self,
        expected: &PeerRecord,
        expected_error: &Option<String>,
        next_error: Option<String>,
    ) -> bool {
        self.tx.send_if_modified(|state| {
            if !state.directory.iter().any(|record| record == expected) {
                return false;
            }
            let Some(status) = state
                .peers
                .iter_mut()
                .find(|status| status.peer_id == expected.peer_id && status.addr == expected.addr)
            else {
                return false;
            };
            if status.last_error != *expected_error || status.last_error == next_error {
                return false;
            }
            status.last_error = next_error;
            true
        })
    }

    /// Replace transport observations only for the exact currently persisted
    /// deployment. This path can never create or resurrect directory state.
    pub(super) fn replace_peer(&self, expected: &PeerRecord, status: ClientPeerStatus) -> bool {
        self.tx.send_if_modified(|state| {
            if !state.directory.iter().any(|record| record == expected)
                || status.peer_id != expected.peer_id
                || status.addr != expected.addr
            {
                return false;
            }
            let Some(existing) = state.peers.iter_mut().find(|existing| {
                existing.peer_id == status.peer_id && existing.addr == status.addr
            }) else {
                return false;
            };
            if *existing == status {
                return false;
            }
            *existing = status;
            true
        })
    }

    fn publish_persisted_directory(&self, mut records: Vec<PeerRecord>) {
        self.publish_persisted_directory_inner(&mut records, None);
    }

    fn publish_persisted_directory_inner(
        &self,
        records: &mut Vec<PeerRecord>,
        last_error_patch: Option<(String, LastErrorPatch)>,
    ) {
        sort_records(records);
        self.tx.send_if_modified(|state| {
            let previous_directory = state.directory.clone();
            let previous_peers = state.peers.clone();
            state.peers = records
                .iter()
                .map(|record| {
                    let mut status = state
                        .directory
                        .iter()
                        .find(|previous| previous.peer_id == record.peer_id)
                        .zip(
                            state
                                .peers
                                .iter()
                                .find(|status| status.peer_id == record.peer_id),
                        )
                        .filter(|(previous, _)| {
                            observation_generation_matches(previous, record)
                                && !(previous.pairing_ready && !record.pairing_ready)
                        })
                        .map(|(_, status)| {
                            let mut status = status.clone();
                            status.label.clone_from(&record.label);
                            status.agent_did.clone_from(&record.agent_did);
                            status.addr.clone_from(&record.addr);
                            status
                        })
                        .unwrap_or_else(|| conservative_status(record));
                    if let Some((peer_id, (expected, next))) = last_error_patch.as_ref() {
                        if peer_id == &record.peer_id && status.last_error == *expected {
                            status.last_error.clone_from(next);
                        }
                    }
                    status
                })
                .collect();
            state.directory.clone_from(records);
            state.directory != previous_directory || state.peers != previous_peers
        });
    }

    pub(super) fn update_peer(
        &self,
        expected: &PeerRecord,
        update: impl FnOnce(&mut ClientPeerStatus),
    ) -> bool {
        self.tx.send_if_modified(|state| {
            if !state.directory.iter().any(|record| record == expected) {
                return false;
            }
            let Some(peer) = state
                .peers
                .iter_mut()
                .find(|peer| peer.peer_id == expected.peer_id && peer.addr == expected.addr)
            else {
                return false;
            };
            let previous = peer.clone();
            update(peer);
            *peer != previous
        })
    }
}

fn observation_generation_matches(previous: &PeerRecord, current: &PeerRecord) -> bool {
    previous.peer_id == current.peer_id
        && previous.addr == current.addr
        && previous.agent_did == current.agent_did
        && previous.source == current.source
        && previous.pairing_network_id == current.pairing_network_id
        && previous.pairing_template == current.pairing_template
        && previous.enrollment_request_digest == current.enrollment_request_digest
        && previous.enrollment_authorization_sequence == current.enrollment_authorization_sequence
        && previous.graphql == current.graphql
}

fn sort_records(records: &mut [PeerRecord]) {
    records.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.peer_id.cmp(&right.peer_id))
    });
}

fn sort_statuses(statuses: &mut [ClientPeerStatus]) {
    statuses.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.peer_id.cmp(&right.peer_id))
    });
}

pub(super) fn conservative_status(record: &PeerRecord) -> ClientPeerStatus {
    ClientPeerStatus {
        peer_id: record.peer_id.clone(),
        label: record.label.clone(),
        agent_did: record.agent_did.clone(),
        addr: record.addr.clone(),
        dial_succeeded: false,
        last_error: None,
        pairing: Vec::new(),
        routes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(peer_id: &str) -> ClientPeerStatus {
        ClientPeerStatus {
            peer_id: peer_id.to_string(),
            label: peer_id.to_string(),
            agent_did: format!("did:key:{peer_id}"),
            addr: format!("endpoint:{peer_id}"),
            dial_succeeded: false,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
        }
    }

    fn record(peer_id: &str) -> PeerRecord {
        let mut record = PeerRecord::new(
            peer_id,
            format!("endpoint:{peer_id}"),
            format!("did:key:{peer_id}"),
        );
        record.peer_id = peer_id.to_string();
        record
    }

    fn assert_one_status_per_record(snapshot: &ClientSyncStateSnapshot) {
        assert_eq!(snapshot.directory.len(), snapshot.peers.len());
        assert!(snapshot.directory.iter().all(|record| snapshot
            .peers
            .iter()
            .any(|status| status.peer_id == record.peer_id)));
    }

    #[test]
    fn configured_peer_mutation_callers_cannot_bypass_the_owner() {
        fn without_trailing_test_module(source: &str) -> &str {
            [
                "\n#[cfg(test)]\nmod tests {",
                "\n#[cfg(test)]\nmod pairing_reconcile_tests {",
                "\n#[cfg(test)]\nmod delete_source_tests {",
            ]
            .into_iter()
            .filter_map(|marker| source.find(marker))
            .min()
            .map_or(source, |index| &source[..index])
        }

        for (name, source) in [
            ("core", include_str!("../core.rs")),
            ("writes", include_str!("writes.rs")),
            ("route_manager", include_str!("route_manager.rs")),
            ("supervisor", include_str!("supervisor.rs")),
            ("observer", include_str!("../observe.rs")),
        ] {
            let production = without_trailing_test_module(source);
            assert!(
                !production.contains(".peer_directory"),
                "{name} retained direct ClientCore peer-directory access"
            );
            assert!(
                !production.contains("PeerDirectory::"),
                "{name} retained a direct PeerDirectory mutation seam"
            );
        }
    }

    #[tokio::test]
    async fn stale_removal_cannot_tombstone_or_hide_a_repaired_generation() {
        let original = record("a");
        let (tempdir, owner) =
            ClientSyncStateOwner::for_test(vec![original.clone()], vec![peer("a")]).await;
        let mut repaired = original.clone();
        repaired.addr = "endpoint:a-repaired".to_string();
        repaired.pairing_ready = true;
        let persisted = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());

        let repair_owner = owner.clone();
        let repair_persisted = persisted.clone();
        let repair_release = release.clone();
        let repair = tokio::spawn(async move {
            repair_owner
                .upsert_with_publication_barrier(repaired, repair_persisted, repair_release)
                .await
        });
        persisted.notified().await;

        let removal_owner = owner.clone();
        let removal_expected = original;
        let stale_expected = removal_expected.clone();
        let removal =
            tokio::spawn(async move { removal_owner.queue_removal(&removal_expected).await });
        tokio::task::yield_now().await;
        assert!(
            !removal.is_finished(),
            "removal must wait for persistence and publication to finish under one owner lock"
        );

        release.notify_one();
        repair.await.unwrap().unwrap();
        assert!(removal.await.unwrap().unwrap().is_none());

        let (persisted, pending_removals) =
            load_peer_directory_snapshot(&tempdir.path().join("peers.json"))
                .await
                .unwrap();
        let snapshot = owner.snapshot();
        assert_eq!(persisted[0].addr, "endpoint:a-repaired");
        assert_eq!(snapshot.directory[0].addr, "endpoint:a-repaired");
        assert_one_status_per_record(&snapshot);
        assert!(pending_removals.is_empty());

        let mut stale_status = peer("a");
        stale_status.addr = "endpoint:a-repaired".to_string();
        stale_status.dial_succeeded = true;
        assert!(!owner.replace_peer(&stale_expected, stale_status));
        assert!(!owner.snapshot().peers[0].dial_succeeded);
    }

    #[tokio::test]
    async fn cancelled_persist_does_not_change_memory_watch_or_durable_directory() {
        let original = record("a");
        let (tempdir, owner) =
            ClientSyncStateOwner::for_test(vec![original.clone()], vec![peer("a")]).await;
        let updates = owner.subscribe();
        let written = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        owner
            .set_directory_persist_barrier(Some(PersistBarrier {
                written: written.clone(),
                release,
            }))
            .await;

        let mut cancelled_record = record("cancelled");
        cancelled_record.addr = "endpoint:never-committed".to_string();
        let cancelled_owner = owner.clone();
        let operation =
            tokio::spawn(async move { cancelled_owner.upsert_for_test(cancelled_record).await });
        written.notified().await;
        operation.abort();
        assert!(operation.await.unwrap_err().is_cancelled());

        assert_eq!(owner.records(), vec![original.clone()]);
        assert!(!updates.has_changed().expect("watch remains open"));
        let (persisted, _) = load_peer_directory_snapshot(&tempdir.path().join("peers.json"))
            .await
            .unwrap();
        assert_eq!(persisted, vec![original]);

        owner.set_directory_persist_barrier(None).await;
        owner.upsert_for_test(record("later")).await.unwrap();
        let (persisted, _) = load_peer_directory_snapshot(&tempdir.path().join("peers.json"))
            .await
            .unwrap();
        assert_eq!(
            persisted
                .iter()
                .map(|record| record.peer_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "later"]
        );
    }

    #[tokio::test]
    async fn live_owner_rejects_an_offline_initializer_on_the_same_directory() {
        let (tempdir, owner) = ClientSyncStateOwner::for_test(Vec::new(), Vec::new()).await;
        let path = tempdir.path().join("peers.json");

        let error = crate::client::initialize_local_standard_peer(
            &path,
            "Competing initializer",
            "endpoint:other",
            "did:key:other",
            "http://127.0.0.1:1/api/v0/graphql",
            "/tmp/test-agent-home",
        )
        .await
        .expect_err("live owner lease must reject an offline writer");
        assert!(error.to_string().contains("already owned"));
        assert!(owner.records().is_empty());

        drop(owner);
        let initialized = crate::client::initialize_local_standard_peer(
            &path,
            "Offline initializer",
            "endpoint:other",
            "did:key:other",
            "http://127.0.0.1:1/api/v0/graphql",
            "/tmp/test-agent-home",
        )
        .await
        .expect("offline initializer may write after live owner exits");
        assert_eq!(initialized.agent_did, "did:key:other");
    }

    #[tokio::test]
    async fn stale_index_failure_cannot_tag_a_new_peer_generation() {
        let old = record("a");
        let (_tempdir, owner) =
            ClientSyncStateOwner::for_test(vec![old.clone()], vec![peer("a")]).await;
        let mut repaired = old.clone();
        repaired.pairing_network_id = Some("network-new".to_string());
        owner
            .replace_record(&old, repaired.clone())
            .await
            .unwrap()
            .expect("replace generation");

        assert!(!owner.set_last_error_for_records(&[old], "stale index failure".to_string()));
        assert_eq!(
            owner.snapshot().directory[0].pairing_network_id,
            repaired.pairing_network_id
        );
        assert_eq!(owner.snapshot().peers[0].last_error, None);
    }

    #[tokio::test]
    async fn delayed_refresh_warning_cannot_clobber_newer_supervisor_status() {
        let expected = record("a");
        let activation_warning = Some("activation warning".to_string());
        let mut activated = peer("a");
        activated.dial_succeeded = true;
        activated.last_error.clone_from(&activation_warning);
        let (_tempdir, owner) =
            ClientSyncStateOwner::for_test(vec![expected.clone()], vec![activated]).await;

        let mut supervised = peer("a");
        supervised.dial_succeeded = true;
        supervised.last_error = Some("new supervisor diagnostic".to_string());
        supervised.routes.push(super::super::ClientRouteStatus {
            route_id: "route-new".to_string(),
            direction: "runtime-to-client".to_string(),
            directory_id: "a".to_string(),
            transport_peer_id: Some("transport-a".to_string()),
            address: Some("endpoint:a".to_string()),
            template: Some("client".to_string()),
            desired: true,
            applied: true,
            live_match: true,
            filter_summary: "owner-scoped".to_string(),
            last_error: None,
            retry_count: 0,
            last_retry_at: None,
            last_retry_error_class: None,
        });
        assert!(owner.replace_peer(&expected, supervised.clone()));

        assert!(!owner.compare_and_set_last_error(
            &expected,
            &activation_warning,
            Some("activation warning; delayed refresh warning".to_string()),
        ));
        assert_eq!(owner.snapshot().peers, vec![supervised]);
    }

    #[tokio::test]
    async fn readiness_publication_preserves_newer_transport_and_route_observations() {
        let (_tempdir, owner) =
            ClientSyncStateOwner::for_test(vec![record("a")], vec![peer("a")]).await;
        let persisted = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());

        let readiness_owner = owner.clone();
        let expected = owner.records()[0].clone();
        let readiness_expected = expected.clone();
        let readiness_persisted = persisted.clone();
        let readiness_release = release.clone();
        let readiness = tokio::spawn(async move {
            readiness_owner
                .set_pairing_ready_with_publication_barrier(
                    &readiness_expected,
                    true,
                    readiness_persisted,
                    readiness_release,
                )
                .await
        });
        persisted.notified().await;

        let mut newest = peer("a");
        newest.dial_succeeded = true;
        newest.routes.push(super::super::ClientRouteStatus {
            route_id: "route-new".to_string(),
            direction: "runtime-to-client".to_string(),
            directory_id: "a".to_string(),
            transport_peer_id: Some("transport-a".to_string()),
            address: Some("endpoint:a".to_string()),
            template: Some("client".to_string()),
            desired: true,
            applied: true,
            live_match: true,
            filter_summary: "owner-scoped".to_string(),
            last_error: None,
            retry_count: 0,
            last_retry_at: None,
            last_retry_error_class: None,
        });
        assert!(owner.replace_peer(&expected, newest.clone()));

        release.notify_one();
        readiness.await.unwrap().unwrap().expect("peer remains");
        let snapshot = owner.snapshot();
        assert!(snapshot.directory[0].pairing_ready);
        assert_eq!(snapshot.peers, vec![newest]);
    }

    #[tokio::test]
    async fn persisted_directory_insert_and_rotation_keep_conservative_status_invariant() {
        let (_tempdir, owner) = ClientSyncStateOwner::for_test(Vec::new(), Vec::new()).await;
        let mut updates = owner.subscribe();
        let mut configured = record("a");

        owner.upsert_for_test(configured.clone()).await.unwrap();
        assert!(updates.has_changed().expect("watch remains open"));
        let inserted = updates.borrow_and_update().clone();
        assert_one_status_per_record(&inserted);
        assert!(!inserted.peers[0].dial_succeeded);

        configured.addr = "endpoint:a-rotated".to_string();
        configured.pairing_ready = false;
        owner.upsert_for_test(configured).await.unwrap();
        assert!(updates.has_changed().expect("watch remains open"));
        let rotated = updates.borrow_and_update().clone();
        assert_one_status_per_record(&rotated);
        assert_eq!(rotated.peers[0].addr, "endpoint:a-rotated");
        assert!(!rotated.peers[0].dial_succeeded);
    }

    #[tokio::test]
    async fn peer_only_mutation_publishes_one_coherent_revision() {
        let (_tempdir, owner) =
            ClientSyncStateOwner::for_test(vec![record("b")], vec![peer("b")]).await;
        let mut updates = owner.subscribe();

        owner.upsert_for_test(record("a")).await.unwrap();

        assert!(updates.has_changed().expect("watch remains open"));
        let snapshot = updates.borrow_and_update().clone();
        assert_one_status_per_record(&snapshot);
        assert_eq!(
            snapshot
                .peers
                .iter()
                .map(|peer| peer.peer_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(snapshot.transport, P2PHealth::default());
    }

    #[tokio::test]
    async fn enrollment_generation_rotation_resets_disk_and_watch_readiness() {
        let (tempdir, owner) = ClientSyncStateOwner::for_test(Vec::new(), Vec::new()).await;
        let mut updates = owner.subscribe();
        let first = owner
            .upsert_enrollment_peer(
                "server-peer",
                "Server",
                "iroh://ticket",
                "did:key:owner",
                "network-a",
                "request-a",
                "digest-a",
                "did:key:admin",
                1,
                "2099-09-29T00:00:00Z",
            )
            .await
            .unwrap();
        let ready = owner
            .set_pairing_ready(&first, true)
            .await
            .unwrap()
            .expect("enrollment remains configured");
        assert!(ready.pairing_ready);
        let rotated = owner
            .upsert_enrollment_peer(
                "server-peer",
                "Server",
                "iroh://ticket",
                "did:key:owner",
                "network-b",
                "request-b",
                "digest-b",
                "did:key:admin",
                2,
                "2099-10-29T00:00:00Z",
            )
            .await
            .unwrap();
        assert!(!rotated.pairing_ready);
        assert_eq!(rotated.pairing_network_id.as_deref(), Some("network-b"));
        assert_eq!(
            rotated.enrollment_request_digest.as_deref(),
            Some("digest-b")
        );
        assert_eq!(rotated.enrollment_authorization_sequence, Some(2));
        assert!(updates.has_changed().expect("watch remains open"));
        let snapshot = updates.borrow_and_update().clone();
        assert_eq!(snapshot.directory, vec![rotated.clone()]);
        assert!(!snapshot.directory[0].pairing_ready);
        let (persisted, _) = load_peer_directory_snapshot(&tempdir.path().join("peers.json"))
            .await
            .unwrap();
        assert_eq!(persisted, vec![rotated]);
    }

    #[tokio::test]
    async fn unchanged_peer_mutation_does_not_publish() {
        let existing = peer("a");
        let (_tempdir, owner) =
            ClientSyncStateOwner::for_test(vec![record("a")], vec![existing.clone()]).await;
        let updates = owner.subscribe();

        let expected = owner.records()[0].clone();
        owner.replace_peer(&expected, existing);

        assert!(!updates.has_changed().expect("watch remains open"));
    }

    #[tokio::test]
    async fn retry_countdown_updates_raw_status_without_republishing_ui_state() {
        let (_tempdir, owner) = ClientSyncStateOwner::for_test(Vec::new(), Vec::new()).await;
        let mut updates = owner.subscribe();
        let first = P2pSyncStatusSnapshot {
            pending_dags: 1,
            next_pending_retry_in_ms: Some(1_000),
            ..P2pSyncStatusSnapshot::default()
        };
        owner.replace_database_observation(P2PHealth::default(), Some(first), None);
        assert!(updates.has_changed().expect("watch remains open"));
        updates.borrow_and_update();

        let second = P2pSyncStatusSnapshot {
            pending_dags: 1,
            next_pending_retry_in_ms: Some(900),
            ..P2pSyncStatusSnapshot::default()
        };
        owner.replace_database_observation(P2PHealth::default(), Some(second), None);

        assert!(!updates.has_changed().expect("watch remains open"));
        assert_eq!(
            owner
                .snapshot()
                .database_sync
                .expect("database observation")
                .next_pending_retry_in_ms,
            Some(900)
        );
    }
}
