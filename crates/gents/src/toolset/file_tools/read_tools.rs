use crate::llm::tool::Tool;
use crate::llm::tool::ToolDefinition;
use anyhow::Result;
use gents_fs_runner::protocol::{
    GlobArgs as NativeGlobArgs, GrepArgs as NativeGrepArgs, ListFilesArgs as NativeListFilesArgs,
    NativeFsRunnerRequest,
};
use serde::Serialize;

use super::super::args::{GlobArgs, GrepArgs, ListFilesArgs, ReadFileArgs};
use super::super::native_runner::NativeFsRunner;
use super::super::shared::{cap_output, render_file_contents, ToolContext, ToolError};
use super::{content_hash, render_tool_output};

#[derive(Clone)]
pub(in crate::toolset) struct ListFilesTool {
    context: ToolContext,
    native_runner: NativeFsRunner,
    default_max_entries: usize,
}

impl ListFilesTool {
    pub(in crate::toolset) fn new(context: ToolContext, default_max_entries: usize) -> Self {
        Self {
            native_runner: NativeFsRunner::new(&context),
            context,
            default_max_entries,
        }
    }
}

#[derive(Clone)]
pub(in crate::toolset) struct ReadFileTool {
    context: ToolContext,
    default_max_chars: usize,
}

impl ReadFileTool {
    pub(in crate::toolset) fn new(context: ToolContext, default_max_chars: usize) -> Self {
        Self {
            context,
            default_max_chars,
        }
    }
}

#[derive(Clone)]
pub(in crate::toolset) struct GlobTool {
    native_runner: NativeFsRunner,
    default_max_matches: usize,
}

impl GlobTool {
    pub(in crate::toolset) fn new(context: ToolContext, default_max_matches: usize) -> Self {
        Self {
            native_runner: NativeFsRunner::new(&context),
            default_max_matches,
        }
    }
}

#[derive(Clone)]
pub(in crate::toolset) struct GrepTool {
    native_runner: NativeFsRunner,
    default_max_matches: usize,
}

impl GrepTool {
    pub(in crate::toolset) fn new(context: ToolContext, default_max_matches: usize) -> Self {
        Self {
            native_runner: NativeFsRunner::new(&context),
            default_max_matches,
        }
    }
}

impl Tool for ListFilesTool {
    const NAME: &'static str = "list_files";

    type Error = ToolError;
    type Args = ListFilesArgs;
    type Output = String;

    fn footprint(&self, _args: &ListFilesArgs) -> crate::llm::tool::ToolFootprint {
        crate::llm::tool::ToolFootprint::Read
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: format!(
                "List files and directories under the allowed root ({}). Relative paths resolve from the active request workspace when one is provided, otherwise from the root. Returns compact text with stable gents_fs metadata, skips common generated directories and paths ignored by in-tree .gitignore files by default, and reports walk stats; large walks stop at a budget with partial results (walk.budget_exhausted=true). Set raw_json=true for structured JSON.",
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
        let output = self
            .native_runner
            .run(
                NativeFsRunnerRequest::ListFiles(NativeListFilesArgs {
                    path: args.path,
                    recursive: args.recursive,
                    max_entries: args
                        .max_entries
                        .unwrap_or(self.default_max_entries)
                        .clamp(1, self.default_max_entries.max(1)),
                    raw_json: args.raw_json,
                    max_entries_visited: None,
                    max_wall_ms: None,
                }),
                Self::NAME,
            )
            .await?;
        if args.raw_json {
            crate::tool_output::render_json_text(output, &["entries", "matches"])
                .map_err(Into::into)
        } else {
            Ok(output)
        }
    }

    fn deferred_writes(
        &self,
        _output: &Self::Output,
    ) -> Option<gents_loop::live_output::DeferredToolWrites> {
        Some(change_capture::unchanged_output())
    }

    fn into_dyn_error(error: Self::Error) -> crate::llm::tool::ToolError {
        error.into_dispatch_error()
    }
}

impl Tool for ReadFileTool {
    const NAME: &'static str = "read_file";

    type Error = ToolError;
    type Args = ReadFileArgs;
    type Output = String;

    fn footprint(&self, _args: &ReadFileArgs) -> crate::llm::tool::ToolFootprint {
        crate::llm::tool::ToolFootprint::Read
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: format!(
                "Read a UTF-8 text file under the allowed root ({}). Relative paths resolve from the active request workspace when one is provided, otherwise from the root. Returns compact line-numbered text with stable gents_fs metadata, including content_hash — the raw-byte identity of the whole file, usable as edit_file or write_file expected_content_hash to guard against concurrent changes. Set raw_json=true for structured JSON.",
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
                        "description": "Maximum bytes of content to return, cut on a character boundary; higher values are capped by the tool."
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
        let content_hash = content_hash(&bytes);
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let rendered = render_file_contents(&text, args.start_line, args.end_line);
        let max_chars = args
            .max_chars
            .unwrap_or(self.default_max_chars)
            .clamp(1, self.default_max_chars.max(1));
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
                content_hash,
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

    fn deferred_writes(
        &self,
        _output: &Self::Output,
    ) -> Option<gents_loop::live_output::DeferredToolWrites> {
        Some(change_capture::unchanged_output())
    }

    fn into_dyn_error(error: Self::Error) -> crate::llm::tool::ToolError {
        error.into_dispatch_error()
    }
}

impl Tool for GlobTool {
    const NAME: &'static str = "glob";

    type Error = ToolError;
    type Args = GlobArgs;
    type Output = String;

    fn footprint(&self, _args: &GlobArgs) -> crate::llm::tool::ToolFootprint {
        crate::llm::tool::ToolFootprint::Read
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Find files matching a glob pattern (supports *, ?, **, [..], and {a,b} alternation) under the allowed root. Relative paths resolve from the active request workspace when one is provided, otherwise from the root. The pattern is matched against the FULL path relative to that directory, so it must include every leading directory (or start with **/); check the search_dir_entries / pattern_prefix_exists fields on a zero-match result before retrying. Returns compact text with stable gents_fs metadata, skips common generated directories and paths ignored by in-tree .gitignore files by default, and reports walk stats; large walks stop at a budget with partial results (walk.budget_exhausted=true). Set raw_json=true for structured JSON.".to_string(),
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
        let output = self
            .native_runner
            .run(
                NativeFsRunnerRequest::Glob(NativeGlobArgs {
                    pattern: args.pattern,
                    path: args.path,
                    max_matches: args
                        .max_matches
                        .unwrap_or(self.default_max_matches)
                        .clamp(1, self.default_max_matches.max(1)),
                    raw_json: args.raw_json,
                    max_entries_visited: None,
                    max_wall_ms: None,
                }),
                Self::NAME,
            )
            .await?;
        if args.raw_json {
            crate::tool_output::render_json_text(output, &["entries", "matches"])
                .map_err(Into::into)
        } else {
            Ok(output)
        }
    }

    fn deferred_writes(
        &self,
        _output: &Self::Output,
    ) -> Option<gents_loop::live_output::DeferredToolWrites> {
        Some(change_capture::unchanged_output())
    }

    fn into_dyn_error(error: Self::Error) -> crate::llm::tool::ToolError {
        error.into_dispatch_error()
    }
}

impl Tool for GrepTool {
    const NAME: &'static str = "grep";

    type Error = ToolError;
    type Args = GrepArgs;
    type Output = String;

    fn footprint(&self, _args: &GrepArgs) -> crate::llm::tool::ToolFootprint {
        crate::llm::tool::ToolFootprint::Read
    }

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Search text files under the allowed root with a regular expression (Rust regex syntax, case-insensitive by default; a pattern that fails to parse as regex is used as a literal substring — the result metadata reports pattern_syntax accordingly). Relative paths resolve from the active request workspace when one is provided, otherwise from the root. The path may be a directory or a single file; prefer passing the narrowest directory you can. Returns compact path:Lline matches with stable gents_fs metadata, skips common generated directories, paths ignored by in-tree .gitignore files, oversized files, and binary files by default, and reports walk stats; large walks stop at a budget with partial results (walk.budget_exhausted=true). Set raw_json=true for structured JSON.".to_string(),
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
        let output = self
            .native_runner
            .run(
                NativeFsRunnerRequest::Grep(NativeGrepArgs {
                    pattern: args.pattern,
                    path: args.path,
                    case_sensitive: args.case_sensitive,
                    max_matches: args
                        .max_matches
                        .unwrap_or(self.default_max_matches)
                        .clamp(1, self.default_max_matches.max(1)),
                    raw_json: args.raw_json,
                    max_entries_visited: None,
                    max_bytes_read: None,
                    max_wall_ms: None,
                }),
                Self::NAME,
            )
            .await?;
        if args.raw_json {
            crate::tool_output::render_json_text(output, &["entries", "matches"])
                .map_err(Into::into)
        } else {
            Ok(output)
        }
    }

    fn deferred_writes(
        &self,
        _output: &Self::Output,
    ) -> Option<gents_loop::live_output::DeferredToolWrites> {
        Some(change_capture::unchanged_output())
    }

    fn into_dyn_error(error: Self::Error) -> crate::llm::tool::ToolError {
        error.into_dispatch_error()
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
    content_hash: String,
}

#[derive(Serialize)]
struct ReadFileOutput {
    #[serde(flatten)]
    metadata: ReadFileMetadata,
    content: String,
}
