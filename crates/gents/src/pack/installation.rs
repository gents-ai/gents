//! Installation records: what a pack install created, so an upgrade can tell
//! an operator's edit from the pack's own document and removal deletes only
//! what the pack created.
//!
//! One `PackInstallation` document per owner and pack coordinate, written in
//! the same transaction as the documents it describes. For each document it
//! holds the content digest the install wrote and whether the install created
//! it (a document that already existed is adopted, never later deleted).
//!
//! On install, a document the record lists whose live content no longer
//! matches the recorded digest was edited by someone else. Install stops and
//! names each one unless the caller chose [`DriftPolicy::Overwrite`] or
//! [`DriftPolicy::Keep`]. A document the previous version created and the new
//! version dropped is removed, under the same rule.

use std::collections::BTreeMap;

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

#[derive(Debug, Default)]
struct Record {
    doc_id: Option<String>,
    digest: Option<String>,
    documents: BTreeMap<String, RecordedDocument>,
    plugins: Vec<InstalledPackPlugin>,
    history: Vec<String>,
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
            r#"{{ {RECORD}(filter: {{ agent_did: {{ _eq: "{}" }}, coordinate: {{ _eq: "{}" }} }}, limit: 2) {{ _docID digest documents plugins history }} }}"#,
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
        serde_json::from_value(row["documents"].clone()).unwrap_or_default();
    Ok(Record {
        doc_id: row["_docID"].as_str().map(str::to_owned),
        digest: row["digest"].as_str().map(str::to_owned),
        documents: documents
            .into_iter()
            .map(|document| (key(&document.collection, &document.id), document))
            .collect(),
        plugins: serde_json::from_value(row["plugins"].clone()).unwrap_or_default(),
        history: serde_json::from_value(row["history"].clone()).unwrap_or_default(),
    })
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

fn edited_error(coordinate: &str, edited: &[String]) -> anyhow::Error {
    anyhow::anyhow!(
        "{} of {coordinate}'s documents were edited since it was installed: {}; \
         choose --overwrite to replace them or --keep to leave them",
        edited.len(),
        edited.join(", ")
    )
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

    for document in &documents {
        let collection = document.collection.graphql_type();
        let id = document.add[document.collection.unique_field()]
            .as_str()
            .context("pack document is missing its logical ID")?;
        let name = key(collection, id);
        let previous = prior.documents.get(&name);
        let live = live_digest(txn, document.collection, owner, id).await?;
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

    let mut removals = Vec::new();
    for (name, previous) in &prior.documents {
        if recorded.contains_key(name) || !previous.created {
            continue;
        }
        let collection = collection_named(&previous.collection)?;
        match live_digest(txn, collection, owner, &previous.id).await? {
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
    for (name, document) in recorded.iter_mut() {
        if keep.contains(name) {
            continue;
        }
        let collection = collection_named(&document.collection)?;
        document.digest = live_digest(txn, collection, owner, &document.id)
            .await?
            .with_context(|| format!("{name} is missing after install"))?;
    }

    let mut history = prior.history;
    if let Some(previous) = prior.digest.filter(|previous| *previous != pack.digest) {
        history.insert(0, previous);
        history.truncate(HISTORY_LIMIT);
    }
    let input = json!({
        "agent_did": owner,
        "coordinate": pack.coordinate,
        "version": pack.version,
        "digest": pack.digest,
        "documents": recorded.into_values().collect::<Vec<_>>(),
        "plugins": pack.plugins,
        "history": history,
        "installed_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
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
    Ok(report)
}

/// Removes what the install of `coordinate` for `owner` created, and its
/// record. Documents it adopted stay.
pub async fn remove_pack(
    access: &ConfigAccess,
    owner: &str,
    coordinate: &str,
    policy: DriftPolicy,
) -> Result<InstallReport> {
    access
        .transact("pack.remove", |txn| {
            Box::pin(async move {
                let record = read_record(txn, owner, coordinate).await?;
                let doc_id = record
                    .doc_id
                    .clone()
                    .with_context(|| format!("{coordinate} is not installed for {owner}"))?;
                let mut report = InstallReport {
                    plugins: record.plugins.clone(),
                    ..InstallReport::default()
                };
                let mut edited = Vec::new();
                let mut removals = Vec::new();
                for (name, document) in &record.documents {
                    if !document.created {
                        report.adopted.push(name.clone());
                        continue;
                    }
                    let collection = collection_named(&document.collection)?;
                    match live_digest(txn, collection, owner, &document.id).await? {
                        None => continue,
                        Some(live) if live != document.digest => match policy {
                            DriftPolicy::Refuse => {
                                edited.push(name.clone());
                                continue;
                            }
                            DriftPolicy::Keep => {
                                report.kept.push(name.clone());
                                continue;
                            }
                            DriftPolicy::Overwrite => {}
                        },
                        Some(_) => {}
                    }
                    report.removed.push(name.clone());
                    removals.push((collection, owner.to_owned(), document.id.clone()));
                }
                if !edited.is_empty() {
                    return Err(edited_error(coordinate, &edited));
                }
                let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(removals)?;
                crate::config_client::validate_desired_state_plan(txn, &plan).await?;
                crate::config_client::apply_desired_state_plan(txn, &plan).await?;
                txn.execute(&format!(
                    r#"mutation {{ delete_{RECORD}(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                    escape_graphql_string(&doc_id)
                ))
                .await?;
                Ok(report)
            })
        })
        .await
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
