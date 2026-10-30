use std::path::PathBuf;

use serde_json::Value;

use super::super::file_tools::{content_hash, file_content_hash, file_mutation_lock_for};
use crate::tools::call::lifecycle::FailureClass;
use crate::toolset::shared::{ToolContext, ToolError};

use super::client::LspClient;
use super::encoding::{position_to_byte_offset, PositionEncoding};

#[derive(Debug, Clone)]
pub struct PreparedEdit {
    pub path: PathBuf,
    pub new_bytes: Vec<u8>,
    pub expected_hash: Option<String>,
    pub version: Option<i64>,
    pub rename_from: Option<PathBuf>,
    pub rename_destination: Option<RenameDestination>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", content = "contentHash", rename_all = "camelCase")]
pub enum RenameDestination {
    Missing,
    ContentHash(String),
}

pub fn prepare_rename(from: PathBuf, to: PathBuf) -> Result<PreparedEdit, String> {
    let source = file_content_hash(&from).map_err(|error| error.to_string())?;
    let destination = match file_content_hash(&to) {
        Ok(hash) => RenameDestination::ContentHash(hash),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => RenameDestination::Missing,
        Err(error) => return Err(error.to_string()),
    };
    Ok(PreparedEdit {
        path: to,
        new_bytes: Vec::new(),
        expected_hash: Some(source),
        version: None,
        rename_from: Some(from),
        rename_destination: Some(destination),
    })
}

pub fn prepare_workspace_edit(
    context: &ToolContext,
    edit: &Value,
    encoding: PositionEncoding,
) -> Result<Vec<PreparedEdit>, String> {
    let mut prepared = Vec::new();
    if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            prepared.push(prepare_uri(context, uri, edits, encoding)?);
        }
    }
    if let Some(document_changes) = edit.get("documentChanges").and_then(Value::as_array) {
        for change in document_changes {
            if let Some(kind) = change.get("kind").and_then(Value::as_str) {
                if kind == "rename" {
                    let old = change
                        .get("oldUri")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "rename missing oldUri".to_string())?;
                    let new = change
                        .get("newUri")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "rename missing newUri".to_string())?;
                    let old_path = resolve_inbound_path(context, old)?;
                    let new_path = resolve_inbound_path_allow_create(context, new)?;
                    prepared.push(prepare_rename(old_path, new_path)?);
                    continue;
                }
                return Err(format!("resource operation {kind} is not applied"));
            }
            let uri = change
                .get("textDocument")
                .and_then(|td| td.get("uri"))
                .and_then(Value::as_str)
                .ok_or_else(|| "documentChanges entry missing uri".to_string())?;
            let version = change
                .pointer("/textDocument/version")
                .and_then(Value::as_i64);
            let edits = change
                .get("edits")
                .ok_or_else(|| "documentChanges entry missing edits".to_string())?;
            let mut prepared_one = prepare_uri(context, uri, edits, encoding)?;
            prepared_one.version = version;
            prepared.push(prepared_one);
        }
    }
    Ok(prepared)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EditApproval<'a> {
    path: &'a std::path::Path,
    rename_from: Option<&'a std::path::Path>,
    rename_destination: Option<&'a RenameDestination>,
    new_text: Option<&'a str>,
    expected_hash: Option<&'a str>,
    version: Option<i64>,
}

/// Obtain consent for every mutation before taking locks or writing any file.
/// Rename sources and destinations are independently bounded file targets.
pub async fn apply_prepared_edits(
    context: &ToolContext,
    client: &LspClient,
    prepared: &[PreparedEdit],
) -> Result<usize, ToolError> {
    if prepared.is_empty() {
        return Ok(0);
    }
    validate_edit_paths(context, prepared)?;
    let mut consents = Vec::new();
    for edit in prepared {
        let input = EditApproval {
            path: &edit.path,
            rename_from: edit.rename_from.as_deref(),
            rename_destination: edit.rename_destination.as_ref(),
            new_text: if edit.rename_from.is_some() {
                None
            } else {
                Some(std::str::from_utf8(&edit.new_bytes).map_err(policy_error)?)
            },
            expected_hash: edit.expected_hash.as_deref(),
            version: edit.version,
        };
        let mut paths = std::iter::once(&edit.path)
            .chain(edit.rename_from.iter())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        for path in paths {
            if let Some(consent) = crate::plugin::approval::workspace::authorize_file_mutation(
                "lsp_edit",
                &context.root(),
                path,
                &input,
            )
            .await
            .map_err(policy_error)?
            {
                consents.push(consent);
            }
        }
    }
    let _guards = acquire_mutation_locks(prepared).await;
    for consent in consents {
        consent.validate().map_err(policy_error)?;
    }
    apply_prepared_with_held_locks(context, client, prepared).await
}

fn policy_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::reported_failure(FailureClass::PolicyDenied, error.to_string())
}

fn validate_edit_paths(context: &ToolContext, prepared: &[PreparedEdit]) -> Result<(), ToolError> {
    for edit in prepared {
        let path = edit
            .path
            .to_str()
            .ok_or_else(|| policy_error("the edit path is not valid UTF-8"))?;
        let current = context
            .resolve_path_allow_create(path)
            .map_err(policy_error)?;
        if current != edit.path
            || (current.try_exists().map_err(policy_error)? && !current.is_file())
        {
            return Err(policy_error(
                "The edit target changed. Retry the language-server action.",
            ));
        }
        if let Some(from) = &edit.rename_from {
            let path = from
                .to_str()
                .ok_or_else(|| policy_error("the rename path is not valid UTF-8"))?;
            let current = context.resolve_existing_file(path).map_err(policy_error)?;
            if current != *from {
                return Err(policy_error(
                    "The rename source changed. Retry the language-server action.",
                ));
            }
        }
    }
    Ok(())
}

pub async fn acquire_mutation_locks(
    prepared: &[PreparedEdit],
) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
    let mut keys: Vec<PathBuf> = prepared
        .iter()
        .flat_map(|edit| {
            let mut paths = vec![edit.path.clone()];
            if let Some(from) = &edit.rename_from {
                paths.push(from.clone());
            }
            paths
        })
        .collect();
    keys.sort();
    keys.dedup();
    let mut guards = Vec::new();
    for path in &keys {
        guards.push(file_mutation_lock_for(path).lock_owned().await);
    }
    guards
}

/// Foreground edits obtain workspace consent before locking. Format-on-write
/// inherits its native file mutation and may only rewrite that same file.
pub(super) async fn apply_prepared_with_held_locks(
    context: &ToolContext,
    client: &LspClient,
    prepared: &[PreparedEdit],
) -> Result<usize, ToolError> {
    validate_edit_paths(context, prepared)?;
    // Validate the entire edit set while all mutation locks are held, before
    // the first write: a stale hash/version in a later file must never leave
    // earlier files rewritten.
    for edit in prepared {
        if let Some(expected) = &edit.expected_hash {
            let checked_path = edit.rename_from.as_ref().unwrap_or(&edit.path);
            let current = file_content_hash(checked_path).map_err(|err| {
                ToolError::reported_failure(FailureClass::ToolReturnedError, err.to_string())
            })?;
            if &current != expected {
                return Err(ToolError::reported_failure(
                    FailureClass::ArgumentInvalid,
                    format!(
                        "{} changed between preflight and write",
                        context.display_path(checked_path)
                    ),
                ));
            }
        }
        if edit.rename_from.is_some() {
            let actual = match file_content_hash(&edit.path) {
                Ok(hash) => RenameDestination::ContentHash(hash),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    RenameDestination::Missing
                }
                Err(error) => {
                    return Err(ToolError::reported_failure(
                        FailureClass::ToolReturnedError,
                        error.to_string(),
                    ))
                }
            };
            let unchanged = match (&edit.rename_destination, actual) {
                (Some(RenameDestination::Missing), RenameDestination::Missing) => true,
                (
                    Some(RenameDestination::ContentHash(expected)),
                    RenameDestination::ContentHash(actual),
                ) => *expected == actual,
                _ => false,
            };
            if !unchanged {
                return Err(ToolError::reported_failure(
                    FailureClass::ArgumentInvalid,
                    format!(
                        "{} changed between preflight and rename",
                        context.display_path(&edit.path)
                    ),
                ));
            }
        }
        if let Some(version) = edit.version {
            let uri = super::uri::path_to_file_uri(&edit.path);
            if let Some(tracked) = client.tracked_version(&uri).await {
                if tracked != version {
                    return Err(ToolError::reported_failure(
                        FailureClass::ArgumentInvalid,
                        format!(
                            "document version mismatch for {}",
                            context.display_path(&edit.path)
                        ),
                    ));
                }
            }
        }
    }

    let mut applied = 0usize;
    for edit in prepared {
        if let Some(parent) = edit.path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                ToolError::reported_failure(FailureClass::ToolReturnedError, err.to_string())
            })?;
        }
        if let Some(from) = &edit.rename_from {
            std::fs::rename(from, &edit.path).map_err(|err| {
                ToolError::reported_failure(FailureClass::ToolReturnedError, err.to_string())
            })?;
        } else {
            std::fs::write(&edit.path, &edit.new_bytes).map_err(|err| {
                ToolError::reported_failure(FailureClass::ToolReturnedError, err.to_string())
            })?;
        }
        applied += 1;
    }
    Ok(applied)
}

const URI_KEYS: &[&str] = &["uri", "targetUri", "newUri", "oldUri"];

/// Rewrite structured URI fields; out-of-root, malformed, and unsupported
/// scheme URIs are omitted. Returns the rewritten value and how many
/// locations were dropped.
pub fn redact_structured_uris(context: &ToolContext, value: &Value) -> (Value, usize) {
    let mut omitted = 0usize;
    let redacted = redact_value(context, value, &mut omitted);
    (redacted, omitted)
}

fn redact_value(context: &ToolContext, value: &Value, omitted: &mut usize) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, child) in map {
                if URI_KEYS.contains(&key.as_str()) {
                    if let Some(uri) = child.as_str() {
                        match redact_outside_root(context, uri) {
                            Some(display) => {
                                out.insert(key.clone(), Value::String(display));
                            }
                            None => {
                                *omitted += 1;
                            }
                        }
                        continue;
                    }
                }
                out.insert(key.clone(), redact_value(context, child, omitted));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_value(context, item, omitted))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn walk_uris(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk_uris_inner(value, &mut out);
    out
}

fn walk_uris_inner(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if URI_KEYS.contains(&key.as_str()) {
                    if let Some(uri) = child.as_str() {
                        out.push(uri.to_string());
                    }
                }
                walk_uris_inner(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_uris_inner(item, out);
            }
        }
        _ => {}
    }
}

fn prepare_uri(
    context: &ToolContext,
    uri: &str,
    edits: &Value,
    encoding: PositionEncoding,
) -> Result<PreparedEdit, String> {
    let path = file_uri_to_path(uri)?;
    let resolved = context
        .resolve_path(&path.to_string_lossy())
        .map_err(|err| err.to_string())?;
    let original = std::fs::read_to_string(&resolved).map_err(|err| err.to_string())?;
    let new_text = apply_text_edits(&original, edits, encoding)?;
    Ok(PreparedEdit {
        path: resolved,
        new_bytes: new_text.into_bytes(),
        expected_hash: Some(content_hash(original.as_bytes())),
        version: None,
        rename_from: None,
        rename_destination: None,
    })
}

pub fn file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    super::uri::file_uri_to_path(uri)
}

fn apply_text_edits(
    original: &str,
    edits: &Value,
    encoding: PositionEncoding,
) -> Result<String, String> {
    let Some(edits) = edits.as_array() else {
        return Err("edits must be an array".into());
    };
    let mut ranges: Vec<(usize, usize, String)> = Vec::new();
    for edit in edits {
        let new_text = edit
            .get("newText")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let range = edit
            .get("range")
            .ok_or_else(|| "text edit missing range".to_string())?;
        let start = lsp_offset(original, range.get("start"), encoding)?;
        let end = lsp_offset(original, range.get("end"), encoding)?;
        if start > end {
            return Err("text edit range start after end".into());
        }
        ranges.push((start, end, new_text));
    }
    ranges.sort_by_key(|(start, _, _)| *start);
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err("overlapping text edits".into());
        }
    }
    let mut out = String::new();
    let mut cursor = 0usize;
    for (start, end, text) in ranges {
        out.push_str(&original[cursor..start]);
        out.push_str(&text);
        cursor = end;
    }
    out.push_str(&original[cursor..]);
    Ok(out)
}

fn lsp_offset(
    text: &str,
    pos: Option<&Value>,
    encoding: PositionEncoding,
) -> Result<usize, String> {
    let pos = pos.ok_or_else(|| "missing position".to_string())?;
    let line = pos.get("line").and_then(Value::as_u64).unwrap_or(0) as usize;
    let character = pos.get("character").and_then(Value::as_u64).unwrap_or(0) as u32;
    let mut offset = 0usize;
    for (idx, text_line) in text.split_inclusive('\n').enumerate() {
        if idx == line {
            let line_body = text_line.trim_end_matches(['\n', '\r']);
            return Ok(offset + position_to_byte_offset(line_body, encoding, character)?);
        }
        offset += text_line.len();
    }
    Ok(text.len())
}

pub fn resolve_inbound_path(
    context: &ToolContext,
    file: &str,
) -> Result<std::path::PathBuf, String> {
    let path = inbound_path(file)?;
    context
        .resolve_path(&path.to_string_lossy())
        .map_err(|err| err.to_string())
}

pub fn resolve_inbound_path_allow_create(
    context: &ToolContext,
    file: &str,
) -> Result<std::path::PathBuf, String> {
    let path = inbound_path(file)?;
    context
        .resolve_path_allow_create(&path.to_string_lossy())
        .map_err(|err| err.to_string())
}

fn inbound_path(file: &str) -> Result<std::path::PathBuf, String> {
    if file.starts_with("file:") {
        super::uri::file_uri_to_path(file)
    } else if has_uri_scheme(file) {
        Err(format!("unsupported URI scheme in {file}"))
    } else {
        Ok(std::path::PathBuf::from(file))
    }
}

fn has_uri_scheme(value: &str) -> bool {
    let Some((scheme, _)) = value.split_once(':') else {
        return false;
    };
    if cfg!(windows) && scheme.len() == 1 && scheme.as_bytes()[0].is_ascii_alphabetic() {
        return false;
    }
    !scheme.is_empty()
        && scheme.as_bytes()[0].is_ascii_alphabetic()
        && scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

pub fn redact_outside_root(context: &ToolContext, uri: &str) -> Option<String> {
    resolve_inbound_path(context, uri)
        .ok()
        .map(|resolved| context.display_path(&resolved))
}
