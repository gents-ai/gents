use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
use identity::Did;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config_client::{
    ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::documents::PackConfig;
use crate::graphql::escape_graphql_string;
use crate::Collection;

mod dependencies;
mod documents;
pub(crate) use documents::sdl_paths;
mod removal;
pub use documents::{
    apply_prepared_dependency_schemas, document_pack_schema_paths, install_documents_with_plugins,
    install_prepared_document_graph, install_prepared_document_pack, plugin_data_unique_keys,
    prepare_document_pack_install, PreparedDependency, PreparedDocumentPackInstall,
};
pub use removal::ensure_plugins_unreferenced;
const RECORD: &str = "PackInstallation";
const HISTORY_LIMIT: usize = 32;

#[derive(Debug, Clone)]
pub struct PackIdentity {
    pub coordinate: String,
    pub version: String,
    pub digest: String,
    pub plugins: Vec<InstalledPackPlugin>,
    pub dependencies: Vec<String>,
    /// False when this install is a dependency of another pack's install; an
    /// explicit install is sticky.
    pub explicit: bool,
}

impl PackIdentity {
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
            explicit: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledPackPlugin {
    pub name: String,
    pub digest: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DriftPolicy {
    #[default]
    Refuse,
    Overwrite,
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
    version: Option<String>,
    digest: Option<String>,
    documents: BTreeMap<String, RecordedDocument>,
    plugins: Vec<InstalledPackPlugin>,
    history: Vec<String>,
    required_by: BTreeSet<String>,
    /// Missing legacy records default to explicit ownership.
    explicit: bool,
    plugin_store: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct InstallReport {
    #[serde(flatten)]
    pub applied: crate::config_client::DesiredStateApplyCounts,
    pub created: Vec<String>,
    pub replaced: Vec<String>,
    pub adopted: Vec<String>,
    pub kept: Vec<String>,
    pub removed: Vec<String>,
    pub plugins: Vec<InstalledPackPlugin>,
}

#[derive(Debug, Serialize)]
pub struct Retained {
    pub item: String,
    pub reason: String,
}

#[derive(Debug, Default, Serialize)]
pub struct RemoveReport {
    pub pack: String,
    #[serde(flatten)]
    pub documents: InstallReport,
    pub retained: Vec<Retained>,
    pub dependencies: Vec<RemoveReport>,
    pub digests: Vec<String>,
}

fn key(collection: &str, id: &str) -> String {
    format!("{collection}/{id}")
}

/// A record can name a collection this build no longer manages; callers
/// leave such documents dormant instead of failing.
fn managed_collection(name: &str) -> Option<Collection> {
    Collection::ALL
        .iter()
        .copied()
        .find(|collection| collection.graphql_type() == name)
}

fn collection_named(name: &str) -> Result<Collection> {
    managed_collection(name)
        .with_context(|| format!("installation record names unknown collection {name:?}"))
}

fn is_reserved(document: &RecordedDocument) -> bool {
    crate::agent::document_view::is_retired_reserved_id(&document.id)
}

async fn read_record(txn: &ConfigApplyTxn<'_>, owner: &str, coordinate: &str) -> Result<Record> {
    let response = txn
        .execute(&format!(
            r#"{{ {RECORD}(filter: {{ node_did: {{ _eq: "{}" }}, coordinate: {{ _eq: "{}" }} }}, limit: 2) {{ _docID version digest documents plugins history required_by explicit plugin_store }} }}"#,
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
        version: row["version"].as_str().map(str::to_owned),
        digest: row["digest"].as_str().map(str::to_owned),
        documents: documents
            .into_iter()
            .map(|document| (key(&document.collection, &document.id), document))
            .collect(),
        plugins: decode_record_field(row, "plugins", owner, coordinate)?,
        history: decode_record_field(row, "history", owner, coordinate)?,
        required_by: required_by.into_iter().collect(),
        explicit: row.get("explicit").and_then(Value::as_bool).unwrap_or(true),
        plugin_store: row["plugin_store"].as_bool().unwrap_or(false),
    })
}

fn decode_record_field<T>(row: &Value, field: &str, owner: &str, coordinate: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    match row.get(field) {
        None | Some(Value::Null) => Ok(T::default()),
        Some(value) => serde_json::from_value(value.clone()).with_context(|| {
            format!(
                "installation record {coordinate:?} for {owner:?} has a malformed {field} field"
            )
        }),
    }
}

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
         use the operator CLI `gents pack install`, `gents pack update`, or `gents pack remove` with --overwrite or --keep to decide how to handle them; the agent pack command does not accept these flags",
        edited.len(),
        edited.join(", ")
    )
}

async fn write_record_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    pack: &PackIdentity,
    prior: &Record,
    documents: Vec<RecordedDocument>,
    explicit: bool,
    plugin_store: bool,
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
        "node_did": owner,
        "coordinate": pack.coordinate,
        "version": pack.version,
        "digest": pack.digest,
        "documents": if documents.is_empty() { Value::Null } else { json!(documents) },
        "plugins": if pack.plugins.is_empty() {
            Value::Null
        } else {
            json!(pack.plugins)
        },
        "history": history,
        // A remove immediately followed by a reinstall must not regenerate
        // the just-tombstoned record's content-addressed docID (see
        // `mint_recreate_identity_timestamp`).
        "installed_at": crate::config_client::mint_recreate_identity_timestamp(),
        "required_by": if required_by.is_empty() { Value::Null } else { json!(required_by) },
        "explicit": prior.explicit || explicit,
        "plugin_store": plugin_store,
    });
    let mutation = match &prior.doc_id {
        Some(doc_id) => {
            let mut update = input;
            if let Some(object) = update.as_object_mut() {
                object.remove("node_did");
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
        .filter_map(|(_, previous)| {
            Some((
                managed_collection(&previous.collection)?,
                previous.id.as_str(),
            ))
        })
        .collect();
    let live_stale = live_digests(txn, owner, stale).await?;

    let mut removals = Vec::new();
    for (name, previous) in &prior.documents {
        if recorded.contains_key(name) || !previous.created {
            continue;
        }
        let Some(collection) = managed_collection(&previous.collection) else {
            continue;
        };
        match live_stale.get(&(collection, previous.id.clone())).cloned() {
            None => {}
            Some(_) if is_reserved(previous) => {
                report.removed.push(name.clone());
                removals.push((collection, owner.to_owned(), previous.id.clone()));
            }
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
    let plan = super::provenance::prepare_pack_plan_in_txn(txn, &applied)
        .await?
        .with_removals(removals)?;
    crate::config_client::validate_desired_state_plan(txn, &plan).await?;
    report.applied = crate::config_client::apply_desired_state_plan(txn, &plan).await?;
    report.plugins = pack.plugins.clone();
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
        pack.explicit,
        false,
    )
    .await?;
    dependencies::claim_in_txn(txn, owner, &pack.coordinate, &pack.dependencies, policy).await?;
    Ok(report)
}

pub(crate) async fn validate_plugin_replacements_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    packs: &[PackIdentity],
) -> Result<()> {
    if let Some(conflict) = plugin_replacement_conflict_in_txn(txn, owner, packs).await? {
        anyhow::bail!(conflict);
    }
    Ok(())
}

async fn plugin_replacement_conflict_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    packs: &[PackIdentity],
) -> Result<Option<String>> {
    let mut replacements = BTreeMap::new();
    for pack in packs {
        let (namespace, _) = crate::pack_registry::split_pack_coordinate(&pack.coordinate);
        for plugin in &pack.plugins {
            let coordinate = format!("{namespace}/{}", plugin.name);
            if let Some(previous) = replacements.insert(coordinate.clone(), plugin.digest.clone()) {
                anyhow::ensure!(
                    previous == plugin.digest,
                    "pack graph replaces {coordinate} with conflicting digests"
                );
            }
        }
    }
    removal::ensure_replaced_plugins_match(txn, owner, &replacements).await
}

/// Local publication is restored only when the transaction owner proves no
/// attempt committed; otherwise the published artifacts remain available.
#[derive(Debug, thiserror::Error)]
pub enum PluginStoreChangeError {
    #[error("{0}")]
    PinConflict(String),
    #[error(transparent)]
    Rejected(anyhow::Error),
    #[error(transparent)]
    Unconfirmed(#[from] anyhow::Error),
}

impl From<crate::config_client::ConfigClientError> for PluginStoreChangeError {
    fn from(error: crate::config_client::ConfigClientError) -> Self {
        Self::Unconfirmed(error.into())
    }
}

impl crate::config_client::TransactionFailure for PluginStoreChangeError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::PinConflict(_) => None,
            Self::Rejected(error) | Self::Unconfirmed(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
        }
    }
}

pub async fn record_plugin_store_change(
    access: &ConfigAccess,
    owner: &str,
    home: &Path,
    coordinate: &str,
    installed: Option<&PackIdentity>,
) -> std::result::Result<(), PluginStoreChangeError> {
    record_plugin_store_change_as(access, owner, None, home, coordinate, installed).await
}

/// Model-facing callers pass the invoking DID; `owner` is document scope, not authentication.
pub async fn record_plugin_store_change_as(
    access: &ConfigAccess,
    owner: &str,
    identity: Option<Did>,
    home: &Path,
    coordinate: &str,
    installed: Option<&PackIdentity>,
) -> std::result::Result<(), PluginStoreChangeError> {
    let result = match access {
        ConfigAccess::Local(local) => {
            ConfigAccess::transact_local_write_with_finality(
                local.node(),
                identity,
                "pack.record_plugin_store",
                |txn| {
                    Box::pin(async move {
                        write_plugin_record(txn, owner, home, coordinate, installed).await
                    })
                },
            )
            .await
        }
        ConfigAccess::Graphql(_) => {
            access
                .transact_write_with_finality("pack.record_plugin_store", |txn| {
                    Box::pin(async move {
                        write_plugin_record(txn, owner, home, coordinate, installed).await
                    })
                })
                .await
        }
    };
    result.map_err(|failure| classify_plugin_store_failure(failure.error, failure.unconfirmed))
}

fn classify_plugin_store_failure(
    error: PluginStoreChangeError,
    unconfirmed: bool,
) -> PluginStoreChangeError {
    if unconfirmed {
        PluginStoreChangeError::Unconfirmed(error.into())
    } else {
        match error {
            PluginStoreChangeError::Unconfirmed(error) => PluginStoreChangeError::Rejected(error),
            other => other,
        }
    }
}

async fn write_plugin_record(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    home: &Path,
    coordinate: &str,
    installed: Option<&PackIdentity>,
) -> std::result::Result<(), PluginStoreChangeError> {
    if let Some(pack) = installed {
        if let Some(conflict) =
            plugin_replacement_conflict_in_txn(txn, owner, std::slice::from_ref(pack)).await?
        {
            return Err(PluginStoreChangeError::PinConflict(conflict));
        }
    }
    let current_home = super::read_home_install(home, coordinate)?;
    let current_identity = current_home.map(|record| PackIdentity {
        coordinate: record.coordinate,
        version: record.version,
        digest: record.digest,
        plugins: record.plugins,
        dependencies: Vec::new(),
        explicit: true,
    });
    let installed = current_identity.as_ref().or(installed);
    let retained = installed.is_none()
        && crate::plugin::store::list_records(home)?
            .iter()
            .any(|plugin| plugin.owner_pack_coordinate.as_deref() == Some(coordinate));
    let prior = read_record(txn, owner, coordinate).await?;
    let documents: Vec<RecordedDocument> = prior.documents.values().cloned().collect();
    let own = prior.doc_id.is_none() || prior.plugin_store;
    match installed {
        Some(pack) if own => {
            return write_record_in_txn(txn, owner, pack, &prior, documents, false, true)
                .await
                .map_err(Into::into)
        }
        None if prior.doc_id.is_none() => return Ok(()),
        None if own && !retained => {
            return remove_record_in_txn(txn, owner, coordinate, &prior, DriftPolicy::Refuse)
                .await
                .map(|_| ())
                .map_err(Into::into);
        }
        _ => {}
    }
    let mut plugins = prior.plugins.clone();
    for plugin in installed.map_or(&[][..], |pack| &pack.plugins[..]) {
        plugins.retain(|kept| kept.name != plugin.name);
        plugins.push(plugin.clone());
    }
    let unchanged = PackIdentity {
        coordinate: coordinate.to_owned(),
        version: prior.version.clone().unwrap_or_default(),
        digest: prior.digest.clone().unwrap_or_default(),
        plugins,
        dependencies: Vec::new(),
        explicit: prior.explicit,
    };
    let plugin_store = prior.plugin_store;
    write_record_in_txn(
        txn,
        owner,
        &unchanged,
        &prior,
        documents,
        false,
        plugin_store,
    )
    .await
    .map_err(Into::into)
}

fn is_plugin_store_row(row: &Value) -> bool {
    row["plugin_store"].as_bool() == Some(true)
}

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

    let checked: Vec<(Collection, &str)> = record
        .documents
        .values()
        .filter(|document| document.created)
        .filter_map(|document| {
            Some((
                managed_collection(&document.collection)?,
                document.id.as_str(),
            ))
        })
        .collect();
    let live = live_digests(txn, owner, checked).await?;

    for (name, document) in &record.documents {
        if !document.created {
            report.documents.adopted.push(name.clone());
            continue;
        }
        let Some(collection) = managed_collection(&document.collection) else {
            report.retained.push(Retained {
                item: format!("{} {}", document.collection, document.id),
                reason: "collection is no longer managed; the document stays dormant".to_owned(),
            });
            continue;
        };
        match live.get(&(collection, document.id.clone())).cloned() {
            None => continue,
            Some(_) if is_reserved(document) => {}
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

    txn.execute(&format!(
        r#"mutation {{ delete_{RECORD}(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
        escape_graphql_string(&doc_id)
    ))
    .await?;

    Ok(report)
}

pub async fn remove_pack(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
    policy: DriftPolicy,
) -> Result<RemoveReport> {
    access
        .transact_write("pack.remove", |txn| {
            Box::pin(async move {
                let record = read_record(txn, owner, coordinate).await?;
                anyhow::ensure!(
                    record.doc_id.is_some(),
                    "{coordinate} is not installed for {owner}"
                );
                anyhow::ensure!(
                    !record.plugin_store,
                    "{coordinate} is not installed as a pack for {owner}: only its plugins \
                     are, through gents plugin install; remove them with gents plugin remove"
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
                let released = std::iter::once(&report)
                    .chain(&report.dependencies)
                    .flat_map(|removed| {
                        let (namespace, _) =
                            crate::pack_registry::split_pack_coordinate(&removed.pack);
                        removed
                            .documents
                            .plugins
                            .iter()
                            .map(move |plugin| format!("{namespace}/{}", plugin.name))
                    })
                    .collect();
                ensure_plugins_unreferenced(txn, owner, &released).await?;
                Ok(report)
            })
        })
        .await
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstalledPack {
    pub coordinate: String,
    pub version: String,
    pub digest: String,
}

pub async fn list_installed_packs(
    access: &ConfigAccess,
    owner: &str,
) -> Result<Vec<InstalledPack>> {
    let response = access
        .execute(&format!(
            r#"{{ {RECORD}(filter: {{ node_did: {{ _eq: "{}" }} }}, order: {{ coordinate: ASC }}) {{ _docID coordinate version digest plugin_store }} }}"#,
            escape_graphql_string(owner)
        ))
        .await?;
    response["data"][RECORD]
        .as_array()
        .context("reading the pack installation records")?
        .iter()
        .filter(|row| !is_plugin_store_row(row))
        .map(|row| {
            Ok(InstalledPack {
                coordinate: required_record_field_str(row, "coordinate")?.to_owned(),
                version: required_record_field_str(row, "version")?.to_owned(),
                digest: required_record_field_str(row, "digest")?.to_owned(),
            })
        })
        .collect()
}

pub async fn read_installed_pack(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
) -> Result<Option<InstalledPack>> {
    let response = access
        .execute(&format!(
            r#"{{ {RECORD}(filter: {{ node_did: {{ _eq: "{}" }}, coordinate: {{ _eq: "{}" }} }}, limit: 2) {{ _docID coordinate version digest plugin_store }} }}"#,
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
        .filter(|row| !is_plugin_store_row(row))
        .map(|row| {
            Ok(InstalledPack {
                coordinate: required_record_field_str(row, "coordinate")?.to_owned(),
                version: required_record_field_str(row, "version")?.to_owned(),
                digest: required_record_field_str(row, "digest")?.to_owned(),
            })
        })
        .transpose()
}

/// The `(collection, id)` pairs `coordinate`'s installation record for
/// `owner` lists, in record order. Empty when the pack has no node record
/// (an assets or plugins pack, or none installed).
pub async fn installed_pack_documents(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
) -> Result<Vec<(Collection, String)>> {
    let response = access
        .execute(&format!(
            r#"{{ {RECORD}(filter: {{ node_did: {{ _eq: "{}" }}, coordinate: {{ _eq: "{}" }} }}, limit: 2) {{ _docID documents plugin_store }} }}"#,
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
    let Some(row) = rows.first().filter(|row| !is_plugin_store_row(row)) else {
        return Ok(Vec::new());
    };
    Ok(
        decode_record_field::<Vec<RecordedDocument>>(row, "documents", owner, coordinate)?
            .into_iter()
            .filter_map(|document| Some((managed_collection(&document.collection)?, document.id)))
            .collect(),
    )
}

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

fn required_record_field_str<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.get(field).and_then(Value::as_str).with_context(|| {
        let doc_id = row.get("_docID").and_then(Value::as_str).unwrap_or("?");
        format!("installation record {doc_id} has a missing or malformed {field} field")
    })
}

pub(crate) fn installable_documents(config: &PackConfig) -> Result<Vec<DesiredStateApplyDocument>> {
    Ok(DesiredStateApplyPlan::from_pack_config(config)?
        .documents()
        .iter()
        .filter(|document| document.collection != Collection::Node)
        .cloned()
        .collect())
}

#[cfg(test)]
mod tests;
