use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::Node as EmbeddedNode;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::store::graphql::{escape_graphql_string, graphql_string_list_literal, rows};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub directory_key: String,
    pub node_did: String,
    pub source_did: String,
    pub display_name: String,
    pub agents: Vec<String>,
    pub agent_dids: Vec<String>,
    pub default_agent_did: String,
    pub runtime_state: String,
    pub last_seen: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentInfo {
    pub agent_did: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub nodes: Vec<(String, String, String)>,
    pub agents: BTreeMap<String, Vec<AgentInfo>>,
    pub runtimes: BTreeMap<String, (String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryTickOutcome {
    pub upserted: BTreeSet<String>,
    pub refreshed: BTreeSet<String>,
    pub retracted: BTreeSet<String>,
}

#[async_trait]
pub trait DirectoryStore: Send + Sync {
    async fn load_source_snapshot(&self) -> Result<SourceSnapshot>;
    async fn list_directory_entries(
        &self,
        source_did: &str,
    ) -> Result<BTreeMap<String, DirectoryEntry>>;
    async fn upsert_directory_entry(&self, entry: &DirectoryEntry) -> Result<()>;
    async fn delete_directory_entry(&self, source_did: &str, node_did: &str) -> Result<()>;
}

pub fn directory_entry_key(source_did: &str, node_did: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(source_did.as_bytes());
    digest.update(b"\x1f");
    digest.update(node_did.as_bytes());
    format!(
        "dir-{}",
        bs58::encode(&digest.finalize()[..16]).into_string()
    )
}

/// DefraDB stores `NodeDirectoryEntry.last_seen` as `DateTime`, normalizing
/// offsets and sub-second precision; match that representation for fixpoints.
fn canonicalize_last_seen(updated_at: &str) -> String {
    let trimmed = updated_at.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    match chrono::DateTime::parse_from_rfc3339(trimmed) {
        Ok(parsed) => parsed
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Secs, true),
        Err(_) => trimmed.to_string(),
    }
}

pub fn derive_directory_entries(
    source_did: &str,
    nodes: &[(String, String, String)],
    agents: &BTreeMap<String, Vec<AgentInfo>>,
    runtimes: &BTreeMap<String, (String, String)>,
) -> BTreeMap<String, DirectoryEntry> {
    nodes
        .iter()
        .filter(|(did, _, _)| !did.trim().is_empty())
        .map(|(did, display_name, default_agent_did)| {
            let mut infos = agents.get(did).cloned().unwrap_or_default();
            infos.sort_by(|a, b| {
                a.display_name
                    .cmp(&b.display_name)
                    .then_with(|| a.agent_did.cmp(&b.agent_did))
            });
            infos.dedup_by(|a, b| a.agent_did == b.agent_did);

            let names = infos.iter().map(|info| info.display_name.clone()).collect();
            let ids: Vec<String> = infos.iter().map(|info| info.agent_did.clone()).collect();
            let (runtime_state, updated_at) = runtimes.get(did).cloned().unwrap_or_default();
            (
                did.clone(),
                DirectoryEntry {
                    directory_key: directory_entry_key(source_did, did),
                    node_did: did.clone(),
                    source_did: source_did.to_string(),
                    display_name: display_name.clone(),
                    agents: names,
                    agent_dids: ids,
                    default_agent_did: default_agent_did.clone(),
                    runtime_state,
                    last_seen: canonicalize_last_seen(&updated_at),
                },
            )
        })
        .collect()
}

pub async fn reconcile_directory_tick(
    store: &dyn DirectoryStore,
    source_did: &str,
) -> Result<DirectoryTickOutcome> {
    let snapshot = store
        .load_source_snapshot()
        .await
        .context("load source snapshot")?;
    let desired = derive_directory_entries(
        source_did,
        &snapshot.nodes,
        &snapshot.agents,
        &snapshot.runtimes,
    );
    let existing = store
        .list_directory_entries(source_did)
        .await
        .context("list directory entries")?;

    let mut outcome = DirectoryTickOutcome::default();
    let mut first_error: Option<anyhow::Error> = None;
    for (did, entry) in &desired {
        match existing.get(did) {
            Some(row) if row == entry => {}
            Some(_) => match store.upsert_directory_entry(entry).await {
                Ok(()) => {
                    outcome.refreshed.insert(did.clone());
                }
                Err(error) => {
                    tracing::warn!(node_did = %did, error = %error, "directory entry refresh failed; continuing sweep");
                    if first_error.is_none() {
                        first_error =
                            Some(error.context(format!("refresh directory entry for {did}")));
                    }
                }
            },
            None => match store.upsert_directory_entry(entry).await {
                Ok(()) => {
                    outcome.upserted.insert(did.clone());
                }
                Err(error) => {
                    tracing::warn!(node_did = %did, error = %error, "directory entry upsert failed; continuing sweep");
                    if first_error.is_none() {
                        first_error =
                            Some(error.context(format!("upsert directory entry for {did}")));
                    }
                }
            },
        }
    }
    for did in existing.keys() {
        if !desired.contains_key(did) {
            match store.delete_directory_entry(source_did, did).await {
                Ok(()) => {
                    outcome.retracted.insert(did.clone());
                }
                Err(error) => {
                    tracing::warn!(node_did = %did, error = %error, "directory entry retraction failed; continuing sweep");
                    if first_error.is_none() {
                        first_error =
                            Some(error.context(format!("retract directory entry for {did}")));
                    }
                }
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(outcome)
}

/// The source rows a directory sweep projects; its own NodeDirectoryEntry
/// writes must not wake it again.
const DIRECTORY_SOURCE_COLLECTIONS: [&str; 3] = ["Node", "NodeReadiness", "Agent"];

pub async fn run_directory_projection(
    node: Arc<EmbeddedNode>,
    source_did: String,
    cancel: CancellationToken,
) -> Result<()> {
    let store = GraphqlDirectoryStore::new(node.clone());
    let mut subscription = crate::subscribe_collections(&node, DIRECTORY_SOURCE_COLLECTIONS)?;
    let mut subscription_open = true;
    let mut interval =
        tokio::time::interval(crate::runtime::agent::p2p_reconcile::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    sweep_directory(&store, &source_did).await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => sweep_directory(&store, &source_did).await,
            batch = subscription.recv(), if subscription_open => {
                if batch.is_none() {
                    tracing::warn!("directory projection change subscription closed; continuing with periodic sweeps");
                    subscription_open = false;
                    continue;
                }
                sweep_directory(&store, &source_did).await;
            }
        }
    }
}

async fn sweep_directory(store: &GraphqlDirectoryStore, source_did: &str) {
    match reconcile_directory_tick(store, source_did).await {
        Ok(outcome) => {
            if !outcome.upserted.is_empty()
                || !outcome.refreshed.is_empty()
                || !outcome.retracted.is_empty()
            {
                tracing::info!(
                    upserted = ?outcome.upserted,
                    refreshed = ?outcome.refreshed,
                    retracted = ?outcome.retracted,
                    "reconciled fleet-discovery directory rows"
                );
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "directory projection reconcile sweep failed")
        }
    }
}

pub(crate) struct GraphqlDirectoryStore {
    node: Arc<EmbeddedNode>,
}

impl GraphqlDirectoryStore {
    pub(crate) fn new(node: Arc<EmbeddedNode>) -> Self {
        Self { node }
    }
}

#[async_trait]
impl DirectoryStore for GraphqlDirectoryStore {
    async fn load_source_snapshot(&self) -> Result<SourceSnapshot> {
        Ok(SourceSnapshot::default())
    }

    async fn list_directory_entries(
        &self,
        source_did: &str,
    ) -> Result<BTreeMap<String, DirectoryEntry>> {
        let source_did = escape_graphql_string(source_did);
        let query = format!(
            r#"{{
            NodeDirectoryEntry(filter: {{ source_did: {{ _eq: "{source_did}" }} }}) {{
                directory_key
                node_did
                source_did
                display_name
                agents
                agent_dids
                default_agent_did
                runtime_state
                last_seen
            }}
        }}"#
        );
        let response = crate::store::graphql::graphql_with_transaction_retry(
            &self.node,
            &query,
            "query NodeDirectoryEntry",
        )
        .await?;
        Ok(rows::<DirectoryRow>(&response, "NodeDirectoryEntry")?
            .into_iter()
            .filter_map(|row| {
                let directory_key = row.directory_key?.trim().to_string();
                if directory_key.is_empty() {
                    return None;
                }
                let did = row.node_did?.trim().to_string();
                if did.is_empty() {
                    return None;
                }
                let entry = DirectoryEntry {
                    directory_key,
                    node_did: did.clone(),
                    source_did: row.source_did.unwrap_or_default(),
                    display_name: row.display_name.unwrap_or_default(),
                    agents: row.agents.unwrap_or_default(),
                    agent_dids: row.agent_dids.unwrap_or_default(),
                    default_agent_did: row.default_agent_did.unwrap_or_default(),
                    runtime_state: row.runtime_state.unwrap_or_default(),
                    last_seen: row.last_seen.unwrap_or_default(),
                };
                Some((did, entry))
            })
            .collect())
    }

    async fn upsert_directory_entry(&self, entry: &DirectoryEntry) -> Result<()> {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let mutation = upsert_directory_entry_mutation(entry, &now);
        Ok(
            crate::store::config_client::ConfigAccess::write_local_response(
                &self.node,
                "p2p.upsert_node_directory",
                &mutation,
            )
            .await
            .map(|_| ())?,
        )
    }

    async fn delete_directory_entry(&self, source_did: &str, node_did: &str) -> Result<()> {
        let mutation = delete_directory_entry_mutation(source_did, node_did);
        Ok(
            crate::store::config_client::ConfigAccess::write_local_response(
                &self.node,
                "p2p.delete_node_directory",
                &mutation,
            )
            .await
            .map(|_| ())?,
        )
    }
}

fn upsert_directory_entry_mutation(entry: &DirectoryEntry, now: &str) -> String {
    let node_did = escape_graphql_string(&entry.node_did);
    let directory_key =
        escape_graphql_string(&directory_entry_key(&entry.source_did, &entry.node_did));
    let source_did = escape_graphql_string(&entry.source_did);
    let display_name = escape_graphql_string(&entry.display_name);
    let agents = graphql_string_list_literal(entry.agents.iter().map(String::as_str));
    let agent_dids = graphql_string_list_literal(entry.agent_dids.iter().map(String::as_str));
    let default_agent_did = escape_graphql_string(&entry.default_agent_did);
    let runtime_state = escape_graphql_string(&entry.runtime_state);
    let last_seen = graphql_nullable_datetime_literal(&entry.last_seen);
    let now = escape_graphql_string(now);
    format!(
        r#"mutation {{
            upsert_NodeDirectoryEntry(
                filter: {{ directory_key: {{ _eq: "{directory_key}" }} }},
                add: {{
                    directory_key: "{directory_key}",
                    node_did: "{node_did}",
                    source_did: "{source_did}",
                    display_name: "{display_name}",
                    agents: {agents},
                    agent_dids: {agent_dids},
                    default_agent_did: "{default_agent_did}",
                    runtime_state: "{runtime_state}",
                    last_seen: {last_seen},
                    updated_at: "{now}"
                }},
                update: {{
                    display_name: "{display_name}",
                    agents: {agents},
                    agent_dids: {agent_dids},
                    default_agent_did: "{default_agent_did}",
                    runtime_state: "{runtime_state}",
                    last_seen: {last_seen},
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

fn delete_directory_entry_mutation(source_did: &str, node_did: &str) -> String {
    let directory_key = escape_graphql_string(&directory_entry_key(source_did, node_did));
    format!(
        r#"mutation {{
            delete_NodeDirectoryEntry(filter: {{
                directory_key: {{ _eq: "{directory_key}" }}
            }}) {{ _docID }}
        }}"#
    )
}

fn graphql_nullable_datetime_literal(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "null".to_string()
    } else {
        format!(r#""{}""#, escape_graphql_string(trimmed))
    }
}

#[derive(Deserialize)]
struct DirectoryRow {
    #[serde(default)]
    directory_key: Option<String>,
    node_did: Option<String>,
    #[serde(default)]
    source_did: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    agents: Option<Vec<String>>,
    #[serde(default)]
    agent_dids: Option<Vec<String>>,
    #[serde(default)]
    default_agent_did: Option<String>,
    #[serde(default)]
    runtime_state: Option<String>,
    #[serde(default)]
    last_seen: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_last_seen_quantizes_offset_and_subseconds_to_utc_seconds() {
        assert_eq!(
            canonicalize_last_seen("2026-07-23T18:30:55.5-05:00"),
            "2026-07-23T23:30:55Z"
        );
        assert_eq!(canonicalize_last_seen(""), "");
    }

    /// ACP-enabled fixture node signed by its own identity key. The directory
    /// schema binds `AGENT_OWNERSHIP_POLICY_ID`, which only an ACP-enabled node
    /// registers, so the plain `node_builder` fixture cannot host it.
    async fn acp_node(dir: &std::path::Path) -> Result<Arc<EmbeddedNode>> {
        use crate::NodeIdentity;
        let key_path = dir.join("node.key");
        let identity = crate::store::identity::KeyIdentity::load_or_create(key_path.clone(), None)?;
        let signing_key =
            crate::store::identity::load_file_identity(&key_path)?.private_key_bytes();
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(signing_key))
                .with_node_acp_enabled()
                .build()
                .await?,
        );
        crate::test_support::acp_policies::install_identity_acp_policies(&node).await?;
        Ok(node)
    }

    #[tokio::test]
    async fn directory_retracts_own_publications_and_preserves_foreign_sources() -> Result<()> {
        let cases =
            &crate::lean_vocab_test::lean_contract_snapshot().identity_directory_publication_cases;
        let mut checked = 0;
        for case in cases.iter().filter(|case| case["name"] != "failed-delete") {
            checked += 1;
            let dir = tempfile::tempdir()?;
            let node = acp_node(dir.path()).await?;
            node.add_collection(gents_protocol::schemas::NODE_DIRECTORY_ENTRY)
                .await?;
            let store = GraphqlDirectoryStore::new(node.clone());
            for entry in case["existing"].as_array().unwrap() {
                let source = entry["source"].as_str().unwrap();
                let did = entry["node"].as_str().unwrap();
                let row = DirectoryEntry {
                    directory_key: directory_entry_key(source, did),
                    source_did: source.to_owned(),
                    node_did: did.to_owned(),
                    display_name: "previously published".into(),
                    ..DirectoryEntry::default()
                };
                store.upsert_directory_entry(&row).await?;
            }
            let source = case["source"].as_str().unwrap();
            assert!(case["expected_desired"].as_array().unwrap().is_empty());
            let result = reconcile_directory_tick(&store, source).await?;
            let deleted = case["deleted"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|value| value.as_str())
                .filter(|did| {
                    case["existing"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry["source"] == source && entry["node"] == *did)
                })
                .map(str::to_owned)
                .collect::<BTreeSet<_>>();
            assert_eq!(result.retracted, deleted, "{case}");
            assert!(result.upserted.is_empty() && result.refreshed.is_empty());
            for source in case["existing"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry["source"].as_str().unwrap())
                .collect::<BTreeSet<_>>()
            {
                let actual = store
                    .list_directory_entries(source)
                    .await?
                    .into_keys()
                    .collect::<BTreeSet<_>>();
                let expected = case["expected_remaining"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|entry| entry["source"] == source)
                    .map(|entry| entry["node"].as_str().unwrap().to_owned())
                    .collect::<BTreeSet<_>>();
                assert_eq!(actual, expected, "{case}");
            }
            assert_eq!(
                reconcile_directory_tick(&store, source).await?,
                DirectoryTickOutcome::default()
            );
            node.shutdown().await?;
        }
        assert_eq!(checked, 3);
        Ok(())
    }

    #[tokio::test]
    async fn directory_failed_deletion_remains_visible() {
        struct FailingStore {
            entries: tokio::sync::Mutex<BTreeMap<String, DirectoryEntry>>,
            deleted: BTreeSet<String>,
        }
        #[async_trait]
        impl DirectoryStore for FailingStore {
            async fn load_source_snapshot(&self) -> Result<SourceSnapshot> {
                Ok(SourceSnapshot::default())
            }
            async fn list_directory_entries(
                &self,
                _source_did: &str,
            ) -> Result<BTreeMap<String, DirectoryEntry>> {
                Ok(self.entries.lock().await.clone())
            }
            async fn upsert_directory_entry(&self, _entry: &DirectoryEntry) -> Result<()> {
                anyhow::bail!("unexpected upsert")
            }
            async fn delete_directory_entry(
                &self,
                _source_did: &str,
                node_did: &str,
            ) -> Result<()> {
                anyhow::ensure!(self.deleted.contains(node_did), "injected delete failure");
                self.entries.lock().await.remove(node_did);
                Ok(())
            }
        }
        let case = crate::lean_vocab_test::lean_contract_snapshot()
            .identity_directory_publication_cases
            .iter()
            .find(|case| case["name"] == "failed-delete")
            .unwrap();
        let entries = case["existing"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                let did = entry["node"].as_str().unwrap().to_owned();
                (
                    did.clone(),
                    DirectoryEntry {
                        node_did: did,
                        source_did: entry["source"].as_str().unwrap().to_owned(),
                        ..DirectoryEntry::default()
                    },
                )
            })
            .collect();
        let store = FailingStore {
            entries: tokio::sync::Mutex::new(entries),
            deleted: case["deleted"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| id.as_str().unwrap().to_owned())
                .collect(),
        };
        assert!(
            reconcile_directory_tick(&store, case["source"].as_str().unwrap())
                .await
                .is_err()
        );
        let actual = store
            .entries
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let expected = case["expected_remaining"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["node"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
    }
}
