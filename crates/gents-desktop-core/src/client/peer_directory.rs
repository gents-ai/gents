use std::{
    fs::{File, OpenOptions, TryLockError},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gents_protocol::enrollment::authorization_lease_is_fresh_at;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_request_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_admin_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_authorization_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrollment_authorization_expires_at: Option<String>,
    #[serde(default)]
    pub pairing_ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_agent_home: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl PeerRecord {
    pub fn is_enrollment(&self) -> bool {
        self.source.as_deref() == Some("enrollment")
    }

    /// Whether this exact durable route generation may authorize a chat write
    /// at `now`.
    ///
    /// `pairing_ready` is applied-state evidence, not timeless authority. An
    /// enrollment row must still carry a complete, unexpired signed
    /// generation at the instant a write is admitted.
    pub fn is_chat_ready_at(&self, now: DateTime<Utc>) -> bool {
        if self.source.as_deref() == Some("local-standard") {
            return true;
        }
        self.is_enrollment()
            && self.pairing_ready
            && self
                .pairing_network_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            && self
                .enrollment_request_digest
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            && self
                .enrollment_authorization_sequence
                .is_some_and(|sequence| sequence > 0)
            && self
                .enrollment_authorization_expires_at
                .as_deref()
                .is_some_and(|expires_at| authorization_lease_is_fresh_at(expires_at, now))
    }

    #[cfg(test)]
    pub(crate) fn new(
        label: impl Into<String>,
        addr: impl Into<String>,
        agent_did: impl Into<String>,
    ) -> Self {
        Self::base(label, addr, agent_did)
    }

    fn base(
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
            enrollment_request_digest: None,
            enrollment_request_id: None,
            enrollment_admin_did: None,
            enrollment_authorization_sequence: None,
            enrollment_authorization_expires_at: None,
            pairing_ready: false,
            graphql: None,
            local_agent_home: None,
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
        let mut record = Self::base(label, addr, agent_did);
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

    pub(in crate::client) async fn upsert_local_standard_peer(
        &mut self,
        label: &str,
        addr: &str,
        agent_did: &str,
        graphql: &str,
        agent_home: &str,
    ) -> Result<PeerRecord> {
        let label = normalize_non_empty("label", label)?;
        let addr = normalize_non_empty("addr", addr)?;
        let agent_did = normalize_non_empty("agent_did", agent_did)?;
        let graphql = normalize_non_empty("graphql", graphql)?;
        let agent_home = normalize_non_empty("agent_home", agent_home)?;

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
        record.local_agent_home = Some(agent_home.to_string());

        let record = candidate.apply_upsert(record);
        self.commit(candidate).await?;
        Ok(record)
    }

    pub(in crate::client) async fn upsert_enrollment_peer(
        &mut self,
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
    ) -> Result<PeerRecord> {
        let peer_id = normalize_non_empty("peer_id", peer_id)?;
        let label = normalize_non_empty("label", label)?;
        let addr = normalize_non_empty("addr", addr)?;
        let agent_did = normalize_non_empty("agent_did", agent_did)?;
        let network_id = normalize_non_empty("network_id", network_id)?;
        let request_id = normalize_non_empty("request_id", request_id)?;
        let request_digest = normalize_non_empty("request_digest", request_digest)?;
        let admin_did = normalize_non_empty("admin_did", admin_did)?;
        anyhow::ensure!(
            authorization_sequence > 0,
            "authorization_sequence must be positive"
        );
        let authorization_expires_at =
            normalize_non_empty("authorization_expires_at", authorization_expires_at)?;

        let mut candidate = self.clone();
        if let Some(conflict) = candidate.peers.iter().find(|record| {
            (record.peer_id == peer_id || record.agent_did == agent_did)
                && record.source.as_deref() != Some("enrollment")
        }) {
            anyhow::bail!(
                "authenticated enrollment conflicts with {}-owned peer {}",
                conflict.source.as_deref().unwrap_or("legacy"),
                conflict.peer_id
            );
        }
        let now = Utc::now().to_rfc3339();
        let existing = candidate
            .peers
            .iter()
            .find(|record| {
                record.source.as_deref() == Some("enrollment")
                    && (record.peer_id == peer_id || record.agent_did == agent_did)
            })
            .cloned();
        let mut record = existing.clone().unwrap_or(PeerRecord {
            peer_id: peer_id.to_string(),
            label: label.to_string(),
            addr: addr.to_string(),
            agent_did: agent_did.to_string(),
            default_behavior_id: None,
            source: Some("enrollment".to_string()),
            pairing_network_id: Some(network_id.to_string()),
            pairing_template: Some("client".to_string()),
            enrollment_request_id: Some(request_id.to_string()),
            enrollment_request_digest: Some(request_digest.to_string()),
            enrollment_admin_did: Some(admin_did.to_string()),
            enrollment_authorization_sequence: Some(authorization_sequence),
            enrollment_authorization_expires_at: Some(authorization_expires_at.to_string()),
            pairing_ready: false,
            graphql: None,
            local_agent_home: None,
            created_at: now.clone(),
            updated_at: now,
        });
        if record.peer_id != peer_id
            || record.addr != addr
            || record.agent_did != agent_did
            || record.pairing_network_id.as_deref() != Some(network_id)
            || record.enrollment_request_id.as_deref() != Some(request_id)
            || record.enrollment_request_digest.as_deref() != Some(request_digest)
            || record.enrollment_admin_did.as_deref() != Some(admin_did)
            || record.enrollment_authorization_sequence != Some(authorization_sequence)
            || record.enrollment_authorization_expires_at.as_deref()
                != Some(authorization_expires_at)
        {
            record.pairing_ready = false;
        }
        record.peer_id = peer_id.to_string();
        // The enrollment label seeds a new record only. Once saved, the label is
        // user-owned presentation state and must survive authority refreshes.
        // Reapplying the enrollment fallback here made every successful status
        // sweep undo a manual rename.
        record.addr = addr.to_string();
        record.agent_did = agent_did.to_string();
        record.source = Some("enrollment".to_string());
        record.pairing_network_id = Some(network_id.to_string());
        record.pairing_template = Some("client".to_string());
        record.enrollment_request_id = Some(request_id.to_string());
        record.enrollment_request_digest = Some(request_digest.to_string());
        record.enrollment_admin_did = Some(admin_did.to_string());
        record.enrollment_authorization_sequence = Some(authorization_sequence);
        record.enrollment_authorization_expires_at = Some(authorization_expires_at.to_string());
        record.graphql = None;

        // Status polling observes the same signed authorization on every
        // supervisor tick. Preserve the durable generation (including its
        // timestamp) when that observation is unchanged; rewriting it makes
        // downstream route and sync fences look new every two seconds.
        if existing.as_ref() == Some(&record) {
            return Ok(record);
        }

        let record = candidate.apply_upsert(record);
        self.commit(candidate).await?;
        Ok(record)
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
    /// process restart. Every non-local deployment must re-establish its live
    /// authority and both route legs before chat writes reopen.
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
    agent_home: &str,
) -> Result<PeerRecord> {
    let mut directory = PeerDirectory::open_writer(path).await?;
    directory
        .upsert_local_standard_peer(label, addr, agent_did, graphql, agent_home)
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

fn normalize_non_empty<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then_some(trimmed)
        .with_context(|| format!("{field} must not be empty"))
}

async fn read_stored_directory(path: &Path) -> Result<StoredPeerDirectory> {
    match tokio::fs::read(path).await {
        Ok(bytes) if bytes.is_empty() => Ok(StoredPeerDirectory::default()),
        Ok(bytes) => {
            let stored = serde_json::from_slice::<StoredPeerDirectory>(&bytes)
                .with_context(|| format!("parsing peer directory {}", path.display()))?;
            #[cfg(not(test))]
            validate_fresh_directory(&stored)?;
            Ok(stored)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(StoredPeerDirectory::default())
        }
        Err(error) => Err(anyhow::Error::from(error))
            .with_context(|| format!("reading peer directory {}", path.display())),
    }
}

fn validate_fresh_directory(stored: &StoredPeerDirectory) -> Result<()> {
    for record in stored.peers.iter().chain(&stored.pending_removals) {
        anyhow::ensure!(
            matches!(
                record.source.as_deref(),
                Some("local-standard" | "enrollment")
            ),
            "peer directory contains unsupported pre-enrollment source"
        );
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_less_peer_is_not_an_immediate_chat_authority() {
        let legacy = PeerRecord::new("Legacy bridge", "iroh://legacy", "did:test:legacy");
        assert!(!legacy.is_chat_ready_at(Utc::now()));
        assert!(validate_fresh_directory(&StoredPeerDirectory {
            peers: vec![legacy],
            pending_removals: Vec::new(),
        })
        .is_err());
    }

    #[tokio::test]
    async fn restart_clears_persisted_managed_readiness_before_chat_reopens() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();
        let mut managed = PeerRecord::new("Mandrake", "iroh://ticket", "did:test:mandrake");
        managed.source = Some("enrollment".to_string());
        managed.pairing_ready = true;
        directory.upsert(managed.clone()).await.unwrap();

        directory.clear_ephemeral_pairing_readiness().await.unwrap();
        let record = directory
            .records()
            .iter()
            .find(|record| record.peer_id == managed.peer_id)
            .unwrap();
        assert!(!record.pairing_ready);
        assert!(!record.is_chat_ready_at(Utc::now()));

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
    async fn enrollment_generation_rotation_is_durable_and_clears_readiness() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();
        let first = directory
            .upsert_enrollment_peer(
                "server-peer",
                "Enrolled server",
                "iroh://ticket",
                "did:key:agent",
                "network-a",
                "request-a",
                "digest-a",
                "did:key:admin",
                7,
                "2099-09-29T00:00:00Z",
            )
            .await
            .unwrap();
        let first = directory
            .set_pairing_ready(&first.peer_id, true)
            .await
            .unwrap()
            .unwrap();
        assert!(first.pairing_ready);

        let rotated = directory
            .upsert_enrollment_peer(
                "server-peer",
                "Enrolled server",
                "iroh://ticket",
                "did:key:agent",
                "network-a",
                "request-a",
                "digest-b",
                "did:key:admin",
                8,
                "2099-10-29T00:00:00Z",
            )
            .await
            .unwrap();
        assert!(!rotated.pairing_ready);
        assert_eq!(
            rotated.enrollment_request_digest.as_deref(),
            Some("digest-b")
        );
        assert_eq!(rotated.enrollment_authorization_sequence, Some(8));
        assert_eq!(
            rotated.enrollment_authorization_expires_at.as_deref(),
            Some("2099-10-29T00:00:00Z")
        );

        drop(directory);
        let reloaded = load_peer_records(&path).await.unwrap();
        assert_eq!(reloaded, vec![rotated]);
    }

    #[tokio::test]
    async fn enrollment_refresh_preserves_the_saved_user_label() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();
        let enrolled = directory
            .upsert_enrollment_peer(
                "server-peer",
                "Enrolled Agent",
                "iroh://ticket",
                "did:key:agent",
                "network-a",
                "request-a",
                "digest-a",
                "did:key:admin",
                7,
                "2099-09-29T00:00:00Z",
            )
            .await
            .unwrap();
        let mut renamed = enrolled.clone();
        renamed.label = "Mandrake".to_string();
        directory
            .replace_if_matches(&enrolled, renamed)
            .await
            .unwrap()
            .expect("enrollment record remains current");

        let refreshed = directory
            .upsert_enrollment_peer(
                "server-peer",
                "Enrolled Agent",
                "iroh://rotated-ticket",
                "did:key:agent",
                "network-a",
                "request-a",
                "digest-a",
                "did:key:admin",
                7,
                "2099-09-29T00:00:00Z",
            )
            .await
            .unwrap();

        assert_eq!(refreshed.label, "Mandrake");
        assert_eq!(load_peer_records(&path).await.unwrap()[0].label, "Mandrake");
    }

    #[tokio::test]
    async fn unchanged_enrollment_refresh_preserves_the_durable_generation() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");
        let mut directory = PeerDirectory::load(&path).await.unwrap();
        let enrolled = directory
            .upsert_enrollment_peer(
                "server-peer",
                "Enrolled Agent",
                "iroh://ticket",
                "did:key:agent",
                "network-a",
                "request-a",
                "digest-a",
                "did:key:admin",
                7,
                "2099-09-29T00:00:00Z",
            )
            .await
            .unwrap();

        let refreshed = directory
            .upsert_enrollment_peer(
                "server-peer",
                "Ignored fallback label",
                "iroh://ticket",
                "did:key:agent",
                "network-a",
                "request-a",
                "digest-a",
                "did:key:admin",
                7,
                "2099-09-29T00:00:00Z",
            )
            .await
            .unwrap();

        assert_eq!(refreshed, enrolled);
        assert_eq!(load_peer_records(&path).await.unwrap(), vec![enrolled]);
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
                "/tmp/test-agent-home",
            )
            .await
            .unwrap();
        let second = directory
            .upsert_local_standard_peer(
                "Local Agent Updated",
                "iroh://second",
                "did:test:default",
                "http://127.0.0.1:9192/api/v0/graphql",
                "/tmp/test-agent-home",
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
