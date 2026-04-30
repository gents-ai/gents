use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::Collection;
use serde_json::Value;

use crate::config_bundle::{sanitize_import_document, select_apply_collection_docs};
use crate::config_writes::{
    write_event_trigger_document, write_schedule_document, write_task_document, ConfigAccess,
    ExistingDocumentRef,
};
use crate::desired_state;
use crate::desired_state::DesiredApplyBundle;
use crate::shared::{ConfigApplyCounts, ConfigExportBundle};
use crate::{
    extract_mutation_doc_id, graphql_input_literal, graphql_string_list_literal,
    CONFIG_EXPORT_FORMAT, CONFIG_EXPORT_FORMAT_V1,
};

const CONFIG_IMPORT_BATCH_SIZE: usize = 50;

const CONFIG_APPLY_ORDER: [Collection; 9] = [
    Collection::InferenceBackend,
    Collection::InferenceProfile,
    Collection::ToolServiceRegistry,
    Collection::ToolSelection,
    Collection::AgentBehavior,
    Collection::Task,
    Collection::Schedule,
    Collection::EventTrigger,
    Collection::AgentPrincipal,
];

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
    access: &ConfigAccess,
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

    if override_existing && uses_custom_apply_writer(collection_name) {
        apply_custom_override_collection_batched(access, collection_name, unique_field, &prepared)
            .await?;
    } else {
        apply_generic_import_collection_batched(
            access,
            collection_name,
            unique_field,
            &prepared,
            override_existing,
        )
        .await?;
    }

    Ok(docs.len())
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
    access: &ConfigAccess,
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

    match execute_aliased_mutation_batches(access, collection_name, &fields).await {
        Ok(()) => Ok(()),
        Err(_) if override_existing => {
            for doc in docs {
                apply_generic_import_document(
                    access,
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

async fn apply_generic_import_document(
    access: &ConfigAccess,
    collection_name: &str,
    unique_field: &str,
    doc: &PreparedImportDocument,
    override_existing: bool,
) -> Result<()> {
    let add_literal = graphql_input_literal(&doc.add_doc)?;
    let mutation = if override_existing {
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
    let response = access.execute(&mutation).await.map_err(|error| {
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
    access: &ConfigAccess,
    collection_name: &str,
    unique_field: &str,
    docs: &[PreparedImportDocument],
) -> Result<()> {
    if has_duplicate_unique_values(docs) {
        return apply_custom_override_documents_individually(access, collection_name, docs).await;
    }

    let existing_by_unique = match query_existing_documents_by_unique_values(
        access,
        collection_name,
        unique_field,
        docs,
    )
    .await
    {
        Ok(existing_by_unique) => existing_by_unique,
        Err(_) => {
            return apply_custom_override_documents_individually(access, collection_name, docs)
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

    match execute_aliased_mutation_batches(access, collection_name, &fields).await {
        Ok(()) => Ok(()),
        Err(_) => apply_custom_override_documents_individually(access, collection_name, docs).await,
    }
}

fn has_duplicate_unique_values(docs: &[PreparedImportDocument]) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    docs.iter()
        .any(|doc| !seen.insert(doc.unique_value.as_str()))
}

async fn query_existing_documents_by_unique_values(
    access: &ConfigAccess,
    collection_name: &str,
    unique_field: &str,
    docs: &[PreparedImportDocument],
) -> Result<BTreeMap<String, Vec<ExistingDocumentRef>>> {
    let unique_values = docs
        .iter()
        .map(|doc| doc.unique_value.clone())
        .collect::<Vec<_>>();
    let limit = unique_values.len().saturating_mul(16).max(16);
    let unique_values_literal = graphql_string_list_literal(&unique_values);
    let query = format!(
        r#"{{
            {collection_name}(
                showDeleted: true,
                filter: {{ {unique_field}: {{ _in: {unique_values_literal} }} }},
                limit: {limit}
            ) {{
                _docID
                _deleted
                {unique_field}
            }}
        }}"#,
    );
    let response = access.execute(&query).await?;
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
        let add_literal = graphql_input_literal(&doc.add_doc)?;
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
    access: &ConfigAccess,
    collection_name: &str,
    docs: &[PreparedImportDocument],
) -> Result<()> {
    for doc in docs {
        let update_doc = doc
            .update_doc
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing update document for {collection_name}"))?;
        let doc_id = match collection_name {
            "Task" => {
                write_task_document(access, &doc.unique_value, &doc.add_doc, update_doc).await
            }
            "Schedule" => {
                write_schedule_document(access, &doc.unique_value, &doc.add_doc, update_doc).await
            }
            "EventTrigger" => {
                write_event_trigger_document(access, &doc.unique_value, &doc.add_doc, update_doc)
                    .await
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
    access: &ConfigAccess,
    collection_name: &str,
    fields: &[AliasedMutationField],
) -> Result<()> {
    for chunk in fields.chunks(CONFIG_IMPORT_BATCH_SIZE) {
        let mutation = build_aliased_mutation(chunk);
        let response = access.execute(&mutation).await?;
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
    access: &ConfigAccess,
    desired_bundle: &DesiredApplyBundle,
    planned: &desired_state::DesiredStateDiffReport,
) -> Result<ConfigApplyCounts> {
    let desired_bundle = desired_bundle.as_bundle();
    let mut counts = ConfigApplyCounts::default();

    for collection in CONFIG_APPLY_ORDER {
        let docs = select_apply_docs_for_collection(desired_bundle, planned, collection)?;
        let applied = apply_import_collection(
            access,
            collection.graphql_type(),
            collection.unique_field(),
            &docs,
            true,
        )
        .await?;
        counts.set(collection, applied);
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
