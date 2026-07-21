use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerRecord {
    pub peer_id: String,
    pub label: String,
    pub addr: String,
    pub agent_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl PeerRecord {
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
            source: None,
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
}

#[derive(Debug, Clone)]
pub struct PeerDirectory {
    path: PathBuf,
    peers: Vec<PeerRecord>,
}

impl PeerDirectory {
    pub async fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let peers = match tokio::fs::read(&path).await {
            Ok(bytes) => {
                serde_json::from_slice::<StoredPeerDirectory>(&bytes)
                    .with_context(|| format!("parsing peer directory {}", path.display()))?
                    .peers
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(anyhow::Error::from(error))
                    .with_context(|| format!("reading peer directory {}", path.display()));
            }
        };

        let mut directory = Self { path, peers };
        directory.sort_records();
        Ok(directory)
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub fn records(&self) -> &[PeerRecord] {
        &self.peers
    }

    pub async fn upsert_saved_peer(
        &mut self,
        label: &str,
        addr: &str,
        agent_did: &str,
    ) -> Result<PeerRecord> {
        self.upsert_saved_peer_with_graphql(label, addr, agent_did, None)
            .await
    }

    pub async fn upsert_saved_peer_with_graphql(
        &mut self,
        label: &str,
        addr: &str,
        agent_did: &str,
        graphql: Option<&str>,
    ) -> Result<PeerRecord> {
        let label = normalize_non_empty("label", label)?;
        let addr = normalize_non_empty("addr", addr)?;
        let agent_did = normalize_non_empty("agent_did", agent_did)?;
        let graphql = normalize_optional(graphql);

        let mut record = self
            .peers
            .iter()
            .find(|existing| existing.addr == addr && existing.agent_did == agent_did)
            .cloned()
            .unwrap_or_else(|| PeerRecord::new(label, addr, agent_did));
        record.label = label.to_string();
        record.addr = addr.to_string();
        record.agent_did = agent_did.to_string();
        if let Some(graphql) = graphql {
            record.graphql = Some(graphql.to_string());
            if record.source.as_deref() != Some("local-standard") {
                record.source = Some("server-status".to_string());
            }
        }

        self.upsert(record.clone()).await?;
        Ok(record)
    }

    pub async fn upsert_local_standard_peer(
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

        let mut record = self
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

        self.upsert(record.clone()).await?;
        Ok(record)
    }

    pub async fn upsert(&mut self, mut record: PeerRecord) -> Result<()> {
        if let Some(existing) = self
            .peers
            .iter_mut()
            .find(|existing| existing.peer_id == record.peer_id)
        {
            record.created_at = existing.created_at.clone();
            record.updated_at = Utc::now().to_rfc3339();
            *existing = record;
        } else {
            self.peers.push(record);
        }

        self.sort_records();
        self.save().await
    }

    pub async fn remove(&mut self, peer_id: &str) -> Result<Option<PeerRecord>> {
        let removed = self
            .peers
            .iter()
            .position(|record| record.peer_id == peer_id)
            .map(|index| self.peers.remove(index));

        self.sort_records();
        self.save().await?;
        Ok(removed)
    }

    async fn save(&self) -> Result<()> {
        write_json_atomically(
            &self.path,
            &StoredPeerDirectory {
                peers: self.peers.clone(),
            },
        )
        .await
    }

    fn sort_records(&mut self) {
        self.peers.sort_by(|left, right| {
            left.label
                .to_lowercase()
                .cmp(&right.label.to_lowercase())
                .then_with(|| left.created_at.cmp(&right.created_at))
                .then_with(|| left.peer_id.cmp(&right.peer_id))
        });
    }
}

async fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating peer directory parent {}", parent.display()))?;
    }

    let bytes = serde_json::to_vec_pretty(value)?;
    let tmp_path = path.with_extension("tmp");

    tokio::fs::write(&tmp_path, bytes)
        .await
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(path).await;
    }
    tokio::fs::rename(&tmp_path, path)
        .await
        .with_context(|| format!("persisting {}", path.display()))?;
    Ok(())
}

fn normalize_non_empty<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then_some(trimmed)
        .with_context(|| format!("{field} must not be empty"))
}

fn normalize_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_file_loads_as_empty_directory() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("peers.json");

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

        let reloaded = PeerDirectory::load(&path).await.unwrap();
        assert!(reloaded.is_empty());
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
            )
            .await
            .unwrap();

        assert_eq!(
            record.graphql.as_deref(),
            Some("http://100.73.235.38:9181/api/v0/graphql")
        );
        assert_eq!(record.source.as_deref(), Some("server-status"));

        let reloaded = PeerDirectory::load(&path).await.unwrap();
        assert_eq!(
            reloaded.records()[0].graphql.as_deref(),
            Some("http://100.73.235.38:9181/api/v0/graphql")
        );
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

        let reloaded = PeerDirectory::load(&path).await.unwrap();
        assert_eq!(
            reloaded.records()[0].graphql.as_deref(),
            Some("http://127.0.0.1:9192/api/v0/graphql")
        );
    }
}
