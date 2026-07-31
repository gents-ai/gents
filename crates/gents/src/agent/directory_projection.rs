//! Fleet-discovery directory projection (issue #714).
//!
//! Projects AgentPrincipal x AgentBehavior x AgentRuntime into
//! AgentDirectoryEntry rows — the replicated agent index the `machine`
//! pairing template pushes to attached clients. Modeled in
//! `Proofs/PeerRegistryDiscovery/DirectoryProjection.lean`; fenced by
//! `tests/conformance/directory_projection.rs`. The sweep runs on source
//! Update events, so the load-bearing property is that a settled state is a
//! write-free fixpoint. Each runtime owns only the rows stamped with its
//! source DID; replicated foreign rows remain outside its projection.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::{EmbeddedNode, EventName, QueryResponse};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub directory_key: String,
    pub agent_did: String,
    pub source_did: String,
    pub display_name: String,
    pub behaviors: Vec<String>,
    /// Index-aligned with `behaviors` (`behavior_ids[i]` names `behaviors[i]`)
    /// so clients can stamp `AgentRequest.behavior_id` from a picked display
    /// name.
    pub behavior_ids: Vec<String>,
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
    async fn load_principals(&self) -> Result<Vec<(String, String)>>;
    /// Per principal, the enabled behaviors as `(behavior_id, display_name)`
    /// pairs (display name falls back to the id when blank).
    async fn load_behaviors(&self) -> Result<BTreeMap<String, Vec<(String, String)>>>;
    async fn load_runtime_states(&self) -> Result<BTreeMap<String, (String, String)>>;
    async fn list_directory_entries(
        &self,
        source_did: &str,
    ) -> Result<BTreeMap<String, DirectoryEntry>>;
    async fn upsert_directory_entry(&self, entry: &DirectoryEntry) -> Result<()>;
    async fn delete_directory_entry(&self, source_did: &str, agent_did: &str) -> Result<()>;
}

pub fn directory_entry_key(source_did: &str, agent_did: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(source_did.as_bytes());
    digest.update(b"\x1f");
    digest.update(agent_did.as_bytes());
    format!(
        "dir-{}",
        bs58::encode(&digest.finalize()[..16]).into_string()
    )
}

/// Canonicalize a runtime `updated_at` into the exact lexical form DefraDB's
/// `DateTime` column stores and returns, so a settled directory row stays a
/// write-free fixpoint.
///
/// `AgentRuntime.updated_at` is a *String* column the runtime writes as
/// `Utc::now().to_rfc3339()` — offset `+00:00`, sub-second precision — but
/// `AgentDirectoryEntry.last_seen` is a `DateTime` column that re-serializes on
/// storage: it renders the offset as `Z`, and its sub-second rendering is not
/// guaranteed byte-stable (Go's DefraDB trims trailing zeros; chrono buckets to
/// 3/6/9 digits). Comparing the raw source string against the normalized stored
/// string made `existing != desired` on *every* sweep, so each tick re-upserted
/// the row, and each upsert re-fired the Update-driven sweep — an unbounded
/// write/event storm that starved the runtime.
///
/// Quantizing to whole-second UTC `...Z` removes the sub-second ambiguity
/// entirely: DefraDB round-trips a second-precision `Z` value unchanged (the
/// conformance fence and the embedded-node fixpoint test pin this), and
/// sub-second freshness is not meaningful for a fleet "last seen". The function
/// is idempotent (`canon(canon(x)) == canon(x)`), preserving the model's
/// projection idempotence. Blank input (a principal with no `AgentRuntime` row)
/// stays blank → rendered as `null`. Genuinely unparseable non-blank input
/// passes through trimmed unchanged; that one row still fails its upsert, as
/// before, under the per-entry error tolerance in `reconcile_directory_tick`.
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

/// The projection: one entry per principal, contents a function of the
/// principal's payload (display name, behavior names, runtime state). The
/// runtime `updated_at` is canonicalized (see `canonicalize_last_seen`) so the
/// derived `last_seen` matches its own stored `DateTime` round-trip.
/// Mirrors Lean `project`.
pub fn derive_directory_entries(
    source_did: &str,
    principals: &[(String, String)],
    behaviors: &BTreeMap<String, Vec<(String, String)>>,
    runtimes: &BTreeMap<String, (String, String)>,
) -> BTreeMap<String, DirectoryEntry> {
    principals
        .iter()
        .filter(|(did, _)| !did.trim().is_empty())
        .map(|(did, display_name)| {
            // Sort by (name, id) for a stable picker order, then dedup by id
            // (a behavior's id determines its identity; same id implies same
            // name, so duplicates land adjacent after the sort).
            let mut pairs = behaviors.get(did).cloned().unwrap_or_default();
            pairs.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
            pairs.dedup_by(|a, b| a.0 == b.0);
            let names = pairs.iter().map(|(_, name)| name.clone()).collect();
            let ids = pairs.into_iter().map(|(id, _)| id).collect();
            let (runtime_state, updated_at) = runtimes.get(did).cloned().unwrap_or_default();
            (
                did.clone(),
                DirectoryEntry {
                    directory_key: directory_entry_key(source_did, did),
                    agent_did: did.clone(),
                    source_did: source_did.to_string(),
                    display_name: display_name.clone(),
                    behaviors: names,
                    behavior_ids: ids,
                    runtime_state,
                    last_seen: canonicalize_last_seen(&updated_at),
                },
            )
        })
        .collect()
}

/// One reconcile sweep: derive the desired directory rows from source
/// collections and diff against this source's `AgentDirectoryEntry` rows,
/// upserting/refreshing/retracting exactly what has drifted. Mirrors Lean
/// `projectStep`; a settled state (desired == existing) must be a
/// write-free fixpoint (`settled_fixpoint`), since the sweep runs on every
/// Update event.
pub async fn reconcile_directory_tick(
    store: &dyn DirectoryStore,
    source_did: &str,
) -> Result<DirectoryTickOutcome> {
    let principals = store
        .load_principals()
        .await
        .context("load agent principals")?;
    let behaviors = store.load_behaviors().await.context("load behaviors")?;
    let runtimes = store
        .load_runtime_states()
        .await
        .context("load runtime states")?;
    let desired = derive_directory_entries(source_did, &principals, &behaviors, &runtimes);
    let existing = store
        .list_directory_entries(source_did)
        .await
        .context("list directory entries")?;

    // Per-entry error tolerance: one principal's malformed source row (e.g.
    // no AgentRuntime row, previously producing an unparseable `last_seen`)
    // must not abort the whole sweep. Warn-and-continue past each failure,
    // collecting only the first error to return once every entry has had a
    // chance to converge; outcome sets only ever record operations that
    // actually succeeded.
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
                    tracing::warn!(agent_did = %did, error = %error, "directory entry refresh failed; continuing sweep");
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
                    tracing::warn!(agent_did = %did, error = %error, "directory entry upsert failed; continuing sweep");
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
                    tracing::warn!(agent_did = %did, error = %error, "directory entry retraction failed; continuing sweep");
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

pub async fn run_directory_projection(
    node: Arc<EmbeddedNode>,
    source_did: String,
    cancel: CancellationToken,
) -> Result<()> {
    let store = GraphqlDirectoryStore::new(node.clone());
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval =
        tokio::time::interval(crate::agent::p2p_reconcile::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    sweep_directory(&store, &source_did).await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => sweep_directory(&store, &source_did).await,
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("directory projection update subscription closed; continuing with periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "directory projection update subscription dropped messages");
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
                enabled
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query AgentPrincipal")?;
        Ok(rows::<PrincipalRow>(&response, "AgentPrincipal")?
            .into_iter()
            .filter_map(|row| {
                if !row.enabled.unwrap_or(true) {
                    return None;
                }
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

    async fn load_behaviors(&self) -> Result<BTreeMap<String, Vec<(String, String)>>> {
        let query = r#"{
            AgentBehavior {
                agent_did
                display_name
                behavior_id
                enabled
            }
        }"#;
        let response = self.node.execute(query).await;
        ensure_no_errors(&response, "query AgentBehavior")?;
        let mut grouped: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for row in rows::<BehaviorRow>(&response, "AgentBehavior")? {
            if !row.enabled.unwrap_or(true) {
                continue;
            }
            let Some(did) = row.agent_did.map(|did| did.trim().to_string()) else {
                continue;
            };
            if did.is_empty() {
                continue;
            }
            let Some(behavior_id) = row
                .behavior_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
            else {
                continue;
            };
            let name = row
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| behavior_id.clone());
            grouped.entry(did).or_default().push((behavior_id, name));
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

    async fn list_directory_entries(
        &self,
        source_did: &str,
    ) -> Result<BTreeMap<String, DirectoryEntry>> {
        let source_did = escape_graphql_string(source_did);
        let query = format!(
            r#"{{
            AgentDirectoryEntry(filter: {{ source_did: {{ _eq: "{source_did}" }} }}) {{
                directory_key
                agent_did
                source_did
                display_name
                behaviors
                behavior_ids
                runtime_state
                last_seen
            }}
        }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query AgentDirectoryEntry")?;
        Ok(rows::<DirectoryRow>(&response, "AgentDirectoryEntry")?
            .into_iter()
            .filter_map(|row| {
                let directory_key = row.directory_key?.trim().to_string();
                if directory_key.is_empty() {
                    return None;
                }
                let did = row.agent_did?.trim().to_string();
                if did.is_empty() {
                    return None;
                }
                let entry = DirectoryEntry {
                    directory_key,
                    agent_did: did.clone(),
                    source_did: row.source_did.unwrap_or_default(),
                    display_name: row.display_name.unwrap_or_default(),
                    behaviors: row.behaviors.unwrap_or_default(),
                    behavior_ids: row.behavior_ids.unwrap_or_default(),
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

    async fn delete_directory_entry(&self, source_did: &str, agent_did: &str) -> Result<()> {
        let mutation = delete_directory_entry_mutation(source_did, agent_did);
        let response = self.node.execute(&mutation).await;
        ensure_no_errors(&response, "delete AgentDirectoryEntry")
    }
}

fn upsert_directory_entry_mutation(entry: &DirectoryEntry, now: &str) -> String {
    let agent_did = escape_graphql_string(&entry.agent_did);
    let directory_key =
        escape_graphql_string(&directory_entry_key(&entry.source_did, &entry.agent_did));
    let source_did = escape_graphql_string(&entry.source_did);
    let display_name = escape_graphql_string(&entry.display_name);
    let behaviors = graphql_string_list_literal(entry.behaviors.iter().map(String::as_str));
    // Index-aligned with `behaviors`; rendered as null (never []) when empty
    // — an empty list literal types as JsonArray and corrupts nillable array
    // columns.
    let behavior_ids = graphql_string_list_literal(entry.behavior_ids.iter().map(String::as_str));
    let runtime_state = escape_graphql_string(&entry.runtime_state);
    let last_seen = graphql_nullable_datetime_literal(&entry.last_seen);
    let now = escape_graphql_string(now);
    format!(
        r#"mutation {{
            upsert_AgentDirectoryEntry(
                filter: {{ directory_key: {{ _eq: "{directory_key}" }} }},
                add: {{
                    directory_key: "{directory_key}",
                    agent_did: "{agent_did}",
                    source_did: "{source_did}",
                    display_name: "{display_name}",
                    behaviors: {behaviors},
                    behavior_ids: {behavior_ids},
                    runtime_state: "{runtime_state}",
                    last_seen: {last_seen},
                    updated_at: "{now}"
                }},
                update: {{
                    display_name: "{display_name}",
                    behaviors: {behaviors},
                    behavior_ids: {behavior_ids},
                    runtime_state: "{runtime_state}",
                    last_seen: {last_seen},
                    updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

fn delete_directory_entry_mutation(source_did: &str, agent_did: &str) -> String {
    let directory_key = escape_graphql_string(&directory_entry_key(source_did, agent_did));
    format!(
        r#"mutation {{
            delete_AgentDirectoryEntry(filter: {{
                directory_key: {{ _eq: "{directory_key}" }}
            }}) {{ _docID }}
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

fn graphql_nullable_datetime_literal(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "null".to_string()
    } else {
        format!(r#""{}""#, escape_graphql_string(trimmed))
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
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Deserialize)]
struct BehaviorRow {
    agent_did: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
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
    #[serde(default)]
    directory_key: Option<String>,
    agent_did: Option<String>,
    #[serde(default)]
    source_did: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    behaviors: Option<Vec<String>>,
    #[serde(default)]
    behavior_ids: Option<Vec<String>>,
    #[serde(default)]
    runtime_state: Option<String>,
    #[serde(default)]
    last_seen: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(agent_did: &str, last_seen: &str) -> DirectoryEntry {
        DirectoryEntry {
            directory_key: directory_entry_key("did:key:home", agent_did),
            agent_did: agent_did.to_string(),
            source_did: "did:key:home".to_string(),
            display_name: "Display".to_string(),
            behaviors: Vec::new(),
            behavior_ids: Vec::new(),
            runtime_state: "running".to_string(),
            last_seen: last_seen.to_string(),
        }
    }

    /// C2 regression: a principal with no `AgentRuntime` row derives a blank
    /// `last_seen`; the mutation must render `null`, never `""` — DefraDB
    /// rejects a non-RFC3339 `DateTime` string on create AND upsert, and an
    /// unconditional quoted render poisoned the whole directory sweep.
    #[test]
    fn directory_entry_key_partitions_same_agent_did_by_source() {
        assert_ne!(
            directory_entry_key("did:key:local-home", "did:key:shared-agent"),
            directory_entry_key("did:key:foreign-home", "did:key:shared-agent"),
        );
    }

    #[test]
    fn upsert_mutation_sets_immutable_source_did_only_when_adding() {
        let mutation = upsert_directory_entry_mutation(
            &entry("did:key:running", "2026-07-20T00:00:00Z"),
            "2026-07-23T00:00:00Z",
        );
        let (add, update) = mutation
            .split_once("update: {")
            .expect("upsert mutation has update payload");

        assert!(add.contains(r#"source_did: "did:key:home""#));
        assert!(
            !update.contains("source_did:"),
            "immutable source_did must not be resent in update payload: {update}"
        );
    }

    #[test]
    fn upsert_mutation_renders_null_for_blank_last_seen() {
        for blank in ["", "   "] {
            let mutation = upsert_directory_entry_mutation(
                &entry("did:key:no-runtime", blank),
                "2026-07-23T00:00:00Z",
            );
            assert!(
                mutation.contains("last_seen: null"),
                "blank last_seen ({blank:?}) must render as null: {mutation}"
            );
            assert!(!mutation.contains(r#"last_seen: """#));
        }
    }

    #[test]
    fn upsert_mutation_renders_quoted_last_seen_when_present() {
        let mutation = upsert_directory_entry_mutation(
            &entry("did:key:running", "2026-07-20T00:00:00Z"),
            "2026-07-23T00:00:00Z",
        );
        assert!(mutation.contains(r#"last_seen: "2026-07-20T00:00:00Z""#));
    }

    /// `behavior_ids` follows the same null-never-[] discipline as
    /// `behaviors`, and stays index-aligned when both are populated.
    #[test]
    fn upsert_mutation_renders_null_behavior_ids_when_empty_and_aligned_list_when_present() {
        let empty = upsert_directory_entry_mutation(
            &entry("did:key:no-behaviors", "2026-07-20T00:00:00Z"),
            "2026-07-23T00:00:00Z",
        );
        assert!(
            empty.contains("behavior_ids: null"),
            "empty behavior_ids must render as null, never []: {empty}"
        );

        let mut with_behaviors = entry("did:key:with-behaviors", "2026-07-20T00:00:00Z");
        with_behaviors.behaviors = vec!["Artist".to_string(), "Coder".to_string()];
        with_behaviors.behavior_ids = vec![
            "did:key:a:artist".to_string(),
            "did:key:a:coder".to_string(),
        ];
        let mutation = upsert_directory_entry_mutation(&with_behaviors, "2026-07-23T00:00:00Z");
        assert!(mutation.contains(r#"behaviors: ["Artist", "Coder"]"#));
        assert!(mutation.contains(r#"behavior_ids: ["did:key:a:artist", "did:key:a:coder"]"#));
    }

    /// Round-trip consistency: a stored `null` `last_seen` must decode back
    /// to `""` so the settled comparison against `derive_directory_entries`'
    /// blank default holds (no perpetual refresh loop for runtime-less
    /// principals).
    #[test]
    fn nullable_datetime_literal_blank_maps_to_null_and_round_trips_via_default() {
        assert_eq!(graphql_nullable_datetime_literal(""), "null");
        assert_eq!(graphql_nullable_datetime_literal("  "), "null");
        // DirectoryRow.last_seen is `Option<String>`; DefraDB's stored `null`
        // decodes to `None`, and `unwrap_or_default()` in
        // `list_directory_entries` maps that back to `""` — the same value
        // `derive_directory_entries` defaults to for a runtime-less principal.
        let row: DirectoryRow = serde_json::from_value(serde_json::json!({
            "directory_key": "dir-no-runtime",
            "agent_did": "did:key:no-runtime",
            "source_did": "did:key:home",
            "display_name": "No Runtime",
            "behaviors": [],
            "runtime_state": "",
            "last_seen": null,
        }))
        .expect("stored directory row with null last_seen should deserialize");
        assert_eq!(row.last_seen.unwrap_or_default(), "");
    }

    #[test]
    fn canonicalize_last_seen_quantizes_offset_and_subseconds_to_utc_seconds() {
        // The exact shape the runtime writes: `Utc::now().to_rfc3339()` —
        // `+00:00` offset, sub-second precision. Must render whole-second `Z`.
        assert_eq!(
            canonicalize_last_seen("2026-07-23T23:30:55.845794+00:00"),
            "2026-07-23T23:30:55Z"
        );
        // Non-UTC offset is converted to UTC before quantizing.
        assert_eq!(
            canonicalize_last_seen("2026-07-23T18:30:55.5-05:00"),
            "2026-07-23T23:30:55Z"
        );
        // Already canonical → unchanged (idempotent).
        assert_eq!(
            canonicalize_last_seen("2026-07-23T23:30:55Z"),
            "2026-07-23T23:30:55Z"
        );
        assert_eq!(
            canonicalize_last_seen(&canonicalize_last_seen("2026-07-23T23:30:55.845794+00:00")),
            "2026-07-23T23:30:55Z"
        );
        // Blank stays blank (rendered as null downstream); unparseable passes
        // through trimmed (that one row fails its upsert, per-entry tolerant).
        assert_eq!(canonicalize_last_seen(""), "");
        assert_eq!(canonicalize_last_seen("   "), "");
        assert_eq!(canonicalize_last_seen("not-a-date"), "not-a-date");
    }

    /// Regression fence for the settled-fixpoint bug: an `AgentRuntime.updated_at`
    /// written in the runtime's real `Utc::now().to_rfc3339()` shape (offset
    /// `+00:00`, sub-second precision) used to differ from its own stored
    /// `AgentDirectoryEntry.last_seen` `DateTime` round-trip (offset `Z`),
    /// making every sweep re-upsert and — since the sweep runs on Update events
    /// — self-perpetuate into an unbounded write/event storm. Seeding the exact
    /// runtime format and asserting the second tick is write-free pins the fix.
    #[tokio::test]
    async fn graphql_tick_is_write_free_fixpoint_for_runtime_updated_at_format() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        let updated_at = Utc::now().to_rfc3339();
        let seed = format!(
            r#"mutation {{
                create_AgentPrincipal(input: {{
                    agent_did: "did:key:real",
                    display_name: "Real",
                    enabled: true,
                    created_at: "2026-07-23T00:00:00Z"
                }}) {{ _docID }}
                create_AgentRuntime(input: {{
                    agent_did: "did:key:real",
                    process_state: "running",
                    updated_at: "{updated_at}"
                }}) {{ _docID }}
            }}"#
        );
        let response = node.execute(&seed).await;
        ensure_no_errors(&response, "seed runtime-format updated_at")?;

        let store = GraphqlDirectoryStore::new(node.clone());
        let first = reconcile_directory_tick(&store, "did:key:home").await?;
        assert_eq!(first.upserted, BTreeSet::from(["did:key:real".to_string()]));

        // The stored, canonicalized last_seen is whole-second UTC `Z`.
        let stored = store.list_directory_entries("did:key:home").await?;
        assert_eq!(
            stored.get("did:key:real").map(|e| e.last_seen.as_str()),
            Some(canonicalize_last_seen(&updated_at).as_str())
        );

        // The load-bearing property: a second tick over unchanged sources is a
        // write-free fixpoint (no self-perpetuating storm).
        let second = reconcile_directory_tick(&store, "did:key:home").await?;
        assert_eq!(
            second,
            DirectoryTickOutcome::default(),
            "settled state must be a write-free fixpoint for the runtime updated_at format"
        );
        Ok(())
    }

    /// C2 regression, embedded-node integration (mirrors reciprocal.rs's
    /// `graphql_tick_retracts_for_signed_revoked_membership`): two principals,
    /// one WITHOUT an `AgentRuntime` row, converge in one tick — the
    /// runtime-less principal's directory row must exist with `last_seen`
    /// round-tripping to `""` rather than aborting the sweep. Deleting a
    /// principal then retracts exactly its row.
    #[tokio::test]
    async fn graphql_tick_converges_runtime_less_principal_and_retracts_on_removal() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;

        let seed = r#"mutation {
            create_AgentPrincipal(input: {
                agent_did: "did:key:with-runtime",
                display_name: "With Runtime",
                enabled: true,
                created_at: "2026-07-23T00:00:00Z"
            }) { _docID }
            create_AgentPrincipal(input: {
                agent_did: "did:key:no-runtime",
                display_name: "No Runtime",
                enabled: true,
                created_at: "2026-07-23T00:00:00Z"
            }) { _docID }
            create_AgentPrincipal(input: {
                agent_did: "did:key:disabled",
                display_name: "Disabled",
                enabled: false,
                created_at: "2026-07-23T00:00:00Z"
            }) { _docID }
            create_AgentBehavior(input: {
                behavior_id: "enabled-behavior",
                agent_did: "did:key:with-runtime",
                display_name: "Enabled Behavior",
                enabled: true
            }) { _docID }
            create_AgentBehavior(input: {
                behavior_id: "artist-behavior",
                agent_did: "did:key:with-runtime",
                display_name: "Artist Behavior",
                enabled: true
            }) { _docID }
            create_AgentBehavior(input: {
                behavior_id: "disabled-behavior",
                agent_did: "did:key:with-runtime",
                display_name: "Disabled Behavior",
                enabled: false
            }) { _docID }
            create_AgentRuntime(input: {
                agent_did: "did:key:with-runtime",
                process_state: "running",
                updated_at: "2026-07-23T12:34:56.845794+00:00"
            }) { _docID }
        }"#;
        let response = node.execute(seed).await;
        ensure_no_errors(&response, "seed directory projection principals")?;

        let store = GraphqlDirectoryStore::new(node.clone());
        let outcome = reconcile_directory_tick(&store, "did:key:home").await?;
        assert_eq!(
            outcome.upserted,
            BTreeSet::from([
                "did:key:with-runtime".to_string(),
                "did:key:no-runtime".to_string(),
            ]),
            "the runtime-less principal must not wedge the sweep"
        );

        let entries = store.list_directory_entries("did:key:home").await?;
        let with_runtime = entries
            .get("did:key:with-runtime")
            .expect("with-runtime directory row");
        assert_eq!(with_runtime.runtime_state, "running");
        assert_eq!(
            with_runtime.behaviors,
            vec![
                "Artist Behavior".to_string(),
                "Enabled Behavior".to_string()
            ]
        );
        assert_eq!(
            with_runtime.behavior_ids,
            vec![
                "artist-behavior".to_string(),
                "enabled-behavior".to_string()
            ],
            "behavior_ids must round-trip index-aligned with behaviors through a real node"
        );
        assert!(
            !entries.contains_key("did:key:disabled"),
            "disabled principals must not be advertised"
        );
        // The non-canonical `+00:00`/sub-second updated_at is quantized to
        // whole-second UTC `Z`, matching its own stored DateTime round-trip so
        // the second tick below is write-free rather than a self-perpetuating
        // storm.
        assert_eq!(with_runtime.last_seen, "2026-07-23T12:34:56Z");
        let no_runtime = entries
            .get("did:key:no-runtime")
            .expect("no-runtime directory row present despite no AgentRuntime row");
        assert_eq!(
            no_runtime.last_seen, "",
            "runtime-less principal's stored null last_seen must round-trip to \"\""
        );

        // Settled state is a write-free fixpoint: the runtime-less principal
        // must not keep re-triggering writes forever.
        let second = reconcile_directory_tick(&store, "did:key:home").await?;
        assert_eq!(second, DirectoryTickOutcome::default());

        let delete = r#"mutation {
            delete_AgentPrincipal(filter: { agent_did: { _eq: "did:key:no-runtime" } }) { _docID }
        }"#;
        let response = node.execute(delete).await;
        ensure_no_errors(&response, "delete directory-projection principal")?;

        let outcome = reconcile_directory_tick(&store, "did:key:home").await?;
        assert_eq!(
            outcome.retracted,
            BTreeSet::from(["did:key:no-runtime".to_string()])
        );

        let entries = store.list_directory_entries("did:key:home").await?;
        assert!(!entries.contains_key("did:key:no-runtime"));
        assert!(entries.contains_key("did:key:with-runtime"));

        Ok(())
    }
}

#[cfg(test)]
mod source_partition_regression_tests {
    use super::*;

    #[tokio::test]
    async fn graphql_tick_preserves_foreign_row_with_same_agent_did() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;

        let foreign_source = "did:key:foreign-home";
        let local_source = "did:key:local-home";
        let agent_did = "did:key:shared-agent";
        let foreign_key = directory_entry_key(foreign_source, agent_did);
        let seed = format!(
            r#"mutation {{
                create_AgentDirectoryEntry(input: {{
                    directory_key: "{foreign_key}",
                    agent_did: "{agent_did}",
                    source_did: "{foreign_source}",
                    display_name: "Foreign",
                    runtime_state: "running",
                    updated_at: "2026-07-23T00:00:00Z"
                }}) {{ _docID }}
                create_AgentPrincipal(input: {{
                    agent_did: "{agent_did}",
                    display_name: "Local",
                    enabled: true,
                    created_at: "2026-07-23T00:00:00Z"
                }}) {{ _docID }}
            }}"#
        );
        ensure_no_errors(
            &node.execute(&seed).await,
            "seed same-DID source partitions",
        )?;

        let store = GraphqlDirectoryStore::new(node.clone());
        let first = reconcile_directory_tick(&store, local_source).await?;
        assert_eq!(first.upserted, BTreeSet::from([agent_did.to_string()]));
        assert_eq!(
            store.list_directory_entries(foreign_source).await?[agent_did].display_name,
            "Foreign",
            "local projection must not overwrite the foreign same-DID row"
        );
        assert_eq!(
            store.list_directory_entries(local_source).await?[agent_did].display_name,
            "Local"
        );

        let delete = format!(
            r#"mutation {{ delete_AgentPrincipal(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ _docID }} }}"#
        );
        ensure_no_errors(
            &node.execute(&delete).await,
            "delete local same-DID principal",
        )?;
        let retracted = reconcile_directory_tick(&store, local_source).await?;
        assert_eq!(retracted.retracted, BTreeSet::from([agent_did.to_string()]));
        assert!(store.list_directory_entries(local_source).await?.is_empty());
        assert_eq!(
            store.list_directory_entries(foreign_source).await?[agent_did].display_name,
            "Foreign"
        );
        Ok(())
    }
}
