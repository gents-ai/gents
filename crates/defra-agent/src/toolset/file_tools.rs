use anyhow::{anyhow, Context as _, Result};
use glob::Pattern;
use rig::completion::ToolDefinition;
use rig::tool::Tool;

use super::args::{
    EditFileArgs, GlobArgs, GrepArgs, ListFilesArgs, ReadFileArgs, WriteFileArgs,
};
use super::shared::{
    collect_entries, collect_glob_matches, collect_grep_matches, render_file_contents,
    truncate_text, ToolContext, ToolError,
};

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
                "List files and directories under the allowed root ({}).",
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
        let mut entries = Vec::new();
        collect_entries(
            &self.context,
            &dir,
            args.recursive,
            args.max_entries.max(1).min(self.default_max_entries.max(1)),
            &mut entries,
        )?;
        entries.sort();

        Ok(if entries.is_empty() {
            format!("(no entries under {})", self.context.display_path(&dir))
        } else {
            entries.join("\n")
        })
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
                "Read a UTF-8 text file under the allowed root ({}).",
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
        let rendered = truncate_text(&rendered, args.max_chars.min(self.default_max_chars).max(1));

        Ok(format!(
            "=== {} ===\n{}",
            self.context.display_path(&path),
            rendered
        ))
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
            description: "Find files matching a glob pattern under the allowed root.".to_string(),
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
        let mut matches = Vec::new();
        collect_glob_matches(
            &self.context,
            &dir,
            &pattern,
            args.max_matches.min(self.default_max_matches).max(1),
            &mut matches,
        )?;
        matches.sort();

        Ok(if matches.is_empty() {
            format!("(no matches for {})", args.pattern)
        } else {
            matches.join("\n")
        })
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
            description: "Search text files under the allowed root for a substring.".to_string(),
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
        let mut matches = Vec::new();
        collect_grep_matches(
            &self.context,
            &dir,
            &args.pattern,
            args.case_sensitive,
            args.max_matches.min(self.default_max_matches).max(1),
            &mut matches,
        )?;

        Ok(if matches.is_empty() {
            format!("(no matches for {})", args.pattern)
        } else {
            matches.join("\n")
        })
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
