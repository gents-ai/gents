use std::{
    fs::{File, OpenOptions, TryLockError},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(test)]
use tokio::sync::Notify;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerRecord {
    pub peer_id: String,
    pub label: String,
    pub addr: String,
    pub agent_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_behavior_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_network_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_template: Option<String>,
    #[serde(default)]
    pub pairing_ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl PeerRecord {
    pub fn is_bearer_pairing(&self) -> bool {
        self.source.as_deref() == Some(super::core::bearer_pairing::BEARER_PAIRING_SOURCE)
    }

    pub fn is_chat_ready(&self) -> bool {
        self.source.as_deref() == Some("local-standard")
            || (self.source.is_none() && self.graphql.is_none())
            || self.pairing_ready
    }

    pub fn new(
        label: impl Into<String>,
        addr: impl Into<String>,
        agent_did: impl Into<String>,
    ) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            peer_id: Uuid::new_v4().to_string(),
            label: label.into(),
            addr: addr.into(),
            agent_did: agent_did.into(),
            default_behavior_id: None,
            source: None,
            pairing_network_id: None,
            pairing_template: None,
            pairing_ready: false,
            graphql: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }

    pub fn local_standard(
        label: impl Into<String>,
        addr: impl Into<String>,
        agent_did: impl Into<String>,
        graphql: impl Into<String>,
    ) -> Self {
        let mut record = Self::new(label, addr, agent_did);
        record.source = Some("local-standard".to_string());
        record.graphql = Some(graphql.into());
        record
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoredPeerDirectory {
    peers: Vec<PeerRecord>,
    #[serde(default)]
    pending_removals: Vec<PeerRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct PeerDirectory {
    path: PathBuf,
    peers: Vec<PeerRecord>,
    pending_removals: Vec<PeerRecord>,
    _lease: Arc<File>,
    #[cfg(test)]
    persist_barrier: Option<PersistBarrier>,
    #[cfg(test)]
    fail_persist: bool,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(in crate::client) struct PersistBarrier {
    pub written: Arc<Notify>,
    pub release: Arc<Notify>,
}

impl PeerDirectory {
    #[cfg(test)]
    async fn load(path: impl Into<PathBuf>) -> Result<Self> {
        Self::open_writer(path).await
    }

    pub(in crate::client) async fn open_writer(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let lock_path = path.with_extension("lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating peer-directory lock parent {}", parent.display())
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening peer-directory lock {}", lock_path.display()))?;
        match file.try_lock() {
            Ok(()) => {
                let lease = Arc::new(file);
                // Read only after acquiring the lifetime lease, so an offline
                // initializer cannot race a live owner's load with a write.
                let stored = read_stored_directory(&path).await?;
                let mut directory = Self {
                    path,
                    peers: stored.peers,
                    pending_removals: stored.pending_removals,
                    _lease: lease,
                    #[cfg(test)]
                    persist_barrier: None,
                    #[cfg(test)]
                    fail_persist: false,
                };
                directory.sort_records();
                Ok(directory)
            }
            Err(TryLockError::WouldBlock) => anyhow::bail!(
                "peer directory {} is already owned by a running desktop process",
                path.display()
            ),
            Err(TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("locking peer directory {}", path.display()))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub(crate) fn records(&self) -> &[PeerRecord] {
        &self.peers
    }

    pub(in crate::client) fn pending_removals(&self) -> &[PeerRecord] {
        &self.pending_removals
    }

    pub(in crate::client) fn has_pending_removal(&self, expected: &PeerRecord) -> bool {
        self.pending_removals
            .iter()
            .any(|pending| pending == expected)
    }

    #[cfg(test)]
    pub(in crate::client) async fn upsert_saved_peer(
        &mut self,
        label: &str,
        addr: &str,
        agent_did: &str,
    ) -> Result<PeerRecord> {
        self.upsert_saved_peer_with_graphql(label, addr, agent_did, None, None)
            .await
    }

    pub(in crate::client) async fn upsert_saved_peer_with_graphql(
        &mut self,
        label: &str,
        addr: &str,
        agent_did: &str,
        graphql: Option<&str>,
        default_behavior_id: Option<&str>,
    ) -> Result<PeerRecord> {
        let label = normalize_non_empty("label", label)?;
        let addr = normalize_non_empty("addr", addr)?;
        let agent_did = normalize_non_empty("agent_did", agent_did)?;
        let graphql = normalize_optional(graphql);
        let default_behavior_id = normalize_optional(default_behavior_id);
        let incoming_source = graphql.map(|_| "server-status");

        let mut candidate = self.clone();
        let matching_active = candidate
            .peers
            .iter()
            .filter(|existing| {
                existing.agent_did == agent_did
                    && (existing.source.as_deref() == incoming_source
                        || (incoming_source == Some("server-status") && existing.source.is_none()))
            })
            .collect::<Vec<_>>();
        if matching_active.len() > 1 {
            anyhow::bail!(
                "multiple saved deployments already own agent {agent_did} for source {:?}; remove the duplicate explicitly before updating",
                incoming_source
            );
        }

        let active = matching_active.first().map(|record| (*record).clone());
        let pending_index = active.is_none().then(|| {
            candidate.pending_removals.iter().position(|existing| {
                existing.agent_did == agent_did
                    && (existing.source.as_deref() == incoming_source
                        || (incoming_source == Some("server-status") && existing.source.is_none()))
            })
        });
        let mut record = active
            .or_else(|| {
                pending_index
                    .flatten()
                    .map(|index| candidate.pending_removals.remove(index))
            })
            .unwrap_or_else(|| PeerRecord::new(label, addr, agent_did));
        if record.addr != addr {
            record.pairing_ready = false;
        }
        record.label = label.to_string();
        record.addr = addr.to_string();
        record.agent_did = agent_did.to_string();
        if record.source.as_deref() != Some(super::core::bearer_pairing::BEARER_PAIRING_SOURCE) {
            if let Some(default_behavior_id) = default_behavior_id {
                record.default_behavior_id = Some(default_behavior_id.to_string());
            }
            if let Some(graphql) = graphql {
                record.graphql = Some(graphql.to_string());
                if record.source.as_deref() != Some("local-standard") {
                    record.source = Some("server-status".to_string());
                }
            }
        }

        let record = candidate.apply_upsert(record);
        self.commit(candidate).await?;
        Ok(record)
    }

    pub(in crate::client) async fn upsert_local_standard_peer(
        &mut self,
        label: &str,
        addr: &str,
        agent_did: &str,
        graphql: &str,
    ) -> Result<PeerRecord> {
        let label = normalize_non_empty("label", label)?;
        let addr = normalize_non_empty("addr", addr)?;
        let agent_did = normalize_non_empty("agent_did", agent_did)?;
        let graphql = normalize_non_empty("graphql", graphql)?;

        let mut candidate = self.clone();
        let mut record = candidate
            .peers
            .iter()
            .find(|existing| existing.source.as_deref() == Some("local-standard"))
            .cloned()
            .unwrap_or_else(|| PeerRecord::local_standard(label, addr, agent_did, graphql));
        record.label = label.to_string();
        record.addr = addr.to_string();
        record.agent_did = agent_did.to_string();
        record.source = Some("local-standard".to_string());
        record.graphql = Some(graphql.to_string());

        let record = candidate.apply_upsert(record);
        self.commit(candidate).await?;
        Ok(record)
    }

    pub(in crate::client) async fn upsert_bearer_peer(
        &mut self,
        label: &str,
        addr: &str,
        agent_did: &str,
        network_id: &str,
        template: &str,
        default_behavior_id: Option<&str>,
    ) -> Result<PeerRecord> {
        let label = normalize_non_empty("label", label)?;
        let addr = normalize_non_empty("addr", addr)?;
        let agent_did = normalize_non_empty("agent_did", agent_did)?;
        let network_id = normalize_non_empty("network_id", network_id)?;
        let template = normalize_non_empty("template", template)?;
        let default_behavior_id = normalize_optional(default_behavior_id);

        let mut candidate = self.clone();
        let mut record = candidate
            .peers
            .iter()
            .find(|existing| existing.agent_did == agent_did)
            .cloned()
            .unwrap_or_else(|| PeerRecord::new(label, addr, agent_did));
        record.label = label.to_string();
        record.addr = addr.to_string();
        record.agent_did = agent_did.to_string();
        record.default_behavior_id = default_behavior_id.map(str::to_owned);
        record.source = Some(super::core::bearer_pairing::BEARER_PAIRING_SOURCE.to_string());
        record.pairing_network_id = Some(network_id.to_string());
        record.pairing_template = Some(template.to_string());
        record.pairing_ready = false;
        record.graphql = None;

        let record = candidate.apply_upsert(record);
        self.commit(candidate).await?;
        Ok(record)
    }

    pub(in crate::client) async fn set_bearer_pairing_ready(
        &mut self,
        peer_id: &str,
        ready: bool,
    ) -> Result<Option<PeerRecord>> {
        let mut candidate = self.clone();
        let Some(mut record) = candidate
            .peers
            .iter()
            .find(|record| record.peer_id == peer_id && record.is_bearer_pairing())
            .cloned()
        else {
            return Ok(None);
        };
        if record.pairing_ready == ready {
            return Ok(Some(record));
        }
        record.pairing_ready = ready;
        let record = candidate.apply_upsert(record);
        self.commit(candidate).await?;
        Ok(Some(record))
    }

    pub(in crate::client) async fn set_pairing_ready(
        &mut self,
        peer_id: &str,
        ready: bool,
    ) -> Result<Option<PeerRecord>> {
        let mut candidate = self.clone();
        let Some(mut record) = candidate
            .peers
            .iter()
            .find(|record| record.peer_id == peer_id)
            .cloned()
        else {
            return Ok(None);
        };
        if record.pairing_ready == ready {
            return Ok(Some(record));
        }
        record.pairing_ready = ready;
        let record = candidate.apply_upsert(record);
        self.commit(candidate).await?;
        Ok(Some(record))
    }

    #[cfg(test)]
    pub(in crate::client) async fn upsert(&mut self, record: PeerRecord) -> Result<()> {
        let mut candidate = self.clone();
        candidate.apply_upsert(record);
        self.commit(candidate).await
    }

    pub(in crate::client) async fn replace_if_matches(
        &mut self,
        expected: &PeerRecord,
        replacement: PeerRecord,
    ) -> Result<Option<PeerRecord>> {
        if !self.peers.iter().any(|record| record == expected) {
            return Ok(None);
        }
        anyhow::ensure!(
            replacement.peer_id == expected.peer_id,
            "replacement peer id must match the expected generation"
        );
        let mut candidate = self.clone();
        let replacement = candidate.apply_upsert(replacement);
        self.commit(candidate).await?;
        Ok(Some(replacement))
    }

    fn apply_upsert(&mut self, mut record: PeerRecord) -> PeerRecord {
        if let Some(existing) = self
            .peers
            .iter_mut()
            .find(|existing| existing.peer_id == record.peer_id)
        {
            record.created_at = existing.created_at.clone();
            record.updated_at = Utc::now().to_rfc3339();
            *existing = record.clone();
        } else {
            self.peers.push(record.clone());
        }

        self.sort_records();
        record
    }

    #[cfg(test)]
    pub(in crate::client) async fn remove(&mut self, peer_id: &str) -> Result<Option<PeerRecord>> {
        let mut candidate = self.clone();
        let removed = candidate
            .peers
            .iter()
            .position(|record| record.peer_id == peer_id)
            .map(|index| candidate.peers.remove(index));

        candidate.sort_records();
        self.commit(candidate).await?;
        Ok(removed)
    }

    pub(in crate::client) async fn queue_removal(
        &mut self,
        expected: &PeerRecord,
    ) -> Result<Option<PeerRecord>> {
        let mut candidate = self.clone();
        let Some(index) = candidate.peers.iter().position(|record| record == expected) else {
            return Ok(None);
        };
        let removed = candidate.peers.remove(index);
        candidate
            .pending_removals
            .retain(|record| record.peer_id != removed.peer_id);
        candidate.pending_removals.push(removed.clone());
        candidate.sort_records();
        self.commit(candidate).await?;
        Ok(Some(removed))
    }

    pub(in crate::client) async fn complete_removal_if_matches(
        &mut self,
        expected: &PeerRecord,
    ) -> Result<bool> {
        let mut candidate = self.clone();
        let Some(index) = candidate
            .pending_removals
            .iter()
            .position(|pending| pending == expected)
        else {
            return Ok(false);
        };
        candidate.pending_removals.remove(index);
        self.commit(candidate).await?;
        Ok(true)
    }

    /// Route readiness is an observation, not durable authority across a
    /// process restart. Managed and bearer deployments must re-establish both
    /// live legs before chat writes reopen. Legacy GraphQL-less rows keep their
    /// explicit one-way compatibility behavior in `PeerRecord::is_chat_ready`.
    pub(in crate::client) async fn clear_ephemeral_pairing_readiness(&mut self) -> Result<()> {
        let mut candidate = self.clone();
        let mut changed = false;
        for record in &mut candidate.peers {
            if record.source.as_deref() != Some("local-standard") && record.pairing_ready {
                record.pairing_ready = false;
                record.updated_at = Utc::now().to_rfc3339();
                changed = true;
            }
        }
        if changed {
            self.commit(candidate).await?;
        }
        Ok(())
    }

    async fn commit(&mut self, candidate: Self) -> Result<()> {
        candidate.save().await?;
        // There is deliberately no await between durable replacement and the
        // in-memory swap, so cancellation cannot expose half a commit.
        *self = candidate;
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::client) fn set_persist_barrier(&mut self, barrier: Option<PersistBarrier>) {
        self.persist_barrier = barrier;
    }

    #[cfg(test)]
    fn set_fail_persist(&mut self, fail: bool) {
        self.fail_persist = fail;
    }

    async fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating peer directory parent {}", parent.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(&StoredPeerDirectory {
            peers: self.peers.clone(),
            pending_removals: self.pending_removals.clone(),
        })?;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let mut staged = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("staging peer directory in {}", parent.display()))?;
        staged
            .write_all(&bytes)
            .with_context(|| format!("writing staged peer directory in {}", parent.display()))?;
        #[cfg(test)]
        if let Some(barrier) = &self.persist_barrier {
            barrier.written.notify_one();
            barrier.release.notified().await;
        }
        #[cfg(test)]
        if self.fail_persist {
            anyhow::bail!("injected peer-directory persistence failure");
        }
        // NamedTempFile::persist provides same-directory atomic replacement
        // on supported platforms. It and the caller's immediate in-memory
        // swap contain no cancellation point.
        staged
            .persist(&self.path)
            .with_context(|| format!("persisting {}", self.path.display()))?;
        Ok(())
    }

    fn sort_records(&mut self) {
        sort_peer_records(&mut self.peers);
    }
}

pub async fn initialize_local_standard_peer(
    path: &Path,
    label: &str,
    addr: &str,
    agent_did: &str,
    graphql: &str,
) -> Result<PeerRecord> {
    let mut directory = PeerDirectory::open_writer(path).await?;
    directory
        .upsert_local_standard_peer(label, addr, agent_did, graphql)
        .await
}

pub async fn load_peer_records(path: &Path) -> Result<Vec<PeerRecord>> {
    let mut records = read_stored_directory(path).await?.peers;
    sort_peer_records(&mut records);
    Ok(records)
}

#[cfg(test)]
pub(crate) async fn load_peer_directory_snapshot(
    path: &Path,
) -> Result<(Vec<PeerRecord>, Vec<PeerRecord>)> {
    let stored = read_stored_directory(path).await?;
    Ok((stored.peers, stored.pending_removals))
}

pub(crate) async fn initialize_status_endpoint_peer(
    path: &Path,
    label: &str,
    addr: &str,
    agent_did: &str,
    graphql: &str,
    default_behavior_id: Option<&str>,
) -> Result<PeerRecord> {
    let mut directory = PeerDirectory::open_writer(path).await?;
    directory
        .upsert_saved_peer_with_graphql(label, addr, agent_did, Some(graphql), default_behavior_id)
        .await
}

fn normalize_non_empty<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then_some(trimmed)
        .with_context(|| format!("{field} must not be empty"))
}

async fn read_stored_directory(path: &Path) -> Result<StoredPeerDirectory> {
    match tokio::fs::read(path).await {
        Ok(bytes) if bytes.is_empty() => Ok(StoredPeerDirectory::default()),
        Ok(bytes) => serde_json::from_slice::<StoredPeerDirectory>(&bytes)
            .with_context(|| format!("parsing peer directory {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(StoredPeerDirectory::default())
        }
        Err(error) => Err(anyhow::Error::from(error))
            .with_context(|| format!("reading peer directory {}", path.display())),
    }
}

fn sort_peer_records(records: &mut [PeerRecord]) {
    records.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.peer_id.cmp(&right.peer_id))
    });
}

fn normalize_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_graphql_less_peer_keeps_one_way_chat_compatibility() {
        let legacy = PeerRecord::new("Legacy bridge", "iroh://legacy", "did:test:legacy");
        assert!(legacy.is_chat_ready());

        let mut managed = legacy;
        managed.source = Some("server-status".to_string());
        managed.graphql = Some("http://runtime.local/graphql".to_string());
        assert!(!managed.is_chat_ready());
        managed.pairing_ready = true;
        assert!(managed.is_chat_ready());
    }

    #[tokio::test]
    async fn restart_clears_persisted_managed_readiness_before_chat_reopens() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();
        let mut managed = PeerRecord::new("Mandrake", "iroh://ticket", "did:test:mandrake");
        managed.source = Some("server-status".to_string());
        managed.graphql = Some("http://runtime.local/graphql".to_string());
        managed.pairing_ready = true;
        directory.upsert(managed.clone()).await.unwrap();

        directory.clear_ephemeral_pairing_readiness().await.unwrap();
        let record = directory
            .records()
            .iter()
            .find(|record| record.peer_id == managed.peer_id)
            .unwrap();
        assert!(!record.pairing_ready);
        assert!(!record.is_chat_ready());

        let reloaded = load_peer_records(&path).await.unwrap();
        assert!(!reloaded[0].pairing_ready);
    }

    #[tokio::test]
    async fn missing_file_loads_as_empty_directory() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");

        let directory = PeerDirectory::load(&path).await.unwrap();

        assert!(directory.is_empty());
    }

    #[tokio::test]
    async fn empty_file_loads_as_empty_directory() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        tokio::fs::write(&path, []).await.unwrap();

        let directory = PeerDirectory::load(&path).await.unwrap();

        assert!(directory.is_empty());
    }

    #[tokio::test]
    async fn add_update_remove_round_trip_persists() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();

        let mut record = PeerRecord::new("Workshop Bay", "iroh://alpha", "did:test:alpha");
        let peer_id = record.peer_id.clone();
        directory.upsert(record.clone()).await.unwrap();

        record.label = "Workshop Bay Updated".to_string();
        directory.upsert(record.clone()).await.unwrap();
        directory.remove(&peer_id).await.unwrap();

        let reloaded = load_peer_records(&path).await.unwrap();
        assert!(reloaded.is_empty());
    }

    #[tokio::test]
    async fn failed_removal_completion_restores_the_cleanup_tombstone() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();
        let record = PeerRecord::new("Amy", "iroh://amy", "did:test:amy");
        directory.upsert(record.clone()).await.unwrap();
        directory.queue_removal(&record).await.unwrap();

        tokio::fs::remove_file(&path).await.unwrap();
        tokio::fs::create_dir(&path).await.unwrap();
        let error = directory
            .complete_removal_if_matches(&record)
            .await
            .expect_err("directory persistence failure must remain retryable");

        assert!(error.to_string().contains("persisting"));
        assert!(directory.has_pending_removal(&record));
    }

    #[tokio::test]
    async fn failed_save_cannot_leak_candidate_into_a_later_commit() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();
        let original = PeerRecord::new("Original", "iroh://original", "did:test:original");
        directory.upsert(original.clone()).await.unwrap();

        directory.set_fail_persist(true);
        let failed = PeerRecord::new("Failed", "iroh://failed", "did:test:failed");
        directory
            .upsert(failed.clone())
            .await
            .expect_err("injected persistence failure must reject the candidate");
        assert_eq!(directory.records(), &[original.clone()]);
        assert_eq!(load_peer_records(&path).await.unwrap(), &[original.clone()]);

        directory.set_fail_persist(false);
        let later = PeerRecord::new("Later", "iroh://later", "did:test:later");
        directory.upsert(later.clone()).await.unwrap();
        let reloaded = load_peer_records(&path).await.unwrap();
        assert_eq!(reloaded.len(), 2);
        assert!(reloaded.contains(&original));
        assert!(reloaded.contains(&later));
        assert!(!reloaded.contains(&failed));
    }

    #[tokio::test]
    async fn upsert_saved_peer_persists_graphql_pairing_endpoint() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();

        let record = directory
            .upsert_saved_peer_with_graphql(
                "Workshop Bay",
                "iroh://alpha",
                "did:key:z6MkAlpha",
                Some(" http://100.73.235.38:9181/api/v0/graphql "),
                Some(" default "),
            )
            .await
            .unwrap();

        assert_eq!(
            record.graphql.as_deref(),
            Some("http://100.73.235.38:9181/api/v0/graphql")
        );
        assert_eq!(record.source.as_deref(), Some("server-status"));
        assert_eq!(record.default_behavior_id.as_deref(), Some("default"));

        let reloaded = load_peer_records(&path).await.unwrap();
        assert_eq!(
            reloaded[0].graphql.as_deref(),
            Some("http://100.73.235.38:9181/api/v0/graphql")
        );
        assert_eq!(reloaded[0].default_behavior_id.as_deref(), Some("default"));
    }

    #[tokio::test]
    async fn records_are_sorted_for_deterministic_output() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();

        directory
            .upsert(PeerRecord::new("Zulu", "iroh://zulu", "did:test:zulu"))
            .await
            .unwrap();
        directory
            .upsert(PeerRecord::new("Alpha", "iroh://alpha", "did:test:alpha"))
            .await
            .unwrap();

        assert_eq!(directory.records()[0].label, "Alpha");
        assert_eq!(directory.records()[1].label, "Zulu");
    }

    #[tokio::test]
    async fn upsert_saved_peer_reuses_existing_record_for_same_addr_and_agent() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();

        let first = directory
            .upsert_saved_peer("Workshop Bay", "iroh://alpha", "did:test:alpha")
            .await
            .unwrap();
        let second = directory
            .upsert_saved_peer("Workshop Bay Updated", "iroh://alpha", "did:test:alpha")
            .await
            .unwrap();

        assert_eq!(first.peer_id, second.peer_id);
        assert_eq!(directory.records().len(), 1);
        assert_eq!(directory.records()[0].label, "Workshop Bay Updated");
    }

    #[tokio::test]
    async fn server_status_ticket_rotation_preserves_directory_id_and_clears_ready() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();
        let mut first = directory
            .upsert_saved_peer_with_graphql(
                "Mandrake",
                "iroh://old-ticket",
                "did:test:mandrake",
                Some("http://mandrake.local/graphql"),
                Some("mandrake-default"),
            )
            .await
            .unwrap();
        first.pairing_ready = true;
        directory.upsert(first.clone()).await.unwrap();

        let rotated = directory
            .upsert_saved_peer_with_graphql(
                "Mandrake",
                "iroh://new-ticket",
                "did:test:mandrake",
                Some("http://mandrake.local/graphql"),
                Some("mandrake-default"),
            )
            .await
            .unwrap();

        assert_eq!(rotated.peer_id, first.peer_id);
        assert_eq!(rotated.addr, "iroh://new-ticket");
        assert!(!rotated.pairing_ready);
        assert_eq!(directory.records().len(), 1);
    }

    #[tokio::test]
    async fn duplicate_agent_source_is_rejected_without_silent_record_loss() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();
        directory
            .upsert(PeerRecord::new(
                "First",
                "iroh://first",
                "did:test:duplicate",
            ))
            .await
            .unwrap();
        directory
            .upsert(PeerRecord::new(
                "Second",
                "iroh://second",
                "did:test:duplicate",
            ))
            .await
            .unwrap();

        let error = directory
            .upsert_saved_peer("Updated", "iroh://updated", "did:test:duplicate")
            .await
            .expect_err("ambiguous ownership must require explicit removal");
        assert!(error
            .to_string()
            .contains("remove the duplicate explicitly"));
        assert_eq!(directory.records().len(), 2);
    }

    #[tokio::test]
    async fn bearer_pairing_upgrades_existing_principal_and_rotates_iroh_ticket() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();

        let first = directory
            .upsert_saved_peer_with_graphql(
                "Amy manual",
                "iroh://old-ticket",
                "did:test:amy",
                Some("http://amy.local/graphql"),
                None,
            )
            .await
            .unwrap();
        let paired = directory
            .upsert_bearer_peer(
                "Amy",
                "iroh://fresh-ticket",
                "did:test:amy",
                "network-amy",
                "machine",
                Some("default"),
            )
            .await
            .unwrap();

        assert_eq!(first.peer_id, paired.peer_id);
        assert_eq!(directory.records().len(), 1);
        assert_eq!(paired.addr, "iroh://fresh-ticket");
        assert_eq!(paired.default_behavior_id.as_deref(), Some("default"));
        assert_eq!(paired.pairing_network_id.as_deref(), Some("network-amy"));
        assert_eq!(paired.pairing_template.as_deref(), Some("machine"));
        assert!(!paired.pairing_ready);
        assert_eq!(
            paired.source.as_deref(),
            Some(crate::client::core::bearer_pairing::BEARER_PAIRING_SOURCE)
        );
        assert_eq!(paired.graphql, None);
    }

    #[tokio::test]
    async fn upsert_local_standard_peer_tracks_graphql_and_source() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();

        let first = directory
            .upsert_local_standard_peer(
                "Local Agent",
                "iroh://first",
                "did:test:default",
                "http://127.0.0.1:9191/api/v0/graphql",
            )
            .await
            .unwrap();
        let second = directory
            .upsert_local_standard_peer(
                "Local Agent Updated",
                "iroh://second",
                "did:test:default",
                "http://127.0.0.1:9192/api/v0/graphql",
            )
            .await
            .unwrap();

        assert_eq!(first.peer_id, second.peer_id);
        assert_eq!(directory.records().len(), 1);
        assert_eq!(
            directory.records()[0].source.as_deref(),
            Some("local-standard")
        );
        assert_eq!(
            directory.records()[0].graphql.as_deref(),
            Some("http://127.0.0.1:9192/api/v0/graphql")
        );

        let reloaded = load_peer_records(&path).await.unwrap();
        assert_eq!(reloaded[0].peer_id, first.peer_id);
        assert_eq!(reloaded[0].addr, "iroh://second");
        assert_eq!(
            reloaded[0].graphql.as_deref(),
            Some("http://127.0.0.1:9192/api/v0/graphql")
        );
    }
}
