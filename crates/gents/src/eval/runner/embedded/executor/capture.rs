use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::defra_node::EmbeddedNode;
use crate::eval::runner::executor::{Capture, CaptureResult, FileRef};
use crate::graphql::{
    escape_graphql_string, graphql_with_transaction_retry, validate_collection_identifier,
    validate_graphql_name,
};

pub(super) async fn run_captures(
    node: &Arc<EmbeddedNode>,
    trial_did: &str,
    session_id: &str,
    workspace: &Path,
    captures: &[Capture],
) -> BTreeMap<String, CaptureResult> {
    let mut results = BTreeMap::new();
    for capture in captures {
        match capture {
            Capture::Schema { name, collections } => {
                let access = crate::config_client::ConfigAccess::local(node.clone());
                let mut versions = Vec::new();
                let mut failed = false;
                for collection in collections {
                    match access.collection_version(collection).await {
                        Ok(Some(version)) => versions.push(version),
                        Ok(None) => {}
                        Err(error) => {
                            tracing::warn!(error = %format!("{error:#}"), capture = %name,
                                "eval schema capture failed; recording no capture");
                            failed = true;
                            break;
                        }
                    }
                }
                if !failed {
                    results.insert(
                        name.clone(),
                        CaptureResult::Schema {
                            collections: versions,
                        },
                    );
                }
            }
            Capture::Documents {
                name,
                collection,
                filter,
                fields,
            } => match capture_documents(
                node,
                collection,
                &bind_capture_session(filter, session_id),
                fields,
                trial_did,
            )
            .await
            {
                Ok(rows) => {
                    results.insert(name.clone(), CaptureResult::Documents { rows });
                }
                Err(error) => tracing::warn!(
                    error = %format!("{error:#}"),
                    capture = %name,
                    "eval document capture failed; the stage records no capture under that name"
                ),
            },
            Capture::File { name, glob } => {
                if let Some(result) = capture_files(workspace, glob) {
                    results.insert(name.clone(), result);
                }
            }
        }
    }
    results
}

fn bind_capture_session(value: &Value, session_id: &str) -> Value {
    match value {
        Value::String(text) if text == "$session" => Value::String(session_id.into()),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| bind_capture_session(v, session_id))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, v)| (key.clone(), bind_capture_session(v, session_id)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) async fn capture_documents(
    node: &EmbeddedNode,
    collection: &str,
    filter: &Value,
    fields: &[String],
    trial_did: &str,
) -> Result<Vec<Value>> {
    let query = documents_capture_query(collection, filter, fields, trial_did)?;
    let response = graphql_with_transaction_retry(node, &query, "eval trial capture").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

pub(in crate::eval::runner::embedded) fn documents_capture_query(
    collection: &str,
    filter: &Value,
    fields: &[String],
    trial_did: &str,
) -> Result<String> {
    validate_collection_identifier(collection)?;
    anyhow::ensure!(
        !gents_protocol::schemas::is_credential_collection(collection)
            || collection == gents_protocol::schemas::INFERENCE_BACKEND_NAME,
        "credential collection {collection:?} cannot be retained as eval capture evidence"
    );
    validate_filter_names(filter)?;
    for field in fields {
        anyhow::ensure!(
            !crate::defra_query::is_restricted_field(collection, field),
            "field {field:?} on {collection:?} is restricted and cannot be captured"
        );
    }
    let mut filter_fields = Vec::new();
    crate::defra_query::query::collect_filter_field_keys(filter, &mut filter_fields);
    for field in &filter_fields {
        anyhow::ensure!(
            !crate::defra_query::is_restricted_field(collection, field),
            "field {field:?} on {collection:?} is restricted and cannot be used in a capture filter"
        );
    }
    let mut selected = vec!["_docID".to_string()];
    let filter_fields = filter
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| !key.starts_with('_'))
        .map(|(key, _)| key.clone());
    for field in fields.iter().cloned().chain(filter_fields) {
        validate_graphql_name(&field)?;
        if !selected.contains(&field) {
            selected.push(field);
        }
    }
    Ok(format!(
        "query {{ {collection}(filter: {}) {{ {} }} }}",
        render_graphql_input(filter, trial_did),
        selected.join(" ")
    ))
}

fn validate_filter_names(filter: &Value) -> Result<()> {
    match filter {
        Value::Object(map) => map.iter().try_for_each(|(key, nested)| {
            validate_graphql_name(key)?;
            validate_filter_names(nested)
        }),
        Value::Array(items) => items.iter().try_for_each(validate_filter_names),
        _ => Ok(()),
    }
}

pub(crate) fn render_graphql_input(value: &Value, trial_did: &str) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(text) => format!(
            "\"{}\"",
            escape_graphql_string(&text.replace("$trial", trial_did))
        ),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(|item| render_graphql_input(item, trial_did))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(key, nested)| format!("{key}: {}", render_graphql_input(nested, trial_did)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn capture_files(workspace: &Path, glob: &str) -> Option<CaptureResult> {
    match collect_files(workspace, glob) {
        Ok((files, outside_workspace)) => Some(CaptureResult::Files {
            files,
            outside_workspace,
        }),
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                glob = %glob,
                "eval file capture was refused or failed; the stage records no capture under that name"
            );
            None
        }
    }
}

fn collect_files(workspace: &Path, glob: &str) -> Result<(Vec<FileRef>, Vec<String>)> {
    anyhow::ensure!(
        Path::new(glob)
            .components()
            .all(|component| matches!(component, Component::Normal(_))),
        "file capture glob {glob:?} must stay inside the trial workspace"
    );
    let resolved_workspace = workspace
        .canonicalize()
        .with_context(|| format!("resolving {}", workspace.display()))?;
    let root = glob::Pattern::escape(
        workspace
            .to_str()
            .with_context(|| format!("{} is not UTF-8", workspace.display()))?,
    );
    let mut files = Vec::new();
    let mut outside_workspace = Vec::new();
    for matched in glob::glob(&format!("{root}/{glob}")).context("compiling a capture glob")? {
        let path = matched.context("reading a capture glob match")?;
        if !path.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(workspace)
            .with_context(|| format!("{} is outside the trial workspace", path.display()))?
            .to_str()
            .with_context(|| format!("{} is not UTF-8", path.display()))?
            .to_string();
        let inside = path
            .canonicalize()
            .is_ok_and(|resolved| resolved.starts_with(&resolved_workspace));
        if !inside {
            tracing::warn!(
                path = %path.display(),
                workspace = %resolved_workspace.display(),
                "eval file capture skipped a match that resolves outside the trial workspace"
            );
            outside_workspace.push(relative);
            continue;
        }
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        files.push(FileRef {
            path: relative,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes: bytes.len() as u64,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    outside_workspace.sort();
    Ok((files, outside_workspace))
}

#[cfg(test)]
mod tests;
