use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::tool_call_lifecycle::FailureClass;
use super::super::file_tools::{content_hash, file_mutation_lock_for};
use crate::toolset::shared::{ToolContext, ToolError};

use super::client::LspClient;
use super::encoding::{position_to_byte_offset, PositionEncoding};

#[derive(Debug, Clone)]
pub struct PreparedEdit {
    pub path: PathBuf,
    pub new_bytes: Vec<u8>,
    pub expected_hash: Option<String>,
    pub version: Option<i64>,
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
                    let new_path = resolve_inbound_path(context, new)?;
                    let bytes = std::fs::read(&old_path).map_err(|err| err.to_string())?;
                    prepared.push(PreparedEdit {
                        path: new_path,
                        new_bytes: bytes,
                        expected_hash: None,
                        version: None,
                    });
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

pub async fn apply_workspace_edit(
    context: &ToolContext,
    client: &LspClient,
    edit: &Value,
) -> Result<usize, ToolError> {
    let encoding = client.position_encoding().await;
    let prepared = prepare_workspace_edit(context, edit, encoding).map_err(|err| {
        ToolError::reported_failure(FailureClass::PolicyDenied, err)
    })?;
    if prepared.is_empty() {
        return Ok(0);
    }
    let mut keys: Vec<_> = prepared.iter().map(|edit| edit.path.clone()).collect();
    keys.sort();
    let mut guards = Vec::new();
    for path in &keys {
        guards.push(file_mutation_lock_for(path).lock_owned().await);
    }
    for edit in &prepared {
        if let Some(expected) = &edit.expected_hash {
            let current = std::fs::read(&edit.path).map_err(|err| {
                ToolError::reported_failure(FailureClass::ToolReturnedError, err.to_string())
            })?;
            if &content_hash(&current) != expected {
                return Err(ToolError::reported_failure(
                    FailureClass::ArgumentInvalid,
                    format!(
                        "{} changed between preflight and write",
                        context.display_path(&edit.path)
                    ),
                ));
            }
        }
        if let Some(version) = edit.version {
            let uri = format!("file://{}", edit.path.display());
            if let Some(tracked) = client.tracked_version(&uri).await {
                if tracked != version {
                    return Err(ToolError::reported_failure(
                        FailureClass::ArgumentInvalid,
                        format!("document version mismatch for {}", context.display_path(&edit.path)),
                    ));
                }
            }
        }
        if let Some(parent) = edit.path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                ToolError::reported_failure(FailureClass::ToolReturnedError, err.to_string())
            })?;
        }
        std::fs::write(&edit.path, &edit.new_bytes).map_err(|err| {
            ToolError::reported_failure(FailureClass::ToolReturnedError, err.to_string())
        })?;
    }
    drop(guards);
    Ok(prepared.len())
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
                if key == "uri" || key == "targetUri" || key == "newUri" || key == "oldUri" {
                    if let Some(uri) = child.as_str() {
                        if uri.starts_with("file:") {
                            out.push(uri.to_string());
                        }
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
    })
}

pub fn file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| format!("unsupported URI scheme: {uri}"))?;
    Ok(PathBuf::from(rest))
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

fn lsp_offset(text: &str, pos: Option<&Value>, encoding: PositionEncoding) -> Result<usize, String> {
    let pos = pos.ok_or_else(|| "missing position".to_string())?;
    let line = pos.get("line").and_then(Value::as_u64).unwrap_or(0) as usize;
    let character = pos.get("character").and_then(Value::as_u64).unwrap_or(0) as u32;
    let mut offset = 0usize;
    for (idx, text_line) in text.split_inclusive('\n').enumerate() {
        if idx == line {
            let line_body = text_line.trim_end_matches(['\n', '\r']);
            return Ok(offset + position_to_byte_offset(line_body, encoding, character));
        }
        offset += text_line.len();
    }
    Ok(text.len())
}

pub fn resolve_inbound_path(context: &ToolContext, file: &str) -> Result<std::path::PathBuf, String> {
    let path = if let Some(rest) = file.strip_prefix("file://") {
        rest
    } else {
        file
    };
    context.resolve_path(path).map_err(|err| err.to_string())
}

pub fn redact_outside_root(context: &ToolContext, uri: &str) -> Option<String> {
    let path = file_uri_to_path(uri).ok()?;
    match context.resolve_path(&path.to_string_lossy()) {
        Ok(resolved) => Some(context.display_path(&resolved)),
        Err(_) => None,
    }
}
