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
                "List files and directories under the allowed root ({}). Returns concise JSON and skips common generated directories by default.",
                self.context.root().display()
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "recursive": { "type": "boolean" },
                    "max_entries": { "type": "integer" }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let dir = self.context.resolve_existing_dir(args.path.as_deref())?;
        let entries = collect_entries(
            &self.context,
            &dir,
            args.recursive,
            args.max_entries.max(1).min(self.default_max_entries.max(1)),
        )?;

        Ok(render_json(&ListFilesOutput {
            path: self.context.display_path(&dir),
            recursive: args.recursive,
            returned_entries: entries.items.len(),
            truncated: entries.truncated,
            default_ignored: default_ignored_names(),
            summary: summarize_entries(&entries.items),
            entries: entries.items,
        })?)
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
                "Read a UTF-8 text file under the allowed root ({}). Returns concise JSON with line metadata.",
                self.context.root().display()
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" },
                    "max_chars": { "type": "integer" }
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

        Ok(render_json(&ReadFileOutput {
            path: self.context.display_path(&path),
            start_line: rendered.start_line,
            end_line: rendered.end_line,
            total_lines: rendered.total_lines,
            returned_lines: rendered.returned_lines,
            truncated: rendered.content.chars().count() > max_chars,
            content: truncate_text(&rendered.content, max_chars),
        })?)
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
            description: "Find files matching a glob pattern under the allowed root. Returns concise JSON and skips common generated directories by default.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "max_matches": { "type": "integer" }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let dir = self.context.resolve_existing_dir(args.path.as_deref())?;
        let pattern = Pattern::new(&args.pattern)
            .with_context(|| format!("invalid glob pattern {}", args.pattern))?;
        let matches = collect_glob_matches(
            &self.context,
            &dir,
            &pattern,
            args.max_matches.min(self.default_max_matches).max(1),
        )?;

        Ok(render_json(&GlobOutput {
            pattern: args.pattern,
            path: self.context.display_path(&dir),
            returned_matches: matches.items.len(),
            truncated: matches.truncated,
            default_ignored: default_ignored_names(),
            matches: matches.items,
        })?)
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
            description: "Search text files under the allowed root for a substring. Returns concise JSON and skips common generated directories by default.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "case_sensitive": { "type": "boolean" },
                    "max_matches": { "type": "integer" }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let dir = self.context.resolve_existing_dir(args.path.as_deref())?;
        let max_matches = args.max_matches.min(self.default_max_matches).max(1);
        let collected = collect_grep_matches(
            &self.context,
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

        Ok(render_json(&GrepOutput {
            pattern: args.pattern,
            path: self.context.display_path(&dir),
            case_sensitive: args.case_sensitive,
            returned_matches: matches.len(),
            files_with_matches: files_with_matches.len(),
            truncated: collected.truncated,
            default_ignored: default_ignored_names(),
            matches,
        })?)
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
            description: "Write full file contents under the configured root.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let path = self.context.resolve_path_allow_create(&args.path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, args.content.as_bytes()).await?;

        Ok(format!(
            "wrote {} bytes to {}",
            args.content.len(),
            self.context.display_path(&path)
        ))
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
            description: "Replace text in an existing file under the configured root.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_text": { "type": "string" },
                    "new_text": { "type": "string" },
                    "replace_all": { "type": "boolean" }
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

        Ok(format!(
            "edited {} ({} replacement{})",
            self.context.display_path(&path),
            if args.replace_all { replacements } else { 1 },
            if args.replace_all && replacements != 1 {
                "s"
            } else {
                ""
            }
        ))
    }
}

#[derive(Serialize)]
struct EntrySummary {
    files: usize,
    directories: usize,
}

#[derive(Serialize)]
struct ListFilesOutput {
    path: String,
    recursive: bool,
    returned_entries: usize,
    truncated: bool,
    default_ignored: &'static [&'static str],
    summary: EntrySummary,
    entries: Vec<FilesystemEntry>,
}

#[derive(Serialize)]
struct ReadFileOutput {
    path: String,
    start_line: usize,
    end_line: usize,
    total_lines: usize,
    returned_lines: usize,
    truncated: bool,
    content: String,
}

#[derive(Serialize)]
struct GlobOutput {
    pattern: String,
    path: String,
    returned_matches: usize,
    truncated: bool,
    default_ignored: &'static [&'static str],
    matches: Vec<FilesystemEntry>,
}

#[derive(Serialize)]
struct GrepOutput {
    pattern: String,
    path: String,
    case_sensitive: bool,
    returned_matches: usize,
    files_with_matches: usize,
    truncated: bool,
    default_ignored: &'static [&'static str],
    matches: Vec<GrepOutputMatch>,
}

#[derive(Serialize)]
struct GrepOutputMatch {
    path: String,
    line_number: usize,
    preview: String,
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

fn render_json(value: &impl Serialize) -> Result<String> {
    serde_json::to_string_pretty(value).context("serializing tool output")
}
