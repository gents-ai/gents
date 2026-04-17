use std::io::Read;

use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use serde_json::Value;

use crate::config_bundle::{sanitize_import_document, select_apply_collection_docs};
use crate::config_writes::{write_scheduled_task_document, ConfigAccess};
use crate::desired_state;
use crate::shared::{ConfigApplyCounts, ConfigExportBundle};
use crate::{
    extract_mutation_doc_id, graphql_input_literal, CONFIG_EXPORT_FORMAT, CONFIG_EXPORT_FORMAT_V1,
};

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
        if override_existing && collection_name == "ScheduledTask" {
            let update_doc = sanitize_import_document(collection_name, doc, true)?;
            let doc_id = write_scheduled_task_document(access, unique_value, &add_doc, &update_doc)
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
    [
        &counts.agent_principal,
        &counts.agent_behaviors,
        &counts.tool_selections,
        &counts.inference_backends,
        &counts.inference_profiles,
        &counts.tool_service_registries,
        &counts.scheduled_tasks,
    ]
    .iter()
    .any(|count| count.create > 0 || count.update > 0)
}

pub(crate) fn config_apply_counts_changed(counts: &ConfigApplyCounts) -> bool {
    counts.agent_principal > 0
        || counts.agent_behaviors > 0
        || counts.tool_selections > 0
        || counts.inference_backends > 0
        || counts.inference_profiles > 0
        || counts.tool_service_registries > 0
        || counts.scheduled_tasks > 0
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
    desired_bundle: &ConfigExportBundle,
    planned: &desired_state::DesiredStateDiffReport,
) -> Result<ConfigApplyCounts> {
    let backend_docs = select_apply_collection_docs(
        &desired_bundle.inference_backends,
        "backend_id",
        "InferenceBackend",
        &planned.collections.inference_backends,
    )?;
    let profile_docs = select_apply_collection_docs(
        &desired_bundle.inference_profiles,
        "profile_id",
        "InferenceProfile",
        &planned.collections.inference_profiles,
    )?;
    let tool_selection_docs = select_apply_collection_docs(
        &desired_bundle.tool_selections,
        "selection_id",
        "ToolSelection",
        &planned.collections.tool_selections,
    )?;
    let tool_service_registry_docs = select_apply_collection_docs(
        &desired_bundle.tool_service_registries,
        "service_id",
        "ToolServiceRegistry",
        &planned.collections.tool_service_registries,
    )?;
    let behavior_docs = select_apply_collection_docs(
        &desired_bundle.agent_behaviors,
        "behavior_id",
        "AgentBehavior",
        &planned.collections.agent_behaviors,
    )?;
    let scheduled_task_docs = select_apply_collection_docs(
        &desired_bundle.scheduled_tasks,
        "task_id",
        "ScheduledTask",
        &planned.collections.scheduled_tasks,
    )?;
    let principal_docs = select_apply_principal_docs(
        desired_bundle.agent_principal.as_ref(),
        &planned.collections.agent_principal,
    )?;

    Ok(ConfigApplyCounts {
        inference_backends: apply_import_collection(
            access,
            "InferenceBackend",
            "backend_id",
            &backend_docs,
            true,
        )
        .await?,
        inference_profiles: apply_import_collection(
            access,
            "InferenceProfile",
            "profile_id",
            &profile_docs,
            true,
        )
        .await?,
        tool_service_registries: apply_import_collection(
            access,
            "ToolServiceRegistry",
            "service_id",
            &tool_service_registry_docs,
            true,
        )
        .await?,
        tool_selections: apply_import_collection(
            access,
            "ToolSelection",
            "selection_id",
            &tool_selection_docs,
            true,
        )
        .await?,
        agent_behaviors: apply_import_collection(
            access,
            "AgentBehavior",
            "behavior_id",
            &behavior_docs,
            true,
        )
        .await?,
        scheduled_tasks: apply_import_collection(
            access,
            "ScheduledTask",
            "task_id",
            &scheduled_task_docs,
            true,
        )
        .await?,
        agent_principal: apply_import_collection(
            access,
            "AgentPrincipal",
            "agent_did",
            &principal_docs,
            true,
        )
        .await?,
    })
}
