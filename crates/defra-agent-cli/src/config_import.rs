use std::io::Read;

use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::Collection;
use serde_json::Value;

use crate::config_bundle::{sanitize_import_document, select_apply_collection_docs};
use crate::config_writes::{
    write_event_trigger_document, write_schedule_document, write_task_document, ConfigAccess,
};
use crate::desired_state;
use crate::desired_state::DesiredApplyBundle;
use crate::shared::{ConfigApplyCounts, ConfigExportBundle};
use crate::{
    extract_mutation_doc_id, graphql_input_literal, CONFIG_EXPORT_FORMAT, CONFIG_EXPORT_FORMAT_V1,
};

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
    for doc in docs {
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
            })?;
        let add_doc = sanitize_import_document(collection_name, doc, false)?;
        if override_existing && collection_name == "Task" {
            let update_doc = sanitize_import_document(collection_name, doc, true)?;
            let doc_id = write_task_document(access, unique_value, &add_doc, &update_doc)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "importing {collection_name} {} failed: {error}",
                        unique_value
                    )
                })?;
            if doc_id.trim().is_empty() {
                anyhow::bail!(
                    "importing {collection_name} {} returned an empty _docID",
                    unique_value
                );
            }
            continue;
        }
        if override_existing && collection_name == "Schedule" {
            let update_doc = sanitize_import_document(collection_name, doc, true)?;
            let doc_id = write_schedule_document(access, unique_value, &add_doc, &update_doc)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "importing {collection_name} {} failed: {error}",
                        unique_value
                    )
                })?;
            if doc_id.trim().is_empty() {
                anyhow::bail!(
                    "importing {collection_name} {} returned an empty _docID",
                    unique_value
                );
            }
            continue;
        }
        if override_existing && collection_name == "EventTrigger" {
            let update_doc = sanitize_import_document(collection_name, doc, true)?;
            let doc_id = write_event_trigger_document(access, unique_value, &add_doc, &update_doc)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "importing {collection_name} {} failed: {error}",
                        unique_value
                    )
                })?;
            if doc_id.trim().is_empty() {
                anyhow::bail!(
                    "importing {collection_name} {} returned an empty _docID",
                    unique_value
                );
            }
            continue;
        }

        let add_literal = graphql_input_literal(&add_doc)?;
        let mutation = if override_existing {
            let update_doc = sanitize_import_document(collection_name, doc, true)?;
            let update_literal = graphql_input_literal(&update_doc)?;
            format!(
                r#"mutation {{
                    upsert_{collection_name}(
                        filter: {{ {unique_field}: {{ _eq: "{unique_value}" }} }},
                        add: {add_literal},
                        update: {update_literal}
                    ) {{ _docID }}
                }}"#,
                collection_name = collection_name,
                unique_field = unique_field,
                unique_value = escape_graphql_string(unique_value),
                add_literal = add_literal,
                update_literal = update_literal,
            )
        } else {
            format!(
                r#"mutation {{
                    create_{collection_name}(input: {add_literal}) {{ _docID }}
                }}"#,
                collection_name = collection_name,
                add_literal = add_literal,
            )
        };
        let response = access.execute(&mutation).await.map_err(|error| {
            if override_existing {
                anyhow::anyhow!(
                    "importing {collection_name} {} failed: {error}",
                    unique_value
                )
            } else {
                anyhow::anyhow!(
                    "importing {collection_name} {} failed: {error}\nNext:\n  1. If the document already exists, rerun with `defra-agent config import --override`\n  2. Or remove the existing document and retry",
                    unique_value
                )
            }
        })?;
        let _ = extract_mutation_doc_id(&response, collection_name)?;
    }

    Ok(docs.len())
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
