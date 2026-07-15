// Soft-cap justified: six filesystem tool structs (list, read, write, edit,
// glob, grep) that share private helpers defined in the same file. Splitting
// each tool into its own file would scatter the shared defaults and make
// cross-tool consistency harder to verify.
use crate::llm::tool::Tool;
use crate::llm::tool::ToolDefinition;
use anyhow::{anyhow, Context as _, Result};
use defra_native_fs_runner::protocol::{
    GlobArgs as NativeGlobArgs, GrepArgs as NativeGrepArgs, ListFilesArgs as NativeListFilesArgs,
    NativeFsRunnerRequest,
};
use serde::Serialize;

use super::args::{EditFileArgs, GlobArgs, GrepArgs, ListFilesArgs, ReadFileArgs, WriteFileArgs};
use super::native_runner::NativeFsRunner;
use super::shared::{cap_output, render_file_contents, ToolContext, ToolError};

const OUTPUT_META_PREFIX: &str = "defra_fs: ";

#[derive(Clone)]
pub(super) struct ListFilesTool {
    context: ToolContext,
    native_runner: NativeFsRunner,
    default_max_entries: usize,
}

impl ListFilesTool {
    pub(super) fn new(context: ToolContext, default_max_entries: usize) -> Self {
        Self {
            native_runner: NativeFsRunner::new(&context),
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
    native_runner: NativeFsRunner,
    default_max_matches: usize,
}

impl GlobTool {
    pub(super) fn new(context: ToolContext, default_max_matches: usize) -> Self {
        Self {
            native_runner: NativeFsRunner::new(&context),
            default_max_matches,
        }
    }
}

#[derive(Clone)]
pub(super) struct GrepTool {
    native_runner: NativeFsRunner,
    default_max_matches: usize,
}

impl GrepTool {
    pub(super) fn new(context: ToolContext, default_max_matches: usize) -> Self {
        Self {
            native_runner: NativeFsRunner::new(&context),
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
                "List files and directories under the allowed root ({}). Relative paths resolve from the active request workspace when one is provided, otherwise from the root. Returns compact text with stable defra_fs metadata and skips common generated directories by default. Set raw_json=true for structured JSON.",
                self.context.root().display()
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "default": ".",
                        "description": "Directory to list, relative to the active workspace/root. Omit or pass an empty string for the active workspace/root."
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
        self.native_runner
            .run(
                NativeFsRunnerRequest::ListFiles(NativeListFilesArgs {
                    path: args.path,
                    recursive: args.recursive,
                    max_entries: args.max_entries.max(1).min(self.default_max_entries.max(1)),
                    raw_json: args.raw_json,
                    max_entries_visited: None,
                    max_wall_ms: None,
                }),
                Self::NAME,
            )
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
                "Read a UTF-8 text file under the allowed root ({}). Relative paths resolve from the active request workspace when one is provided, otherwise from the root. Returns compact line-numbered text with stable defra_fs metadata. Set raw_json=true for structured JSON.",
                self.context.root().display()
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File to read, relative to the active workspace/root unless an allowed absolute path is provided."
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
        let (content, truncated) = cap_output(&rendered.content, max_chars);

        let output = ReadFileOutput {
            metadata: ReadFileMetadata {
                ok: true,
                status: "success",
                tool: Self::NAME,
                path: self.context.display_path(&path),
                returned_count: rendered.returned_lines,
                total_count: Some(rendered.total_lines),
                truncated,
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
            description: "Find files matching a glob pattern (supports *, ?, **, [..], and {a,b} alternation) under the allowed root. Relative paths resolve from the active request workspace when one is provided, otherwise from the root. The pattern is matched against the FULL path relative to that directory, so it must include every leading directory (or start with **/); check the search_dir_entries / pattern_prefix_exists fields on a zero-match result before retrying. Returns compact text with stable defra_fs metadata, skips common generated directories by default, and reports walk stats; large walks stop at a budget with partial results (walk.budget_exhausted=true). Set raw_json=true for structured JSON.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern matched against paths displayed relative to the active workspace/root."
                    },
                    "path": {
                        "type": "string",
                        "default": ".",
                        "description": "Directory to search, relative to the active workspace/root. Omit or pass an empty string for the active workspace/root."
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
        self.native_runner
            .run(
                NativeFsRunnerRequest::Glob(NativeGlobArgs {
                    pattern: args.pattern,
                    path: args.path,
                    max_matches: args.max_matches.min(self.default_max_matches).max(1),
                    raw_json: args.raw_json,
                    max_entries_visited: None,
                    max_wall_ms: None,
                }),
                Self::NAME,
            )
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
            description: "Search text files under the allowed root with a regular expression (Rust regex syntax, case-insensitive by default; a pattern that fails to parse as regex is used as a literal substring — the result metadata reports pattern_syntax accordingly). Relative paths resolve from the active request workspace when one is provided, otherwise from the root. The path may be a directory or a single file; prefer passing the narrowest directory you can. Returns compact path:Lline matches with stable defra_fs metadata, skips common generated directories, oversized files, and binary files by default, and reports walk stats; large walks stop at a budget with partial results (walk.budget_exhausted=true). Set raw_json=true for structured JSON.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regular expression (Rust regex syntax) matched against each line. Patterns that fail to parse as regex are matched as literal substrings."
                    },
                    "path": {
                        "type": "string",
                        "default": ".",
                        "description": "Directory or file to search, relative to the active workspace/root. Omit or pass an empty string for the active workspace/root."
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
        self.native_runner
            .run(
                NativeFsRunnerRequest::Grep(NativeGrepArgs {
                    pattern: args.pattern,
                    path: args.path,
                    case_sensitive: args.case_sensitive,
                    max_matches: args.max_matches.min(self.default_max_matches).max(1),
                    raw_json: args.raw_json,
                    max_entries_visited: None,
                    max_bytes_read: None,
                    max_wall_ms: None,
                }),
                Self::NAME,
            )
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
            description: "Write full file contents under the configured root. Relative paths resolve from the active request workspace when one is provided, otherwise from the root. Returns compact success metadata by default. Set raw_json=true for structured JSON.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File to create or overwrite, relative to the active workspace/root unless an allowed absolute path is provided."
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
            description: "Replace text in an existing file under the configured root. Relative paths resolve from the active request workspace when one is provided, otherwise from the root. Returns compact success metadata by default. Set raw_json=true for structured JSON.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Existing file to edit, relative to the active workspace/root unless an allowed absolute path is provided."
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
