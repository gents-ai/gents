//! Fleet-discovery directory projection (issue #714).
//!
//! Projects AgentPrincipal x AgentBehavior x AgentBehaviorReadiness into
//! AgentDirectoryEntry rows — the replicated agent index the `machine`
//! pairing template pushes to attached clients. Modeled in
//! `Proofs/PeerRegistryDiscovery/DirectoryProjection.lean`; fenced by
//! `tests/conformance/directory_projection.rs`. The sweep runs on source
//! Update events, so the load-bearing property is that a settled state is a
//! write-free fixpoint. Each runtime owns only the rows stamped with its
//! source DID; replicated foreign rows remain outside its projection.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::{EmbeddedNode, EventName, QueryResponse};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::agent::persona_presets::{builtin_preset_names, classify_tools};
#[cfg(test)]
use crate::graphql::ensure_no_errors;
use crate::graphql::{escape_graphql_string, rows};

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
    /// The agent's resolved default (from `AgentPrincipal.default_behavior_id`),
    /// so clients can badge threads bound to a NON-default behavior; empty
    /// when the principal has none.
    pub default_behavior_id: String,
    /// Per-behavior persona dimensions, index-aligned with `behavior_ids`.
    /// `"backend_id|model_name"`, split on the FIRST `|` (ids must not
    /// contain `|`). Both dimensions come from the selected same-owner profile.
    pub behavior_models: Vec<String>,
    /// Per-behavior HostTools root, empty when absent, index-aligned with `behavior_ids`.
    pub behavior_roots: Vec<String>,
    /// Per-behavior built-in preset name, or `""` for a custom selection (or
    /// a missing one), index-aligned with `behavior_ids`.
    pub behavior_presets: Vec<String>,
    /// Explicit per-behavior inference profile ID, index-aligned with `behavior_ids`.
    pub behavior_profiles: Vec<String>,
    /// Principal-scoped pickable options for the persona composer, flattened into the four
    /// `available_models`/`allowed_roots`/`permission_presets`/
    /// `available_profiles` columns at upsert/list time.
    pub options: CatalogOptions,
    pub runtime_state: String,
    pub last_seen: String,
}

/// Derived persona dimensions resolved through same-owner context and inference links.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BehaviorInfo {
    pub behavior_id: String,
    pub display_name: String,
    pub backend_id: String,
    pub model_name: String,
    pub host_root: String,
    pub preset: String,
    pub inference_profile_id: String,
}

/// Principal-scoped pickable options for the persona composer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogOptions {
    /// `"backend_id|model_name"`, sorted, deduped.
    pub available_models: Vec<String>,
    /// Enabled local-operator `WorkspaceRoot` paths, sorted and ceiling-filtered.
    /// These currently have no principal scope; this is not a per-principal ACP grant.
    pub allowed_roots: Vec<String>,
    /// `builtin_preset_names()`.
    pub permission_presets: Vec<String>,
    /// `"profile_id|display_name"`, sorted; split on the FIRST `|` —
    /// display names may contain `|`, profile ids must not.
    pub available_profiles: Vec<String>,
    /// Compact JSON object per profile (#1050's info-card fields),
    /// INDEX-ALIGNED with `available_profiles` (`params[i]` describes
    /// `profiles[i]`). See [`render_profile_params_json`] for the exact
    /// shape.
    pub available_profile_params: Vec<String>,
}

/// Everything `derive_directory_entries` consumes, loaded as one unit. The
/// sweep runs on every source Update event (and concurrently across
/// automated homes), so the store contract is a *single* snapshot load per
/// tick — `GraphqlDirectoryStore` satisfies it with one multi-root query
/// rather than one round trip per collection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceSnapshot {
    /// One row per enabled principal: `(agent_did, display_name,
    /// default_behavior_id)`. `default_behavior_id` is empty when the
    /// principal has none.
    pub principals: Vec<(String, String, String)>,
    /// Per principal, the enabled behaviors as `BehaviorInfo` (display name
    /// falls back to the id when blank).
    pub behaviors: BTreeMap<String, Vec<BehaviorInfo>>,
    /// Per principal, `(process_state, updated_at)` from the authoritative
    /// `AgentBehaviorReadiness` projection.
    pub runtimes: BTreeMap<String, (String, String)>,
    /// Composer options keyed by principal; configuration and OAuth catalogs
    /// must never be inherited from another principal on this source.
    pub options: BTreeMap<String, CatalogOptions>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryTickOutcome {
    pub upserted: BTreeSet<String>,
    pub refreshed: BTreeSet<String>,
    pub retracted: BTreeSet<String>,
}

#[async_trait]
pub trait DirectoryStore: Send + Sync {
    /// Load every projection input in one shot (see [`SourceSnapshot`]).
    async fn load_source_snapshot(&self) -> Result<SourceSnapshot>;
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
/// `AgentBehaviorReadiness.updated_at` is a *String* column the runtime writes as
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
/// projection idempotence. Blank input (a principal with no readiness row)
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
    principals: &[(String, String, String)],
    behaviors: &BTreeMap<String, Vec<BehaviorInfo>>,
    runtimes: &BTreeMap<String, (String, String)>,
    options: &BTreeMap<String, CatalogOptions>,
) -> BTreeMap<String, DirectoryEntry> {
    principals
        .iter()
        .filter(|(did, _, _)| !did.trim().is_empty())
        .map(|(did, display_name, default_behavior_id)| {
            // Sort by (name, id) for a stable picker order, then dedup by id
            // (a behavior's id determines its identity; same id implies same
            // name, so duplicates land adjacent after the sort).
            let mut infos = behaviors.get(did).cloned().unwrap_or_default();
            infos.sort_by(|a, b| {
                a.display_name
                    .cmp(&b.display_name)
                    .then_with(|| a.behavior_id.cmp(&b.behavior_id))
            });
            infos.dedup_by(|a, b| a.behavior_id == b.behavior_id);

            let names = infos.iter().map(|info| info.display_name.clone()).collect();
            let ids: Vec<String> = infos.iter().map(|info| info.behavior_id.clone()).collect();
            let behavior_models = infos
                .iter()
                .map(|info| {
                    if info.backend_id.is_empty() && info.model_name.is_empty() {
                        String::new()
                    } else {
                        format!("{}|{}", info.backend_id, info.model_name)
                    }
                })
                .collect();
            let behavior_roots = infos.iter().map(|info| info.host_root.clone()).collect();
            let behavior_presets = infos.iter().map(|info| info.preset.clone()).collect();
            let behavior_profiles = infos
                .iter()
                .map(|info| info.inference_profile_id.clone())
                .collect();

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
                    default_behavior_id: default_behavior_id.clone(),
                    behavior_models,
                    behavior_roots,
                    behavior_presets,
                    behavior_profiles,
                    options: options.get(did).cloned().unwrap_or_default(),
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
    let snapshot = store
        .load_source_snapshot()
        .await
        .context("load source snapshot")?;
    let desired = derive_directory_entries(
        source_did,
        &snapshot.principals,
        &snapshot.behaviors,
        &snapshot.runtimes,
        &snapshot.options,
    );
    let existing = store
        .list_directory_entries(source_did)
        .await
        .context("list directory entries")?;

    // Per-entry error tolerance: one principal's malformed source row (e.g.
    // no readiness row, previously producing an unparseable `last_seen`)
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
    ceiling_root: Option<std::path::PathBuf>,
    cancel: CancellationToken,
) -> Result<()> {
    let store = GraphqlDirectoryStore::new(node.clone(), ceiling_root);
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

// `pub(crate)`: the persona-request reconciler's embedded-node integration
// test constructs this directly to run a directory-projection tick over the
// same node and prove the two reconcilers converge together (issue's PR 1+2
// tie-in) — this is the only reason it needs to be more than module-private.
pub(crate) struct GraphqlDirectoryStore {
    node: Arc<EmbeddedNode>,
    /// Operator tool-root ceiling (`--tool-root`); see
    /// [`filter_roots_to_ceiling`].
    ceiling_root: Option<std::path::PathBuf>,
}

impl GraphqlDirectoryStore {
    pub(crate) fn new(node: Arc<EmbeddedNode>, ceiling_root: Option<std::path::PathBuf>) -> Self {
        Self { node, ceiling_root }
    }
}

#[async_trait]
impl DirectoryStore for GraphqlDirectoryStore {
    async fn load_source_snapshot(&self) -> Result<SourceSnapshot> {
        // One multi-root document on purpose: the sweep re-runs on every
        // source Update event, and homes running concurrent automation
        // multiply that rate — per-collection executes here would each pay
        // their own parse/plan/transaction overhead per sweep.
        let mut query = String::from("{ AgentPrincipal { agent_did display_name default_behavior_id enabled } AgentBehaviorReadiness { agent_did snapshot_json updated_at } WorkspaceRoot { root_path enabled }");
        for collection in DIRECTORY_CONFIG_COLLECTIONS {
            let (fields, _) = crate::config_client::config_projection(*collection, None)?;
            query.push_str(&format!(
                " {} {{ {}",
                collection.graphql_type(),
                fields.join(" ")
            ));
            if *collection == crate::collection::Collection::InferenceBackend {
                query.push_str(" catalogs probe_status last_probe");
            }
            query.push_str(" }");
        }
        query.push('}');
        let response = crate::graphql::graphql_with_transaction_retry(
            &self.node,
            &query,
            "query directory source snapshot",
        )
        .await?;
        let principals = parse_principals(&response)?;
        let (behaviors, options) =
            parse_config_projection(&response, &principals, self.ceiling_root.as_deref())?;
        Ok(SourceSnapshot {
            principals,
            behaviors,
            runtimes: parse_runtime_states(&response)?,
            options,
        })
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
                default_behavior_id
                behavior_models
                behavior_roots
                behavior_presets
                behavior_profiles
                available_models
                allowed_roots
                permission_presets
                available_profiles
                available_profile_params
                runtime_state
                last_seen
            }}
        }}"#
        );
        let response = crate::graphql::graphql_with_transaction_retry(
            &self.node,
            &query,
            "query AgentDirectoryEntry",
        )
        .await?;
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
                    default_behavior_id: row.default_behavior_id.unwrap_or_default(),
                    behavior_models: row.behavior_models.unwrap_or_default(),
                    behavior_roots: row.behavior_roots.unwrap_or_default(),
                    behavior_presets: row.behavior_presets.unwrap_or_default(),
                    behavior_profiles: row.behavior_profiles.unwrap_or_default(),
                    options: CatalogOptions {
                        available_models: row.available_models.unwrap_or_default(),
                        allowed_roots: row.allowed_roots.unwrap_or_default(),
                        permission_presets: row.permission_presets.unwrap_or_default(),
                        available_profiles: row.available_profiles.unwrap_or_default(),
                        available_profile_params: row.available_profile_params.unwrap_or_default(),
                    },
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
        crate::config_client::ConfigAccess::write_local_response(
            &self.node,
            "p2p.upsert_agent_directory",
            &mutation,
        )
        .await
        .map(|_| ())
    }

    async fn delete_directory_entry(&self, source_did: &str, agent_did: &str) -> Result<()> {
        let mutation = delete_directory_entry_mutation(source_did, agent_did);
        crate::config_client::ConfigAccess::write_local_response(
            &self.node,
            "p2p.delete_agent_directory",
            &mutation,
        )
        .await
        .map(|_| ())
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
    let default_behavior_id = escape_graphql_string(&entry.default_behavior_id);
    let behavior_models =
        graphql_string_list_literal(entry.behavior_models.iter().map(String::as_str));
    let behavior_roots =
        graphql_string_list_literal(entry.behavior_roots.iter().map(String::as_str));
    let behavior_presets =
        graphql_string_list_literal(entry.behavior_presets.iter().map(String::as_str));
    let behavior_profiles =
        graphql_string_list_literal(entry.behavior_profiles.iter().map(String::as_str));
    let available_models =
        graphql_string_list_literal(entry.options.available_models.iter().map(String::as_str));
    let allowed_roots =
        graphql_string_list_literal(entry.options.allowed_roots.iter().map(String::as_str));
    let permission_presets =
        graphql_string_list_literal(entry.options.permission_presets.iter().map(String::as_str));
    let available_profiles =
        graphql_string_list_literal(entry.options.available_profiles.iter().map(String::as_str));
    let available_profile_params = graphql_string_list_literal(
        entry
            .options
            .available_profile_params
            .iter()
            .map(String::as_str),
    );
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
                    default_behavior_id: "{default_behavior_id}",
                    behavior_models: {behavior_models},
                    behavior_roots: {behavior_roots},
                    behavior_presets: {behavior_presets},
                    behavior_profiles: {behavior_profiles},
                    available_models: {available_models},
                    allowed_roots: {allowed_roots},
                    permission_presets: {permission_presets},
                    available_profiles: {available_profiles},
                    available_profile_params: {available_profile_params},
                    runtime_state: "{runtime_state}",
                    last_seen: {last_seen},
                    updated_at: "{now}"
                }},
                update: {{
                    display_name: "{display_name}",
                    behaviors: {behaviors},
                    behavior_ids: {behavior_ids},
                    default_behavior_id: "{default_behavior_id}",
                    behavior_models: {behavior_models},
                    behavior_roots: {behavior_roots},
                    behavior_presets: {behavior_presets},
                    behavior_profiles: {behavior_profiles},
                    available_models: {available_models},
                    allowed_roots: {allowed_roots},
                    permission_presets: {permission_presets},
                    available_profiles: {available_profiles},
                    available_profile_params: {available_profile_params},
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

// The per-collection decoders behind `load_source_snapshot`, each reading its
// root field out of the shared multi-root response.

fn parse_principals(response: &QueryResponse) -> Result<Vec<(String, String, String)>> {
    Ok(rows::<PrincipalRow>(response, "AgentPrincipal")?
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
            let default_behavior_id = row
                .default_behavior_id
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .to_string();
            Some((did, display_name, default_behavior_id))
        })
        .collect())
}

fn parse_runtime_states(response: &QueryResponse) -> Result<BTreeMap<String, (String, String)>> {
    Ok(
        rows::<gents_protocol::row::AgentBehaviorReadinessRow>(response, "AgentBehaviorReadiness")?
            .into_iter()
            .filter_map(|row| {
                let did = row.agent_did.trim().to_string();
                if did.is_empty() {
                    return None;
                }
                let snapshot =
                    gents_protocol::row::decode_behavior_readiness_snapshot(&row, &did).ok()?;
                let process_state = snapshot.process_state.as_str().to_string();
                let updated_at = row.updated_at.trim().to_string();
                Some((did, (process_state, updated_at)))
            })
            .collect(),
    )
}

/// Hand-assembles a compact JSON object describing a resolved profile's
/// display-relevant parameters (#1050's info-card fields), in a FIXED key
/// order: `context_window`, `max_output_tokens`, `max_turns`, `temperature`,
/// `top_p`. Deliberately NOT `serde_json::to_string` of a map — a map's key
/// order is not guaranteed stable, and byte-stability across sweeps is what
/// feeds `directory_projection`'s write-free settled fixpoint. Each key is
/// omitted entirely when its source field is `None`; `{}` when the profile
/// has none of them.
///
/// Floats render via Rust's `{}` `Display` on exactly the decoded row
/// value — no canonicalization (unlike `canonicalize_last_seen`). That's
/// sufficient for the fixpoint: the same stored row decodes to the same
/// `f64` and re-renders byte-identical on every sweep, so settled
/// comparison never spuriously drifts.
fn render_profile_params_json(
    context_window: Option<i64>,
    max_output_tokens: Option<i64>,
    max_turns: Option<i64>,
    temperature: Option<f64>,
    top_p: Option<f64>,
) -> String {
    let mut fields = Vec::new();
    if let Some(value) = context_window {
        fields.push(format!(r#""context_window":{value}"#));
    }
    if let Some(value) = max_output_tokens {
        fields.push(format!(r#""max_output_tokens":{value}"#));
    }
    if let Some(value) = max_turns {
        fields.push(format!(r#""max_turns":{value}"#));
    }
    if let Some(value) = temperature {
        fields.push(format!(r#""temperature":{value}"#));
    }
    if let Some(value) = top_p {
        fields.push(format!(r#""top_p":{value}"#));
    }
    format!("{{{}}}", fields.join(","))
}

/// Keep only workspace roots inside the operator tool-root ceiling
/// (`gents server --tool-root`). The serve-time guard in
/// `tool_surface::build` refuses any behavior whose file tool root escapes
/// the ceiling, so publishing such a root in the catalog — or admitting it
/// into a persona — mints admitted-yet-unusable config (#1051). Resolution
/// mirrors the guard exactly (`resolve_configured_tool_root` +
/// `starts_with`); a root (or ceiling) that fails to resolve is dropped,
/// fail-closed: never offer what the guard may refuse.
pub(crate) fn filter_roots_to_ceiling(
    roots: Vec<String>,
    ceiling_root: Option<&std::path::Path>,
) -> Vec<String> {
    let Some(ceiling) = ceiling_root else {
        return roots;
    };
    let Ok(ceiling) = crate::tool_surface::resolve_configured_tool_root(ceiling) else {
        return Vec::new();
    };
    roots
        .into_iter()
        .filter(|root| {
            crate::tool_surface::resolve_configured_tool_root(std::path::Path::new(root))
                .map(|resolved| resolved.starts_with(&ceiling))
                .unwrap_or(false)
        })
        .collect()
}

const DIRECTORY_CONFIG_COLLECTIONS: &[crate::collection::Collection] = &[
    crate::collection::Collection::AgentBehavior,
    crate::collection::Collection::AgentContext,
    crate::collection::Collection::Tools,
    crate::collection::Collection::DatastoreToolSurface,
    crate::collection::Collection::InferenceBackend,
    crate::collection::Collection::InferenceProfile,
    crate::collection::Collection::InferenceSampling,
    crate::collection::Collection::InferenceExecution,
];

fn config_documents<T: serde::de::DeserializeOwned>(
    response: &QueryResponse,
    collection: crate::collection::Collection,
) -> Result<BTreeMap<(String, String), T>> {
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
        // Backend observations share storage with config but never enter its
        // strict serde shape. Select desired fields using the canonical owner.
        let mut document = value
            .as_object()
            .context("configuration row is not an object")?
            .clone();
        document.retain(|field, _| fields.contains(&field.as_str()));
        let document = serde_json::Value::Object(document);
        let (_, normalized) = crate::config_client::config_projection(collection, Some(&document))?;
        let document = normalized.context("canonical configuration projection missing value")?;
        let owner = document
            .get("agent_did")
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

fn scoped_config<'a, T>(
    documents: &'a BTreeMap<(String, String), T>,
    owner: &str,
    id: &str,
) -> Result<&'a T> {
    documents
        .get(&(owner.to_string(), id.to_string()))
        .with_context(|| {
            format!("directory configuration reference {id:?} missing in principal {owner:?}")
        })
}

fn parse_config_projection(
    response: &QueryResponse,
    principals: &[(String, String, String)],
    ceiling_root: Option<&std::path::Path>,
) -> Result<(
    BTreeMap<String, Vec<BehaviorInfo>>,
    BTreeMap<String, CatalogOptions>,
)> {
    use crate::collection::Collection;
    use crate::document_config::{
        AgentBehavior, AgentContext, BackendAuth, DatastoreToolSurfaceDocument, InferenceBackend,
        InferenceBackendObservation, InferenceExecution, InferenceProfile, InferenceSampling,
        Tools,
    };
    let behaviors = config_documents::<AgentBehavior>(response, Collection::AgentBehavior)?;
    let contexts = config_documents::<AgentContext>(response, Collection::AgentContext)?;
    let tools = config_documents::<Tools>(response, Collection::Tools)?;
    let surfaces = config_documents::<DatastoreToolSurfaceDocument>(
        response,
        Collection::DatastoreToolSurface,
    )?;
    let backends = config_documents::<InferenceBackend>(response, Collection::InferenceBackend)?;
    let profiles = config_documents::<InferenceProfile>(response, Collection::InferenceProfile)?;
    let samplings = config_documents::<InferenceSampling>(response, Collection::InferenceSampling)?;
    let executions =
        config_documents::<InferenceExecution>(response, Collection::InferenceExecution)?;
    let mut observations = BTreeMap::new();
    for value in rows::<serde_json::Value>(response, "InferenceBackend")? {
        let owner = value
            .get("agent_did")
            .and_then(serde_json::Value::as_str)
            .context("backend observation owner missing")?
            .to_string();
        let observed: InferenceBackendObservation = serde_json::from_value(value)?;
        anyhow::ensure!(
            observations
                .insert((owner, observed.backend_id.clone()), observed)
                .is_none(),
            "duplicate scoped backend observation"
        );
    }
    let mut allowed_roots = filter_roots_to_ceiling(
        rows::<WorkspaceRootRow>(response, "WorkspaceRoot")?
            .into_iter()
            .filter(|row| row.enabled.unwrap_or(false))
            .filter_map(|row| row.root_path)
            .collect(),
        ceiling_root,
    );
    allowed_roots.sort();
    allowed_roots.dedup();
    let mut by_owner = BTreeMap::new();
    let mut options_by_owner = BTreeMap::new();
    for (owner, _, _) in principals {
        let mut infos = Vec::new();
        for ((behavior_owner, _), behavior) in &behaviors {
            if behavior_owner != owner || !behavior.enabled {
                continue;
            }
            let profile = scoped_config(&profiles, owner, &behavior.inference_profile_id)?;
            scoped_config(&backends, owner, &profile.backend_id)?;
            let context = behavior
                .context_id
                .as_deref()
                .map(|id| scoped_config(&contexts, owner, id))
                .transpose()?;
            let selected_tools = context
                .and_then(|context| context.tools_id.as_deref())
                .map(|id| scoped_config(&tools, owner, id))
                .transpose()?;
            let (host_root, preset) = if let Some(tools) = selected_tools {
                tools.validate()?;
                let host_root = tools
                    .host
                    .as_ref()
                    .and_then(|host| host.root.clone())
                    .unwrap_or_default();
                let merged = crate::document_config::merge_datastore_tool_surfaces(
                    tools,
                    surfaces.values(),
                )?;
                let preset = classify_tools(&tools, &merged)?
                    .unwrap_or_default()
                    .to_string();
                (host_root, preset)
            } else {
                (String::new(), String::new())
            };
            anyhow::ensure!(
                !profile.backend_id.contains('|'),
                "directory model column cannot encode backend ID containing '|'"
            );
            infos.push(BehaviorInfo {
                behavior_id: behavior.behavior_id.clone(),
                display_name: behavior
                    .display_name
                    .as_deref()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or(&behavior.behavior_id)
                    .to_string(),
                backend_id: profile.backend_id.clone(),
                model_name: profile.model_name.clone(),
                inference_profile_id: profile.profile_id.clone(),
                host_root,
                preset,
            });
        }
        by_owner.insert(owner.clone(), infos);
        let mut available_models = BTreeSet::new();
        for ((backend_owner, id), backend) in &backends {
            if backend_owner != owner || !backend.enabled {
                continue;
            }
            let credential_scope =
                matches!(backend.auth, BackendAuth::PrincipalOAuth).then_some(owner.as_str());
            if let Some(catalog) = observations
                .get(&(owner.clone(), id.clone()))
                .map(|observation| observation.catalog_for(credential_scope))
                .transpose()?
                .flatten()
            {
                anyhow::ensure!(
                    !id.contains('|'),
                    "directory model column cannot encode backend ID containing '|'"
                );
                for model in &catalog.models {
                    available_models.insert(format!("{id}|{}", model.model_name));
                }
            }
        }
        let mut profile_pairs = Vec::new();
        for ((profile_owner, id), profile) in &profiles {
            if profile_owner != owner {
                continue;
            }
            let backend = scoped_config(&backends, owner, &profile.backend_id)?;
            let sampling = profile
                .sampling_id
                .as_deref()
                .map(|id| scoped_config(&samplings, owner, id))
                .transpose()?;
            let execution = profile
                .execution_id
                .as_deref()
                .map(|id| scoped_config(&executions, owner, id))
                .transpose()?;
            let credential_scope =
                matches!(backend.auth, BackendAuth::PrincipalOAuth).then_some(owner.as_str());
            if let Some(catalog) = observations
                .get(&(owner.clone(), backend.backend_id.clone()))
                .map(|observation| observation.catalog_for(credential_scope))
                .transpose()?
                .flatten()
            {
                let selected = catalog
                    .models
                    .iter()
                    .filter(|model| model.model_name == profile.model_name)
                    .collect::<Vec<_>>();
                anyhow::ensure!(
                    selected.len() == 1,
                    "profile {id:?} selects an unadvertised or ambiguous model"
                );
                if let (Some(effort), Some(supported)) = (
                    profile.reasoning_effort,
                    selected[0].reasoning_efforts.as_ref(),
                ) {
                    anyhow::ensure!(
                        supported.contains(&effort),
                        "profile {id:?} selects an unadvertised effort"
                    );
                }
            }
            anyhow::ensure!(
                !id.contains('|'),
                "directory profile column cannot encode ID containing '|'"
            );
            let name = profile
                .display_name
                .as_deref()
                .filter(|name| !name.is_empty())
                .unwrap_or(id);
            profile_pairs.push((
                format!("{id}|{name}"),
                render_profile_params_json(
                    profile.context_window,
                    profile.max_output_tokens,
                    execution.and_then(|execution| execution.max_turns),
                    sampling.and_then(|sampling| sampling.temperature),
                    sampling.and_then(|sampling| sampling.top_p),
                ),
            ));
        }
        profile_pairs.sort();
        let (available_profiles, available_profile_params) = profile_pairs.into_iter().unzip();
        options_by_owner.insert(
            owner.clone(),
            CatalogOptions {
                available_models: available_models.into_iter().collect(),
                allowed_roots: allowed_roots.clone(),
                permission_presets: builtin_preset_names()
                    .iter()
                    .map(|name| name.to_string())
                    .collect(),
                available_profiles,
                available_profile_params,
            },
        );
    }
    Ok((by_owner, options_by_owner))
}

#[derive(Deserialize)]
struct PrincipalRow {
    agent_did: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    default_behavior_id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Deserialize)]
struct WorkspaceRootRow {
    #[serde(default)]
    root_path: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
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
    default_behavior_id: Option<String>,
    #[serde(default)]
    behavior_models: Option<Vec<String>>,
    #[serde(default)]
    behavior_roots: Option<Vec<String>>,
    #[serde(default)]
    behavior_presets: Option<Vec<String>>,
    #[serde(default)]
    behavior_profiles: Option<Vec<String>>,
    #[serde(default)]
    available_models: Option<Vec<String>>,
    #[serde(default)]
    allowed_roots: Option<Vec<String>>,
    #[serde(default)]
    permission_presets: Option<Vec<String>>,
    #[serde(default)]
    available_profiles: Option<Vec<String>>,
    #[serde(default)]
    available_profile_params: Option<Vec<String>>,
    #[serde(default)]
    runtime_state: Option<String>,
    #[serde(default)]
    last_seen: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::filter_roots_to_ceiling;

    #[test]
    fn ceiling_filter_matrix() {
        let roots = vec![
            "/ceil/ws/app".to_string(),
            "/ceil/ws".to_string(),
            "/ceil/wsx/app".to_string(),
            "/outside/app".to_string(),
        ];

        // No ceiling: untouched.
        assert_eq!(
            filter_roots_to_ceiling(roots.clone(), None),
            roots,
            "no ceiling must publish every enabled root"
        );

        // Component-wise containment: "/ceil/wsx" is NOT within "/ceil/ws"
        // (the sibling-prefix trap a string prefix check would fall into).
        assert_eq!(
            filter_roots_to_ceiling(roots, Some(std::path::Path::new("/ceil/ws"))),
            vec!["/ceil/ws/app".to_string(), "/ceil/ws".to_string()],
            "only roots within the ceiling (incl. the ceiling itself) survive"
        );
    }

    use super::*;

    fn entry(agent_did: &str, last_seen: &str) -> DirectoryEntry {
        DirectoryEntry {
            directory_key: directory_entry_key("did:key:home", agent_did),
            agent_did: agent_did.to_string(),
            source_did: "did:key:home".to_string(),
            display_name: "Display".to_string(),
            behaviors: Vec::new(),
            behavior_ids: Vec::new(),
            default_behavior_id: String::new(),
            behavior_models: Vec::new(),
            behavior_roots: Vec::new(),
            behavior_presets: Vec::new(),
            behavior_profiles: Vec::new(),
            options: CatalogOptions::default(),
            runtime_state: "running".to_string(),
            last_seen: last_seen.to_string(),
        }
    }

    /// C2 regression: a principal with no readiness row derives a blank
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

    /// The nine new dimension/option columns follow the same null-never-[]
    /// discipline as `behaviors`/`behavior_ids`, in BOTH the add and update
    /// payloads — an empty list literal types as `JsonArray` and corrupts
    /// nillable array columns. `available_profile_params` (#1050) is the
    /// newest of these.
    #[test]
    fn upsert_mutation_renders_null_for_all_nine_new_columns_when_empty() {
        let mutation = upsert_directory_entry_mutation(
            &entry("did:key:no-dimensions", "2026-07-20T00:00:00Z"),
            "2026-07-23T00:00:00Z",
        );
        for column in [
            "behavior_models",
            "behavior_roots",
            "behavior_presets",
            "behavior_profiles",
            "available_models",
            "allowed_roots",
            "permission_presets",
            "available_profiles",
            "available_profile_params",
        ] {
            let needle = format!("{column}: null");
            assert!(
                mutation.contains(&needle),
                "empty {column} must render as null, never []: {mutation}"
            );
        }
    }

    /// Populated dimension/option lists render as aligned array literals in
    /// both the add and update payloads. `available_profile_params` carries
    /// a JSON string (embedded double quotes) to pin that
    /// `graphql_string_list_literal`'s per-element `escape_graphql_string`
    /// survives the mutation round-trip rather than producing invalid
    /// GraphQL.
    #[test]
    fn upsert_mutation_renders_populated_dimension_and_option_lists() {
        let mut with_dimensions = entry("did:key:with-dimensions", "2026-07-20T00:00:00Z");
        with_dimensions.behavior_ids = vec!["did:key:a:coder".to_string()];
        with_dimensions.behavior_models = vec!["openai|gpt-5".to_string()];
        with_dimensions.behavior_roots = vec!["/repo/a".to_string()];
        with_dimensions.behavior_presets = vec!["readonly".to_string()];
        with_dimensions.behavior_profiles = vec!["fast-profile".to_string()];
        with_dimensions.options = CatalogOptions {
            available_models: vec!["openai|gpt-5".to_string()],
            allowed_roots: vec!["/repo/a".to_string()],
            permission_presets: vec!["readonly".to_string(), "write".to_string()],
            available_profiles: vec!["fast-profile|Fast".to_string()],
            available_profile_params: vec![
                r#"{"context_window":128000,"temperature":0.2}"#.to_string()
            ],
        };
        let mutation = upsert_directory_entry_mutation(&with_dimensions, "2026-07-23T00:00:00Z");
        assert!(mutation.contains(r#"behavior_models: ["openai|gpt-5"]"#));
        assert!(mutation.contains(r#"behavior_roots: ["/repo/a"]"#));
        assert!(mutation.contains(r#"behavior_presets: ["readonly"]"#));
        assert!(mutation.contains(r#"behavior_profiles: ["fast-profile"]"#));
        assert!(mutation.contains(r#"available_models: ["openai|gpt-5"]"#));
        assert!(mutation.contains(r#"allowed_roots: ["/repo/a"]"#));
        assert!(mutation.contains(r#"permission_presets: ["readonly", "write"]"#));
        assert!(mutation.contains(r#"available_profiles: ["fast-profile|Fast"]"#));
        assert!(
            mutation.contains(
                r#"available_profile_params: ["{\"context_window\":128000,\"temperature\":0.2}"]"#
            ),
            "the params JSON string's embedded quotes must survive escaped: {mutation}"
        );
    }

    /// `default_behavior_id` is a plain string field (unlike the nullable
    /// list/DateTime fields above): empty stays `""`, never `null`, so the
    /// client can compare it against a picked `behavior_id` unconditionally.
    #[test]
    fn upsert_mutation_renders_default_behavior_id_as_plain_string() {
        let empty = upsert_directory_entry_mutation(
            &entry("did:key:no-default", "2026-07-20T00:00:00Z"),
            "2026-07-23T00:00:00Z",
        );
        assert!(
            empty.contains(r#"default_behavior_id: """#),
            "empty default_behavior_id must render as an empty string, never null: {empty}"
        );

        let mut with_default = entry("did:key:with-default", "2026-07-20T00:00:00Z");
        with_default.default_behavior_id = "did:key:a:coder".to_string();
        let mutation = upsert_directory_entry_mutation(&with_default, "2026-07-23T00:00:00Z");
        assert!(mutation.contains(r#"default_behavior_id: "did:key:a:coder""#));
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

    /// Regression fence for the settled-fixpoint bug: a readiness `updated_at`
    /// written in the runtime's real `Utc::now().to_rfc3339()` shape (offset
    /// `+00:00`, sub-second precision) used to differ from its own stored
    /// `AgentDirectoryEntry.last_seen` `DateTime` round-trip (offset `Z`),
    /// making every sweep re-upsert and — since the sweep runs on Update events
    /// — self-perpetuate into an unbounded write/event storm. Seeding the exact
    /// runtime format and asserting the second tick is write-free pins the fix.
    /// The param-JSON builder matrix (#1050): fixed key order
    /// (`context_window`, `max_output_tokens`, `max_turns`, `temperature`,
    /// `top_p`), each key omitted entirely when its source field is `None`,
    /// `{}` when the profile has none of them.
    #[test]
    fn render_profile_params_json_matrix() {
        assert_eq!(
            render_profile_params_json(Some(128000), Some(4096), Some(50), Some(0.2), Some(0.9)),
            r#"{"context_window":128000,"max_output_tokens":4096,"max_turns":50,"temperature":0.2,"top_p":0.9}"#
        );
        assert_eq!(
            render_profile_params_json(Some(128000), None, None, Some(0.2), None),
            r#"{"context_window":128000,"temperature":0.2}"#
        );
        assert_eq!(
            render_profile_params_json(None, None, None, None, None),
            "{}"
        );
    }

    /// Float rendering uses Rust's `{}` `Display` on exactly the decoded row
    /// value — no canonicalization — so the same row re-renders
    /// byte-identical on every sweep, which is all the settled-fixpoint
    /// comparison needs.
    #[test]
    fn render_profile_params_json_float_display_is_byte_stable_across_renders() {
        let first = render_profile_params_json(None, None, None, Some(0.1), Some(1.0));
        let second = render_profile_params_json(None, None, None, Some(0.1), Some(1.0));
        assert_eq!(first, second);
        assert_eq!(first, r#"{"temperature":0.1,"top_p":1}"#);
    }

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
        let snapshot_json = escape_graphql_string(
            &serde_json::json!({
                "format_version": 1,
                "process_state": "ready",
                "active_generation": 1,
                "router_generation": 1,
                "default_behavior_id": "general",
                "behaviors": [{
                    "behavior_id": "general",
                    "state": "ready",
                    "reason": null
                }]
            })
            .to_string(),
        );
        let seed = format!(
            r#"mutation {{
                create_AgentPrincipal(input: {{
                    agent_did: "did:key:real",
                    display_name: "Real",
                    enabled: true,
                    created_at: "2026-07-23T00:00:00Z"
                }}) {{ _docID }}
                create_AgentBehaviorReadiness(input: {{
                    agent_did: "did:key:real",
                    snapshot_json: "{snapshot_json}",
                    updated_at: "{updated_at}"
                }}) {{ _docID }}
            }}"#
        );
        let response = node.execute(&seed).await;
        ensure_no_errors(&response, "seed runtime-format updated_at")?;

        let store = GraphqlDirectoryStore::new(node.clone(), None);
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
    /// one WITHOUT an `AgentBehaviorReadiness` row, converge in one tick — the
    /// readiness-less principal's directory row must exist with `last_seen`
    /// round-tripping to `""` rather than aborting the sweep. Deleting a
    /// principal then retracts exactly its row.
    ///
    /// Also covers persona-catalog round-trip (issue #714 PR 3): one behavior
    /// wired to a readonly-matching `Tools` (with root), backend,
    /// model, and profile; the other selects the same profile with no context.
    /// Foreign same-ID config and OAuth observations must not cross scope. A disabled `WorkspaceRoot`
    /// must be excluded from `allowed_roots`. The second tick over the same
    /// settled state must still be write-free — the nine new columns must
    /// not break the storm-regression invariant. The `InferenceProfile`
    /// carries a partial params set (`context_window` + `temperature`,
    /// `max_output_tokens`/`max_turns`/`top_p` unset) so
    /// `available_profile_params` (#1050) exercises the omit-when-null path
    /// through a real node, aligned with `available_profiles`, and stays
    /// write-free on the settled re-tick (the float-display byte-stability
    /// this fences).
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
                default_behavior_id: "enabled-behavior",
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
                context_id: "readonly-context",
                inference_profile_id: "fast-profile",
                enabled: true
            }) { _docID }
            create_AgentBehavior(input: {
                behavior_id: "artist-behavior",
                agent_did: "did:key:with-runtime",
                display_name: "Artist Behavior",
                inference_profile_id: "fast-profile",
                enabled: true
            }) { _docID }
            create_AgentBehavior(input: {
                behavior_id: "disabled-behavior",
                agent_did: "did:key:with-runtime",
                display_name: "Disabled Behavior",
                inference_profile_id: "fast-profile",
                enabled: false
            }) { _docID }
            create_AgentBehaviorReadiness(input: {
                agent_did: "did:key:with-runtime",
                snapshot_json: "{\"format_version\":1,\"process_state\":\"ready\",\"active_generation\":1,\"router_generation\":1,\"default_behavior_id\":\"enabled-behavior\",\"behaviors\":[{\"behavior_id\":\"artist-behavior\",\"state\":\"ready\",\"reason\":null},{\"behavior_id\":\"enabled-behavior\",\"state\":\"ready\",\"reason\":null}]}",
                updated_at: "2026-07-23T12:34:56.845794+00:00"
            }) { _docID }
            create_AgentContext(input: {
                context_id: "readonly-context", agent_did: "did:key:with-runtime", tools_id: "readonly-tools"
            }) { _docID }
            create_Tools(input: {
                tools_id: "readonly-tools", agent_did: "did:key:with-runtime",
                host: { root: "/repo/with-runtime", files: { mode: "ReadOnly" }, bash: { mode: "ReadOnly" } }
            }) { _docID }
            create_Tools(input: {
                tools_id: "readonly-tools", agent_did: "did:key:foreign",
                host: { root: "/repo/foreign", files: { mode: "ReadWrite" }, bash: { mode: "Unrestricted" } }
            }) { _docID }
            create_InferenceBackend(input: {
                backend_id: "openai", agent_did: "did:key:with-runtime", name: "OpenAI",
                provider_kind: "OpenAiCompatible", endpoint: "http://localhost:8000", auth: { kind: "unauthenticated" },
                enabled: true,
                catalogs: [
                    {agent_did: null, observed_at: "2026-07-23T00:00:00Z", models: [{model_name: "gpt-5"}, {model_name: "gpt-5-mini"}]},
                    {agent_did: "did:key:foreign", observed_at: "2026-07-23T00:00:00Z", models: [{model_name: "foreign-private-model"}]}
                ]
            }) { _docID }
            create_InferenceBackend(input: {
                backend_id: "openai", agent_did: "did:key:foreign", name: "Foreign",
                provider_kind: "OpenAiCompatible", endpoint: "http://localhost:8001", auth: { kind: "unauthenticated" },
                enabled: true, catalogs: [{agent_did: null, observed_at: "2026-07-23T00:00:00Z", models: [{model_name: "foreign-backend-model"}]}]
            }) { _docID }
            create_InferenceProfile(input: {
                profile_id: "fast-profile", agent_did: "did:key:with-runtime", display_name: "Fast Profile",
                backend_id: "openai", model_name: "gpt-5", sampling_id: "sampling", context_window: 128000
            }) { _docID }
            create_InferenceProfile(input: {
                profile_id: "fast-profile", agent_did: "did:key:foreign", display_name: "Foreign Profile",
                backend_id: "openai", model_name: "foreign-backend-model"
            }) { _docID }
            create_InferenceSampling(input: {
                sampling_id: "sampling", agent_did: "did:key:with-runtime", temperature: 0.2
            }) { _docID }
            create_InferenceSampling(input: {
                sampling_id: "sampling", agent_did: "did:key:foreign", temperature: 0.9
            }) { _docID }
            create_WorkspaceRoot(input: {
                root_path: "/repo/enabled",
                display_name: "Enabled Root",
                enabled: true
            }) { _docID }
            create_WorkspaceRoot(input: {
                root_path: "/repo/disabled",
                display_name: "Disabled Root",
                enabled: false
            }) { _docID }
        }"#;
        let response = node.execute(seed).await;
        ensure_no_errors(&response, "seed directory projection principals")?;

        let store = GraphqlDirectoryStore::new(node.clone(), None);
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
        assert_eq!(with_runtime.runtime_state, "ready");
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
        assert_eq!(
            with_runtime.default_behavior_id, "enabled-behavior",
            "default_behavior_id must round-trip from AgentPrincipal through a real node"
        );
        // Persona dimensions, index-aligned with the sorted behavior_ids
        // above (artist-behavior first, enabled-behavior second): the
        // both behaviors select the explicit inference profile; only the
        // enabled behavior has a context/tools link. Foreign same-ID config
        // documents must never influence any dimension.
        assert_eq!(
            with_runtime.behavior_models,
            vec!["openai|gpt-5".to_string(), "openai|gpt-5".to_string()],
            "behavior_models must round-trip backend_id|model_name, aligned"
        );
        assert_eq!(
            with_runtime.behavior_roots,
            vec![String::new(), "/repo/with-runtime".to_string()],
            "behavior_roots must round-trip the wired tools' host root, aligned"
        );
        assert_eq!(
            with_runtime.behavior_presets,
            vec![String::new(), "readonly".to_string()],
            "behavior_presets must classify the readonly-matching selection, aligned"
        );
        assert_eq!(
            with_runtime.behavior_profiles,
            vec!["fast-profile".to_string(), "fast-profile".to_string()],
            "behavior_profiles must round-trip inference_profile_id, aligned"
        );
        assert_eq!(
            with_runtime.options.available_models,
            vec!["openai|gpt-5".to_string(), "openai|gpt-5-mini".to_string()],
            "available_models must list every model of every enabled backend, sorted"
        );
        assert_eq!(
            with_runtime.options.allowed_roots,
            vec!["/repo/enabled".to_string()],
            "the disabled WorkspaceRoot must be excluded from allowed_roots"
        );
        assert_eq!(
            with_runtime.options.permission_presets,
            crate::agent::persona_presets::builtin_preset_names()
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            with_runtime.options.available_profiles,
            vec!["fast-profile|Fast Profile".to_string()]
        );
        assert_eq!(
            with_runtime.options.available_profile_params,
            vec![r#"{"context_window":128000,"temperature":0.2}"#.to_string()],
            "available_profile_params must round-trip through a real node, aligned with \
             available_profiles, omitting the unset max_output_tokens/max_turns/top_p keys"
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
            .expect("no-runtime directory row present despite no readiness row");
        assert_eq!(
            no_runtime.last_seen, "",
            "readiness-less principal's stored null last_seen must round-trip to \"\""
        );
        assert_eq!(
            no_runtime.default_behavior_id, "",
            "a principal with no default_behavior_id must stay empty, not null-coerced garbage"
        );
        // Config and authentication catalogs never cross principal boundaries.
        assert!(no_runtime.options.available_models.is_empty());
        assert!(no_runtime.options.available_profiles.is_empty());
        assert!(no_runtime.options.available_profile_params.is_empty());
        // WorkspaceRoot is still local operator policy, with the same ceiling.
        assert_eq!(
            no_runtime.options.allowed_roots,
            with_runtime.options.allowed_roots
        );

        // Settled state is a write-free fixpoint: the runtime-less principal
        // must not keep re-triggering writes forever, and the eight new
        // dimension/option columns must not break settled-comparison either.
        let second = reconcile_directory_tick(&store, "did:key:home").await?;
        assert_eq!(second, DirectoryTickOutcome::default());

        let oauth = r#"mutation { update_InferenceBackend(
            filter: { agent_did: { _eq: "did:key:with-runtime" }, backend_id: { _eq: "openai" } },
            input: { provider_kind: "ChatGptCodex", auth: { kind: "principal_oauth" } }
        ) { _docID } }"#;
        ensure_no_errors(&node.execute(oauth).await, "select principal OAuth catalog")?;
        let oauth_snapshot = store.load_source_snapshot().await?;
        assert!(
            oauth_snapshot.options["did:key:with-runtime"]
                .available_models
                .is_empty(),
            "OAuth must not borrow shared or foreign catalog observations"
        );
        let scoped_catalog = r#"mutation { update_InferenceBackend(
            filter: { agent_did: { _eq: "did:key:with-runtime" }, backend_id: { _eq: "openai" } },
            input: { catalogs: [
                {agent_did: "did:key:with-runtime", observed_at: "2026-07-23T00:00:00Z", models: [{model_name: "gpt-5"}]},
                {agent_did: null, observed_at: "2026-07-23T00:00:00Z", models: [{model_name: "shared-only-model"}]},
                {agent_did: "did:key:foreign", observed_at: "2026-07-23T00:00:00Z", models: [{model_name: "foreign-private-model"}]}
            ] }
        ) { _docID } }"#;
        ensure_no_errors(
            &node.execute(scoped_catalog).await,
            "publish exact OAuth catalog",
        )?;
        let scoped_snapshot = store.load_source_snapshot().await?;
        assert_eq!(
            scoped_snapshot.options["did:key:with-runtime"].available_models,
            vec!["openai|gpt-5"]
        );

        let custom_tools = r#"mutation { update_Tools(
            filter: { agent_did: { _eq: "did:key:with-runtime" }, tools_id: { _eq: "readonly-tools" } },
            input: { host: { root: "/repo/with-runtime", files: { mode: "ReadOnly" },
                bash: { mode: "ReadOnly", allowed_argv_prefixes: [["git", "status"]] } } }
        ) { _docID } }"#;
        ensure_no_errors(
            &node.execute(custom_tools).await,
            "customize canonical bash argv policy",
        )?;
        let custom = store.load_source_snapshot().await?;
        let customized = custom.behaviors["did:key:with-runtime"]
            .iter()
            .find(|behavior| behavior.behavior_id == "enabled-behavior")
            .expect("customized behavior");
        assert_eq!(customized.host_root, "/repo/with-runtime");
        assert_eq!(
            customized.preset, "",
            "extra argv prefixes must classify as custom, not readonly"
        );

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

        let store = GraphqlDirectoryStore::new(node.clone(), None);
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
