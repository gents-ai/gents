use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::directory_projection::{DirectoryEntry, DirectoryStore, SourceSnapshot};
use crate::p2p_reconcile::diff::{DiffOp, PairingApplied, PairingDesired};
use crate::p2p_reconcile::engine::{
    EnrollmentMaterialization, EnrollmentRouteGeneration, LoadedPairingApplied,
    LoadedPairingDesired, PairingStateStore,
};
use crate::p2p_reconcile::enrollment_reconcile::EnrollmentProjectionReader;
use crate::p2p_reconcile::enrollment_store::EnrollmentProjection;
use crate::p2p_reconcile::templates::PairingFilters;
use crate::p2p_reconcile::{
    to_replication_filters, RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult,
    RemoteReplicator,
};

pub(crate) struct FailingStore {
    pub(crate) entries: tokio::sync::Mutex<BTreeMap<String, DirectoryEntry>>,
    pub(crate) deleted: BTreeSet<String>,
}

#[async_trait]
impl DirectoryStore for FailingStore {
    async fn load_source_snapshot(&self) -> Result<SourceSnapshot> {
        Ok(SourceSnapshot::default())
    }

    async fn list_directory_entries(
        &self,
        source_did: &str,
    ) -> Result<BTreeMap<String, DirectoryEntry>> {
        Ok(self
            .entries
            .lock()
            .await
            .iter()
            .filter(|(_, entry)| entry.source_did == source_did)
            .map(|(did, entry)| (did.clone(), entry.clone()))
            .collect())
    }

    async fn upsert_directory_entry(&self, _entry: &DirectoryEntry) -> Result<()> {
        anyhow::bail!("unexpected upsert")
    }

    async fn delete_directory_entry(&self, _source_did: &str, node_did: &str) -> Result<()> {
        anyhow::ensure!(self.deleted.contains(node_did), "injected delete failure");
        self.entries.lock().await.remove(node_did);
        Ok(())
    }
}

pub(crate) struct CurrentProjectionReader(pub(crate) Arc<Mutex<EnrollmentProjection>>);

#[async_trait]
impl EnrollmentProjectionReader for CurrentProjectionReader {
    async fn load_authority_projection(&self) -> Result<EnrollmentProjection> {
        Ok(self.0.lock().unwrap().clone())
    }
}

pub(crate) fn mock_collection_id(name: &str) -> String {
    format!("col_{name}_id")
}

pub(crate) fn mock_live_filters(filters: &PairingFilters) -> defra_p2p_adapter::ReplicationFilters {
    to_replication_filters(filters)
        .expect("mock filters are representable")
        .into_iter()
        .map(|(collection, filter)| (mock_collection_id(&collection), filter))
        .collect()
}

#[derive(Default)]
pub(crate) struct MockAdmin {
    pub(crate) replicators: Mutex<BTreeMap<String, RemoteReplicator>>,
    pub(crate) emitted: Mutex<Vec<DiffOp>>,
    pub(crate) connects: Mutex<Vec<Vec<String>>>,
    pub(crate) recorded_filters: Mutex<Vec<(Vec<String>, PairingFilters)>>,
    pub(crate) deleted_replicator_collections: Mutex<Vec<Vec<String>>>,
    pub(crate) active: Mutex<Vec<String>>,
    pub(crate) fail_active_peers: bool,
    pub(crate) active_peers_started: Option<Arc<tokio::sync::Notify>>,
    pub(crate) active_peers_release: Option<Arc<tokio::sync::Notify>>,
    pub(crate) fail_connect: bool,
    pub(crate) blocked_connect_address: Option<String>,
    pub(crate) blocked_connect_started: Option<Arc<tokio::sync::Notify>>,
    pub(crate) blocked_connect_release: Option<Arc<tokio::sync::Notify>>,
    pub(crate) replicator_installed: Option<Arc<tokio::sync::Notify>>,
    pub(crate) fail_add_replicator_attempts: Mutex<usize>,
}

#[async_trait]
impl RemoteP2pAdmin for MockAdmin {
    async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
        if let Some(started) = &self.active_peers_started {
            started.notify_one();
        }
        if let Some(release) = &self.active_peers_release {
            release.notified().await;
        }
        if self.fail_active_peers {
            return Err(RemoteP2pAdminError::RpcError("active_peers down".into()));
        }
        Ok(self.active.lock().unwrap().clone())
    }

    async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()> {
        self.connects.lock().unwrap().push(addresses.to_vec());
        if self
            .blocked_connect_address
            .as_ref()
            .is_some_and(|blocked| addresses.iter().any(|address| address == blocked))
        {
            if let Some(started) = &self.blocked_connect_started {
                started.notify_one();
            }
            if let Some(release) = &self.blocked_connect_release {
                release.notified().await;
            }
        }
        if self.fail_connect {
            return Err(RemoteP2pAdminError::RpcTimeout);
        }
        Ok(())
    }

    async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
        Ok(self.replicators.lock().unwrap().values().cloned().collect())
    }

    async fn add_replicator(
        &self,
        addresses: &[String],
        collections: &[String],
        filters: &PairingFilters,
    ) -> RemoteP2pAdminResult<()> {
        self.recorded_filters
            .lock()
            .unwrap()
            .push((addresses.to_vec(), filters.clone()));
        let mut remaining_failures = self.fail_add_replicator_attempts.lock().unwrap();
        if *remaining_failures > 0 {
            *remaining_failures -= 1;
            return Err(RemoteP2pAdminError::RpcError(
                "transient add_replicator failure".into(),
            ));
        }
        drop(remaining_failures);
        let resolved_filters = mock_live_filters(filters);
        for address in addresses {
            self.replicators.lock().unwrap().insert(
                address.clone(),
                RemoteReplicator {
                    id: Some(format!("id-{address}")),
                    collections: collections.iter().map(|c| mock_collection_id(c)).collect(),
                    address: Some(address.clone()),
                    filters: Some(resolved_filters.clone()),
                },
            );
            self.emitted
                .lock()
                .unwrap()
                .push(DiffOp::InstallReplicator(address.clone()));
            if let Some(installed) = &self.replicator_installed {
                installed.notify_one();
            }
        }
        Ok(())
    }

    async fn delete_replicator(
        &self,
        id: &str,
        collections: &[String],
    ) -> RemoteP2pAdminResult<()> {
        self.deleted_replicator_collections
            .lock()
            .unwrap()
            .push(collections.to_vec());
        let key = self
            .replicators
            .lock()
            .unwrap()
            .iter()
            .find_map(|(address, replicator)| {
                (replicator.id.as_deref() == Some(id) || address == id).then(|| address.clone())
            });
        if let Some(key) = key {
            self.replicators.lock().unwrap().remove(&key);
            self.emitted
                .lock()
                .unwrap()
                .push(DiffOp::TeardownReplicator(key));
        }
        Ok(())
    }

    async fn resolve_collection_id(&self, name: &str) -> RemoteP2pAdminResult<Option<String>> {
        Ok(Some(mock_collection_id(name)))
    }

    async fn resolve_collection_name(&self, id: &str) -> RemoteP2pAdminResult<Option<String>> {
        Ok(id
            .strip_prefix("col_")
            .and_then(|rest| rest.strip_suffix("_id"))
            .map(str::to_string))
    }
}

pub(crate) struct MockStore {
    pub(crate) desired: Mutex<Result<Option<PairingDesired>, String>>,
    pub(crate) desired_reads: Mutex<VecDeque<PairingDesired>>,
    pub(crate) applied: Mutex<PairingApplied>,
    pub(crate) saved: Mutex<Vec<PairingApplied>>,
    pub(crate) deleted: Mutex<usize>,
    pub(crate) list_peer_ids_failures: Mutex<usize>,
    pub(crate) list_peer_ids_calls: Mutex<usize>,
    pub(crate) list_peer_ids_retry_started: Option<Arc<tokio::sync::Notify>>,
    pub(crate) list_peer_ids_retry_release: Option<Arc<tokio::sync::Notify>>,
    pub(crate) persist_applied_completed: Option<Arc<tokio::sync::Notify>>,
}

impl Default for MockStore {
    fn default() -> Self {
        Self {
            desired: Mutex::new(Ok(None)),
            desired_reads: Mutex::new(VecDeque::new()),
            applied: Mutex::new(PairingApplied::default()),
            saved: Mutex::new(Vec::new()),
            deleted: Mutex::new(0),
            list_peer_ids_failures: Mutex::new(0),
            list_peer_ids_calls: Mutex::new(0),
            list_peer_ids_retry_started: None,
            list_peer_ids_retry_release: None,
            persist_applied_completed: None,
        }
    }
}

impl MockStore {
    pub(crate) fn with_desired(desired: Option<PairingDesired>) -> Self {
        Self {
            desired: Mutex::new(Ok(desired)),
            ..Default::default()
        }
    }

    pub(crate) fn with_desired_reads(
        reads: impl Into<VecDeque<PairingDesired>>,
        applied: PairingApplied,
    ) -> Self {
        Self {
            desired_reads: Mutex::new(reads.into()),
            applied: Mutex::new(applied),
            ..Default::default()
        }
    }
}

#[async_trait]
impl PairingStateStore for MockStore {
    async fn load_desired(&self, _peer_id: &str) -> Result<Option<PairingDesired>> {
        let mut reads = self.desired_reads.lock().unwrap();
        if !reads.is_empty() {
            let read = if reads.len() > 1 {
                reads.pop_front()
            } else {
                reads.front().cloned()
            };
            return Ok(read);
        }
        drop(reads);
        self.desired
            .lock()
            .unwrap()
            .clone()
            .map_err(|message| anyhow!(message))
    }

    async fn load_applied(&self, _peer_id: &str) -> Result<LoadedPairingApplied> {
        Ok(LoadedPairingApplied {
            state: self.applied.lock().unwrap().clone(),
            ..Default::default()
        })
    }

    async fn persist_applied(&self, _peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        *self.applied.lock().unwrap() = applied.state.clone();
        if applied.state.is_empty() {
            *self.deleted.lock().unwrap() += 1;
        } else {
            self.saved.lock().unwrap().push(applied.state.clone());
            if let Some(completed) = &self.persist_applied_completed {
                completed.notify_one();
            }
        }
        Ok(())
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        *self.list_peer_ids_calls.lock().unwrap() += 1;
        let should_fail = {
            let mut failures = self.list_peer_ids_failures.lock().unwrap();
            if *failures == 0 {
                false
            } else {
                *failures -= 1;
                true
            }
        };
        if should_fail {
            anyhow::bail!("transient list_peer_ids failure");
        }
        if let Some(started) = &self.list_peer_ids_retry_started {
            started.notify_one();
        }
        if let Some(release) = &self.list_peer_ids_retry_release {
            release.notified().await;
        }
        Ok(["peer-a".to_owned()].into_iter().collect())
    }
}

pub(crate) struct MultiPeerStore {
    pub(crate) desired: BTreeMap<String, PairingDesired>,
    pub(crate) applied: Mutex<BTreeMap<String, PairingApplied>>,
}

#[async_trait]
impl PairingStateStore for MultiPeerStore {
    async fn load_desired(&self, peer_id: &str) -> Result<Option<PairingDesired>> {
        Ok(self.desired.get(peer_id).cloned())
    }

    async fn load_applied(&self, peer_id: &str) -> Result<LoadedPairingApplied> {
        Ok(LoadedPairingApplied {
            state: self
                .applied
                .lock()
                .unwrap()
                .get(peer_id)
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        })
    }

    async fn persist_applied(&self, peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        let mut states = self.applied.lock().unwrap();
        if applied.state.is_empty() {
            states.remove(peer_id);
        } else {
            states.insert(peer_id.to_string(), applied.state.clone());
        }
        Ok(())
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        Ok(self.desired.keys().cloned().collect())
    }
}

pub(crate) struct EnrollmentFenceStore {
    pub(crate) desired: PairingDesired,
    pub(crate) generation: EnrollmentRouteGeneration,
    pub(crate) applied: Mutex<PairingApplied>,
    pub(crate) materialization: Mutex<EnrollmentMaterialization>,
    pub(crate) saved: Mutex<Vec<LoadedPairingApplied>>,
    pub(crate) authority_checks: Mutex<VecDeque<Result<bool, String>>>,
}

#[async_trait]
impl PairingStateStore for EnrollmentFenceStore {
    async fn load_desired(&self, _peer_id: &str) -> Result<Option<PairingDesired>> {
        Ok(Some(self.desired.clone()))
    }

    async fn load_desired_with_authority(&self, _peer_id: &str) -> Result<LoadedPairingDesired> {
        Ok(LoadedPairingDesired {
            state: Some(self.desired.clone()),
            enrollment_generation: Some(self.generation.clone()),
        })
    }

    async fn enrollment_generation_is_current(
        &self,
        _generation: &EnrollmentRouteGeneration,
    ) -> Result<bool> {
        self.authority_checks
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(false))
            .map_err(anyhow::Error::msg)
    }

    async fn load_applied(&self, _peer_id: &str) -> Result<LoadedPairingApplied> {
        Ok(LoadedPairingApplied {
            state: self.applied.lock().unwrap().clone(),
            enrollment: self.materialization.lock().unwrap().clone(),
            ..Default::default()
        })
    }

    async fn persist_applied(&self, _peer_id: &str, applied: &LoadedPairingApplied) -> Result<()> {
        self.saved.lock().unwrap().push(applied.clone());
        *self.applied.lock().unwrap() = applied.state.clone();
        *self.materialization.lock().unwrap() = applied.enrollment.clone();
        Ok(())
    }

    async fn list_peer_ids(&self) -> Result<BTreeSet<String>> {
        Ok(["peer-a".to_owned()].into_iter().collect())
    }
}

struct NoToolSources;

pub(crate) fn no_tool_sources() -> Arc<dyn gents_store::local::ToolSourcePolicy> {
    Arc::new(NoToolSources)
}

impl gents_store::local::ToolSourcePolicy for NoToolSources {
    fn node_operation_sources(
        &self,
        _tools: &serde_json::Value,
        _surfaces: &[serde_json::Value],
    ) -> Result<Vec<gents_store::local::ProjectedOperationSource>> {
        Ok(Vec::new())
    }

    fn plugin_refs(
        &self,
        _tools: &serde_json::Value,
    ) -> Result<Vec<gents_store::local::ProjectedPluginRef>> {
        Ok(Vec::new())
    }

    fn selected_tools(&self, _node: &str, _context: &serde_json::Value) -> Option<String> {
        None
    }
}
