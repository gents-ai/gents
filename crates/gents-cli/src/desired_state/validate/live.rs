use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Result;
use gents::parse_template_for_validation;

use crate::config_writes::ConfigAccess;

use super::super::DesiredStateManifest;

/// Validate live state that cannot be checked from the manifest alone.
///
/// Apply validates trigger filter syntax and top-level `doc.*` template fields.
/// Resolving fields below the top level remains outside this contract.
pub(crate) async fn validate_manifest_against_live(
    manifest: &DesiredStateManifest,
    access: &ConfigAccess,
) -> Result<Vec<String>> {
    let mut errors = Vec::new();
    for trigger in &manifest.event_triggers {
        let source_collection = trigger.source_collection.trim();
        let trigger_id = trigger.trigger_id.trim();
        if source_collection.is_empty() || trigger_id.is_empty() {
            continue;
        }
        if let Err(error) = gents::graphql::validate_collection_identifier(source_collection) {
            errors.push(format!(
                "event_trigger {} has invalid source_collection {:?}: {}",
                trigger_id, trigger.source_collection, error
            ));
            continue;
        }

        if let Some(filter) = trigger.filter.as_deref().map(str::trim) {
            if !filter.is_empty() {
                // `filter` is interpolated into the probe query as a raw filter
                // fragment; validate it like the runtime trigger engine does
                // (`trigger_engine::event_source`) before building the probe.
                // `source_collection` is already validated by the guard above.
                if let Err(err) = gents::graphql::validate_graphql_filter_fragment(filter) {
                    errors.push(format!(
                        "event_trigger {} filter is not a valid filter fragment: {}",
                        trigger_id, err
                    ));
                } else {
                    let probe = format!(
                        r#"query {{ {collection}(filter: {filter}, limit: 1) {{ _docID }} }}"#,
                        collection = source_collection,
                        filter = filter,
                    );
                    match access.execute(&probe).await {
                        Ok(_) => {}
                        Err(err) => {
                            errors.push(format!(
                                "event_trigger {} filter syntax error: {}",
                                trigger_id, err
                            ));
                        }
                    }
                }
            }
        }

        let task_id = trigger.task_id.trim();
        if task_id.is_empty() {
            continue;
        }
        let Some(task) = manifest.tasks.iter().find(|t| t.task_id.trim() == task_id) else {
            continue;
        };
        let refs = match parse_template_for_validation(&task.prompt_template) {
            Ok(refs) => refs,
            Err(_) => {
                continue;
            }
        };
        let doc_paths: Vec<Vec<String>> = refs
            .into_iter()
            .filter(|v| v.root() == Some("doc"))
            .map(|v| v.path.clone())
            .collect();
        if doc_paths.is_empty()
            && trigger.correlation_field.is_none()
            && trigger.expected_count_field.is_none()
        {
            continue;
        }

        let introspect = format!(
            r#"query {{ __type(name: "{name}") {{ fields {{ name type {{ name kind }} }} }} }}"#,
            name = gents::graphql::escape_graphql_string(source_collection),
        );
        let response = match access.execute(&introspect).await {
            Ok(response) => response,
            Err(err) => {
                errors.push(format!(
                    "event_trigger {} introspection of source_collection {} failed: {}",
                    trigger_id, source_collection, err
                ));
                continue;
            }
        };
        let type_node = response.get("data").and_then(|d| d.get("__type"));
        let fields = type_node
            .filter(|v| !v.is_null())
            .and_then(|t| t.get("fields"))
            .and_then(serde_json::Value::as_array);
        let Some(fields) = fields else {
            errors.push(format!(
                "event_trigger {} references unknown source_collection {}",
                trigger_id, source_collection
            ));
            continue;
        };
        let top_level: HashSet<&str> = fields
            .iter()
            .filter_map(|f| f.get("name").and_then(|n| n.as_str()))
            .collect();
        let field_types: HashMap<&str, &str> = fields
            .iter()
            .filter_map(|field| {
                Some((
                    field.get("name")?.as_str()?,
                    field.get("type")?.get("name")?.as_str()?,
                ))
            })
            .collect();
        if let Some(field) = trigger
            .correlation_field
            .as_deref()
            .map(str::trim)
            .filter(|field| !field.is_empty())
        {
            match field_types.get(field).copied() {
                Some("String") => {}
                Some(actual) => errors.push(format!(
                    "event_trigger {} correlation_field {} must be String, found {}",
                    trigger_id, field, actual
                )),
                None => errors.push(format!(
                    "event_trigger {} correlation_field {} does not exist on {}",
                    trigger_id, field, source_collection
                )),
            }
        }
        if let Some(field) = trigger
            .expected_count_field
            .as_deref()
            .map(str::trim)
            .filter(|field| !field.is_empty())
        {
            match field_types.get(field).copied() {
                Some("String" | "Int") => {}
                Some(actual) => errors.push(format!(
                    "event_trigger {} expected_count_field {} must be String or Int, found {}",
                    trigger_id, field, actual
                )),
                None => errors.push(format!(
                    "event_trigger {} expected_count_field {} does not exist on {}",
                    trigger_id, field, source_collection
                )),
            }
        }
        let mut reported: BTreeSet<String> = BTreeSet::new();
        for path in &doc_paths {
            let Some(first) = path.get(1).map(String::as_str) else {
                continue;
            };
            if top_level.contains(first) {
                continue;
            }
            if !reported.insert(first.to_string()) {
                continue;
            }
            errors.push(format!(
                "event_trigger {} template references doc.{} but {} has no such field",
                trigger_id, first, source_collection
            ));
        }
    }

    Ok(errors)
}
