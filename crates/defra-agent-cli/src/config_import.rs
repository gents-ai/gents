use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::Collection;
use serde_json::Value;

use crate::config_bundle::{sanitize_import_document, select_apply_collection_docs};
#[cfg(test)]
use crate::config_writes::ConfigAccess;
use crate::config_writes::{
    write_event_trigger_document, write_schedule_document, write_task_document, ConfigApplyTxn,
    ExistingDocumentRef,
};
use crate::desired_state;
use crate::desired_state::DesiredApplyBundle;
use crate::shared::{ConfigApplyCounts, ConfigExportBundle};
use crate::{
    extract_mutation_doc_id, graphql_input_literal, graphql_string_list_literal,
    CONFIG_EXPORT_FORMAT, CONFIG_EXPORT_FORMAT_V1,
};

#[cfg(test)]
#[path = "../../defra-agent/src/lean_vocab_test.rs"]
mod lean_vocab_test;

const CONFIG_IMPORT_BATCH_SIZE: usize = 50;

// Topological desired-state write order. Every prefix is safe to observe: a
// written document may only reference documents in earlier apply ranks, and a
// retry recomputes diff against the partial state before continuing. Production
// realizes the Lean retry model by rebuilding the live diff at the start of
// each `config apply` attempt, then applying selected documents with
// unique-field upserts or equivalent override writers.
const CONFIG_APPLY_ORDER: [Collection; 12] = [
    Collection::PeerPairingDesired,
    Collection::InferenceBackend,
    Collection::InferenceProfile,
    Collection::ToolServiceRegistry,
    Collection::ToolSelection,
    Collection::Skill,
    Collection::AgentBehavior,
    Collection::ProjectionAcpBinding,
    Collection::Task,
    Collection::Schedule,
    Collection::EventTrigger,
    Collection::AgentPrincipal,
];

const CONFIG_PRUNE_ORDER: [Collection; 12] = [
    Collection::AgentPrincipal,
    Collection::EventTrigger,
    Collection::Schedule,
    Collection::Task,
    Collection::ProjectionAcpBinding,
    Collection::AgentBehavior,
    Collection::Skill,
    Collection::ToolSelection,
    Collection::ToolServiceRegistry,
    Collection::InferenceProfile,
    Collection::InferenceBackend,
    Collection::PeerPairingDesired,
];

#[cfg(test)]
pub(crate) const CONFIG_APPLY_ORDER_FOR_TESTS: &[Collection] = &CONFIG_APPLY_ORDER;
#[cfg(test)]
pub(crate) const CONFIG_PRUNE_ORDER_FOR_TESTS: &[Collection] = &CONFIG_PRUNE_ORDER;

#[derive(Debug, Clone)]
struct PreparedImportDocument {
    unique_value: String,
    add_doc: Value,
    update_doc: Option<Value>,
}

#[derive(Debug, Clone)]
struct AliasedMutationField {
    alias: String,
    field: String,
}

pub(crate) fn read_config_import_bundle(
    path: Option<&std::path::Path>,
) -> Result<ConfigExportBundle> {
    let contents = match path {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading config import from {}", path.display()))?,
        None => {
            let mut contents = String::new();
            std::io::stdin()
                .read_to_string(&mut contents)
                .context("reading config import from stdin")?;
            contents
        }
    };
    let mut bundle: ConfigExportBundle =
        serde_json::from_str(&contents).context("decoding config import JSON")?;
    migrate_config_import_bundle(&mut bundle);
    Ok(bundle)
}

pub(crate) fn validate_config_import_bundle(bundle: &ConfigExportBundle) -> Result<()> {
    if !matches!(
        bundle.format.as_str(),
        CONFIG_EXPORT_FORMAT | CONFIG_EXPORT_FORMAT_V1
    ) {
        anyhow::bail!(
            "unsupported config import format {}; expected {}",
            bundle.format,
            CONFIG_EXPORT_FORMAT
        );
    }
    if bundle.agent_did.trim().is_empty() {
        anyhow::bail!("config import is missing agent_did");
    }
    Ok(())
}

pub(crate) fn migrate_config_import_bundle(bundle: &mut ConfigExportBundle) {
    for backend in &mut bundle.inference_backends {
        if let Some(object) = backend.as_object_mut() {
            desired_state::strip_deprecated_inference_backend_fields(object);
        }
    }
    if bundle.format == CONFIG_EXPORT_FORMAT_V1 {
        bundle.format = CONFIG_EXPORT_FORMAT.to_string();
    }
}

pub(crate) async fn apply_import_collection(
    txn: &ConfigApplyTxn<'_>,
    collection_name: &str,
    unique_field: &str,
    docs: &[Value],
    override_existing: bool,
) -> Result<usize> {
    let prepared =
        prepare_import_documents(collection_name, unique_field, docs, override_existing)?;
    if prepared.is_empty() {
        return Ok(0);
    }

    if override_existing && collection_name == "PeerPairingDesired" {
        apply_manifest_pairing_documents(txn, &prepared).await?;
    } else if override_existing && uses_custom_apply_writer(collection_name) {
        apply_custom_override_collection_batched(txn, collection_name, unique_field, &prepared)
            .await?;
    } else {
        apply_generic_import_collection_batched(
            txn,
            collection_name,
            unique_field,
            &prepared,
            override_existing,
        )
        .await?;
    }

    Ok(docs.len())
}

async fn apply_manifest_pairing_documents(
    txn: &ConfigApplyTxn<'_>,
    docs: &[PreparedImportDocument],
) -> Result<()> {
    let fields = docs
        .iter()
        .enumerate()
        .map(|(index, doc)| {
            let source = doc
                .add_doc
                .get("source")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|source| {
                    source.starts_with(desired_state::PEER_PAIRING_MANIFEST_SOURCE_PREFIX)
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "manifest PeerPairingDesired {} is missing manifest source provenance",
                        doc.unique_value
                    )
                })?;
            let add_literal = graphql_input_literal(&doc.add_doc)?;
            let update_literal =
                graphql_input_literal(doc.update_doc.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("missing update document for PeerPairingDesired")
                })?)?;
            Ok(AliasedMutationField {
                alias: format!("doc_{index}"),
                field: format!(
                    r#"doc_{index}: upsert_PeerPairingDesired(
                        filter: {{
                            peer_id: {{ _eq: "{peer_id}" }},
                            source: {{ _eq: "{source}" }}
                        }},
                        add: {add_literal},
                        update: {update_literal}
                    ) {{ _docID }}"#,
                    peer_id = escape_graphql_string(&doc.unique_value),
                    source = escape_graphql_string(source),
                ),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    execute_aliased_mutation_batches(txn, "PeerPairingDesired", &fields).await
}

async fn apply_delete_manifest_pairings(
    txn: &ConfigApplyTxn<'_>,
    ids: &[String],
    owner_agent_did: &str,
) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let source = desired_state::peer_pairing_manifest_source(owner_agent_did);
    let fields = ids
        .iter()
        .enumerate()
        .map(|(index, peer_id)| manifest_pairing_delete_mutation_field(index, peer_id, &source))
        .collect::<Vec<_>>();
    execute_aliased_mutation_batches(txn, "PeerPairingDesired", &fields).await?;
    Ok(ids.len())
}

fn manifest_pairing_delete_mutation_field(
    index: usize,
    peer_id: &str,
    source: &str,
) -> AliasedMutationField {
    AliasedMutationField {
        alias: format!("doc_{index}"),
        field: format!(
            r#"doc_{index}: delete_PeerPairingDesired(
                filter: {{
                    peer_id: {{ _eq: "{peer_id}" }},
                    source: {{ _eq: "{source}" }}
                }}
            ) {{ _docID }}"#,
            peer_id = escape_graphql_string(peer_id),
            source = escape_graphql_string(source),
        ),
    }
}

pub(crate) async fn apply_delete_collection(
    txn: &ConfigApplyTxn<'_>,
    collection_name: &str,
    unique_field: &str,
    ids: &[String],
) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }

    let fields = ids
        .iter()
        .enumerate()
        .map(|(index, id)| delete_mutation_field(index, collection_name, unique_field, id))
        .collect::<Vec<_>>();
    execute_aliased_mutation_batches(txn, collection_name, &fields).await?;
    Ok(ids.len())
}

fn prepare_import_documents(
    collection_name: &str,
    unique_field: &str,
    docs: &[Value],
    override_existing: bool,
) -> Result<Vec<PreparedImportDocument>> {
    docs.iter()
        .map(|doc| {
            let unique_value = doc
                .get(unique_field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} import document is missing {}: {}",
                        collection_name,
                        unique_field,
                        doc
                    )
                })?
                .to_string();
            let add_doc = sanitize_import_document(collection_name, doc, false)?;
            let update_doc = if override_existing {
                Some(sanitize_import_document(collection_name, doc, true)?)
            } else {
                None
            };
            Ok(PreparedImportDocument {
                unique_value,
                add_doc,
                update_doc,
            })
        })
        .collect()
}

fn uses_custom_apply_writer(collection_name: &str) -> bool {
    matches!(collection_name, "Task" | "Schedule" | "EventTrigger")
}

async fn apply_generic_import_collection_batched(
    txn: &ConfigApplyTxn<'_>,
    collection_name: &str,
    unique_field: &str,
    docs: &[PreparedImportDocument],
    override_existing: bool,
) -> Result<()> {
    let fields = docs
        .iter()
        .enumerate()
        .map(|(index, doc)| {
            generic_import_mutation_field(
                index,
                collection_name,
                unique_field,
                doc,
                override_existing,
            )
        })
        .collect::<Result<Vec<_>>>()?;

    match execute_aliased_mutation_batches(txn, collection_name, &fields).await {
        Ok(()) => Ok(()),
        Err(_) if override_existing => {
            for doc in docs {
                apply_generic_import_document(
                    txn,
                    collection_name,
                    unique_field,
                    doc,
                    override_existing,
                )
                .await?;
            }
            Ok(())
        }
        Err(error) => Err(anyhow::anyhow!(
            "importing {collection_name} batch failed: {error}\nNext:\n  1. If a document already exists, rerun with `defra-agent config import --override`\n  2. Or remove the existing document and retry"
        )),
    }
}

fn generic_import_mutation_field(
    index: usize,
    collection_name: &str,
    unique_field: &str,
    doc: &PreparedImportDocument,
    override_existing: bool,
) -> Result<AliasedMutationField> {
    let alias = format!("doc_{index}");
    let add_literal = graphql_input_literal(&doc.add_doc)?;
    let field = if override_existing {
        // This is the generic production bridge for apply retry convergence:
        // after a partial prefix, rerunning `config apply` recomputes live diff
        // and uses unique-field upsert so already-written rows are updated with
        // the same desired payload while missing rows are created.
        let update_literal =
            graphql_input_literal(doc.update_doc.as_ref().ok_or_else(|| {
                anyhow::anyhow!("missing update document for {collection_name}")
            })?)?;
        format!(
            r#"{alias}: upsert_{collection_name}(
                filter: {{ {unique_field}: {{ _eq: "{unique_value}" }} }},
                add: {add_literal},
                update: {update_literal}
            ) {{ _docID }}"#,
            unique_value = escape_graphql_string(&doc.unique_value),
        )
    } else {
        format!(r#"{alias}: create_{collection_name}(input: {add_literal}) {{ _docID }}"#)
    };
    Ok(AliasedMutationField { alias, field })
}

fn delete_mutation_field(
    index: usize,
    collection_name: &str,
    unique_field: &str,
    unique_value: &str,
) -> AliasedMutationField {
    let alias = format!("doc_{index}");
    let field = format!(
        r#"{alias}: delete_{collection_name}(
            filter: {{ {unique_field}: {{ _eq: "{unique_value}" }} }}
        ) {{ _docID }}"#,
        unique_value = escape_graphql_string(unique_value),
    );
    AliasedMutationField { alias, field }
}

async fn apply_generic_import_document(
    txn: &ConfigApplyTxn<'_>,
    collection_name: &str,
    unique_field: &str,
    doc: &PreparedImportDocument,
    override_existing: bool,
) -> Result<()> {
    let add_literal = graphql_input_literal(&doc.add_doc)?;
    let mutation = if override_existing {
        // Per-document fallback preserves the same retry property as the batch
        // path: the unique-field filter makes repeated successful writes land
        // on the same logical document.
        let update_literal =
            graphql_input_literal(doc.update_doc.as_ref().ok_or_else(|| {
                anyhow::anyhow!("missing update document for {collection_name}")
            })?)?;
        format!(
            r#"mutation {{
                upsert_{collection_name}(
                    filter: {{ {unique_field}: {{ _eq: "{unique_value}" }} }},
                    add: {add_literal},
                    update: {update_literal}
                ) {{ _docID }}
            }}"#,
            unique_value = escape_graphql_string(&doc.unique_value),
        )
    } else {
        format!(r#"mutation {{ create_{collection_name}(input: {add_literal}) {{ _docID }} }}"#)
    };
    let response = txn.execute(&mutation).await.map_err(|error| {
        if override_existing {
            anyhow::anyhow!(
                "importing {collection_name} {} failed: {error}",
                doc.unique_value
            )
        } else {
            anyhow::anyhow!(
                "importing {collection_name} {} failed: {error}\nNext:\n  1. If the document already exists, rerun with `defra-agent config import --override`\n  2. Or remove the existing document and retry",
                doc.unique_value
            )
        }
    })?;
    let _ = extract_mutation_doc_id(&response, collection_name)?;
    Ok(())
}

async fn apply_custom_override_collection_batched(
    txn: &ConfigApplyTxn<'_>,
    collection_name: &str,
    unique_field: &str,
    docs: &[PreparedImportDocument],
) -> Result<()> {
    if has_duplicate_unique_values(docs) {
        return apply_custom_override_documents_individually(txn, collection_name, docs).await;
    }

    let existing_by_unique =
        match query_existing_documents_by_unique_values(txn, collection_name, unique_field, docs)
            .await
        {
            Ok(existing_by_unique) => existing_by_unique,
            Err(_) => {
                return apply_custom_override_documents_individually(txn, collection_name, docs)
                    .await;
            }
        };
    let fields = docs
        .iter()
        .enumerate()
        .map(|(index, doc)| {
            custom_override_mutation_field(
                index,
                collection_name,
                unique_field,
                doc,
                existing_by_unique
                    .get(&doc.unique_value)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
        })
        .collect::<Result<Vec<_>>>()?;

    match execute_aliased_mutation_batches(txn, collection_name, &fields).await {
        Ok(()) => Ok(()),
        Err(_) => apply_custom_override_documents_individually(txn, collection_name, docs).await,
    }
}

fn has_duplicate_unique_values(docs: &[PreparedImportDocument]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    docs.iter()
        .any(|doc| !seen.insert(doc.unique_value.as_str()))
}

async fn query_existing_documents_by_unique_values(
    txn: &ConfigApplyTxn<'_>,
    collection_name: &str,
    unique_field: &str,
    docs: &[PreparedImportDocument],
) -> Result<BTreeMap<String, Vec<ExistingDocumentRef>>> {
    let unique_values = docs
        .iter()
        .map(|doc| doc.unique_value.clone())
        .collect::<Vec<_>>();
    query_document_refs_by_unique_values(txn, collection_name, unique_field, &unique_values, true)
        .await
}

async fn query_document_refs_by_unique_values(
    txn: &ConfigApplyTxn<'_>,
    collection_name: &str,
    unique_field: &str,
    unique_values: &[String],
    show_deleted: bool,
) -> Result<BTreeMap<String, Vec<ExistingDocumentRef>>> {
    if unique_values.is_empty() {
        return Ok(BTreeMap::new());
    }

    let show_deleted_arg = if show_deleted {
        "showDeleted: true,"
    } else {
        ""
    };
    let unique_values_literal = graphql_string_list_literal(unique_values);
    let limit = unique_values.len().saturating_mul(16).max(16);
    let query = format!(
        r#"{{
            {collection_name}(
                {show_deleted_arg}
                filter: {{ {unique_field}: {{ _in: {unique_values_literal} }} }},
                limit: {limit}
            ) {{
                _docID
                _deleted
                {unique_field}
            }}
        }}"#,
    );
    let response = txn.execute(&query).await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get(collection_name))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut by_unique: BTreeMap<String, Vec<ExistingDocumentRef>> = BTreeMap::new();
    for row in rows {
        let unique_value = row
            .get(unique_field)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("{collection_name} lookup row missing {unique_field}: {row}")
            })?
            .to_string();
        let doc_ref = ExistingDocumentRef {
            doc_id: row
                .get("_docID")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{collection_name} lookup row missing _docID for {unique_field}={unique_value}: {row}"
                    )
                })?
                .to_string(),
            deleted: row
                .get("_deleted")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };
        by_unique.entry(unique_value).or_default().push(doc_ref);
    }

    Ok(by_unique)
}

fn custom_override_mutation_field(
    index: usize,
    collection_name: &str,
    unique_field: &str,
    doc: &PreparedImportDocument,
    existing_rows: &[ExistingDocumentRef],
) -> Result<AliasedMutationField> {
    let alias = format!("doc_{index}");
    let existing = select_existing_import_document(
        collection_name,
        unique_field,
        &doc.unique_value,
        existing_rows,
    )?;
    let field = if existing.as_ref().is_some_and(|existing| !existing.deleted) {
        let update_literal =
            graphql_input_literal(doc.update_doc.as_ref().ok_or_else(|| {
                anyhow::anyhow!("missing update document for {collection_name}")
            })?)?;
        let doc_id = existing
            .as_ref()
            .expect("existing checked above")
            .doc_id
            .as_str();
        format!(
            r#"{alias}: update_{collection_name}(docID: "{doc_id}", input: {update_literal}) {{ _docID }}"#,
            doc_id = escape_graphql_string(doc_id),
        )
    } else {
        // A tombstone occupies this unique value: identical manifest content
        // would regenerate the tombstoned docID and never produce a live row
        // (#700) — mint a fresh identity for the new incarnation.
        let add_doc = if existing.is_some() {
            crate::config_writes::mint_recreate_identity(&doc.add_doc)
        } else {
            doc.add_doc.clone()
        };
        let add_literal = graphql_input_literal(&add_doc)?;
        format!(r#"{alias}: create_{collection_name}(input: {add_literal}) {{ _docID }}"#)
    };

    Ok(AliasedMutationField { alias, field })
}

fn select_existing_import_document(
    collection_name: &str,
    unique_field: &str,
    unique_value: &str,
    rows: &[ExistingDocumentRef],
) -> Result<Option<ExistingDocumentRef>> {
    let live_rows = rows.iter().filter(|row| !row.deleted).collect::<Vec<_>>();
    if live_rows.len() > 1 {
        anyhow::bail!(
            "multiple live {collection_name} documents share {unique_field}={unique_value}"
        );
    }
    if let Some(row) = live_rows.first() {
        return Ok(Some((*row).clone()));
    }

    let deleted_rows = rows.iter().filter(|row| row.deleted).collect::<Vec<_>>();
    if deleted_rows.len() > 1 {
        anyhow::bail!(
            "multiple deleted {collection_name} tombstones share {unique_field}={unique_value}"
        );
    }

    Ok(deleted_rows.first().map(|row| (*row).clone()))
}

async fn apply_custom_override_documents_individually(
    txn: &ConfigApplyTxn<'_>,
    collection_name: &str,
    docs: &[PreparedImportDocument],
) -> Result<()> {
    for doc in docs {
        let update_doc = doc
            .update_doc
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing update document for {collection_name}"))?;
        let doc_id = match collection_name {
            "Task" => write_task_document(txn, &doc.unique_value, &doc.add_doc, update_doc).await,
            "Schedule" => {
                write_schedule_document(txn, &doc.unique_value, &doc.add_doc, update_doc).await
            }
            "EventTrigger" => {
                write_event_trigger_document(txn, &doc.unique_value, &doc.add_doc, update_doc).await
            }
            _ => unreachable!("custom apply writer only supports selected collections"),
        }
        .map_err(|error| {
            anyhow::anyhow!(
                "importing {collection_name} {} failed: {error}",
                doc.unique_value
            )
        })?;
        if doc_id.trim().is_empty() {
            anyhow::bail!(
                "importing {collection_name} {} returned an empty _docID",
                doc.unique_value
            );
        }
    }

    Ok(())
}

async fn execute_aliased_mutation_batches(
    txn: &ConfigApplyTxn<'_>,
    collection_name: &str,
    fields: &[AliasedMutationField],
) -> Result<()> {
    for chunk in fields.chunks(CONFIG_IMPORT_BATCH_SIZE) {
        let mutation = build_aliased_mutation(chunk);
        let response = txn.execute(&mutation).await?;
        for field in chunk {
            let _ = extract_aliased_mutation_doc_id(&response, &field.alias, collection_name)?;
        }
    }

    Ok(())
}

fn build_aliased_mutation(fields: &[AliasedMutationField]) -> String {
    let body = fields
        .iter()
        .map(|field| field.field.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    format!("mutation {{\n{body}\n}}")
}

fn extract_aliased_mutation_doc_id(
    response: &Value,
    alias: &str,
    collection_name: &str,
) -> Result<String> {
    let data = response
        .get("data")
        .ok_or_else(|| anyhow::anyhow!("graphql response missing data: {response}"))?;
    if let Some(doc_id) = data
        .get(alias)
        .and_then(|value| value.get("_docID"))
        .and_then(Value::as_str)
    {
        return Ok(doc_id.to_string());
    }
    if let Some(doc_id) = data
        .get(alias)
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
    {
        return Ok(doc_id.to_string());
    }
    anyhow::bail!(
        "graphql mutation alias {alias} returned no _docID for {collection_name}: {response}"
    );
}

pub(crate) fn diff_has_pending_apply(
    counts: &desired_state::DesiredStateDiffCollectionsCounts,
) -> bool {
    counts.has_pending_apply()
}

pub(crate) fn config_apply_counts_changed(counts: &ConfigApplyCounts) -> bool {
    counts.changed()
}

pub(crate) fn select_apply_principal_docs(
    doc: Option<&Value>,
    diff: &desired_state::DesiredStateCollectionDiff,
) -> Result<Vec<Value>> {
    if diff.create.is_empty() && diff.update.is_empty() {
        return Ok(Vec::new());
    }
    let doc =
        doc.ok_or_else(|| anyhow::anyhow!("desired-state apply is missing AgentPrincipal"))?;
    Ok(vec![doc.clone()])
}

pub(crate) async fn apply_desired_state_changes(
    txn: &ConfigApplyTxn<'_>,
    desired_bundle: &DesiredApplyBundle,
    planned: &desired_state::DesiredStateDiffReport,
) -> Result<ConfigApplyCounts> {
    let desired_bundle = desired_bundle.as_bundle();
    let mut counts = ConfigApplyCounts::default();

    let per_collection_sleep = std::env::var("DEFRA_AGENT_CONFIG_APPLY_SLEEP_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(std::time::Duration::from_millis);

    for collection in CONFIG_APPLY_ORDER {
        let docs = select_apply_docs_for_collection(desired_bundle, planned, collection)?;
        let applied = apply_import_collection(
            txn,
            collection.graphql_type(),
            collection.unique_field(),
            &docs,
            true,
        )
        .await?;
        counts.set(collection, applied);

        if let Some(sleep) = per_collection_sleep {
            tokio::time::sleep(sleep).await;
        }
    }

    for collection in CONFIG_PRUNE_ORDER {
        let diff = planned.collections.get(collection);
        let deleted = if collection == Collection::PeerPairingDesired {
            apply_delete_manifest_pairings(txn, &diff.delete, &planned.agent_did).await?
        } else {
            apply_delete_collection(
                txn,
                collection.graphql_type(),
                collection.unique_field(),
                &diff.delete,
            )
            .await?
        };
        counts.add(collection, deleted);

        if let Some(sleep) = per_collection_sleep {
            tokio::time::sleep(sleep).await;
        }
    }

    Ok(counts)
}

fn select_apply_docs_for_collection(
    desired_bundle: &ConfigExportBundle,
    planned: &desired_state::DesiredStateDiffReport,
    collection: Collection,
) -> Result<Vec<Value>> {
    let diff = planned.collections.get(collection);
    if collection == Collection::AgentPrincipal {
        return select_apply_principal_docs(desired_bundle.agent_principal.as_ref(), diff);
    }

    let docs = desired_bundle
        .docs_for_collection(collection)
        .expect("non-principal desired-state collection has document slice");
    select_apply_collection_docs(
        docs,
        collection.unique_field(),
        collection.graphql_type(),
        diff,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    #[test]
    fn config_apply_order_contains_each_collection_once() {
        let actual = CONFIG_APPLY_ORDER_FOR_TESTS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let expected = Collection::ALL.into_iter().collect::<BTreeSet<_>>();

        assert_eq!(actual, expected);
        assert_eq!(CONFIG_APPLY_ORDER_FOR_TESTS.len(), Collection::ALL.len());
    }

    #[test]
    fn config_prune_order_contains_each_collection_once() {
        let actual = CONFIG_PRUNE_ORDER_FOR_TESTS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let expected = Collection::ALL.into_iter().collect::<BTreeSet<_>>();

        assert_eq!(actual, expected);
        assert_eq!(CONFIG_PRUNE_ORDER_FOR_TESTS.len(), Collection::ALL.len());
    }

    #[test]
    fn config_apply_order_has_retry_safe_prefixes() {
        for prefix_len in 0..=CONFIG_APPLY_ORDER_FOR_TESTS.len() {
            let prefix = &CONFIG_APPLY_ORDER_FOR_TESTS[..prefix_len];
            let suffix = &CONFIG_APPLY_ORDER_FOR_TESTS[prefix_len..];
            for written in prefix {
                for pending in suffix {
                    assert!(
                        written.apply_order() <= pending.apply_order(),
                        "prefix {prefix_len} writes {:?} before lower-rank {:?}",
                        written,
                        pending,
                    );
                }
            }
        }
    }

    #[test]
    fn config_prune_order_deletes_referrers_before_dependencies() {
        for prefix_len in 0..=CONFIG_PRUNE_ORDER_FOR_TESTS.len() {
            let prefix = &CONFIG_PRUNE_ORDER_FOR_TESTS[..prefix_len];
            let suffix = &CONFIG_PRUNE_ORDER_FOR_TESTS[prefix_len..];
            for deleted in prefix {
                for pending in suffix {
                    assert!(
                        pending.apply_order() <= deleted.apply_order(),
                        "prefix {prefix_len} deletes lower-rank {:?} before higher-rank {:?}",
                        deleted,
                        pending,
                    );
                }
            }
        }
    }

    #[test]
    fn build_aliased_mutation_wraps_all_fields() {
        let fields = vec![
            AliasedMutationField {
                alias: "doc_0".to_string(),
                field: r#"doc_0: create_Task(input: { task_id: "a" }) { _docID }"#.to_string(),
            },
            AliasedMutationField {
                alias: "doc_1".to_string(),
                field: r#"doc_1: create_Task(input: { task_id: "b" }) { _docID }"#.to_string(),
            },
        ];

        assert_eq!(
            build_aliased_mutation(&fields),
            r#"mutation {
doc_0: create_Task(input: { task_id: "a" }) { _docID }
doc_1: create_Task(input: { task_id: "b" }) { _docID }
}"#
        );
    }

    #[test]
    fn delete_mutation_field_escapes_unique_value() {
        let field = delete_mutation_field(7, "Task", "task_id", r#"task"with\chars"#);

        assert_eq!(field.alias, "doc_7");
        assert_eq!(
            field.field,
            r#"doc_7: delete_Task(
            filter: { task_id: { _eq: "task\"with\\chars" } }
        ) { _docID }"#
        );
    }

    #[test]
    fn manifest_pairing_delete_is_owner_scoped_and_escaped() {
        let field = manifest_pairing_delete_mutation_field(
            3,
            r#"peer"with\chars"#,
            r#"manifest:did:key:owner"with\chars"#,
        );

        assert_eq!(field.alias, "doc_3");
        assert!(field
            .field
            .contains(r#"peer_id: { _eq: "peer\"with\\chars" }"#));
        assert!(field
            .field
            .contains(r#"source: { _eq: "manifest:did:key:owner\"with\\chars" }"#));
    }

    #[test]
    fn extract_aliased_mutation_doc_id_accepts_object_and_array_shapes() {
        let response = json!({
            "data": {
                "doc_0": { "_docID": "doc-a" },
                "doc_1": [{ "_docID": "doc-b" }]
            }
        });

        assert_eq!(
            extract_aliased_mutation_doc_id(&response, "doc_0", "Task").unwrap(),
            "doc-a"
        );
        assert_eq!(
            extract_aliased_mutation_doc_id(&response, "doc_1", "Task").unwrap(),
            "doc-b"
        );
    }

    #[test]
    fn delete_mutation_field_targets_unique_field() {
        let field = delete_mutation_field(0, "Task", "task_id", "task-a");

        assert_eq!(field.alias, "doc_0");
        assert!(
            field.field.contains(r#"doc_0: delete_Task("#),
            "expected delete field, got {}",
            field.field
        );
        assert!(field.field.contains(r#"task_id: { _eq: "task-a" }"#));
    }

    #[test]
    fn has_duplicate_unique_values_detects_repeated_import_ids() {
        let docs = vec![
            PreparedImportDocument {
                unique_value: "task-a".to_string(),
                add_doc: json!({ "task_id": "task-a" }),
                update_doc: Some(json!({ "task_id": "task-a" })),
            },
            PreparedImportDocument {
                unique_value: "task-b".to_string(),
                add_doc: json!({ "task_id": "task-b" }),
                update_doc: Some(json!({ "task_id": "task-b" })),
            },
            PreparedImportDocument {
                unique_value: "task-a".to_string(),
                add_doc: json!({ "task_id": "task-a", "enabled": true }),
                update_doc: Some(json!({ "enabled": true })),
            },
        ];

        assert!(has_duplicate_unique_values(&docs));
    }

    #[test]
    fn custom_override_mutation_field_updates_live_doc_by_doc_id() {
        let doc = PreparedImportDocument {
            unique_value: "task-a".to_string(),
            add_doc: json!({ "task_id": "task-a", "enabled": true }),
            update_doc: Some(json!({ "enabled": false })),
        };
        let existing = vec![ExistingDocumentRef {
            doc_id: "doc-live".to_string(),
            deleted: false,
        }];

        let field = custom_override_mutation_field(0, "Task", "task_id", &doc, &existing).unwrap();

        assert_eq!(field.alias, "doc_0");
        assert!(
            field
                .field
                .contains(r#"doc_0: update_Task(docID: "doc-live""#),
            "expected update field, got {}",
            field.field
        );
        assert!(field.field.contains("enabled: false"));
    }

    #[test]
    fn custom_override_mutation_field_recreates_deleted_doc() {
        let doc = PreparedImportDocument {
            unique_value: "task-a".to_string(),
            add_doc: json!({ "task_id": "task-a", "enabled": true }),
            update_doc: Some(json!({ "enabled": false })),
        };
        let existing = vec![ExistingDocumentRef {
            doc_id: "doc-deleted".to_string(),
            deleted: true,
        }];

        let field = custom_override_mutation_field(0, "Task", "task_id", &doc, &existing).unwrap();

        assert_eq!(field.alias, "doc_0");
        assert!(
            field.field.contains("doc_0: create_Task(input:"),
            "expected create field, got {}",
            field.field
        );
    }
}

#[cfg(test)]
mod lean_apply_write_boundary_tests {
    use super::lean_vocab_test::{
        lean_apply_reconcile_cases, LeanApplyDesiredDoc, LeanApplyDocRef, LeanApplyLiveDoc,
        LeanApplyReconcileCase, LeanApplySelectedDoc,
    };
    use super::*;
    use axum::{extract::State, routing::post, Json, Router};
    use defra_agent::BackendProviderKind;
    use regex::Regex;
    use serde_json::{json, Map};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    const DEFAULT_AGENT_DID: &str = "did:example:agent";
    const DEFAULT_BEHAVIOR_ID: &str = "behavior-a";
    const DEFAULT_TASK_ID: &str = "task-a";

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ObservedWrite {
        kind: String,
        collection: Collection,
        unique_value: String,
    }

    #[derive(Clone, Default)]
    struct RecordingGraphqlState {
        queries: Arc<Mutex<Vec<String>>>,
        transactions: Arc<Mutex<BTreeMap<String, Vec<ObservedWrite>>>>,
        committed: Arc<Mutex<Vec<ObservedWrite>>>,
        next_tx_id: Arc<AtomicU64>,
        fail_injection: Arc<Mutex<Option<FailInjection>>>,
        tx_begin_count: Arc<AtomicU64>,
        tx_commit_count: Arc<AtomicU64>,
        tx_discard_count: Arc<AtomicU64>,
    }

    #[derive(Debug, Clone)]
    struct FailInjection {
        tx_id: String,
        write_index: usize,
    }

    impl RecordingGraphqlState {
        fn committed_state(&self) -> Vec<ObservedWrite> {
            self.committed.lock().expect("committed lock").clone()
        }

        /// Returns committed writes plus any still-pending in-tx writes (across all
        /// open transactions). Useful for backward-compat assertions against
        /// "every write attempted." **Use `committed_state()` to verify durability**
        /// — e.g., to confirm a discard left no externally-observable state.
        fn observed_writes(&self) -> Vec<ObservedWrite> {
            let mut all = self.committed.lock().expect("committed lock").clone();
            let txs = self.transactions.lock().expect("tx lock").clone();
            for (_id, writes) in txs.iter() {
                all.extend(writes.iter().cloned());
            }
            all
        }

        /// Returns in-flight writes across open transactions only. This excludes
        /// committed state, so failure-path caps can measure per-tx writes after
        /// an injected error and before discard removes the transaction. Callers
        /// should use this when at most one transaction is open, or when an
        /// aggregate across all open transactions is explicitly intended.
        fn pending_state(&self) -> Vec<ObservedWrite> {
            self.transactions
                .lock()
                .expect("tx lock")
                .values()
                .flat_map(|writes| writes.iter().cloned())
                .collect()
        }

        fn tx_lifecycle_counts(&self) -> (u64, u64, u64) {
            (
                self.tx_begin_count.load(Ordering::SeqCst),
                self.tx_commit_count.load(Ordering::SeqCst),
                self.tx_discard_count.load(Ordering::SeqCst),
            )
        }

        fn install_fail_at(&self, tx_id: impl Into<String>, write_index: usize) {
            *self.fail_injection.lock().expect("fail lock") = Some(FailInjection {
                tx_id: tx_id.into(),
                write_index,
            });
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn generated_apply_reconcile_cases_fence_production_apply_write_boundary() {
        let cases = lean_apply_reconcile_cases();
        assert!(
            cases
                .iter()
                .any(|case| case.name == "production_write_boundary_all_collections"),
            "Lean must emit a production write-boundary case covering every collection"
        );

        for case in cases {
            assert_write_order_projection_matches_production(case);
            assert_prune_order_projection_matches_production(case);
            assert!(case.write_order_prefix_safe);
            assert!(case.prune_order_referrers_before_dependencies);
            assert!(case.delete_safety_holds);
            assert!(case.production_prefixes_referrers_closed);

            let desired_manifest = desired_manifest_from_lean(case);
            let desired_bundle =
                desired_state::export_bundle_from_manifest(&desired_manifest, "graphql")
                    .unwrap_or_else(|error| {
                        panic!(
                            "failed to build desired apply bundle for Lean case {}: {error}",
                            case.name
                        )
                    });
            let planned = diff_report_from_lean(case);
            assert_selected_documents_match_lean(case, desired_bundle.as_bundle(), &planned);

            let (graphql, recorder) = start_recording_graphql().await;
            let access = ConfigAccess::Graphql(graphql);
            let txn = access.begin_apply_txn().await.expect("begin apply tx");
            let counts = match apply_desired_state_changes(&txn, &desired_bundle, &planned).await {
                Ok(counts) => {
                    txn.commit().await.expect("commit");
                    counts
                }
                Err(error) => {
                    let _ = txn.discard().await;
                    panic!(
                        "production apply_desired_state_changes failed for Lean case {}: {error}",
                        case.name
                    );
                }
            };

            assert_counts_match_lean(case, &counts);

            let observed = recorder.committed_state();
            let mut expected = case
                .expected_selected_writes
                .iter()
                .map(observed_write_from_lean)
                .collect::<Vec<_>>();
            expected.extend(
                case.expected_selected_delete_docs
                    .iter()
                    .map(observed_write_from_lean),
            );
            assert_eq!(
                observed, expected,
                "production mutation sequence drifted from Lean write-boundary projection for case {}",
                case.name
            );
            assert_observed_prefixes_are_referrer_closed(case, &observed);
            assert_live_payloads_not_written(case, &recorder);

            let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
            assert_eq!(
                (begin_count, commit_count, discard_count),
                (1, 1, 0),
                "success path must drive exactly one begin/commit and zero discard for Lean case {}",
                case.name,
            );

            if case.prefix_len > 0 && case.prefix_len < expected.len() {
                let initial_external_state = case
                    .pre_live
                    .iter()
                    .map(observed_write_from_lean_live_doc)
                    .collect::<Vec<_>>();
                let (graphql, recorder) =
                    start_recording_graphql_with_committed_state(initial_external_state).await;
                let access = ConfigAccess::Graphql(graphql);

                // Begin a tx. The recorder hands out sequential numeric ids starting at 0;
                // the first tx in this fresh recorder is "0".
                let txn = access
                    .begin_apply_txn()
                    .await
                    .expect("begin failure-case tx");
                recorder.install_fail_at("0", case.prefix_len);

                let result = apply_desired_state_changes(&txn, &desired_bundle, &planned).await;
                assert!(
                    result.is_err(),
                    "injected failure at write {} must surface as Err for Lean case {}",
                    case.prefix_len,
                    case.name,
                );
                let pending_after_failure = recorder.pending_state();

                // discard errors are not under test here; the apply error path is what we're verifying.
                let _ = txn.discard().await;

                let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
                assert_eq!(
                    (begin_count, commit_count, discard_count),
                    (1, 0, 1),
                    "failure path must drive exactly one begin/discard and zero commit for Lean case {}",
                    case.name,
                );

                let observed = recorder.committed_state();
                // Today Lean emits `pre_live` here because apply has no live-write
                // constructor; #57 can make this projection diverge without
                // changing the Rust assertion shape. Lean's list order is the
                // canonical order for this exact doc-for-doc comparison.
                let expected = case
                    .expected_external_state_after_abort
                    .iter()
                    .map(observed_write_from_lean_live_doc)
                    .collect::<Vec<_>>();
                assert_eq!(
                    observed, expected,
                    "failure path must leave externally-observed committed state equal to Lean expected_external_state_after_abort for case {}",
                    case.name,
                );

                assert!(
                    pending_after_failure.len() <= case.prefix_len,
                    "failure path observed {} writes; the batch containing the failing write must be rejected and writes after it must not happen — cap is prefix_len = {} for Lean case {}",
                    pending_after_failure.len(),
                    case.prefix_len,
                    case.name,
                );
            }
        }
    }

    fn assert_write_order_projection_matches_production(case: &LeanApplyReconcileCase) {
        let expected_order = case
            .expected_write_order
            .iter()
            .map(|entry| {
                let collection = collection_from_lean_name(&entry.collection);
                assert_eq!(
                    entry.graphql_type,
                    collection.graphql_type(),
                    "Lean GraphQL type mapping drifted for {:?}",
                    collection
                );
                assert_eq!(
                    entry.unique_field,
                    collection.unique_field(),
                    "Lean unique-field mapping drifted for {:?}",
                    collection
                );
                assert_eq!(
                    entry.apply_order,
                    collection.apply_order() as usize,
                    "Lean apply-order mapping drifted for {:?}",
                    collection
                );
                collection
            })
            .collect::<Vec<_>>();

        assert_eq!(
            expected_order, CONFIG_APPLY_ORDER,
            "CONFIG_APPLY_ORDER must match Lean's production write order projection for case {}",
            case.name
        );
    }

    fn assert_prune_order_projection_matches_production(case: &LeanApplyReconcileCase) {
        let expected_order = case
            .expected_prune_order
            .iter()
            .map(|entry| {
                let collection = collection_from_lean_name(&entry.collection);
                assert_eq!(
                    entry.graphql_type,
                    collection.graphql_type(),
                    "Lean prune GraphQL type mapping drifted for {:?}",
                    collection
                );
                assert_eq!(
                    entry.unique_field,
                    collection.unique_field(),
                    "Lean prune unique-field mapping drifted for {:?}",
                    collection
                );
                assert_eq!(
                    entry.apply_order,
                    collection.apply_order() as usize,
                    "Lean prune apply-order mapping drifted for {:?}",
                    collection
                );
                collection
            })
            .collect::<Vec<_>>();

        assert_eq!(
            expected_order, CONFIG_PRUNE_ORDER,
            "CONFIG_PRUNE_ORDER must match Lean's production prune order projection for case {}",
            case.name
        );
    }

    fn assert_selected_documents_match_lean(
        case: &LeanApplyReconcileCase,
        desired_bundle: &ConfigExportBundle,
        planned: &desired_state::DesiredStateDiffReport,
    ) {
        let create_docs = case
            .expected_selected_create_docs
            .iter()
            .map(observed_write_from_lean)
            .collect::<Vec<_>>();
        let update_docs = case
            .expected_selected_update_docs
            .iter()
            .map(observed_write_from_lean)
            .collect::<Vec<_>>();
        let delete_docs = case
            .expected_selected_delete_docs
            .iter()
            .map(observed_write_from_lean)
            .collect::<Vec<_>>();

        for collection in Collection::ALL {
            let diff = planned.collections.get(collection);
            assert_eq!(
                diff.create,
                ids_for_collection(&create_docs, collection),
                "planned create ids must match Lean selected-create docs for case {} / {:?}",
                case.name,
                collection
            );
            assert_eq!(
                diff.update,
                ids_for_collection(&update_docs, collection),
                "planned update ids must match Lean selected-update docs for case {} / {:?}",
                case.name,
                collection
            );
            assert_eq!(
                diff.delete,
                ids_for_collection(&delete_docs, collection),
                "planned delete ids must match Lean selected-delete docs for case {} / {:?}",
                case.name,
                collection
            );

            let selected = select_apply_docs_for_collection(desired_bundle, planned, collection)
                .unwrap_or_else(|error| {
                    panic!(
                        "production selection failed for Lean case {} / {:?}: {error}",
                        case.name, collection
                    )
                });
            let actual_ids = selected
                .iter()
                .map(|doc| unique_value_from_doc(doc, collection))
                .collect::<Vec<_>>();
            let expected = case
                .expected_selected_writes
                .iter()
                .filter(|doc| collection_from_lean_ref(&doc.target) == collection)
                .collect::<Vec<_>>();
            let expected_ids = expected
                .iter()
                .map(|doc| doc.unique_value.clone())
                .collect::<Vec<_>>();

            assert_eq!(
                actual_ids, expected_ids,
                "selected production docs must match Lean selected writes for case {} / {:?}",
                case.name, collection
            );
            assert_no_live_only_doc_selected(case, collection, &actual_ids);
            assert_selected_docs_carry_lean_content(case, collection, &selected, &expected);
            assert_selected_docs_keep_live_fields_out(case, collection, &selected);
        }
    }

    fn assert_selected_docs_carry_lean_content(
        case: &LeanApplyReconcileCase,
        collection: Collection,
        selected: &[Value],
        expected: &[&LeanApplySelectedDoc],
    ) {
        let by_id = selected
            .iter()
            .map(|doc| (unique_value_from_doc(doc, collection), doc))
            .collect::<BTreeMap<_, _>>();
        for expected_doc in expected {
            let selected_doc = by_id.get(&expected_doc.unique_value).unwrap_or_else(|| {
                panic!(
                    "selected production docs missing {} for Lean case {} / {:?}",
                    expected_doc.unique_value, case.name, collection
                )
            });
            let encoded = serde_json::to_string(selected_doc).expect("selected doc JSON");
            assert!(
                encoded.contains(&expected_doc.content),
                "selected production doc for Lean case {} / {:?} / {} did not carry Lean content {:?}: {}",
                case.name,
                collection,
                expected_doc.unique_value,
                expected_doc.content,
                encoded
            );
        }
    }

    fn assert_selected_docs_keep_live_fields_out(
        case: &LeanApplyReconcileCase,
        collection: Collection,
        selected: &[Value],
    ) {
        let prepared = prepare_import_documents(
            collection.graphql_type(),
            collection.unique_field(),
            selected,
            true,
        )
        .unwrap_or_else(|error| {
            panic!(
                "failed to prepare selected docs for Lean case {} / {:?}: {error}",
                case.name, collection
            )
        });
        for doc in prepared {
            for field in runtime_owned_fields(collection) {
                assert!(
                    doc.add_doc.get(field).is_none(),
                    "add document for Lean case {} / {:?} / {} contains live-owned field {field}",
                    case.name,
                    collection,
                    doc.unique_value
                );
                let update_doc = doc.update_doc.as_ref().expect("override update doc");
                assert!(
                    update_doc.get(field).is_none(),
                    "update document for Lean case {} / {:?} / {} contains live-owned field {field}",
                    case.name,
                    collection,
                    doc.unique_value
                );
            }
        }
    }

    fn assert_no_live_only_doc_selected(
        case: &LeanApplyReconcileCase,
        collection: Collection,
        actual_ids: &[String],
    ) {
        let actual_ids = actual_ids.iter().collect::<BTreeSet<_>>();
        for live_only in &case.expected_live_only {
            if collection_from_lean_ref(live_only) == collection {
                assert!(
                    !actual_ids.contains(&live_only.id),
                    "live-only doc was selected for production write in Lean case {} / {:?}: {}",
                    case.name,
                    collection,
                    live_only.id
                );
            }
        }
    }

    fn assert_counts_match_lean(case: &LeanApplyReconcileCase, counts: &ConfigApplyCounts) {
        for collection in Collection::ALL {
            let expected_writes = case
                .expected_selected_writes
                .iter()
                .filter(|doc| collection_from_lean_ref(&doc.target) == collection)
                .count();
            let expected_deletes = case
                .expected_selected_delete_docs
                .iter()
                .filter(|doc| collection_from_lean_ref(&doc.target) == collection)
                .count();
            assert_eq!(
                count_for_collection(counts, collection),
                expected_writes + expected_deletes,
                "apply_desired_state_changes count mismatch for Lean case {} / {:?}",
                case.name,
                collection
            );
        }
    }

    fn assert_observed_prefixes_are_referrer_closed(
        case: &LeanApplyReconcileCase,
        observed: &[ObservedWrite],
    ) {
        let refs_by_key = case
            .pre_desired
            .iter()
            .chain(case.manifest.iter())
            .map(|doc| {
                (
                    doc_key_from_desired(doc),
                    doc.refs.iter().map(doc_key).collect(),
                )
            })
            .collect::<BTreeMap<_, Vec<_>>>();
        let mut present = case
            .pre_desired
            .iter()
            .map(doc_key_from_desired)
            .collect::<BTreeSet<_>>();

        assert_present_refs_closed(case, 0, &present, &refs_by_key);

        for (index, mutation) in observed.iter().enumerate() {
            let prefix_len = index + 1;
            let key = (mutation.collection, mutation.unique_value.clone());
            match mutation.kind.as_str() {
                "write" => {
                    present.insert(key.clone());
                }
                "delete" => {
                    for referrer in &present {
                        let refs = refs_by_key.get(referrer).cloned().unwrap_or_default();
                        assert!(
                            !refs.contains(&key),
                            "production delete prefix {prefix_len} deletes {:?} while live referrer {:?} still references it in Lean case {}",
                            key,
                            referrer,
                            case.name
                        );
                    }
                    present.remove(&key);
                }
                "live" => {}
                other => panic!("unknown observed mutation kind {other:?}"),
            }

            assert_present_refs_closed(case, prefix_len, &present, &refs_by_key);
        }
    }

    fn assert_present_refs_closed(
        case: &LeanApplyReconcileCase,
        prefix_len: usize,
        present: &BTreeSet<(Collection, String)>,
        refs_by_key: &BTreeMap<(Collection, String), Vec<(Collection, String)>>,
    ) {
        for key in present {
            let refs = refs_by_key.get(key).cloned().unwrap_or_default();
            for reference in refs {
                assert!(
                    present.contains(&reference),
                    "production prefix {prefix_len} leaves referrer {:?} dangling on {:?} in Lean case {}",
                    key,
                    reference,
                    case.name
                );
            }
        }
    }

    fn assert_live_payloads_not_written(
        case: &LeanApplyReconcileCase,
        recorder: &RecordingGraphqlState,
    ) {
        let queries = recorder.queries.lock().expect("queries lock").join("\n");
        for live in &case.pre_live {
            assert!(
                !queries.contains(&live.content),
                "production write boundary leaked live payload {:?} into GraphQL for Lean case {}",
                live.content,
                case.name
            );
        }
    }

    async fn start_recording_graphql() -> (String, RecordingGraphqlState) {
        start_recording_graphql_with_committed_state(Vec::new()).await
    }

    async fn start_recording_graphql_with_committed_state(
        committed: Vec<ObservedWrite>,
    ) -> (String, RecordingGraphqlState) {
        let state = RecordingGraphqlState::default();
        *state.committed.lock().expect("committed lock") = committed;
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind recording GraphQL listener");
        let addr = listener.local_addr().expect("recording GraphQL addr");
        let app = Router::new()
            .route("/api/v0/graphql", post(recording_graphql_handler))
            // Go-compatible transaction routes (mirrors DefraDB HTTP API):
            //   POST /api/v0/tx          → begin
            //   POST /api/v0/tx/{id}     → commit
            //   DELETE /api/v0/tx/{id}   → discard/rollback
            .route("/api/v0/tx", post(recording_tx_begin_handler))
            .route(
                "/api/v0/tx/{id}",
                post(recording_tx_commit_handler).delete(recording_tx_discard_handler),
            )
            .with_state(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("recording GraphQL server");
        });
        (format!("http://{addr}/api/v0/graphql"), state)
    }

    async fn recording_tx_begin_handler(State(state): State<RecordingGraphqlState>) -> Json<Value> {
        let id = state.next_tx_id.fetch_add(1, Ordering::SeqCst);
        state
            .transactions
            .lock()
            .expect("tx lock")
            .insert(id.to_string(), Vec::new());
        state.tx_begin_count.fetch_add(1, Ordering::SeqCst);
        Json(json!({ "id": id.to_string() }))
    }

    async fn recording_tx_commit_handler(
        State(state): State<RecordingGraphqlState>,
        axum::extract::Path(id): axum::extract::Path<String>,
    ) -> axum::http::StatusCode {
        let mut transactions = state.transactions.lock().expect("tx lock");
        let Some(writes) = transactions.remove(&id) else {
            return axum::http::StatusCode::NOT_FOUND;
        };
        drop(transactions);
        state
            .committed
            .lock()
            .expect("committed lock")
            .extend(writes);
        state.tx_commit_count.fetch_add(1, Ordering::SeqCst);
        axum::http::StatusCode::OK
    }

    async fn recording_tx_discard_handler(
        State(state): State<RecordingGraphqlState>,
        axum::extract::Path(id): axum::extract::Path<String>,
    ) -> axum::http::StatusCode {
        let removed = state
            .transactions
            .lock()
            .expect("tx lock")
            .remove(&id)
            .is_some();
        if removed {
            state.tx_discard_count.fetch_add(1, Ordering::SeqCst);
            axum::http::StatusCode::OK
        } else {
            axum::http::StatusCode::NOT_FOUND
        }
    }

    async fn recording_graphql_handler(
        State(state): State<RecordingGraphqlState>,
        headers: axum::http::HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        state
            .queries
            .lock()
            .expect("queries lock")
            .push(query.clone());

        let tx_id = headers
            .get("x-defradb-tx")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        if query.contains("mutation") {
            let writes = parse_mutation_writes(&query);

            if let Some(fail) = state.fail_injection.lock().expect("fail lock").clone() {
                if tx_id.as_deref() == Some(fail.tx_id.as_str()) {
                    let prior = state
                        .transactions
                        .lock()
                        .expect("tx lock")
                        .get(&fail.tx_id)
                        .map(|v| v.len())
                        .unwrap_or(0);
                    if (prior..prior + writes.len()).contains(&fail.write_index) {
                        return Json(json!({
                            "errors": [{ "message": "injected failure at recorder" }]
                        }));
                    }
                }
            }

            match tx_id {
                Some(id) => {
                    let mut transactions = state.transactions.lock().expect("tx lock");
                    let entry = transactions.entry(id).or_default();
                    entry.extend(writes);
                }
                None => {
                    state
                        .committed
                        .lock()
                        .expect("committed lock")
                        .extend(writes);
                }
            }
            Json(json!({ "data": aliased_mutation_response(&query) }))
        } else {
            Json(json!({ "data": empty_collection_query_response(&query) }))
        }
    }

    fn aliased_mutation_response(query: &str) -> Value {
        let alias_re = Regex::new(r"\b(doc_\d+)\s*:").expect("alias regex");
        let mut data = Map::new();
        for capture in alias_re.captures_iter(query) {
            let alias = capture[1].to_string();
            data.insert(alias.clone(), json!({ "_docID": format!("{alias}-id") }));
        }
        Value::Object(data)
    }

    fn empty_collection_query_response(query: &str) -> Value {
        let mut data = Map::new();
        for collection in Collection::ALL {
            if query.contains(&format!("{}(", collection.graphql_type())) {
                data.insert(
                    collection.graphql_type().to_string(),
                    Value::Array(Vec::new()),
                );
            }
        }
        Value::Object(data)
    }

    fn parse_mutation_writes(query: &str) -> Vec<ObservedWrite> {
        let field_re =
            Regex::new(r"(?:\bdoc_\d+\s*:\s*)?(create|update|upsert|delete)_([A-Za-z]+)\s*\(")
                .expect("mutation field regex");
        let matches = field_re
            .captures_iter(query)
            .map(|capture| {
                let whole = capture.get(0).expect("whole match");
                let action = capture.get(1).expect("action match").as_str();
                let collection_name = capture.get(2).expect("collection match").as_str();
                (
                    whole.start(),
                    if action == "delete" {
                        "delete"
                    } else {
                        "write"
                    },
                    collection_from_lean_name(collection_name),
                )
            })
            .collect::<Vec<_>>();

        let mut writes = Vec::new();
        for (index, (start, kind, collection)) in matches.iter().copied().enumerate() {
            let end = matches
                .get(index + 1)
                .map(|(next_start, _, _)| *next_start)
                .unwrap_or(query.len());
            let segment = &query[start..end];
            let value_re = Regex::new(&format!(
                r#"\b{}\s*:\s*(?:"([^"]+)"|\{{\s*_eq\s*:\s*"([^"]+)"\s*\}})"#,
                regex::escape(collection.unique_field())
            ))
            .expect("unique-field regex");
            let unique_value = value_re
                .captures(segment)
                .and_then(|capture| capture.get(1).or_else(|| capture.get(2)))
                .unwrap_or_else(|| {
                    panic!(
                        "mutation segment for {:?} did not carry unique field {}: {}",
                        collection,
                        collection.unique_field(),
                        segment
                    )
                })
                .as_str()
                .to_string();
            writes.push(ObservedWrite {
                kind: kind.to_string(),
                collection,
                unique_value,
            });
        }
        writes
    }

    fn desired_manifest_from_lean(
        case: &LeanApplyReconcileCase,
    ) -> desired_state::DesiredStateManifest {
        let agent_did = agent_did_for_case(case);
        let principal_doc = docs_for_collection(case, Collection::AgentPrincipal)
            .into_iter()
            .next();
        desired_state::DesiredStateManifest {
            agent_principal: principal_doc
                .map(|doc| desired_principal(doc))
                .unwrap_or_else(|| desired_state::DesiredAgentPrincipal {
                    agent_did: agent_did.clone(),
                    display_name: Some("default-principal".to_string()),
                    default_behavior_id: first_manifest_id(case, Collection::AgentBehavior),
                    enabled: true,
                }),
            agent_behaviors: docs_for_collection(case, Collection::AgentBehavior)
                .into_iter()
                .map(|doc| desired_behavior(doc, &agent_did))
                .collect(),
            skills: docs_for_collection(case, Collection::Skill)
                .into_iter()
                .map(|doc| desired_skill(doc, &agent_did))
                .collect(),
            tool_selections: docs_for_collection(case, Collection::ToolSelection)
                .into_iter()
                .map(|doc| desired_tool_selection(doc, &agent_did))
                .collect(),
            inference_backends: docs_for_collection(case, Collection::InferenceBackend)
                .into_iter()
                .map(desired_backend)
                .collect(),
            inference_profiles: docs_for_collection(case, Collection::InferenceProfile)
                .into_iter()
                .map(desired_profile)
                .collect(),
            tool_service_registries: docs_for_collection(case, Collection::ToolServiceRegistry)
                .into_iter()
                .map(desired_tool_service)
                .collect(),
            projection_acp_bindings: docs_for_collection(case, Collection::ProjectionAcpBinding)
                .into_iter()
                .map(|doc| desired_projection_acp_binding(doc, &agent_did))
                .collect(),
            peer_pairings: docs_for_collection(case, Collection::PeerPairingDesired)
                .into_iter()
                .map(desired_peer_pairing)
                .collect(),
            tasks: docs_for_collection(case, Collection::Task)
                .into_iter()
                .map(desired_task)
                .collect(),
            schedules: docs_for_collection(case, Collection::Schedule)
                .into_iter()
                .map(desired_schedule)
                .collect(),
            event_triggers: docs_for_collection(case, Collection::EventTrigger)
                .into_iter()
                .map(desired_event_trigger)
                .collect(),
        }
    }

    fn desired_principal(doc: &LeanApplyDesiredDoc) -> desired_state::DesiredAgentPrincipal {
        desired_state::DesiredAgentPrincipal {
            agent_did: doc.id.clone(),
            display_name: Some(doc.content.clone()),
            default_behavior_id: ref_id(doc, Collection::AgentBehavior),
            enabled: true,
        }
    }

    fn desired_behavior(
        doc: &LeanApplyDesiredDoc,
        agent_did: &str,
    ) -> desired_state::DesiredAgentBehavior {
        desired_state::DesiredAgentBehavior {
            behavior_id: doc.id.clone(),
            agent_did: agent_did.to_string(),
            display_name: Some(doc.content.clone()),
            description: None,
            summary: None,
            system_prompt: Some(doc.content.clone()),
            request_context_template: None,
            backend_id: ref_id(doc, Collection::InferenceBackend),
            model_name: Some(format!("model-{}", doc.id)),
            tool_selection_id: ref_id(doc, Collection::ToolSelection),
            inference_profile_id: ref_id(doc, Collection::InferenceProfile),
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            skill_refs: ref_ids_for_collection(&doc.refs, Collection::Skill),
            skill_excludes: Vec::new(),
        }
    }

    fn desired_skill(doc: &LeanApplyDesiredDoc, agent_did: &str) -> desired_state::DesiredSkill {
        desired_state::DesiredSkill {
            skill_id: doc.id.clone(),
            agent_did: agent_did.to_string(),
            scope: "behavior".to_string(),
            name: doc.content.clone(),
            description: Some(doc.content.clone()),
            instructions: None,
            tool_refs: ref_ids_for_collection(&doc.refs, Collection::ToolServiceRegistry),
            display_name: Some(doc.content.clone()),
            interface_json: None,
            enabled: true,
        }
    }

    fn desired_tool_selection(
        doc: &LeanApplyDesiredDoc,
        agent_did: &str,
    ) -> desired_state::DesiredToolSelection {
        desired_state::DesiredToolSelection {
            selection_id: doc.id.clone(),
            agent_did: agent_did.to_string(),
            display_name: Some(doc.content.clone()),
            tool_policy_version: None,
            enable_file_tools: false,
            file_tools_mode: "disabled".to_string(),
            file_tool_root: None,
            enable_bash: false,
            bash_mode: "disabled".to_string(),
            command_execution_policy: None,
            command_allowed_argv_prefixes: Vec::new(),
            command_forbidden_argv_prefixes: Vec::new(),
            read_only_command_allowlist: Vec::new(),
            command_network_mode: None,
            cli_tool_names: Vec::new(),
            enable_meta_tools: false,
            allowed_mcp_service_ids: doc
                .refs
                .iter()
                .filter(|reference| {
                    collection_from_lean_ref(reference) == Collection::ToolServiceRegistry
                })
                .map(|reference| reference.id.clone())
                .collect(),
            delegate_to: Vec::new(),
            backgroundable_tool_names: Vec::new(),
            enable_memory: false,
            enable_session_history_tool: false,
            enable_context_budget: true,
            enable_defra_query: true,
            defra_query_collections: Vec::new(),
            subagent_targets: Vec::new(),
            subagent_spawn_enabled: false,
            orchestration_enabled: false,
            subagent_steering_enabled: false,
            subagent_background_enabled: false,
            subagent_default_await_mode: None,
            subagent_allow_cross_deployment: false,
            cross_deployment_spawn_timeout_seconds: None,
            write_tools: Vec::new(),
        }
    }

    fn desired_backend(doc: &LeanApplyDesiredDoc) -> desired_state::DesiredInferenceBackend {
        desired_state::DesiredInferenceBackend {
            backend_id: doc.id.clone(),
            name: doc.content.clone(),
            provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: None,
            endpoint: format!("http://127.0.0.1/{}/v1", doc.id),
            api_key: None,
            api_key_env_var: None,
            max_concurrent: 1,
            max_queue_depth: 10,
            enabled: true,
            models: vec![doc.content.clone()],
        }
    }

    fn desired_profile(doc: &LeanApplyDesiredDoc) -> desired_state::DesiredInferenceProfile {
        desired_state::DesiredInferenceProfile {
            profile_id: doc.id.clone(),
            display_name: Some(doc.content.clone()),
            context_window: None,
            max_output_tokens: None,
            max_turns: None,
            temperature: None,
            stream_batch_ms: None,
            stream_liveness_timeout_secs: None,
            deadline_duration_secs: None,
            retry_max_transport: None,
            retry_backoff_ms: None,
            retry_max_resample: None,
            retry_allow_repair: None,
            retry_interactive_max: None,
        }
    }

    fn desired_tool_service(
        doc: &LeanApplyDesiredDoc,
    ) -> desired_state::DesiredToolServiceRegistry {
        desired_state::DesiredToolServiceRegistry {
            service_id: doc.id.clone(),
            display_name: Some(doc.content.clone()),
            description: Some(doc.content.clone()),
            hostname: None,
            tailscale_ip: None,
            lan_ip: None,
            mcp_port: None,
            mcp_path: None,
            send_agent_did: false,
        }
    }

    fn desired_projection_acp_binding(
        doc: &LeanApplyDesiredDoc,
        agent_did: &str,
    ) -> desired_state::DesiredProjectionAcpBinding {
        desired_state::DesiredProjectionAcpBinding {
            binding_id: doc.id.clone(),
            agent_did: Some(agent_did.to_string()),
            behavior_id: ref_id(doc, Collection::AgentBehavior),
            projection_id: Some(format!("projection-{}", doc.id)),
            policy_id: format!("policy-{}", doc.content),
            staged_policy_id: None,
            previous_policy_id: None,
            resource_map_json: Some(r#"{"AgentRequest":"AgentRequest"}"#.to_string()),
            publication_status: Some("published".to_string()),
            published_at: None,
            enabled: true,
        }
    }

    fn desired_task(doc: &LeanApplyDesiredDoc) -> desired_state::DesiredTask {
        desired_state::DesiredTask {
            task_id: doc.id.clone(),
            name: doc.content.clone(),
            description: Some(doc.content.clone()),
            behavior_id: ref_id(doc, Collection::AgentBehavior)
                .unwrap_or_else(|| DEFAULT_BEHAVIOR_ID.to_string()),
            prompt_template: doc.content.clone(),
            enabled: true,
            output_schema_ref: None,
        }
    }

    fn desired_peer_pairing(doc: &LeanApplyDesiredDoc) -> desired_state::DesiredPeerPairing {
        desired_state::DesiredPeerPairing {
            peer_did: format!("did:key:{}", doc.content),
            addresses: vec![format!("{}@127.0.0.1:4100", doc.id)],
            template: "conversation".to_string(),
            enabled: true,
            peer_id: doc.id.clone(),
        }
    }

    fn desired_schedule(doc: &LeanApplyDesiredDoc) -> desired_state::DesiredSchedule {
        desired_state::DesiredSchedule {
            schedule_id: doc.id.clone(),
            task_id: ref_id(doc, Collection::Task).unwrap_or_else(|| DEFAULT_TASK_ID.to_string()),
            interval_secs: Some(60),
            cron: None,
            timezone: None,
            missed_run_policy: None,
            enabled: true,
            concurrency: doc.content.clone(),
        }
    }

    fn desired_event_trigger(doc: &LeanApplyDesiredDoc) -> desired_state::DesiredEventTrigger {
        desired_state::DesiredEventTrigger {
            trigger_id: doc.id.clone(),
            task_id: ref_id(doc, Collection::Task).unwrap_or_else(|| DEFAULT_TASK_ID.to_string()),
            source_collection: "Task".to_string(),
            event_kind: "created".to_string(),
            filter: Some(doc.content.clone()),
            enabled: true,
            concurrency: "serial".to_string(),
        }
    }

    fn diff_report_from_lean(
        case: &LeanApplyReconcileCase,
    ) -> desired_state::DesiredStateDiffReport {
        let collections = desired_state::DesiredStateDiffCollections {
            agent_principal: diff_for_collection(case, Collection::AgentPrincipal),
            agent_behaviors: diff_for_collection(case, Collection::AgentBehavior),
            skills: diff_for_collection(case, Collection::Skill),
            tool_selections: diff_for_collection(case, Collection::ToolSelection),
            inference_backends: diff_for_collection(case, Collection::InferenceBackend),
            inference_profiles: diff_for_collection(case, Collection::InferenceProfile),
            tool_service_registries: diff_for_collection(case, Collection::ToolServiceRegistry),
            projection_acp_bindings: diff_for_collection(case, Collection::ProjectionAcpBinding),
            peer_pairings: diff_for_collection(case, Collection::PeerPairingDesired),
            tasks: diff_for_collection(case, Collection::Task),
            schedules: diff_for_collection(case, Collection::Schedule),
            event_triggers: diff_for_collection(case, Collection::EventTrigger),
        };
        let counts = collections.counts();
        let ok = counts.is_exact_match();
        desired_state::DesiredStateDiffReport {
            status: "diffed",
            ok,
            root: format!("lean://{}", case.name),
            access_mode: "graphql".to_string(),
            agent_did: agent_did_for_case(case),
            live_validation_errors: Vec::new(),
            counts,
            collections,
        }
    }

    fn diff_for_collection(
        case: &LeanApplyReconcileCase,
        collection: Collection,
    ) -> desired_state::DesiredStateCollectionDiff {
        desired_state::DesiredStateCollectionDiff {
            create: ref_ids_for_collection(&case.expected_create, collection),
            update: ref_ids_for_collection(&case.expected_update, collection),
            delete: ref_ids_for_collection(&case.expected_delete, collection),
            unchanged: ref_ids_for_collection(&case.expected_unchanged, collection),
            live_only: ref_ids_for_collection(&case.expected_live_only, collection),
        }
    }

    fn collection_from_lean_name(name: &str) -> Collection {
        Collection::ALL
            .into_iter()
            .find(|collection| collection.graphql_type() == name)
            .unwrap_or_else(|| panic!("unknown Lean collection name {name:?}"))
    }

    fn collection_from_lean_ref(reference: &LeanApplyDocRef) -> Collection {
        collection_from_lean_name(&reference.collection)
    }

    fn docs_for_collection(
        case: &LeanApplyReconcileCase,
        collection: Collection,
    ) -> Vec<&LeanApplyDesiredDoc> {
        case.manifest
            .iter()
            .filter(|doc| collection_from_lean_name(&doc.collection) == collection)
            .collect()
    }

    fn first_manifest_id(case: &LeanApplyReconcileCase, collection: Collection) -> Option<String> {
        docs_for_collection(case, collection)
            .into_iter()
            .next()
            .map(|doc| doc.id.clone())
    }

    fn agent_did_for_case(case: &LeanApplyReconcileCase) -> String {
        first_manifest_id(case, Collection::AgentPrincipal)
            .unwrap_or_else(|| DEFAULT_AGENT_DID.to_string())
    }

    fn ref_id(doc: &LeanApplyDesiredDoc, collection: Collection) -> Option<String> {
        doc.refs
            .iter()
            .find(|reference| collection_from_lean_ref(reference) == collection)
            .map(|reference| reference.id.clone())
    }

    fn ref_ids_for_collection(refs: &[LeanApplyDocRef], collection: Collection) -> Vec<String> {
        refs.iter()
            .filter(|reference| collection_from_lean_ref(reference) == collection)
            .map(|reference| reference.id.clone())
            .collect()
    }

    fn ids_for_collection(writes: &[ObservedWrite], collection: Collection) -> Vec<String> {
        writes
            .iter()
            .filter(|write| write.collection == collection)
            .map(|write| write.unique_value.clone())
            .collect()
    }

    fn observed_write_from_lean(doc: &LeanApplySelectedDoc) -> ObservedWrite {
        assert_eq!(doc.unique_value, doc.target.id);
        assert_eq!(doc.graphql_type, doc.target.collection);
        let collection = collection_from_lean_ref(&doc.target);
        assert_eq!(doc.unique_field, collection.unique_field());
        ObservedWrite {
            kind: if doc.action == "delete" {
                "delete".to_string()
            } else {
                "write".to_string()
            },
            collection,
            unique_value: doc.unique_value.clone(),
        }
    }

    fn observed_write_from_lean_live_doc(doc: &LeanApplyLiveDoc) -> ObservedWrite {
        ObservedWrite {
            kind: "live".to_string(),
            collection: collection_from_lean_name(&doc.collection),
            unique_value: doc.id.clone(),
        }
    }

    fn unique_value_from_doc(doc: &Value, collection: Collection) -> String {
        doc.get(collection.unique_field())
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!(
                    "selected {:?} document is missing unique field {}: {}",
                    collection,
                    collection.unique_field(),
                    doc
                )
            })
            .to_string()
    }

    fn count_for_collection(counts: &ConfigApplyCounts, collection: Collection) -> usize {
        match collection {
            Collection::AgentPrincipal => counts.agent_principal,
            Collection::AgentBehavior => counts.agent_behaviors,
            Collection::Skill => counts.skills,
            Collection::ToolSelection => counts.tool_selections,
            Collection::InferenceBackend => counts.inference_backends,
            Collection::InferenceProfile => counts.inference_profiles,
            Collection::ToolServiceRegistry => counts.tool_service_registries,
            Collection::ProjectionAcpBinding => counts.projection_acp_bindings,
            Collection::PeerPairingDesired => counts.peer_pairings,
            Collection::Task => counts.tasks,
            Collection::Schedule => counts.schedules,
            Collection::EventTrigger => counts.event_triggers,
        }
    }

    fn runtime_owned_fields(collection: Collection) -> &'static [&'static str] {
        match collection {
            Collection::InferenceBackend => &["probe_status"],
            Collection::ToolServiceRegistry => &["tools", "version"],
            Collection::ProjectionAcpBinding => &[],
            Collection::PeerPairingDesired => &[],
            Collection::Schedule => &[
                "next_run_at",
                "last_attempt_at",
                "last_status",
                "last_error",
                "fire_count",
            ],
            Collection::EventTrigger => &[
                "last_attempt_at",
                "last_fired_source_doc_id",
                "last_status",
                "last_error",
                "fire_count",
            ],
            _ => &[],
        }
    }

    fn doc_key(reference: &LeanApplyDocRef) -> (Collection, String) {
        (collection_from_lean_ref(reference), reference.id.clone())
    }

    fn doc_key_from_desired(doc: &LeanApplyDesiredDoc) -> (Collection, String) {
        (collection_from_lean_name(&doc.collection), doc.id.clone())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_apply_txn_round_trip_against_recorder() {
        let (graphql, recorder) = start_recording_graphql().await;
        let access = ConfigAccess::Graphql(graphql);
        let txn = access.begin_apply_txn().await.expect("begin");

        let _ = txn
            .execute("mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }")
            .await
            .expect("execute in tx");

        assert!(recorder.committed_state().is_empty());

        txn.commit().await.expect("commit");

        let committed = recorder.committed_state();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].unique_value, "task-a");
        let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
        assert_eq!((begin_count, commit_count, discard_count), (1, 1, 0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_apply_txn_discard_leaves_committed_empty() {
        let (graphql, recorder) = start_recording_graphql().await;
        let access = ConfigAccess::Graphql(graphql);
        let txn = access.begin_apply_txn().await.expect("begin");

        let _ = txn
            .execute("mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }")
            .await
            .expect("execute in tx");

        txn.discard().await.expect("discard");

        assert!(recorder.committed_state().is_empty());
        let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
        assert_eq!((begin_count, commit_count, discard_count), (1, 0, 1));
    }

    #[cfg(test)]
    mod recorder_unit_tests {
        use super::*;
        use serde_json::json;

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn recorder_begin_returns_numeric_id_and_commit_appends_to_committed() {
            let (graphql, recorder) = start_recording_graphql().await;
            let api_base =
                crate::graphql_access::graphql_api_base(&graphql).expect("graphql endpoint");
            let client = reqwest::Client::new();

            let begin = client
                .post(format!("{api_base}/tx"))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap();
            let txn_id = begin
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_string();
            assert!(txn_id.parse::<u64>().is_ok(), "tx id must be numeric");

            let _write = client
                .post(&graphql)
                .header("x-defradb-tx", &txn_id)
                .json(&json!({
                    "query": "mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }",
                }))
                .send()
                .await
                .unwrap();

            // Before commit, committed window is empty.
            assert!(recorder.committed_state().is_empty());

            let commit = client
                .post(format!("{api_base}/tx/{txn_id}"))
                .send()
                .await
                .unwrap();
            assert!(commit.status().is_success());

            let committed = recorder.committed_state();
            assert_eq!(committed.len(), 1);
            assert_eq!(committed[0].collection, Collection::Task);
            assert_eq!(committed[0].unique_value, "task-a");
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn recorder_discard_drops_pending_writes() {
            let (graphql, recorder) = start_recording_graphql().await;
            let api_base =
                crate::graphql_access::graphql_api_base(&graphql).expect("graphql endpoint");
            let client = reqwest::Client::new();

            let begin = client
                .post(format!("{api_base}/tx"))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap();
            let txn_id = begin
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_string();

            let _write = client
                .post(&graphql)
                .header("x-defradb-tx", &txn_id)
                .json(&serde_json::json!({
                    "query": "mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }",
                }))
                .send()
                .await
                .unwrap();

            let discard = client
                .delete(format!("{api_base}/tx/{txn_id}"))
                .send()
                .await
                .unwrap();
            assert!(discard.status().is_success());

            assert!(
                recorder.committed_state().is_empty(),
                "discarded tx must not contribute to committed state"
            );
            let (begin_count, commit_count, discard_count) = recorder.tx_lifecycle_counts();
            assert_eq!((begin_count, commit_count, discard_count), (1, 0, 1));
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn recorder_fail_injection_aborts_at_target_index() {
            let (graphql, recorder) = start_recording_graphql().await;
            let api_base =
                crate::graphql_access::graphql_api_base(&graphql).expect("graphql endpoint");
            let client = reqwest::Client::new();

            let begin = client
                .post(format!("{api_base}/tx"))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap();
            let txn_id = begin
                .get("id")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_string();
            recorder.install_fail_at(&txn_id, 1);

            let ok = client
                .post(&graphql)
                .header("x-defradb-tx", &txn_id)
                .json(&serde_json::json!({
                    "query": "mutation { doc_0: create_Task(input: { task_id: \"task-a\" }) { _docID } }",
                }))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap();
            assert!(ok.get("errors").is_none(), "first mutation should succeed");

            // Confirm the first write was actually buffered into the tx's pending window —
            // a broken recorder that ignored writes silently would still pass the
            // `errors.is_none()` check above.
            let pending_after_first = recorder.observed_writes();
            assert_eq!(
                pending_after_first.len(),
                1,
                "first mutation must be buffered into tx pending window"
            );
            assert_eq!(pending_after_first[0].unique_value, "task-a");
            assert!(
                recorder.committed_state().is_empty(),
                "buffered tx writes must not appear in committed state yet"
            );

            let fail = client
                .post(&graphql)
                .header("x-defradb-tx", &txn_id)
                .json(&serde_json::json!({
                    "query": "mutation { doc_0: create_Task(input: { task_id: \"task-b\" }) { _docID } }",
                }))
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap();
            assert!(fail.get("errors").is_some(), "second mutation should fail");
        }
    }
}
