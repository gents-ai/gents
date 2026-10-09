use anyhow::{Context, Result};
use serde::Serialize;

use crate::model::{FilesystemEntry, WalkStats, DEFAULT_IGNORED_NAMES};

const DEFAULT_GREP_PREVIEW_CHARS: usize = 240;

/// Largest response the runner writes. The host reads at most 2 MiB of runner
/// stdout (`MAX_NATIVE_RUNNER_OUTPUT_BYTES`) and a longer response fails to
/// decode instead of truncating, so results are cut here. Measured on the
/// encoded response, after both JSON encodings (`raw_json` output is itself
/// JSON), with headroom for the envelope and trailing newline.
pub const MAX_RESPONSE_BYTES: usize = 1536 * 1024;

/// Largest request the runner reads from stdin. Requests carry a pattern and
/// a path, never file contents; a larger request is refused, not truncated.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

/// Characters of a caller-supplied pattern echoed back in result metadata.
const MAX_ECHOED_PATTERN_CHARS: usize = 1024;

/// Characters of an error message the runner reports. Escaping can grow each
/// character to at most six bytes, so the error envelope stays far below
/// `MAX_RESPONSE_BYTES`.
const MAX_ERROR_CHARS: usize = 64 * 1024;

/// Echo a caller-supplied pattern in metadata, cut to a bounded prefix.
pub(crate) fn echoed_pattern(pattern: &str) -> String {
    truncate_inline(pattern, MAX_ECHOED_PATTERN_CHARS)
}

/// The response line (with trailing newline) reporting `error`, bounded.
pub fn error_response_line(error: &anyhow::Error) -> String {
    let message = truncate_inline(&format!("{error:#}"), MAX_ERROR_CHARS);
    let mut line = serde_json::to_string(&crate::protocol::NativeFsRunnerResponse {
        ok: false,
        output: None,
        error: Some(message),
    })
    .unwrap_or_else(|_| r#"{"ok":false,"output":null,"error":"runner error"}"#.to_owned());
    line.push('\n');
    line
}

/// Bytes of the `NativeFsRunnerResponse` line the runner writes for `output`.
pub(crate) fn encoded_response_len(output: &str) -> Result<usize> {
    let response = crate::protocol::NativeFsRunnerResponse {
        ok: true,
        output: Some(output.to_owned()),
        error: None,
    };
    Ok(serde_json::to_string(&response)
        .context("measuring runner response")?
        .len()
        + 1)
}

/// Render `items`, cutting to the longest prefix whose encoded response fits
/// `budget`; a cut result reports `truncated`. Rendered length grows with the
/// prefix, so the cut is found by bisection.
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
    anyhow::ensure!(
        encoded_response_len(&cut)? <= budget,
        "result metadata alone exceeds the {budget}-byte runner response budget"
    );
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
    serde_json::to_string(value).context("serializing tool output")
}

fn truncate_inline(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}... [truncated]")
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn zero_item_render_over_budget_fails_clearly() {
        let error = render_within_budget(&[1u8, 2, 3], false, 64, |items, _| {
            Ok(format!("{}{}", "m".repeat(200), items.len()))
        })
        .unwrap_err();
        assert!(
            error.to_string().contains("result metadata alone exceeds"),
            "{error}"
        );
        let error =
            render_within_budget::<u8>(&[], false, 64, |_, _| Ok("m".repeat(200))).unwrap_err();
        assert!(error.to_string().contains("exceeds the 64-byte"), "{error}");
    }
}
