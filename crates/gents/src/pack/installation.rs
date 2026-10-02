//! Installation records: what a pack install created, so an upgrade can tell
//! an operator's edit from the pack's own document and removal deletes only
//! what the pack created.
//!
//! One `PackInstallation` document per owner and pack coordinate, written in
//! the same transaction as the documents it describes. For each document it
//! holds the content digest the install wrote and whether the install created
//! it (a document that already existed is adopted, never later deleted).
//! `documents` and `history` and mutation writers are shared by the
//! documents installer ([`install_in_txn`]) and the graph installer
//! ([`graph::record_graph_install_in_txn`]) through [`write_record_in_txn`],
//! so the two paths cannot diverge on how a record is written.
//!
//! On install, a document the record lists whose live content no longer
//! matches the recorded digest was edited by someone else. Install stops and
//! names each one unless the caller chose [`DriftPolicy::Overwrite`] or
//! [`DriftPolicy::Keep`]. A document the previous version created and the new
//! version dropped is removed, under the same rule. A graph pack's
//! runtime-derived documents (see [`graph`]) are never drift-checked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config_client::{
    read_desired_state_record_in_txn, ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use crate::document_config::PackConfig;
use crate::graphql::escape_graphql_string;
use crate::Collection;

mod dependencies;
mod graph;

pub(crate) use graph::{observe_graph_install_in_txn, record_graph_install_in_txn};

const RECORD: &str = "PackInstallation";
/// Previous digests kept for rollback, newest first.
const HISTORY_LIMIT: usize = 32;

/// Which pack an install is.
#[derive(Debug, Clone)]
pub struct PackIdentity {
    /// `{namespace}/{name}`.
    pub coordinate: String,
    pub version: String,
    pub digest: String,
    /// The plugins this install put in the host's plugin store.
    pub plugins: Vec<InstalledPackPlugin>,
    /// Coordinates of other packs this install depends on. Only a documents
    /// pack may declare these (`pack::validate_pack_manifest` refuses a
    /// graph pack any dependency of its own).
    pub dependencies: Vec<String>,
}

impl PackIdentity {
    /// The identity of the pack `manifest` describes, whose contents hash to `digest`.
    pub fn new(
        manifest: &super::PackManifest,
        digest: impl Into<String>,
        plugins: Vec<InstalledPackPlugin>,
    ) -> Self {
        Self {
            coordinate: format!("{}/{}", manifest.metadata.namespace, manifest.name),
            version: manifest.version.clone(),
            digest: digest.into(),
            plugins,
            dependencies: Vec::new(),
        }
    }
}

/// A plugin a pack install stored, by name and artifact digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledPackPlugin {
    pub name: String,
    pub digest: String,
}

/// What to do with a document someone edited since the pack wrote it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DriftPolicy {
    /// Stop and name every edited document.
    #[default]
    Refuse,
    /// Replace (or remove) it with the pack's version.
    Overwrite,
    /// Leave the edit in place.
    Keep,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RecordedDocument {
    collection: String,
    id: String,
    digest: String,
    created: bool,
}

#[derive(Debug, Default, Clone)]
struct Record {
    doc_id: Option<String>,
    digest: Option<String>,
    documents: BTreeMap<String, RecordedDocument>,
    plugins: Vec<InstalledPackPlugin>,
    history: Vec<String>,
    /// Coordinates of installed packs that depend on this one. Non-empty
    /// blocks a direct `gents pack remove` of this coordinate.
    required_by: BTreeSet<String>,
    /// Whether this pack was installed for its own sake (`gents pack
    /// install <it>` directly), rather than only as another pack's
    /// dependency. A legacy record with no `explicit` field reads as `true`.
    explicit: bool,
}

/// What an install or removal did, each document as `Collection/id`.
#[derive(Debug, Default, Serialize)]
pub struct InstallReport {
    /// Documents written, per collection.
    #[serde(flatten)]
    pub applied: crate::config_client::DesiredStateApplyCounts,
    pub created: Vec<String>,
    pub replaced: Vec<String>,
    pub adopted: Vec<String>,
    pub kept: Vec<String>,
    pub removed: Vec<String>,
    /// Plugins the install recorded, or the removal released.
    pub plugins: Vec<InstalledPackPlugin>,
}

/// Something removal left in place, and why: a run's result view that needs
/// its graph reinstalled, or a package SDL schema DefraDB cannot drop.
#[derive(Debug, Serialize)]
pub struct Retained {
    pub item: String,
    pub reason: String,
}

/// What `gents pack remove` did: the documents its own record listed (in the
/// same shape [`InstallReport`] already reports for an install), anything
/// removal could not or should not take with it, and any dependency the
/// removal released in turn.
#[derive(Debug, Default, Serialize)]
pub struct RemoveReport {
    pub pack: String,
    #[serde(flatten)]
    pub documents: InstallReport,
    pub retained: Vec<Retained>,
    pub dependencies: Vec<RemoveReport>,
    /// The current and history digests of the removed record, so a caller
    /// can tell which archives and plugin bytes nothing references any more.
    pub digests: Vec<String>,
}

fn key(collection: &str, id: &str) -> String {
    format!("{collection}/{id}")
}

fn collection_named(name: &str) -> Result<Collection> {
    Collection::ALL
        .iter()
        .copied()
        .find(|collection| collection.graphql_type() == name)
        .with_context(|| format!("installation record names unknown collection {name:?}"))
}

async fn read_record(txn: &ConfigApplyTxn<'_>, owner: &str, coordinate: &str) -> Result<Record> {
    let response = txn
        .execute(&format!(
            r#"{{ {RECORD}(filter: {{ agent_did: {{ _eq: "{}" }}, coordinate: {{ _eq: "{}" }} }}, limit: 2) {{ _docID digest documents plugins history required_by explicit }} }}"#,
            escape_graphql_string(owner),
            escape_graphql_string(coordinate)
        ))
        .await?;
    let rows = response["data"][RECORD]
        .as_array()
        .context("reading the pack installation record")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "{coordinate} has more than one installation record for {owner}"
    );
    let Some(row) = rows.first() else {
        return Ok(Record::default());
    };
    let documents: Vec<RecordedDocument> =
        decode_record_field(row, "documents", owner, coordinate)?;
    let required_by: Vec<String> = decode_record_field(row, "required_by", owner, coordinate)?;
    Ok(Record {
        doc_id: row["_docID"].as_str().map(str::to_owned),
        digest: row["digest"].as_str().map(str::to_owned),
        documents: documents
            .into_iter()
            .map(|document| (key(&document.collection, &document.id), document))
            .collect(),
        plugins: decode_record_field(row, "plugins", owner, coordinate)?,
        history: decode_record_field(row, "history", owner, coordinate)?,
        required_by: required_by.into_iter().collect(),
        explicit: row.get("explicit").and_then(Value::as_bool).unwrap_or(true),
    })
}

/// Decodes `row[field]` as `T`, defaulting only a genuinely absent (missing
/// or null) field; a present-but-malformed field fails loudly naming the
/// record and field, rather than silently reading as empty.
fn decode_record_field<T: serde::de::DeserializeOwned + Default>(
    row: &Value,
    field: &str,
    owner: &str,
    coordinate: &str,
) -> Result<T> {
    match row.get(field) {
        None | Some(Value::Null) => Ok(T::default()),
        Some(value) => serde_json::from_value(value.clone()).with_context(|| {
            format!(
                "installation record {coordinate:?} for {owner:?} has a malformed {field} field"
            )
        }),
    }
}

async fn live_digest(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<Option<String>> {
    match read_desired_state_record_in_txn(txn, collection, owner, id).await? {
        Some((_, live)) => Ok(Some(super::pack_artifact_document_digest(&live)?)),
        None => Ok(None),
    }
}

/// Batched form of [`live_digest`]: one query per collection (through
/// [`read_desired_state_records_in_txn`]'s own paging) instead of one query
/// per document, grouping `docs` by collection first. A document absent from
/// the result is simply missing from the map, the same as `live_digest`
/// returning `None`.
async fn live_digests<'a>(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    docs: impl IntoIterator<Item = (Collection, &'a str)>,
) -> Result<BTreeMap<(Collection, String), String>> {
    let mut grouped: BTreeMap<Collection, BTreeSet<&str>> = BTreeMap::new();
    for (collection, id) in docs {
        grouped.entry(collection).or_default().insert(id);
    }
    let mut out = BTreeMap::new();
    for (collection, ids) in grouped {
        let ids: Vec<&str> = ids.into_iter().collect();
        let records =
            crate::config_client::read_desired_state_records_in_txn(txn, collection, owner, &ids)
                .await?;
        for (id, (_, live)) in records {
            out.insert(
                (collection, id),
                super::pack_artifact_document_digest(&live)?,
            );
        }
    }
    Ok(out)
}

fn edited_error(coordinate: &str, edited: &[String]) -> anyhow::Error {
    anyhow::anyhow!(
        "{} of {coordinate}'s documents were edited since it was installed: {}; \
         choose --overwrite to replace them or --keep to leave them",
        edited.len(),
        edited.join(", ")
    )
}

/// Writes `documents` as `pack`'s installation record for `owner`, updating
/// the existing record named by `prior.doc_id` or creating one. The one
/// writer both [`install_in_txn`] and [`graph::record_graph_install_in_txn`]
/// use, so history, `required_by` and `explicit` are kept exactly the same
/// way by either installer.
async fn write_record_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    pack: &PackIdentity,
    prior: &Record,
    documents: Vec<RecordedDocument>,
    explicit: bool,
) -> Result<()> {
    let mut history = prior.history.clone();
    if let Some(previous) = prior
        .digest
        .clone()
        .filter(|previous| *previous != pack.digest)
    {
        history.insert(0, previous);
        history.truncate(HISTORY_LIMIT);
    }
    let required_by: Vec<&String> = prior.required_by.iter().collect();
    let input = json!({
        "agent_did": owner,
        "coordinate": pack.coordinate,
        "version": pack.version,
        "digest": pack.digest,
        "documents": documents,
        "plugins": pack.plugins,
        "history": history,
        // A nanosecond, process-monotonic stamp, not a plain timestamp: a
        // remove immediately followed by a reinstall must not regenerate the
        // just-tombstoned record's content-addressed docID (see
        // `mint_recreate_identity_timestamp`).
        "installed_at": crate::config_client::mint_recreate_identity_timestamp(),
        "required_by": if required_by.is_empty() { Value::Null } else { json!(required_by) },
        "explicit": prior.explicit || explicit,
    });
    let mutation = match &prior.doc_id {
        Some(doc_id) => {
            let mut update = input;
            if let Some(object) = update.as_object_mut() {
                object.remove("agent_did");
                object.remove("coordinate");
            }
            txn.execute_with_variables(
                &format!(
                    r#"mutation($input: {RECORD}MutationInputArg!) {{ update_{RECORD}(docID: "{}", input: $input) {{ _docID }} }}"#,
                    escape_graphql_string(doc_id)
                ),
                &json!({ "input": update }),
            )
            .await?
        }
        None => {
            txn.execute_with_variables(
                &format!(
                    "mutation($input: {RECORD}MutationInputArg!) {{ create_{RECORD}(input: $input) {{ _docID }} }}"
                ),
                &json!({ "input": input }),
            )
            .await?
        }
    };
    anyhow::ensure!(
        mutation.get("errors").is_none_or(Value::is_null),
        "writing the installation record failed: {}",
        mutation["errors"]
    );
    Ok(())
}

/// Installs `documents` for `owner` as `pack` and records what it did, in
/// the transaction `txn`.
pub(crate) async fn install_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    pack: &PackIdentity,
    documents: Vec<DesiredStateApplyDocument>,
    policy: DriftPolicy,
) -> Result<InstallReport> {
    let prior = read_record(txn, owner, &pack.coordinate).await?;
    let mut report = InstallReport::default();
    let mut recorded = BTreeMap::new();
    let mut edited = Vec::new();
    let mut keep = Vec::new();

    let document_ids: Vec<(Collection, &str)> = documents
        .iter()
        .map(|document| {
            let id = document.add[document.collection.unique_field()]
                .as_str()
                .context("pack document is missing its logical ID")?;
            Ok((document.collection, id))
        })
        .collect::<Result<_>>()?;
    let live_before = live_digests(txn, owner, document_ids).await?;

    for document in &documents {
        let collection = document.collection.graphql_type();
        let id = document.add[document.collection.unique_field()]
            .as_str()
            .context("pack document is missing its logical ID")?;
        let name = key(collection, id);
        let previous = prior.documents.get(&name);
        let live = live_before
            .get(&(document.collection, id.to_owned()))
            .cloned();
        let created = match (&live, previous) {
            (None, _) => {
                report.created.push(name.clone());
                true
            }
            (Some(live), Some(previous)) if *live != previous.digest => {
                match policy {
                    DriftPolicy::Refuse => edited.push(name.clone()),
                    DriftPolicy::Overwrite => report.replaced.push(name.clone()),
                    DriftPolicy::Keep => {
                        report.kept.push(name.clone());
                        keep.push(name.clone());
                        recorded.insert(name, previous.clone());
                        continue;
                    }
                }
                previous.created
            }
            (Some(_), Some(previous)) => {
                report.replaced.push(name.clone());
                previous.created
            }
            (Some(_), None) => {
                report.adopted.push(name.clone());
                false
            }
        };
        recorded.insert(
            name,
            RecordedDocument {
                collection: collection.to_owned(),
                id: id.to_owned(),
                digest: String::new(),
                created,
            },
        );
    }

    let stale: Vec<(Collection, &str)> = prior
        .documents
        .iter()
        .filter(|(name, previous)| !recorded.contains_key(*name) && previous.created)
        .map(|(_, previous)| {
            Ok((
                collection_named(&previous.collection)?,
                previous.id.as_str(),
            ))
        })
        .collect::<Result<_>>()?;
    let live_stale = live_digests(txn, owner, stale).await?;

    let mut removals = Vec::new();
    for (name, previous) in &prior.documents {
        if recorded.contains_key(name) || !previous.created {
            continue;
        }
        let collection = collection_named(&previous.collection)?;
        match live_stale.get(&(collection, previous.id.clone())).cloned() {
            None => {}
            Some(live) if live != previous.digest && policy == DriftPolicy::Refuse => {
                edited.push(name.clone());
            }
            Some(live) if live != previous.digest && policy == DriftPolicy::Keep => {
                report.kept.push(name.clone());
            }
            Some(_) => {
                report.removed.push(name.clone());
                removals.push((collection, owner.to_owned(), previous.id.clone()));
            }
        }
    }
    if !edited.is_empty() {
        return Err(edited_error(&pack.coordinate, &edited));
    }

    let applied: Vec<_> = documents
        .into_iter()
        .filter(|document| {
            let id = document.add[document.collection.unique_field()]
                .as_str()
                .unwrap_or_default();
            !keep.contains(&key(document.collection.graphql_type(), id))
        })
        .collect();
    let plan = super::provenance::prepare_pack_plan_in_txn(txn, &applied, false)
        .await?
        .with_removals(removals)?;
    crate::config_client::validate_desired_state_plan(txn, &plan).await?;
    report.applied = crate::config_client::apply_desired_state_plan(txn, &plan).await?;
    report.plugins = pack.plugins.clone();
    // The record holds what is stored, not what was authored: the stored
    // document carries merged tags and normalized fields, and drift is judged
    // against it.
    let written: Vec<(Collection, &str)> = recorded
        .iter()
        .filter(|(name, _)| !keep.contains(name))
        .map(|(_, document)| {
            Ok((
                collection_named(&document.collection)?,
                document.id.as_str(),
            ))
        })
        .collect::<Result<_>>()?;
    let live_after = live_digests(txn, owner, written).await?;
    for (name, document) in recorded.iter_mut() {
        if keep.contains(name) {
            continue;
        }
        let collection = collection_named(&document.collection)?;
        document.digest = live_after
            .get(&(collection, document.id.clone()))
            .cloned()
            .with_context(|| format!("{name} is missing after install"))?;
    }

    write_record_in_txn(
        txn,
        owner,
        pack,
        &prior,
        recorded.into_values().collect(),
        true,
    )
    .await?;
    dependencies::claim_in_txn(txn, owner, &pack.coordinate, &pack.dependencies, policy).await?;
    Ok(report)
}

/// Removes what `record` lists for `coordinate`/`owner`: a graph-owned
/// document (see [`graph::is_graph_owned`]) unconditionally when this install
/// created it, a pack-authored one only when its live content still matches
/// what the pack wrote (subject to `policy` otherwise); either kind is left
/// alone, adopted, when the record says this install did not create it.
/// Refuses while any of the record's graphs has a nonterminal run. Deletes
/// the record itself. Does not touch dependency bookkeeping on other
/// records; see [`remove_pack`] and [`dependencies`].
async fn remove_record_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    coordinate: &str,
    record: &Record,
    policy: DriftPolicy,
) -> Result<RemoveReport> {
    let doc_id = record
        .doc_id
        .clone()
        .with_context(|| format!("{coordinate} is not installed for {owner}"))?;

    let graph_ids: BTreeSet<String> = record
        .documents
        .values()
        .filter(|document| document.collection == "GraphDefinition")
        .map(|document| document.id.clone())
        .collect();
    if !graph_ids.is_empty() {
        graph::ensure_no_unfinished_runs_in_txn(txn, owner, &graph_ids).await?;
    }

    let mut report = RemoveReport {
        pack: coordinate.to_owned(),
        documents: InstallReport {
            plugins: record.plugins.clone(),
            ..InstallReport::default()
        },
        retained: Vec::new(),
        dependencies: Vec::new(),
        digests: std::iter::once(record.digest.clone())
            .flatten()
            .chain(record.history.iter().cloned())
            .collect(),
    };
    let mut edited = Vec::new();
    let mut removals = Vec::new();
    let mut revision_digests = Vec::new();

    // Every created document that needs a live check (everything except an
    // adopted one and a `GraphRevision`, checked separately below), read in
    // one batched pass instead of one query per document.
    let checked: Vec<(Collection, &str)> = record
        .documents
        .values()
        .filter(|document| document.created && document.collection != "GraphRevision")
        .map(|document| {
            Ok((
                collection_named(&document.collection)?,
                document.id.as_str(),
            ))
        })
        .collect::<Result<_>>()?;
    let live = live_digests(txn, owner, checked).await?;

    for (name, document) in &record.documents {
        if graph::is_graph_owned(document) {
            if !document.created {
                report.documents.adopted.push(name.clone());
                continue;
            }
            if document.collection == "GraphRevision" {
                revision_digests.push(document.id.clone());
                continue;
            }
            let collection = collection_named(&document.collection)?;
            if live.contains_key(&(collection, document.id.clone())) {
                report.documents.removed.push(name.clone());
                removals.push((collection, owner.to_owned(), document.id.clone()));
            }
            continue;
        }
        if !document.created {
            report.documents.adopted.push(name.clone());
            continue;
        }
        let collection = collection_named(&document.collection)?;
        match live.get(&(collection, document.id.clone())).cloned() {
            None => continue,
            Some(live) if live != document.digest => match policy {
                DriftPolicy::Refuse => {
                    edited.push(name.clone());
                    continue;
                }
                DriftPolicy::Keep => {
                    report.documents.kept.push(name.clone());
                    continue;
                }
                DriftPolicy::Overwrite => {}
            },
            Some(_) => {}
        }
        report.documents.removed.push(name.clone());
        removals.push((collection, owner.to_owned(), document.id.clone()));
    }
    if !edited.is_empty() {
        return Err(edited_error(coordinate, &edited));
    }

    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(removals)?;
    crate::config_client::validate_desired_state_plan(txn, &plan).await?;
    report.documents.applied = crate::config_client::apply_desired_state_plan(txn, &plan).await?;

    if !revision_digests.is_empty() {
        let (removed, retained) =
            graph::delete_revisions_in_txn(txn, owner, &revision_digests).await?;
        report.documents.removed.extend(removed);
        report.retained.extend(retained);
    }

    txn.execute(&format!(
        r#"mutation {{ delete_{RECORD}(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
        escape_graphql_string(&doc_id)
    ))
    .await?;

    Ok(report)
}

/// Removes what the install of `coordinate` for `owner` created, and its
/// record. Documents it adopted stay. Refused while another installed pack
/// still depends on `coordinate`. Releases (and reports) any dependency this
/// was the last non-explicit claim on.
pub async fn remove_pack(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
    policy: DriftPolicy,
) -> Result<RemoveReport> {
    access
        .transact("pack.remove", |txn| {
            Box::pin(async move {
                let record = read_record(txn, owner, coordinate).await?;
                anyhow::ensure!(
                    record.doc_id.is_some(),
                    "{coordinate} is not installed for {owner}"
                );
                if !record.required_by.is_empty() {
                    let dependents: Vec<&str> =
                        record.required_by.iter().map(String::as_str).collect();
                    anyhow::bail!(
                        "{coordinate} is required by {}; remove {} first",
                        dependents.join(", "),
                        if dependents.len() == 1 { "it" } else { "them" }
                    );
                }
                let mut report =
                    remove_record_in_txn(txn, owner, coordinate, &record, policy).await?;
                report.dependencies =
                    dependencies::release_in_txn(txn, owner, coordinate, policy).await?;
                Ok(report)
            })
        })
        .await
}

/// One installed pack, as its record states it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstalledPack {
    pub coordinate: String,
    pub version: String,
    pub digest: String,
}

/// Every pack installed for `owner`, by coordinate.
pub async fn list_installed_packs(
    access: &ConfigAccess,
    owner: &str,
) -> Result<Vec<InstalledPack>> {
    let response = access
        .execute(&format!(
            r#"{{ {RECORD}(filter: {{ agent_did: {{ _eq: "{}" }} }}, order: {{ coordinate: ASC }}) {{ _docID coordinate version digest }} }}"#,
            escape_graphql_string(owner)
        ))
        .await?;
    response["data"][RECORD]
        .as_array()
        .context("reading the pack installation records")?
        .iter()
        .map(|row| {
            Ok(InstalledPack {
                coordinate: required_record_field_str(row, "coordinate")?.to_owned(),
                version: required_record_field_str(row, "version")?.to_owned(),
                digest: required_record_field_str(row, "digest")?.to_owned(),
            })
        })
        .collect()
}

/// The installed record for `coordinate`, if any: one query bounded to two
/// rows, so a duplicate record (which should never exist) fails loud
/// instead of silently picking one.
pub async fn read_installed_pack(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
) -> Result<Option<InstalledPack>> {
    let response = access
        .execute(&format!(
            r#"{{ {RECORD}(filter: {{ agent_did: {{ _eq: "{}" }}, coordinate: {{ _eq: "{}" }} }}, limit: 2) {{ _docID coordinate version digest }} }}"#,
            escape_graphql_string(owner),
            escape_graphql_string(coordinate)
        ))
        .await?;
    let rows = response["data"][RECORD]
        .as_array()
        .context("reading the pack installation records")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "{coordinate} has more than one installation record for its owner"
    );
    rows.first()
        .map(|row| {
            Ok(InstalledPack {
                coordinate: required_record_field_str(row, "coordinate")?.to_owned(),
                version: required_record_field_str(row, "version")?.to_owned(),
                digest: required_record_field_str(row, "digest")?.to_owned(),
            })
        })
        .transpose()
}

/// Every pack installed for `owner`, combining node-recorded installs
/// (documents and graph packs) with `home`'s file-recorded ones (assets and
/// plugins packs), sorted by coordinate. `pack outdated`/`update` and the
/// desktop installed-pack listing use this rather than each keeping their
/// own merge of the two sources.
pub async fn installed_packs(
    home: Option<&Path>,
    node: Option<(&ConfigAccess, &str)>,
) -> Result<Vec<InstalledPack>> {
    let mut packs = BTreeMap::new();
    if let Some(home) = home {
        for record in super::list_home_installs(home)? {
            packs.insert(
                record.coordinate.clone(),
                InstalledPack {
                    coordinate: record.coordinate,
                    version: record.version,
                    digest: record.digest,
                },
            );
        }
    }
    if let Some((access, owner)) = node {
        for pack in list_installed_packs(access, owner).await? {
            packs.insert(pack.coordinate.clone(), pack);
        }
    }
    Ok(packs.into_values().collect())
}

/// Every pack digest referenced by an install record in `home` (file
/// records) or reachable through `node` (every owner's `PackInstallation`,
/// current digest plus history): what `gents pack remove` must keep an
/// archive or its unpacked copy for.
pub async fn referenced_pack_digests(
    home: &Path,
    node: Option<&ConfigAccess>,
) -> Result<BTreeSet<String>> {
    let mut digests = BTreeSet::new();
    for record in super::list_home_installs(home)? {
        digests.insert(record.digest);
    }
    if let Some(access) = node {
        let response = access
            .execute(&format!("{{ {RECORD} {{ digest history }} }}"))
            .await?;
        for row in response["data"][RECORD]
            .as_array()
            .context("reading the pack installation records")?
        {
            if let Some(digest) = row.get("digest").and_then(Value::as_str) {
                digests.insert(digest.to_owned());
            }
            let history: Vec<String> = decode_record_field(row, "history", "*", "*")?;
            digests.extend(history);
        }
    }
    Ok(digests)
}

/// A required string field of an installation record, failing loudly and
/// naming the record's document ID when it is missing or not a string.
fn required_record_field_str<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field).and_then(Value::as_str).with_context(|| {
        let doc_id = row.get("_docID").and_then(Value::as_str).unwrap_or("?");
        format!("installation record {doc_id} has a missing or malformed {field} field")
    })
}

/// The documents of `config` an install writes: all but the principal,
/// which is shared identity, never pack-owned.
pub(crate) fn installable_documents(config: &PackConfig) -> Result<Vec<DesiredStateApplyDocument>> {
    Ok(DesiredStateApplyPlan::from_pack_config(config)?
        .documents()
        .iter()
        .filter(|document| document.collection != Collection::AgentPrincipal)
        .cloned()
        .collect())
}

#[cfg(test)]
mod tests;
