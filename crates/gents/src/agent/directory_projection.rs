use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::{EmbeddedNode, QueryResponse};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::graphql::{escape_graphql_string, graphql_string_list_literal, rows};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub directory_key: String,
    pub node_did: String,
    pub source_did: String,
    pub display_name: String,
    pub agents: Vec<String>,
    pub agent_ids: Vec<String>,
    pub default_agent_id: String,
    pub runtime_state: String,
    pub last_seen: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentInfo {
    pub agent_id: String,
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
        .map(|(did, display_name, default_agent_id)| {
            let mut infos = agents.get(did).cloned().unwrap_or_default();
            infos.sort_by(|a, b| {
                a.display_name
                    .cmp(&b.display_name)
                    .then_with(|| a.agent_id.cmp(&b.agent_id))
            });
            infos.dedup_by(|a, b| a.agent_id == b.agent_id);

            let names = infos.iter().map(|info| info.display_name.clone()).collect();
            let ids: Vec<String> = infos.iter().map(|info| info.agent_id.clone()).collect();
            let (runtime_state, updated_at) = runtimes.get(did).cloned().unwrap_or_default();
            (
                did.clone(),
                DirectoryEntry {
                    directory_key: directory_entry_key(source_did, did),
                    node_did: did.clone(),
                    source_did: source_did.to_string(),
                    display_name: display_name.clone(),
                    agents: names,
                    agent_ids: ids,
                    default_agent_id: default_agent_id.clone(),
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
        tokio::time::interval(crate::agent::p2p_reconcile::intervals::sweep_interval());
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
        let (fields, _) = crate::config_client::config_projection(
            crate::store::collection::Collection::Agent,
            None,
        )?;
        let query = format!(
            "{{ Node {{ node_did display_name default_agent_id enabled }} NodeReadiness {{ node_did snapshot_json updated_at }} Agent {{ {} }} }}",
            fields.join(" "),
        );
        let response = crate::graphql::graphql_with_transaction_retry(
            &self.node,
            &query,
            "query directory source snapshot",
        )
        .await?;
        let nodes = parse_nodes(&response)?;
        let agents = parse_agents(&response)?;
        Ok(SourceSnapshot {
            nodes,
            agents,
            runtimes: parse_runtime_states(&response)?,
        })
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
                agent_ids
                default_agent_id
                runtime_state
                last_seen
            }}
        }}"#
        );
        let response = crate::graphql::graphql_with_transaction_retry(
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
                    agent_ids: row.agent_ids.unwrap_or_default(),
                    default_agent_id: row.default_agent_id.unwrap_or_default(),
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
        Ok(crate::config_client::ConfigAccess::write_local_response(
            &self.node,
            "p2p.upsert_node_directory",
            &mutation,
        )
        .await
        .map(|_| ())?)
    }

    async fn delete_directory_entry(&self, source_did: &str, node_did: &str) -> Result<()> {
        let mutation = delete_directory_entry_mutation(source_did, node_did);
        Ok(crate::config_client::ConfigAccess::write_local_response(
            &self.node,
            "p2p.delete_node_directory",
            &mutation,
        )
        .await
        .map(|_| ())?)
    }
}

fn upsert_directory_entry_mutation(entry: &DirectoryEntry, now: &str) -> String {
    let node_did = escape_graphql_string(&entry.node_did);
    let directory_key =
        escape_graphql_string(&directory_entry_key(&entry.source_did, &entry.node_did));
    let source_did = escape_graphql_string(&entry.source_did);
    let display_name = escape_graphql_string(&entry.display_name);
    let agents = graphql_string_list_literal(entry.agents.iter().map(String::as_str));
    let agent_ids = graphql_string_list_literal(entry.agent_ids.iter().map(String::as_str));
    let default_agent_id = escape_graphql_string(&entry.default_agent_id);
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
                    agent_ids: {agent_ids},
                    default_agent_id: "{default_agent_id}",
                    runtime_state: "{runtime_state}",
                    last_seen: {last_seen},
                    updated_at: "{now}"
                }},
                update: {{
                    display_name: "{display_name}",
                    agents: {agents},
                    agent_ids: {agent_ids},
                    default_agent_id: "{default_agent_id}",
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

fn parse_nodes(response: &QueryResponse) -> Result<Vec<(String, String, String)>> {
    Ok(rows::<NodeRow>(response, "Node")?
        .into_iter()
        .filter_map(|row| {
            if !row.enabled.unwrap_or(true) {
                return None;
            }
            let did = row.node_did?.trim().to_string();
            if did.is_empty() {
                return None;
            }
            let display_name = row
                .display_name
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            let default_agent_id = row
                .default_agent_id
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            Some((did, display_name, default_agent_id))
        })
        .collect())
}

fn parse_runtime_states(response: &QueryResponse) -> Result<BTreeMap<String, (String, String)>> {
    Ok(
        rows::<gents_protocol::row::NodeReadinessRow>(response, "NodeReadiness")?
            .into_iter()
            .filter_map(|row| {
                let did = row.node_did.trim().to_string();
                if did.is_empty() {
                    return None;
                }
                let snapshot =
                    gents_protocol::row::decode_node_readiness_snapshot(&row, &did).ok()?;
                let process_state = snapshot.process_state.as_str().to_string();
                let updated_at = row.updated_at.trim().to_string();
                Some((did, (process_state, updated_at)))
            })
            .collect(),
    )
}

fn config_documents<T>(
    response: &QueryResponse,
    collection: crate::store::collection::Collection,
) -> Result<BTreeMap<(String, String), T>>
where
    T: serde::de::DeserializeOwned,
{
    let name = collection.graphql_type();
    let (fields, _) = crate::config_client::config_projection(collection, None)?;
    let source = response
        .data
        .as_ref()
        .and_then(|data| data.get(name))
        .and_then(serde_json::Value::as_array)
        .with_context(|| format!("directory snapshot missing {name}"))?;
    let mut documents = BTreeMap::new();
    for value in source {
        let mut document = value
            .as_object()
            .context("configuration row is not an object")?
            .clone();
        document.retain(|field, _| fields.contains(&field.as_str()));
        let document = serde_json::Value::Object(document);
        let (_, normalized) = crate::config_client::config_projection(collection, Some(&document))?;
        let document = normalized.context("canonical configuration projection missing value")?;
        let owner = document
            .get("node_did")
            .and_then(serde_json::Value::as_str)
            .context("configuration owner missing")?;
        let id = document
            .get(collection.unique_field())
            .and_then(serde_json::Value::as_str)
            .context("configuration ID missing")?;
        anyhow::ensure!(
            !owner.trim().is_empty() && !id.trim().is_empty(),
            "blank {name} owner or ID"
        );
        let key = (owner.to_string(), id.to_string());
        let parsed =
            serde_json::from_value(document).with_context(|| format!("decoding {name}"))?;
        anyhow::ensure!(
            documents.insert(key, parsed).is_none(),
            "duplicate scoped {name} identity"
        );
    }
    Ok(documents)
}

fn parse_agents(response: &QueryResponse) -> Result<BTreeMap<String, Vec<AgentInfo>>> {
    let documents = config_documents::<crate::documents::Agent>(
        response,
        crate::store::collection::Collection::Agent,
    )?;
    let mut agents: BTreeMap<String, Vec<AgentInfo>> = BTreeMap::new();
    for ((owner, _), agent) in documents {
        if !agent.enabled {
            continue;
        }
        agents.entry(owner).or_default().push(AgentInfo {
            display_name: agent
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(&agent.agent_id)
                .to_string(),
            agent_id: agent.agent_id,
        });
    }
    Ok(agents)
}

#[derive(Deserialize)]
struct NodeRow {
    node_did: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    default_agent_id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
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
    agent_ids: Option<Vec<String>>,
    #[serde(default)]
    default_agent_id: Option<String>,
    #[serde(default)]
    runtime_state: Option<String>,
    #[serde(default)]
    last_seen: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn graphql_tick_is_write_free_fixpoint_for_runtime_updated_at_format() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = Arc::new(
            crate::test_support::node_builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        let updated_at = Utc::now().to_rfc3339();
        let snapshot_json = escape_graphql_string(
            &serde_json::json!({
                "format_version": gents_protocol::node_readiness::NODE_READINESS_FORMAT_VERSION,
                "process_state": "ready",
                "active_generation": 1,
                "router_generation": 1,
                "default_agent_id": "general",
                "agents": [{
                    "agent_id": "general",
                    "state": "ready",
                    "reason": null
                }]
            })
            .to_string(),
        );
        let seed = format!(
            r#"mutation {{
                create_Node(input: {{
                    node_did: "did:key:real",
                    display_name: "Real",
                    enabled: true,
                    created_at: "2026-07-23T00:00:00Z"
                }}) {{ _docID }}
                create_NodeReadiness(input: {{
                    node_did: "did:key:real",
                    snapshot_json: "{snapshot_json}",
                    updated_at: "{updated_at}"
                }}) {{ _docID }}
            }}"#
        );
        crate::config_client::ConfigAccess::write_local_response(
            &node,
            "test.seed_runtime_readiness",
            &seed,
        )
        .await?;

        let store = GraphqlDirectoryStore::new(node.clone());
        let first = reconcile_directory_tick(&store, "did:key:home").await?;
        assert_eq!(first.upserted, BTreeSet::from(["did:key:real".to_string()]));

        let stored = store.list_directory_entries("did:key:home").await?;
        assert_eq!(
            stored.get("did:key:real").map(|e| e.last_seen.as_str()),
            Some(canonicalize_last_seen(&updated_at).as_str())
        );

        let second = reconcile_directory_tick(&store, "did:key:home").await?;
        assert_eq!(
            second,
            DirectoryTickOutcome::default(),
            "settled state must be a write-free fixpoint for the runtime updated_at format"
        );
        Ok(())
    }

    #[test]
    fn canonicalize_last_seen_quantizes_offset_and_subseconds_to_utc_seconds() {
        assert_eq!(
            canonicalize_last_seen("2026-07-23T23:30:55.845794+00:00"),
            "2026-07-23T23:30:55Z"
        );
        assert_eq!(
            canonicalize_last_seen("2026-07-23T18:30:55.5-05:00"),
            "2026-07-23T23:30:55Z"
        );
        assert_eq!(
            canonicalize_last_seen("2026-07-23T23:30:55Z"),
            "2026-07-23T23:30:55Z"
        );
        assert_eq!(
            canonicalize_last_seen(&canonicalize_last_seen("2026-07-23T23:30:55.845794+00:00")),
            "2026-07-23T23:30:55Z"
        );
        assert_eq!(canonicalize_last_seen(""), "");
        assert_eq!(canonicalize_last_seen("   "), "");
        assert_eq!(canonicalize_last_seen("not-a-date"), "not-a-date");
    }

    #[tokio::test]
    async fn graphql_tick_projects_enabled_agents_and_retracts_removed_nodes() -> Result<()> {
        let node = Arc::new(crate::test_support::node_builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let seed = r#"mutation {
            local_node: create_Node(input: { node_did: "did:key:local", display_name: "Local",
                default_agent_id: "coder", enabled: true, created_at: "2026-07-23T00:00:00Z" }) { _docID }
            disabled_node: create_Node(input: { node_did: "did:key:disabled", enabled: false,
                created_at: "2026-07-23T00:00:00Z" }) { _docID }
            coder_agent: create_Agent(input: { node_did: "did:key:local", agent_id: "coder",
                display_name: "Coder", inference_profile_id: "profile", enabled: true }) { _docID }
            artist_agent: create_Agent(input: { node_did: "did:key:local", agent_id: "artist",
                display_name: "Artist", inference_profile_id: "profile", enabled: true }) { _docID }
            disabled_agent: create_Agent(input: { node_did: "did:key:local", agent_id: "disabled",
                inference_profile_id: "profile", enabled: false }) { _docID }
            foreign_agent: create_Agent(input: { node_did: "did:key:foreign", agent_id: "coder",
                display_name: "Foreign", inference_profile_id: "profile", enabled: true }) { _docID }
        }"#;
        crate::config_client::ConfigAccess::write_local_response(
            &node,
            "test.seed_directory",
            seed,
        )
        .await?;
        let store = GraphqlDirectoryStore::new(node.clone());
        let first = reconcile_directory_tick(&store, "did:key:home").await?;
        assert_eq!(
            first.upserted,
            BTreeSet::from(["did:key:local".to_string()])
        );
        let entries = store.list_directory_entries("did:key:home").await?;
        let entry = &entries["did:key:local"];
        assert_eq!(entry.agents, ["Artist", "Coder"]);
        assert_eq!(entry.agent_ids, ["artist", "coder"]);
        assert_eq!(entry.default_agent_id, "coder");
        assert_eq!(entry.last_seen, "");
        assert_eq!(
            reconcile_directory_tick(&store, "did:key:home").await?,
            DirectoryTickOutcome::default()
        );
        crate::config_client::ConfigAccess::write_local_response(&node, "test.remove_directory_node",
            r#"mutation { delete_Node(filter: { node_did: { _eq: "did:key:local" } }) { _docID } }"#).await?;
        assert_eq!(
            reconcile_directory_tick(&store, "did:key:home")
                .await?
                .retracted,
            BTreeSet::from(["did:key:local".to_string()])
        );
        assert!(store
            .list_directory_entries("did:key:home")
            .await?
            .is_empty());
        Ok(())
    }
}

#[cfg(test)]
mod source_partition_regression_tests {
    use super::*;

    #[tokio::test]
    async fn graphql_tick_preserves_foreign_row_with_same_node_did() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = Arc::new(
            crate::test_support::node_builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;

        let foreign_source = "did:key:foreign-home";
        let local_source = "did:key:local-home";
        let node_did = "did:key:shared-agent";
        let foreign_key = directory_entry_key(foreign_source, node_did);
        let seed = format!(
            r#"mutation {{
                create_NodeDirectoryEntry(input: {{
                    directory_key: "{foreign_key}",
                    node_did: "{node_did}",
                    source_did: "{foreign_source}",
                    display_name: "Foreign",
                    runtime_state: "running",
                    updated_at: "2026-07-23T00:00:00Z"
                }}) {{ _docID }}
                create_Node(input: {{
                    node_did: "{node_did}",
                    display_name: "Local",
                    enabled: true,
                    created_at: "2026-07-23T00:00:00Z"
                }}) {{ _docID }}
            }}"#
        );
        crate::config_client::ConfigAccess::write_local_response(
            &node,
            "test.seed_source_partitions",
            &seed,
        )
        .await?;

        let store = GraphqlDirectoryStore::new(node.clone());
        let first = reconcile_directory_tick(&store, local_source).await?;
        assert_eq!(first.upserted, BTreeSet::from([node_did.to_string()]));
        assert_eq!(
            store.list_directory_entries(foreign_source).await?[node_did].display_name,
            "Foreign",
            "local projection must not overwrite the foreign same-DID row"
        );
        assert_eq!(
            store.list_directory_entries(local_source).await?[node_did].display_name,
            "Local"
        );

        let delete = format!(
            r#"mutation {{ delete_Node(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{ _docID }} }}"#
        );
        crate::config_client::ConfigAccess::write_local_response(
            &node,
            "test.remove_local_source",
            &delete,
        )
        .await?;
        let retracted = reconcile_directory_tick(&store, local_source).await?;
        assert_eq!(retracted.retracted, BTreeSet::from([node_did.to_string()]));
        assert!(store.list_directory_entries(local_source).await?.is_empty());
        assert_eq!(
            store.list_directory_entries(foreign_source).await?[node_did].display_name,
            "Foreign"
        );
        Ok(())
    }
}
