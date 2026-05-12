// Soft-cap justified: six filesystem tool structs (list, read, write, edit,
// glob, grep) that share private helpers defined in the same file. Splitting
// each tool into its own file would scatter the shared defaults and make
// cross-tool consistency harder to verify.
use std::collections::BTreeSet;

use anyhow::{anyhow, Context as _, Result};
use glob::Pattern;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::Serialize;

use super::args::{EditFileArgs, GlobArgs, GrepArgs, ListFilesArgs, ReadFileArgs, WriteFileArgs};
use super::shared::{
    collect_entries, collect_glob_matches, collect_grep_matches, default_ignored_names,
    render_file_contents, truncate_inline, truncate_text, FilesystemEntry, ToolContext, ToolError,
};

const DEFAULT_GREP_PREVIEW_CHARS: usize = 240;
const OUTPUT_META_PREFIX: &str = "defra_fs: ";

async fn run_filesystem_boundary<F, T>(operation: F) -> Result<T, ToolError>
where
    F: FnOnce() -> Result<T, ToolError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| ToolError(anyhow!("filesystem tool boundary failed: {error}")))?
}

#[derive(Clone)]
pub(super) struct ListFilesTool {
    context: ToolContext,
    default_max_entries: usize,
}

impl ListFilesTool {
    pub(super) fn new(context: ToolContext, default_max_entries: usize) -> Self {
        Self {
            context,
            default_max_entries,
        }
    }
}

#[derive(Clone)]
pub(super) struct ReadFileTool {
    context: ToolContext,
    default_max_chars: usize,
}

impl ReadFileTool {
    pub(super) fn new(context: ToolContext, default_max_chars: usize) -> Self {
        Self {
            context,
            default_max_chars,
        }
    }
}

#[derive(Clone)]
pub(super) struct GlobTool {
    context: ToolContext,
    default_max_matches: usize,
}

impl GlobTool {
    pub(super) fn new(context: ToolContext, default_max_matches: usize) -> Self {
        Self {
            context,
            default_max_matches,
        }
    }
}

#[derive(Clone)]
pub(super) struct GrepTool {
    context: ToolContext,
    default_max_matches: usize,
}

impl GrepTool {
    pub(super) fn new(context: ToolContext, default_max_matches: usize) -> Self {
        Self {
            context,
            default_max_matches,
        }
    }
}

#[derive(Clone)]
pub(super) struct WriteFileTool {
    context: ToolContext,
}

impl WriteFileTool {
    pub(super) fn new(context: ToolContext) -> Self {
        Self { context }
    }
}

#[derive(Clone)]
pub(super) struct EditFileTool {
    context: ToolContext,
}

impl EditFileTool {
    pub(super) fn new(context: ToolContext) -> Self {
        Self { context }
    }
}

impl Tool for ListFilesTool {
    const NAME: &'static str = "list_files";

    type Error = ToolError;
    type Args = ListFilesArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: format!(
                "List files and directories under the allowed root ({}). Returns compact text with stable defra_fs metadata and skips common generated directories by default. Set raw_json=true for structured JSON.",
                self.context.root().display()
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "default": ".",
                        "description": "Directory to list, relative to the allowed root. Omit or pass an empty string for the root."
                    },
                    "recursive": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, walk subdirectories while still skipping common generated directories."
                    },
                    "max_entries": {
                        "type": "integer",
                        "default": self.default_max_entries,
                        "minimum": 1,
                        "maximum": self.default_max_entries,
                        "description": "Maximum entries to return; higher values are capped by the tool."
                    },
                    "raw_json": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return structured JSON instead of the compact default text."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let context = self.context.clone();
        let default_max_entries = self.default_max_entries;

        run_filesystem_boundary(move || {
            let dir = context.resolve_existing_dir(args.path.as_deref())?;
            let entries = collect_entries(
                &context,
                &dir,
                args.recursive,
                args.max_entries.max(1).min(default_max_entries.max(1)),
            )?;

            let metadata = ListFilesMetadata {
                ok: true,
                status: "success",
                tool: Self::NAME,
                path: context.display_path(&dir),
                recursive: args.recursive,
                returned_count: entries.items.len(),
                total_count: total_count(entries.items.len(), entries.truncated),
                truncated: entries.truncated,
                default_ignored: default_ignored_names(),
                summary: summarize_entries(&entries.items),
            };
            let output = ListFilesOutput {
                metadata,
                entries: entries.items,
            };

            Ok(render_tool_output(
                &output.metadata,
                format_entries("entries", &output.entries),
                &output,
                args.raw_json,
            )?)
        })
        .await
    }
}

impl Tool for ReadFileTool {
    const NAME: &'static str = "read_file";

    type Error = ToolError;
    type Args = ReadFileArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: format!(
                "Read a UTF-8 text file under the allowed root ({}). Returns compact line-numbered text with stable defra_fs metadata. Set raw_json=true for structured JSON.",
                self.context.root().display()
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File to read, relative to the allowed root unless an allowed absolute path is provided."
                    },
                    "start_line": {
                        "type": "integer",
                        "default": 1,
                        "minimum": 1,
                        "description": "First 1-based line to return."
                    },
                    "end_line": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Last 1-based line to return. Omit to read through the file end."
                    },
                    "max_chars": {
                        "type": "integer",
                        "default": self.default_max_chars,
                        "minimum": 1,
                        "maximum": self.default_max_chars,
                        "description": "Maximum characters to return; higher values are capped by the tool."
                    },
                    "raw_json": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return structured JSON instead of the compact default text."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = self.context.resolve_existing_file(&args.path)?;
        let bytes = tokio::fs::read(&path).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let rendered = render_file_contents(&text, args.start_line, args.end_line);
        let max_chars = args.max_chars.min(self.default_max_chars).max(1);
        let content = truncate_text(&rendered.content, max_chars);

        let output = ReadFileOutput {
            metadata: ReadFileMetadata {
                ok: true,
                status: "success",
                tool: Self::NAME,
                path: self.context.display_path(&path),
                returned_count: rendered.returned_lines,
                total_count: Some(rendered.total_lines),
                truncated: rendered.content.chars().count() > max_chars,
                start_line: rendered.start_line,
                end_line: rendered.end_line,
            },
            content,
        };

        Ok(render_tool_output(
            &output.metadata,
            format!("content:\n{}", output.content),
            &output,
            args.raw_json,
        )?)
    }
}

impl Tool for GlobTool {
    const NAME: &'static str = "glob";

    type Error = ToolError;
    type Args = GlobArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Find files matching a glob pattern under the allowed root. Returns compact text with stable defra_fs metadata and skips common generated directories by default. Set raw_json=true for structured JSON.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern matched against paths displayed relative to the allowed root."
                    },
                    "path": {
                        "type": "string",
                        "default": ".",
                        "description": "Directory to search, relative to the allowed root. Omit or pass an empty string for the root."
                    },
                    "max_matches": {
                        "type": "integer",
                        "default": self.default_max_matches,
                        "minimum": 1,
                        "maximum": self.default_max_matches,
                        "description": "Maximum matching paths to return; higher values are capped by the tool."
                    },
                    "raw_json": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return structured JSON instead of the compact default text."
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let context = self.context.clone();
        let default_max_matches = self.default_max_matches;

        run_filesystem_boundary(move || {
            let dir = context.resolve_existing_dir(args.path.as_deref())?;
            let pattern = Pattern::new(&args.pattern)
                .with_context(|| format!("invalid glob pattern {}", args.pattern))?;
            let matches = collect_glob_matches(
                &context,
                &dir,
                &pattern,
                args.max_matches.min(default_max_matches).max(1),
            )?;

            let output = GlobOutput {
                metadata: GlobMetadata {
                    ok: true,
                    status: "success",
                    tool: Self::NAME,
                    pattern: args.pattern,
                    path: context.display_path(&dir),
                    returned_count: matches.items.len(),
                    total_count: total_count(matches.items.len(), matches.truncated),
                    truncated: matches.truncated,
                    default_ignored: default_ignored_names(),
                },
                matches: matches.items,
            };

            Ok(render_tool_output(
                &output.metadata,
                format_entries("matches", &output.matches),
                &output,
                args.raw_json,
            )?)
        })
        .await
    }
}

impl Tool for GrepTool {
    const NAME: &'static str = "grep";

    type Error = ToolError;
    type Args = GrepArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Search text files under the allowed root for a substring. Returns compact path:Lline matches with stable defra_fs metadata and skips common generated directories by default. Set raw_json=true for structured JSON.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Literal substring to search for in UTF-8 text files."
                    },
                    "path": {
                        "type": "string",
                        "default": ".",
                        "description": "Directory to search, relative to the allowed root. Omit or pass an empty string for the root."
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "default": false,
                        "description": "When false, match case-insensitively."
                    },
                    "max_matches": {
                        "type": "integer",
                        "default": self.default_max_matches,
                        "minimum": 1,
                        "maximum": self.default_max_matches,
                        "description": "Maximum matching lines to return; higher values are capped by the tool."
                    },
                    "raw_json": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return structured JSON instead of the compact default text."
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let context = self.context.clone();
        let default_max_matches = self.default_max_matches;

        run_filesystem_boundary(move || {
            let dir = context.resolve_existing_dir(args.path.as_deref())?;
            let max_matches = args.max_matches.min(default_max_matches).max(1);
            let collected = collect_grep_matches(
                &context,
                &dir,
                &args.pattern,
                args.case_sensitive,
                max_matches,
            )?;

            let mut files_with_matches = BTreeSet::new();
            let matches = collected
                .items
                .into_iter()
                .map(|entry| {
                    files_with_matches.insert(entry.path.clone());
                    GrepOutputMatch {
                        path: entry.path,
                        line_number: entry.line_number,
                        preview: truncate_inline(&entry.line, DEFAULT_GREP_PREVIEW_CHARS),
                    }
                })
                .collect::<Vec<_>>();

            let output = GrepOutput {
                metadata: GrepMetadata {
                    ok: true,
                    status: "success",
                    tool: Self::NAME,
                    pattern: args.pattern,
                    path: context.display_path(&dir),
                    case_sensitive: args.case_sensitive,
                    returned_count: matches.len(),
                    total_count: total_count(matches.len(), collected.truncated),
                    files_with_matches: files_with_matches.len(),
                    truncated: collected.truncated,
                    default_ignored: default_ignored_names(),
                },
                matches,
            };

            Ok(render_tool_output(
                &output.metadata,
                format_grep_matches(&output.matches),
                &output,
                args.raw_json,
            )?)
        })
        .await
    }
}

impl Tool for WriteFileTool {
    const NAME: &'static str = "write_file";

    type Error = ToolError;
    type Args = WriteFileArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Write full file contents under the configured root. Returns compact success metadata by default. Set raw_json=true for structured JSON.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File to create or overwrite, relative to the configured writable root unless an allowed absolute path is provided."
                    },
                    "content": {
                        "type": "string",
                        "description": "Complete file contents to write. Existing file contents are replaced."
                    },
                    "raw_json": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return structured JSON instead of the compact default text."
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = self.context.resolve_path_allow_create(&args.path)?;
        let created = !path.exists();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, args.content.as_bytes()).await?;

        let output = WriteFileOutput {
            metadata: WriteFileMetadata {
                ok: true,
                status: "success",
                tool: Self::NAME,
                path: self.context.display_path(&path),
                returned_count: 0,
                total_count: Some(0),
                truncated: false,
                bytes_written: args.content.len(),
                created,
            },
        };

        Ok(render_tool_output(
            &output.metadata,
            format!(
                "write_file: wrote {} bytes to {}",
                output.metadata.bytes_written, output.metadata.path
            ),
            &output,
            args.raw_json,
        )?)
    }
}

// Cancellation opt-ins for the read-only filesystem tools.
//
// `ToolError` in this crate is a newtype wrapper around `anyhow::Error`
// rather than an enum, so the cancellation error is constructed via
// `anyhow!(...)` and converted with `Into::into`. See
// `super::cancellable` for the dispatch-integration status — these
// impls are infrastructure-ready but not yet wired from production
// (rig owns tool dispatch today).
//
// Write/edit tools and bash tools deliberately keep the default
// (non-cancellable) behavior.

impl super::cancellable::CancellableTool for ListFilesTool {
    fn supports_cancellation(&self) -> bool {
        true
    }

    async fn call_cancellable(
        &self,
        args: Self::Args,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        tokio::select! {
            _ = cancel.cancelled() => Err(anyhow!("tool call cancelled").into()),
            result = self.call(args) => result,
        }
    }
}

impl super::cancellable::CancellableTool for ReadFileTool {
    fn supports_cancellation(&self) -> bool {
        true
    }

    async fn call_cancellable(
        &self,
        args: Self::Args,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        tokio::select! {
            _ = cancel.cancelled() => Err(anyhow!("tool call cancelled").into()),
            result = self.call(args) => result,
        }
    }
}

impl super::cancellable::CancellableTool for GlobTool {
    fn supports_cancellation(&self) -> bool {
        true
    }

    async fn call_cancellable(
        &self,
        args: Self::Args,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        tokio::select! {
            _ = cancel.cancelled() => Err(anyhow!("tool call cancelled").into()),
            result = self.call(args) => result,
        }
    }
}

impl super::cancellable::CancellableTool for GrepTool {
    fn supports_cancellation(&self) -> bool {
        true
    }

    async fn call_cancellable(
        &self,
        args: Self::Args,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Self::Output, Self::Error> {
        tokio::select! {
            _ = cancel.cancelled() => Err(anyhow!("tool call cancelled").into()),
            result = self.call(args) => result,
        }
    }
}

impl Tool for EditFileTool {
    const NAME: &'static str = "edit_file";

    type Error = ToolError;
    type Args = EditFileArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Replace text in an existing file under the configured root. Returns compact success metadata by default. Set raw_json=true for structured JSON.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Existing file to edit, relative to the configured writable root unless an allowed absolute path is provided."
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Exact text to replace. The call fails if this text is not found."
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Replacement text."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "default": false,
                        "description": "When false, replace only the first occurrence. When true, replace every occurrence."
                    },
                    "raw_json": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return structured JSON instead of the compact default text."
                    }
                },
                "required": ["path", "old_text", "new_text"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = self.context.resolve_existing_file(&args.path)?;
        let original = tokio::fs::read_to_string(&path).await?;
        let replacements = original.matches(&args.old_text).count();
        if replacements == 0 {
            return Err(anyhow!(
                "text to replace was not found in {}",
                self.context.display_path(&path)
            )
            .into());
        }

        let updated = if args.replace_all {
            original.replace(&args.old_text, &args.new_text)
        } else {
            original.replacen(&args.old_text, &args.new_text, 1)
        };
        tokio::fs::write(&path, updated.as_bytes()).await?;

        let replacements_applied = if args.replace_all { replacements } else { 1 };
        let output = EditFileOutput {
            metadata: EditFileMetadata {
                ok: true,
                status: "success",
                tool: Self::NAME,
                path: self.context.display_path(&path),
                returned_count: replacements_applied,
                total_count: Some(replacements),
                truncated: false,
                replacements_applied,
                replace_all: args.replace_all,
                bytes_written: updated.len(),
            },
        };

        Ok(render_tool_output(
            &output.metadata,
            format!(
                "edit_file: edited {} ({} replacement{})",
                output.metadata.path,
                output.metadata.replacements_applied,
                if output.metadata.replacements_applied != 1 {
                    "s"
                } else {
                    ""
                }
            ),
            &output,
            args.raw_json,
        )?)
    }
}

#[derive(Serialize)]
struct EntrySummary {
    files: usize,
    directories: usize,
}

#[derive(Serialize)]
struct ListFilesMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    path: String,
    recursive: bool,
    returned_count: usize,
    total_count: Option<usize>,
    truncated: bool,
    default_ignored: &'static [&'static str],
    summary: EntrySummary,
}

#[derive(Serialize)]
struct ListFilesOutput {
    #[serde(flatten)]
    metadata: ListFilesMetadata,
    entries: Vec<FilesystemEntry>,
}

#[derive(Serialize)]
struct ReadFileMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    path: String,
    returned_count: usize,
    total_count: Option<usize>,
    truncated: bool,
    start_line: usize,
    end_line: usize,
}

#[derive(Serialize)]
struct ReadFileOutput {
    #[serde(flatten)]
    metadata: ReadFileMetadata,
    content: String,
}

#[derive(Serialize)]
struct GlobMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    pattern: String,
    path: String,
    returned_count: usize,
    total_count: Option<usize>,
    truncated: bool,
    default_ignored: &'static [&'static str],
}

#[derive(Serialize)]
struct GlobOutput {
    #[serde(flatten)]
    metadata: GlobMetadata,
    matches: Vec<FilesystemEntry>,
}

#[derive(Serialize)]
struct GrepMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    pattern: String,
    path: String,
    case_sensitive: bool,
    returned_count: usize,
    total_count: Option<usize>,
    files_with_matches: usize,
    truncated: bool,
    default_ignored: &'static [&'static str],
}

#[derive(Serialize)]
struct GrepOutput {
    #[serde(flatten)]
    metadata: GrepMetadata,
    matches: Vec<GrepOutputMatch>,
}

#[derive(Serialize)]
struct GrepOutputMatch {
    path: String,
    line_number: usize,
    preview: String,
}

#[derive(Serialize)]
struct WriteFileMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    path: String,
    returned_count: usize,
    total_count: Option<usize>,
    truncated: bool,
    bytes_written: usize,
    created: bool,
}

#[derive(Serialize)]
struct WriteFileOutput {
    #[serde(flatten)]
    metadata: WriteFileMetadata,
}

#[derive(Serialize)]
struct EditFileMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    path: String,
    returned_count: usize,
    total_count: Option<usize>,
    truncated: bool,
    replacements_applied: usize,
    replace_all: bool,
    bytes_written: usize,
}

#[derive(Serialize)]
struct EditFileOutput {
    #[serde(flatten)]
    metadata: EditFileMetadata,
}

fn summarize_entries(entries: &[FilesystemEntry]) -> EntrySummary {
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

fn total_count(returned_count: usize, truncated: bool) -> Option<usize> {
    (!truncated).then_some(returned_count)
}

fn format_entries(label: &str, entries: &[FilesystemEntry]) -> String {
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

fn format_grep_matches(matches: &[GrepOutputMatch]) -> String {
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

fn render_tool_output(
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

fn render_json(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).context("serializing tool output")
}
