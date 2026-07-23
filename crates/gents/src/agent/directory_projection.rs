//! Fleet-discovery directory projection (issue #714).
//!
//! Projects AgentPrincipal x AgentBehavior x AgentRuntime into
//! AgentDirectoryEntry rows — the replicated agent index the `machine`
//! pairing template pushes to attached clients. Modeled in
//! `Proofs/PeerRegistryDiscovery/DirectoryProjection.lean`; fenced by
//! `tests/conformance/directory_projection.rs`. The sweep runs on source
//! Update events, so the load-bearing property is that a settled state is a
//! write-free fixpoint. The projection owns every row in the collection —
//! derived state, rebuildable at any time.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::{EmbeddedNode, EventName, QueryResponse};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;

/// One row of the fleet-discovery directory: a principal's identity,
/// available behaviors, and last-known runtime state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub agent_did: String,
    pub display_name: String,
    pub behaviors: Vec<String>,
    pub runtime_state: String,
    pub last_seen: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryTickOutcome {
    pub upserted: BTreeSet<String>,
    pub refreshed: BTreeSet<String>,
    pub retracted: BTreeSet<String>,
}

#[async_trait]
pub trait DirectoryStore: Send + Sync {
    /// Live `AgentPrincipal` rows, as (agent_did, display_name).
    async fn load_principals(&self) -> Result<Vec<(String, String)>>;
    /// Behavior display names for each principal, keyed by agent_did.
    async fn load_behavior_names(&self) -> Result<BTreeMap<String, Vec<String>>>;
    /// Runtime (process_state, updated_at) for each principal, keyed by
    /// agent_did. `updated_at` here is `AgentRuntime.updated_at` — NOT
    /// wall-clock — so the settled check stays a pure function of source
    /// rows.
    async fn load_runtime_states(&self) -> Result<BTreeMap<String, (String, String)>>;
    async fn list_directory_entries(&self) -> Result<BTreeMap<String, DirectoryEntry>>;
    async fn upsert_directory_entry(&self, entry: &DirectoryEntry) -> Result<()>;
    async fn delete_directory_entry(&self, agent_did: &str) -> Result<()>;
}

/// The projection: one entry per principal, contents a function of the
/// principal's payload (display name, behavior names, runtime state).
/// Mirrors Lean `project`.
pub fn derive_directory_entries(
    principals: &[(String, String)],
    behaviors: &BTreeMap<String, Vec<String>>,
    runtimes: &BTreeMap<String, (String, String)>,
) -> BTreeMap<String, DirectoryEntry> {
    principals
        .iter()
        .filter(|(did, _)| !did.trim().is_empty())
        .map(|(did, display_name)| {
            let mut names = behaviors.get(did).cloned().unwrap_or_default();
            names.sort();
            names.dedup();
            let (runtime_state, last_seen) = runtimes.get(did).cloned().unwrap_or_default();
            (
                did.clone(),
                DirectoryEntry {
                    agent_did: did.clone(),
                    display_name: display_name.clone(),
                    behaviors: names,
                    runtime_state,
                    last_seen,
                },
            )
        })
        .collect()
}

/// One reconcile sweep: derive the desired directory rows from source
/// collections and diff against the live `AgentDirectoryEntry` rows,
/// upserting/refreshing/retracting exactly what has drifted. Mirrors Lean
/// `projectStep`; a settled state (desired == existing) must be a
/// write-free fixpoint (`settled_fixpoint`), since the sweep runs on every
/// Update event.
pub async fn reconcile_directory_tick(store: &dyn DirectoryStore) -> Result<DirectoryTickOutcome> {
    let principals = store
        .load_principals()
        .await
        .context("load agent principals")?;
    let behaviors = store
        .load_behavior_names()
        .await
        .context("load behavior names")?;
    let runtimes = store
        .load_runtime_states()
        .await
        .context("load runtime states")?;
    let desired = derive_directory_entries(&principals, &behaviors, &runtimes);
    let existing = store
        .list_directory_entries()
        .await
        .context("list directory entries")?;

    let mut outcome = DirectoryTickOutcome::default();
    for (did, entry) in &desired {
        match existing.get(did) {
            Some(row) if row == entry => {}
            Some(_) => {
                store
                    .upsert_directory_entry(entry)
                    .await
                    .with_context(|| format!("refresh directory entry for {did}"))?;
                outcome.refreshed.insert(did.clone());
            }
            None => {
                store
                    .upsert_directory_entry(entry)
                    .await
                    .with_context(|| format!("upsert directory entry for {did}"))?;
                outcome.upserted.insert(did.clone());
            }
        }
    }
    for did in existing.keys() {
        if !desired.contains_key(did) {
            store
                .delete_directory_entry(did)
                .await
                .with_context(|| format!("retract directory entry for {did}"))?;
            outcome.retracted.insert(did.clone());
        }
    }
    Ok(outcome)
}

/// Run the directory projection reconciler until cancelled. Unlike the P2P
/// reconcilers, this has no P2P-idle guard: the directory is useful even
/// without P2P transport (the desktop reads it locally), so it always runs.
pub async fn run_directory_projection(
    node: Arc<EmbeddedNode>,
    cancel: CancellationToken,
) -> Result<()> {
    let store = GraphqlDirectoryStore::new(node.clone());
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval =
        tokio::time::interval(crate::agent::p2p_reconcile::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    sweep_directory(&store).await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => sweep_directory(&store).await,
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("directory projection update subscription closed; continuing with periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "directory projection update subscription dropped messages");
                }
                sweep_directory(&store).await;
            }
        }
    }
}

async fn sweep_directory(store: &GraphqlDirectoryStore) {
    match reconcile_directory_tick(store).await {
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

struct GraphqlDirectoryStore {
    node: Arc<EmbeddedNode>,
}

impl GraphqlDirectoryStore {
    fn new(node: Arc<EmbeddedNode>) -> Self {
        Self { node }
    }
}

#[async_trait]
impl DirectoryStore for GraphqlDirectoryStore {
    async fn load_principals(&self) -> Result<Vec<(String, String)>> {
        let query = r#"{
            AgentPrincipal {
                agent_did
                display_name
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query AgentPrincipal")?;
        Ok(rows::<PrincipalRow>(&response, "AgentPrincipal")?
            .into_iter()
            .filter_map(|row| {
                let did = row.agent_did?.trim().to_string();
                if did.is_empty() {
                    return None;
                }
                let display_name = row
                    .display_name
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string();
                Some((did, display_name))
            })
            .collect())
    }

    async fn load_behavior_names(&self) -> Result<BTreeMap<String, Vec<String>>> {
        let query = r#"{
            AgentBehavior {
                agent_did
                display_name
                behavior_id
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query AgentBehavior")?;
        let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in rows::<BehaviorRow>(&response, "AgentBehavior")? {
            let Some(did) = row.agent_did.map(|did| did.trim().to_string()) else {
                continue;
            };
            if did.is_empty() {
                continue;
            }
            let name = row
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| {
                    row.behavior_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned)
                });
            if let Some(name) = name {
                grouped.entry(did).or_default().push(name);
            }
        }
        Ok(grouped)
    }

    async fn load_runtime_states(&self) -> Result<BTreeMap<String, (String, String)>> {
        let query = r#"{
            AgentRuntime {
                agent_did
                process_state
                updated_at
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query AgentRuntime")?;
        Ok(rows::<RuntimeRow>(&response, "AgentRuntime")?
            .into_iter()
            .filter_map(|row| {
                let did = row.agent_did?.trim().to_string();
                if did.is_empty() {
                    return None;
                }
                let process_state = row
                    .process_state
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string();
                let updated_at = row
                    .updated_at
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string();
                Some((did, (process_state, updated_at)))
            })
            .collect())
    }

    async fn list_directory_entries(&self) -> Result<BTreeMap<String, DirectoryEntry>> {
        let query = r#"{
            AgentDirectoryEntry {
                agent_did
                display_name
                behaviors
                runtime_state
                last_seen
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query AgentDirectoryEntry")?;
        Ok(rows::<DirectoryRow>(&response, "AgentDirectoryEntry")?
            .into_iter()
            .filter_map(|row| {
                let did = row.agent_did?.trim().to_string();
                if did.is_empty() {
                    return None;
                }
                let entry = DirectoryEntry {
                    agent_did: did.clone(),
                    display_name: row.display_name.unwrap_or_default(),
                    behaviors: row.behaviors.unwrap_or_default(),
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
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "upsert AgentDirectoryEntry")
    }

    async fn delete_directory_entry(&self, agent_did: &str) -> Result<()> {
        let mutation = delete_directory_entry_mutation(agent_did);
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "delete AgentDirectoryEntry")
    }
}

fn upsert_directory_entry_mutation(entry: &DirectoryEntry, now: &str) -> String {
    let agent_did = escape_graphql_string(&entry.agent_did);
    let display_name = escape_graphql_string(&entry.display_name);
    let behaviors = graphql_string_list_literal(entry.behaviors.iter().map(String::as_str));
    let runtime_state = escape_graphql_string(&entry.runtime_state);
    let last_seen = escape_graphql_string(&entry.last_seen);
    let now = escape_graphql_string(now);
    format!(
        r#"mutation {{
            upsert_AgentDirectoryEntry(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                add: {{
                    agent_did: "{agent_did}",
                    display_name: "{display_name}",
                    behaviors: {behaviors},
                    runtime_state: "{runtime_state}",
                    last_seen: "{last_seen}",
                    updated_at: "{now}"
                }},
                update: {{
                    display_name: "{display_name}",
                    behaviors: {behaviors},
                    runtime_state: "{runtime_state}",
                    last_seen: "{last_seen}",
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

fn delete_directory_entry_mutation(agent_did: &str) -> String {
    let agent_did = escape_graphql_string(agent_did);
    format!(
        r#"mutation {{
            delete_AgentDirectoryEntry(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ _docID }}
        }}"#
    )
}

/// Renders a GraphQL string-list literal. Empty renders as `null`, never
/// `[]` — an empty list literal types as `JsonArray` and corrupts nillable
/// array columns.
fn graphql_string_list_literal<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let items = values
        .into_iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>();
    if items.is_empty() {
        "null".to_string()
    } else {
        format!("[{}]", items.join(", "))
    }
}

fn ensure_no_errors(response: &QueryResponse, label: &str) -> Result<()> {
    if response.has_errors() {
        bail!("{label} failed: {:?}", response.errors);
    }
    Ok(())
}

fn rows<T>(response: &QueryResponse, field: &str) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(value) = response.data.as_ref().and_then(|data| data.get(field)) else {
        return Ok(Vec::new());
    };
    serde_json::from_value(value.clone()).with_context(|| format!("decode {field} rows"))
}

#[derive(Deserialize)]
struct PrincipalRow {
    agent_did: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct BehaviorRow {
    agent_did: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
}

#[derive(Deserialize)]
struct RuntimeRow {
    agent_did: Option<String>,
    #[serde(default)]
    process_state: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Deserialize)]
struct DirectoryRow {
    agent_did: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    behaviors: Option<Vec<String>>,
    #[serde(default)]
    runtime_state: Option<String>,
    #[serde(default)]
    last_seen: Option<String>,
}
