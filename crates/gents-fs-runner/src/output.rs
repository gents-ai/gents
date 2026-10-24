use serde::Serialize;

use crate::model::{FilesystemEntry, WalkStats, DEFAULT_IGNORED_NAMES};

const DEFAULT_GREP_PREVIEW_CHARS: usize = 240;

pub const MAX_RESPONSE_BYTES: usize = 1536 * 1024;

pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

const MAX_ECHOED_PATTERN_CHARS: usize = 1024;

const MAX_ERROR_CHARS: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error("measuring runner response")]
    Measure(#[source] serde_json::Error),
    #[error("result metadata alone exceeds the {budget}-byte runner response budget")]
    MetadataExceedsBudget { budget: usize },
    #[error("serializing tool output")]
    Serialize(#[source] serde_json::Error),
}

type Result<T, E = OutputError> = std::result::Result<T, E>;

pub(crate) fn echoed_pattern(pattern: &str) -> String {
    truncate_inline(pattern, MAX_ECHOED_PATTERN_CHARS)
}

/// The error text is the source chain joined by `": "`.
pub fn error_response_line(error: &dyn std::error::Error) -> String {
    let chain = std::iter::successors(Some(error), |error| error.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    let message = truncate_inline(&chain, MAX_ERROR_CHARS);
    let mut line = serde_json::to_string(&crate::protocol::NativeFsRunnerResponse {
        ok: false,
        output: None,
        error: Some(message),
    })
    .unwrap_or_else(|_| r#"{"ok":false,"output":null,"error":"runner error"}"#.to_owned());
    line.push('\n');
    line
}

pub(crate) fn encoded_response_len(output: &str) -> Result<usize> {
    let response = crate::protocol::NativeFsRunnerResponse {
        ok: true,
        output: Some(output.to_owned()),
        error: None,
    };
    Ok(serde_json::to_string(&response)
        .map_err(OutputError::Measure)?
        .len()
        + 1)
}

pub(crate) fn render_within_budget<T>(
    items: &[T],
    truncated: bool,
    budget: usize,
    render: impl Fn(&[T], bool) -> Result<String>,
) -> Result<String> {
    let full = render(items, truncated)?;
    if encoded_response_len(&full)? <= budget {
        return Ok(full);
    }
    let (mut fits, mut exceeds) = (0usize, items.len());
    while exceeds - fits > 1 {
        let mid = fits + (exceeds - fits) / 2;
        if encoded_response_len(&render(&items[..mid], true)?)? <= budget {
            fits = mid;
        } else {
            exceeds = mid;
        }
    }
    let cut = render(&items[..fits], true)?;
    if encoded_response_len(&cut)? > budget {
        return Err(OutputError::MetadataExceedsBudget { budget });
    }
    Ok(cut)
}
const OUTPUT_META_PREFIX: &str = "gents_fs: ";

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
    pub(crate) walk: WalkStats,
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
    pub(crate) pattern_prefix: Option<String>,
    pub(crate) pattern_prefix_exists: bool,
    pub(crate) search_dir_entries: Option<Vec<String>>,
    pub(crate) path: String,
    pub(crate) returned_count: usize,
    pub(crate) total_count: Option<usize>,
    pub(crate) truncated: bool,
    pub(crate) default_ignored: &'static [&'static str],
    pub(crate) walk: WalkStats,
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
    pub(crate) pattern_syntax: &'static str,
    pub(crate) search_dir_entries: Option<Vec<String>>,
    pub(crate) path: String,
    pub(crate) case_sensitive: bool,
    pub(crate) returned_count: usize,
    pub(crate) total_count: Option<usize>,
    pub(crate) files_with_matches: usize,
    pub(crate) truncated: bool,
    pub(crate) default_ignored: &'static [&'static str],
    pub(crate) bytes_read: u64,
    pub(crate) skipped_large_files: usize,
    pub(crate) skipped_binary_files: usize,
    pub(crate) walk: WalkStats,
}

#[derive(Serialize)]
pub(crate) struct GrepOutput {
    #[serde(flatten)]
    pub(crate) metadata: GrepMetadata,
    pub(crate) matches: Vec<GrepOutputMatch>,
}

#[derive(Clone, Serialize)]
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
    serde_json::to_string(value).map_err(OutputError::Serialize)
}

fn truncate_inline(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}... [truncated]")
}
