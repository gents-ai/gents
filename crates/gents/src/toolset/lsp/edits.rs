use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::toolset::shared::ToolContext;

#[derive(Debug, Clone)]
pub struct PreparedEdit {
    pub path: PathBuf,
    pub new_bytes: Vec<u8>,
}

pub fn prepare_workspace_edit(
    context: &ToolContext,
    edit: &Value,
) -> Result<Vec<PreparedEdit>, String> {
    let mut prepared = Vec::new();
    if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            prepared.push(prepare_uri(context, uri, edits)?);
        }
    }
    if let Some(document_changes) = edit.get("documentChanges").and_then(Value::as_array) {
        for change in document_changes {
            if change.get("kind").is_some() {
                return Err("resource operations are not applied without preflight create/rename/delete support in this call".into());
            }
            let uri = change
                .get("textDocument")
                .and_then(|td| td.get("uri"))
                .and_then(Value::as_str)
                .ok_or_else(|| "documentChanges entry missing uri".to_string())?;
            let edits = change
                .get("edits")
                .ok_or_else(|| "documentChanges entry missing edits".to_string())?;
            prepared.push(prepare_uri(context, uri, edits)?);
        }
    }
    Ok(prepared)
}

fn prepare_uri(context: &ToolContext, uri: &str, edits: &Value) -> Result<PreparedEdit, String> {
    let path = file_uri_to_path(uri)?;
    let resolved = context
        .resolve_path(&path.to_string_lossy())
        .map_err(|err| err.to_string())?;
    let original = std::fs::read_to_string(&resolved).map_err(|err| err.to_string())?;
    let new_text = apply_text_edits(&original, edits)?;
    Ok(PreparedEdit {
        path: resolved,
        new_bytes: new_text.into_bytes(),
    })
}

pub fn file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| format!("unsupported URI scheme: {uri}"))?;
    Ok(PathBuf::from(rest))
}

fn apply_text_edits(original: &str, edits: &Value) -> Result<String, String> {
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
        let start = lsp_offset(original, range.get("start"))?;
        let end = lsp_offset(original, range.get("end"))?;
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

fn lsp_offset(text: &str, pos: Option<&Value>) -> Result<usize, String> {
    let pos = pos.ok_or_else(|| "missing position".to_string())?;
    let line = pos.get("line").and_then(Value::as_u64).unwrap_or(0) as usize;
    let character = pos.get("character").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut offset = 0usize;
    for (idx, text_line) in text.split_inclusive('\n').enumerate() {
        if idx == line {
            return Ok(offset + character.min(text_line.len()));
        }
        offset += text_line.len();
    }
    Ok(text.len())
}

pub fn apply_prepared(edits: &[PreparedEdit]) -> Result<(), String> {
    for edit in edits {
        if let Some(parent) = edit.path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        std::fs::write(&edit.path, &edit.new_bytes).map_err(|err| err.to_string())?;
    }
    Ok(())
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
