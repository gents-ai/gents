use anyhow::{Context, Result};
use serde::Serialize;

use crate::model::{FilesystemEntry, DEFAULT_IGNORED_NAMES};

const DEFAULT_GREP_PREVIEW_CHARS: usize = 240;
const OUTPUT_META_PREFIX: &str = "defra_fs: ";

#[derive(Serialize)]
pub(crate) struct EntrySummary {
    files: usize,
    directories: usize,
}

#[derive(Serialize)]
pub(crate) struct ListFilesMetadata {
    pub(crate) ok: bool,
    pub(crate) status: &'static str,
    pub(crate) tool: &'static str,
    pub(crate) path: String,
    pub(crate) recursive: bool,
    pub(crate) returned_count: usize,
    pub(crate) total_count: Option<usize>,
    pub(crate) truncated: bool,
    pub(crate) default_ignored: &'static [&'static str],
    pub(crate) summary: EntrySummary,
}

#[derive(Serialize)]
pub(crate) struct ListFilesOutput {
    #[serde(flatten)]
    pub(crate) metadata: ListFilesMetadata,
    pub(crate) entries: Vec<FilesystemEntry>,
}

#[derive(Serialize)]
pub(crate) struct GlobMetadata {
    pub(crate) ok: bool,
    pub(crate) status: &'static str,
    pub(crate) tool: &'static str,
    pub(crate) pattern: String,
    pub(crate) path: String,
    pub(crate) returned_count: usize,
    pub(crate) total_count: Option<usize>,
    pub(crate) truncated: bool,
    pub(crate) default_ignored: &'static [&'static str],
}

#[derive(Serialize)]
pub(crate) struct GlobOutput {
    #[serde(flatten)]
    pub(crate) metadata: GlobMetadata,
    pub(crate) matches: Vec<FilesystemEntry>,
}

#[derive(Serialize)]
pub(crate) struct GrepMetadata {
    pub(crate) ok: bool,
    pub(crate) status: &'static str,
    pub(crate) tool: &'static str,
    pub(crate) pattern: String,
    pub(crate) path: String,
    pub(crate) case_sensitive: bool,
    pub(crate) returned_count: usize,
    pub(crate) total_count: Option<usize>,
    pub(crate) files_with_matches: usize,
    pub(crate) truncated: bool,
    pub(crate) default_ignored: &'static [&'static str],
}

#[derive(Serialize)]
pub(crate) struct GrepOutput {
    #[serde(flatten)]
    pub(crate) metadata: GrepMetadata,
    pub(crate) matches: Vec<GrepOutputMatch>,
}

#[derive(Serialize)]
pub(crate) struct GrepOutputMatch {
    pub(crate) path: String,
    pub(crate) line_number: usize,
    pub(crate) preview: String,
}

pub(crate) fn summarize_entries(entries: &[FilesystemEntry]) -> EntrySummary {
    let mut files = 0;
    let mut directories = 0;
    for entry in entries {
        match entry.entry_type {
            "file" => files += 1,
            "directory" => directories += 1,
            _ => {}
        }
    }
    EntrySummary { files, directories }
}

pub(crate) fn total_count(returned_count: usize, truncated: bool) -> Option<usize> {
    (!truncated).then_some(returned_count)
}

pub(crate) fn format_entries(label: &str, entries: &[FilesystemEntry]) -> String {
    let mut out = String::from(label);
    out.push(':');
    if entries.is_empty() {
        out.push_str("\n(none)");
        return out;
    }
    for entry in entries {
        out.push('\n');
        out.push_str(entry.entry_type);
        out.push(' ');
        out.push_str(&entry.path);
    }
    out
}

pub(crate) fn format_grep_matches(matches: &[GrepOutputMatch]) -> String {
    let mut out = String::from("matches:");
    if matches.is_empty() {
        out.push_str("\n(none)");
        return out;
    }
    for entry in matches {
        out.push('\n');
        out.push_str(&entry.path);
        out.push_str(":L");
        out.push_str(&entry.line_number.to_string());
        out.push_str(": ");
        out.push_str(&entry.preview);
    }
    out
}

pub(crate) fn render_tool_output(
    metadata: &impl Serialize,
    body: String,
    raw_value: &impl Serialize,
    raw_json: bool,
) -> Result<String> {
    if raw_json {
        return render_json(raw_value);
    }
    let mut out = String::from(OUTPUT_META_PREFIX);
    out.push_str(&render_json(metadata)?);
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body);
    }
    Ok(out)
}

pub(crate) fn truncate_grep_preview(text: &str) -> String {
    truncate_inline(text, DEFAULT_GREP_PREVIEW_CHARS)
}

pub(crate) fn default_ignored_names() -> &'static [&'static str] {
    DEFAULT_IGNORED_NAMES
}

fn render_json(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).context("serializing tool output")
}

fn truncate_inline(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}... [truncated]")
}
