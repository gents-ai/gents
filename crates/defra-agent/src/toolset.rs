use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use defra_node::EmbeddedNode;
use glob::Pattern;
use rig::completion::ToolDefinition;
use rig::tool::{Tool, ToolDyn};
use serde::Deserialize;
use tokio::process::Command;

use crate::graphql::escape_graphql_string;
use crate::lifecycle::DEFAULT_REQUEST_MAX_RETRIES;

const DEFAULT_MAX_FILE_CHARS: usize = 32_000;
const DEFAULT_MAX_COMMAND_CHARS: usize = 16_000;
const DEFAULT_MAX_LIST_ENTRIES: usize = 200;
const DEFAULT_MAX_MATCHES: usize = 200;
const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 10;
const DELEGATE_POLL_INTERVAL_MS: u64 = 200;
const DELEGATE_WAIT_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSet {
    tools: Vec<NativeTool>,
    read_root: Option<PathBuf>,
}

impl ToolSet {
    pub fn readonly() -> Self {
        Self {
            tools: vec![
                NativeTool::ListFiles {
                    max_entries: DEFAULT_MAX_LIST_ENTRIES,
                },
                NativeTool::ReadFile {
                    max_chars: DEFAULT_MAX_FILE_CHARS,
                },
                NativeTool::Glob {
                    max_matches: DEFAULT_MAX_MATCHES,
                },
                NativeTool::Grep {
                    max_matches: DEFAULT_MAX_MATCHES,
                },
                NativeTool::BashReadOnly {
                    timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
                    allowlist: default_read_only_commands(),
                },
            ],
            read_root: None,
        }
    }

    pub fn readwrite(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            tools: vec![
                NativeTool::ListFiles {
                    max_entries: DEFAULT_MAX_LIST_ENTRIES,
                },
                NativeTool::ReadFile {
                    max_chars: DEFAULT_MAX_FILE_CHARS,
                },
                NativeTool::Glob {
                    max_matches: DEFAULT_MAX_MATCHES,
                },
                NativeTool::Grep {
                    max_matches: DEFAULT_MAX_MATCHES,
                },
                NativeTool::BashReadOnly {
                    timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
                    allowlist: default_read_only_commands(),
                },
                NativeTool::WriteFile { root: root.clone() },
                NativeTool::EditFile { root: root.clone() },
                NativeTool::BashUnrestricted {
                    timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
                    root: root.clone(),
                },
            ],
            read_root: Some(root),
        }
    }

    pub fn meta_only() -> Self {
        Self {
            tools: Vec::new(),
            read_root: None,
        }
    }

    pub fn builder() -> ToolSetBuilder {
        ToolSetBuilder::default()
    }

    pub fn native_tools(&self) -> &[NativeTool] {
        &self.tools
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(NativeTool::tool_name).collect()
    }

    pub fn build_native_tools(&self) -> Result<Vec<Box<dyn ToolDyn>>> {
        let read_context = match &self.read_root {
            Some(root) => ToolContext::new(root.clone(), false)?,
            None => ToolContext::from_home()?,
        };

        let mut built: Vec<Box<dyn ToolDyn>> = Vec::new();
        for tool in &self.tools {
            match tool {
                NativeTool::ListFiles { max_entries } => built.push(Box::new(ListFilesTool::new(
                    read_context.clone(),
                    *max_entries,
                ))),
                NativeTool::ReadFile { max_chars } => built.push(Box::new(ReadFileTool::new(
                    read_context.clone(),
                    *max_chars,
                ))),
                NativeTool::Glob { max_matches } => {
                    built.push(Box::new(GlobTool::new(read_context.clone(), *max_matches)))
                }
                NativeTool::Grep { max_matches } => {
                    built.push(Box::new(GrepTool::new(read_context.clone(), *max_matches)))
                }
                NativeTool::WriteFile { root } => built.push(Box::new(WriteFileTool::new(
                    ToolContext::new(root.clone(), true)?,
                ))),
                NativeTool::EditFile { root } => built.push(Box::new(EditFileTool::new(
                    ToolContext::new(root.clone(), true)?,
                ))),
                NativeTool::BashReadOnly { timeout, allowlist } => built.push(Box::new(
                    ReadOnlyBashTool::new(read_context.clone(), *timeout, allowlist.clone()),
                )),
                NativeTool::BashUnrestricted { timeout, root } => built.push(Box::new(
                    UnrestrictedBashTool::new(ToolContext::new(root.clone(), true)?, *timeout),
                )),
            }
        }
        Ok(built)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeTool {
    ListFiles {
        max_entries: usize,
    },
    ReadFile {
        max_chars: usize,
    },
    Glob {
        max_matches: usize,
    },
    Grep {
        max_matches: usize,
    },
    WriteFile {
        root: PathBuf,
    },
    EditFile {
        root: PathBuf,
    },
    BashReadOnly {
        timeout: Duration,
        allowlist: Vec<String>,
    },
    BashUnrestricted {
        timeout: Duration,
        root: PathBuf,
    },
}

impl NativeTool {
    pub fn tool_name(&self) -> String {
        match self {
            Self::ListFiles { .. } => "list_files".to_string(),
            Self::ReadFile { .. } => "read_file".to_string(),
            Self::Glob { .. } => "glob".to_string(),
            Self::Grep { .. } => "grep".to_string(),
            Self::WriteFile { .. } => "write_file".to_string(),
            Self::EditFile { .. } => "edit_file".to_string(),
            Self::BashReadOnly { .. } => "bash".to_string(),
            Self::BashUnrestricted { .. } => "bash_unrestricted".to_string(),
        }
    }
}

#[derive(Debug, Default)]
pub struct ToolSetBuilder {
    tools: Vec<NativeTool>,
    read_root: Option<PathBuf>,
}

impl ToolSetBuilder {
    pub fn read_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.read_root = Some(root.into());
        self
    }

    pub fn list_files(mut self) -> Self {
        self.tools.push(NativeTool::ListFiles {
            max_entries: DEFAULT_MAX_LIST_ENTRIES,
        });
        self
    }

    pub fn read_file(mut self) -> Self {
        self.tools.push(NativeTool::ReadFile {
            max_chars: DEFAULT_MAX_FILE_CHARS,
        });
        self
    }

    pub fn glob(mut self) -> Self {
        self.tools.push(NativeTool::Glob {
            max_matches: DEFAULT_MAX_MATCHES,
        });
        self
    }

    pub fn grep(mut self) -> Self {
        self.tools.push(NativeTool::Grep {
            max_matches: DEFAULT_MAX_MATCHES,
        });
        self
    }

    pub fn write_file(mut self, root: impl Into<PathBuf>) -> Self {
        self.tools.push(NativeTool::WriteFile { root: root.into() });
        self
    }

    pub fn edit_file(mut self, root: impl Into<PathBuf>) -> Self {
        self.tools.push(NativeTool::EditFile { root: root.into() });
        self
    }

    pub fn bash_read_only(mut self) -> Self {
        self.tools.push(NativeTool::BashReadOnly {
            timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
            allowlist: default_read_only_commands(),
        });
        self
    }

    pub fn bash_unrestricted(mut self, root: impl Into<PathBuf>) -> Self {
        self.tools.push(NativeTool::BashUnrestricted {
            timeout: Duration::from_secs(DEFAULT_COMMAND_TIMEOUT_SECS),
            root: root.into(),
        });
        self
    }

    pub fn build(self) -> ToolSet {
        ToolSet {
            tools: self.tools,
            read_root: self.read_root,
        }
    }
}

/// Legacy helper preserved for the current `agent-daemon` binary.
pub fn build_native_tools() -> Result<Vec<Box<dyn ToolDyn>>> {
    ToolSet::builder()
        .list_files()
        .read_file()
        .bash_read_only()
        .build()
        .build_native_tools()
}

pub fn build_delegate_tool(node: Arc<EmbeddedNode>) -> Box<dyn ToolDyn> {
    Box::new(DelegateToAgentTool::new(node))
}

fn default_read_only_commands() -> Vec<String> {
    [
        "pwd", "ls", "find", "cat", "head", "tail", "sed", "grep", "rg", "wc", "stat", "file",
        "git",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

#[derive(Clone)]
struct ToolContext {
    root: Arc<PathBuf>,
}

#[derive(Debug)]
struct ToolError(anyhow::Error);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for ToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

impl From<std::io::Error> for ToolError {
    fn from(error: std::io::Error) -> Self {
        Self(error.into())
    }
}

impl ToolContext {
    fn from_home() -> Result<Self> {
        let root = dirs::home_dir()
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| anyhow!("unable to determine a tool root directory"))?;
        Self::new(root, false)
    }

    fn new(root: PathBuf, create_missing: bool) -> Result<Self> {
        if create_missing && !root.exists() {
            std::fs::create_dir_all(&root)
                .with_context(|| format!("creating tool root {}", root.display()))?;
        }

        let canonical = std::fs::canonicalize(&root)
            .with_context(|| format!("canonicalizing tool root {}", root.display()))?;

        Ok(Self {
            root: Arc::new(canonical),
        })
    }

    fn resolve_path_allow_create(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            normalize_for_creation(candidate)?
        } else {
            normalize_for_creation(&self.root.join(candidate))?
        };
        self.ensure_allowed(resolved)
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            std::fs::canonicalize(candidate)
                .with_context(|| format!("resolving path {}", candidate.display()))?
        } else {
            let joined = self.root.join(candidate);
            std::fs::canonicalize(&joined)
                .with_context(|| format!("resolving path {}", joined.display()))?
        };
        self.ensure_allowed(resolved)
    }

    fn resolve_existing_dir(&self, path: Option<&str>) -> Result<PathBuf> {
        let resolved = match path {
            Some(path) if !path.trim().is_empty() => self.resolve_path(path)?,
            _ => (*self.root).clone(),
        };

        if !resolved.is_dir() {
            bail!("path is not a directory: {}", resolved.display());
        }

        Ok(resolved)
    }

    fn resolve_existing_file(&self, path: &str) -> Result<PathBuf> {
        let resolved = self.resolve_path(path)?;
        if !resolved.is_file() {
            bail!("path is not a file: {}", resolved.display());
        }
        Ok(resolved)
    }

    fn ensure_allowed(&self, path: PathBuf) -> Result<PathBuf> {
        if path.starts_with(self.root.as_path()) {
            Ok(path)
        } else {
            bail!(
                "path is outside the allowed tool root {}: {}",
                self.root.display(),
                path.display()
            );
        }
    }

    fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(self.root.as_path())
            .ok()
            .map(|relative| {
                let display = relative.to_string_lossy().replace('\\', "/");
                if display.is_empty() {
                    ".".to_string()
                } else {
                    display
                }
            })
            .unwrap_or_else(|| path.display().to_string())
    }
}

fn normalize_for_creation(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

#[derive(Debug, Deserialize)]
struct ListFilesArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    recursive: bool,
    #[serde(default = "default_max_list_entries")]
    max_entries: usize,
}

#[derive(Debug, Deserialize)]
struct ReadFileArgs {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default = "default_max_file_chars")]
    max_chars: usize,
}

#[derive(Debug, Deserialize)]
struct GlobArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default = "default_max_matches")]
    max_matches: usize,
}

#[derive(Debug, Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default = "default_max_matches")]
    max_matches: usize,
}

#[derive(Debug, Deserialize)]
struct WriteFileArgs {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct EditFileArgs {
    path: String,
    old_text: String,
    new_text: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Debug, Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default = "default_command_timeout_secs")]
    timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
struct DelegateToAgentArgs {
    target_did: String,
    content: String,
    wait: bool,
}

#[derive(Clone)]
struct ListFilesTool {
    context: ToolContext,
    default_max_entries: usize,
}

impl ListFilesTool {
    fn new(context: ToolContext, default_max_entries: usize) -> Self {
        Self {
            context,
            default_max_entries,
        }
    }
}

#[derive(Clone)]
struct ReadFileTool {
    context: ToolContext,
    default_max_chars: usize,
}

impl ReadFileTool {
    fn new(context: ToolContext, default_max_chars: usize) -> Self {
        Self {
            context,
            default_max_chars,
        }
    }
}

#[derive(Clone)]
struct GlobTool {
    context: ToolContext,
    default_max_matches: usize,
}

impl GlobTool {
    fn new(context: ToolContext, default_max_matches: usize) -> Self {
        Self {
            context,
            default_max_matches,
        }
    }
}

#[derive(Clone)]
struct GrepTool {
    context: ToolContext,
    default_max_matches: usize,
}

impl GrepTool {
    fn new(context: ToolContext, default_max_matches: usize) -> Self {
        Self {
            context,
            default_max_matches,
        }
    }
}

#[derive(Clone)]
struct WriteFileTool {
    context: ToolContext,
}

impl WriteFileTool {
    fn new(context: ToolContext) -> Self {
        Self { context }
    }
}

#[derive(Clone)]
struct EditFileTool {
    context: ToolContext,
}

impl EditFileTool {
    fn new(context: ToolContext) -> Self {
        Self { context }
    }
}

#[derive(Clone)]
struct ReadOnlyBashTool {
    context: ToolContext,
    default_timeout: Duration,
    allowlist: Vec<String>,
}

impl ReadOnlyBashTool {
    fn new(context: ToolContext, default_timeout: Duration, allowlist: Vec<String>) -> Self {
        Self {
            context,
            default_timeout,
            allowlist,
        }
    }
}

#[derive(Clone)]
struct UnrestrictedBashTool {
    context: ToolContext,
    default_timeout: Duration,
}

impl UnrestrictedBashTool {
    fn new(context: ToolContext, default_timeout: Duration) -> Self {
        Self {
            context,
            default_timeout,
        }
    }
}

#[derive(Clone)]
struct DelegateToAgentTool {
    node: Arc<EmbeddedNode>,
}

impl DelegateToAgentTool {
    fn new(node: Arc<EmbeddedNode>) -> Self {
        Self { node }
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
                self.context.root.display()
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
                self.context.root.display()
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

impl Tool for ReadOnlyBashTool {
    const NAME: &'static str = "bash";

    type Error = ToolError;
    type Args = BashArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Run a single read-only command under the allowed root.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "cwd": { "type": "string" },
                    "timeout_secs": { "type": "integer" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_read_only_command(&args.command, &args.args, &self.allowlist)?;
        run_command(
            &self.context,
            &args.command,
            &args.args,
            args.cwd.as_deref(),
            Duration::from_secs(args.timeout_secs.max(1)).min(self.default_timeout),
        )
        .await
        .map_err(Into::into)
    }
}

impl Tool for UnrestrictedBashTool {
    const NAME: &'static str = "bash_unrestricted";

    type Error = ToolError;
    type Args = BashArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Run a command under the configured writable root.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "cwd": { "type": "string" },
                    "timeout_secs": { "type": "integer" }
                },
                "required": ["command"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        run_command(
            &self.context,
            &args.command,
            &args.args,
            args.cwd.as_deref(),
            Duration::from_secs(args.timeout_secs.max(1)).min(self.default_timeout),
        )
        .await
        .map_err(Into::into)
    }
}

impl Tool for DelegateToAgentTool {
    const NAME: &'static str = "delegate_to_agent";

    type Error = ToolError;
    type Args = DelegateToAgentArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Delegate a request to another defra-agent DID via DefraDB.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target_did": { "type": "string" },
                    "content": { "type": "string" },
                    "wait": { "type": "boolean" }
                },
                "required": ["target_did", "content", "wait"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();
        let escaped_target = escape_graphql_string(&args.target_did);
        let escaped_content = escape_graphql_string(&args.content);
        let escaped_request_id = escape_graphql_string(&request_id);
        let escaped_session_id = escape_graphql_string(&session_id);
        let escaped_created_at = escape_graphql_string(&created_at);

        let mutation = format!(
            r#"mutation {{
                create_AgentRequest(
                    input: {{
                        request_id: "{escaped_request_id}",
                        agent_did: "{escaped_target}",
                        session_id: "{escaped_session_id}",
                        retry_parent_request: "",
                        retry_root_request: "{escaped_request_id}",
                        superseded_by_request: "",
                        content: "{escaped_content}",
                        status: "pending",
                        lifecycle_state: "pending",
                        admission_state: "released",
                        backend_id: "",
                        execution_origin: "interactive",
                        created_at: "{escaped_created_at}",
                        retry_count: 0,
                        max_retries: {max_retries}
                    }}
                ) {{ _docID }}
            }}"#,
            max_retries = DEFAULT_REQUEST_MAX_RETRIES,
        );

        let resp = self.node.execute(&mutation).await;
        if resp.has_errors() {
            return Err(anyhow!(
                "delegate_to_agent failed to create request: {:?}",
                resp.errors
            )
            .into());
        }

        let lineage_mutation = format!(
            r#"mutation {{
                update_AgentRequest(
                    filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                    input: {{
                        retry_parent_request: "",
                        retry_root_request: "{escaped_request_id}",
                        superseded_by_request: ""
                    }}
                ) {{ _docID }}
            }}"#
        );
        let lineage_resp = self.node.execute(&lineage_mutation).await;
        if lineage_resp.has_errors() {
            return Err(anyhow!(
                "delegate_to_agent failed to persist request lineage: {:?}",
                lineage_resp.errors
            )
            .into());
        }

        if !args.wait {
            return Ok(request_id);
        }

        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(DELEGATE_WAIT_TIMEOUT_SECS);
        loop {
            let query = format!(
                r#"{{
                    AgentResponse(
                        filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                        order: {{ created_at: DESC }},
                        limit: 1
                    ) {{
                        content
                        status
                    }}
                }}"#
            );

            let resp = self.node.execute(&query).await;
            if resp.has_errors() {
                return Err(anyhow!(
                    "delegate_to_agent failed to query response: {:?}",
                    resp.errors
                )
                .into());
            }

            let rows: Vec<AgentResponseRow> = resp
                .data
                .as_ref()
                .and_then(|data| data.get("AgentResponse"))
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default();

            if let Some(row) = rows.into_iter().next() {
                match row.status.as_str() {
                    "complete" => return Ok(row.content),
                    "error" => {
                        return Err(
                            anyhow!("delegated agent returned error: {}", row.content).into()
                        )
                    }
                    _ => {}
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(
                    anyhow!("timed out waiting for delegated response {request_id}").into(),
                );
            }

            tokio::time::sleep(Duration::from_millis(DELEGATE_POLL_INTERVAL_MS)).await;
        }
    }
}

#[derive(Debug, Deserialize)]
struct AgentResponseRow {
    #[serde(default)]
    content: String,
    #[serde(default)]
    status: String,
}

async fn run_command(
    context: &ToolContext,
    command_name: &str,
    args: &[String],
    cwd: Option<&str>,
    timeout: Duration,
) -> Result<String> {
    let cwd = context.resolve_existing_dir(cwd)?;
    let mut command = Command::new(command_name);
    command
        .args(args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .env("TERM", "dumb");

    let output = tokio::time::timeout(timeout, command.output())
        .await
        .with_context(|| format!("timed out after {}s", timeout.as_secs()))??;

    let stdout = truncate_text(
        &String::from_utf8_lossy(&output.stdout),
        DEFAULT_MAX_COMMAND_CHARS,
    );
    let stderr = truncate_text(
        &String::from_utf8_lossy(&output.stderr),
        DEFAULT_MAX_COMMAND_CHARS,
    );
    let exit_code = output.status.code().unwrap_or(-1);
    let command_line = std::iter::once(command_name)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");

    Ok(format!(
        "cwd: {}\ncommand: {}\nexit_code: {}\nstdout:\n{}\nstderr:\n{}",
        context.display_path(&cwd),
        command_line,
        exit_code,
        if stdout.is_empty() {
            "(empty)"
        } else {
            &stdout
        },
        if stderr.is_empty() {
            "(empty)"
        } else {
            &stderr
        },
    ))
}

fn collect_entries(
    context: &ToolContext,
    dir: &Path,
    recursive: bool,
    max_entries: usize,
    entries: &mut Vec<String>,
) -> Result<()> {
    let mut children = std::fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());

    for entry in children {
        if entries.len() >= max_entries {
            break;
        }

        let path = entry.path();
        let metadata = entry.metadata()?;
        let mut display = context.display_path(&path);
        if metadata.is_dir() {
            display.push('/');
        }
        entries.push(display);

        if recursive && metadata.is_dir() && entries.len() < max_entries {
            collect_entries(context, &path, true, max_entries, entries)?;
        }
    }

    Ok(())
}

fn collect_glob_matches(
    context: &ToolContext,
    dir: &Path,
    pattern: &Pattern,
    max_matches: usize,
    matches: &mut Vec<String>,
) -> Result<()> {
    let mut children = std::fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());

    for entry in children {
        if matches.len() >= max_matches {
            break;
        }

        let path = entry.path();
        let metadata = entry.metadata()?;
        let display = context.display_path(&path);
        if pattern.matches(&display) {
            matches.push(if metadata.is_dir() {
                format!("{display}/")
            } else {
                display
            });
        }

        if metadata.is_dir() && matches.len() < max_matches {
            collect_glob_matches(context, &path, pattern, max_matches, matches)?;
        }
    }

    Ok(())
}

fn collect_grep_matches(
    context: &ToolContext,
    dir: &Path,
    pattern: &str,
    case_sensitive: bool,
    max_matches: usize,
    matches: &mut Vec<String>,
) -> Result<()> {
    let mut children = std::fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());

    for entry in children {
        if matches.len() >= max_matches {
            break;
        }

        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_grep_matches(
                context,
                &path,
                pattern,
                case_sensitive,
                max_matches,
                matches,
            )?;
            continue;
        }

        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(_) => continue,
        };

        let needle = if case_sensitive {
            pattern.to_string()
        } else {
            pattern.to_lowercase()
        };

        for (idx, line) in contents.lines().enumerate() {
            if matches.len() >= max_matches {
                break;
            }
            let haystack = if case_sensitive {
                line.to_string()
            } else {
                line.to_lowercase()
            };
            if haystack.contains(&needle) {
                matches.push(format!(
                    "{}:{}:{}",
                    context.display_path(&path),
                    idx + 1,
                    line
                ));
            }
        }
    }

    Ok(())
}

fn render_file_contents(text: &str, start_line: Option<usize>, end_line: Option<usize>) -> String {
    let start_line = start_line.unwrap_or(1).max(1);
    let end_line = end_line.unwrap_or(usize::MAX).max(start_line);

    let mut rendered = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_number = idx + 1;
        if line_number < start_line || line_number > end_line {
            continue;
        }
        rendered.push(format!("{line_number}: {line}"));
    }

    if rendered.is_empty() && text.is_empty() {
        "(empty file)".to_string()
    } else if rendered.is_empty() {
        "(no lines in requested range)".to_string()
    } else {
        rendered.join("\n")
    }
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}\n[truncated to {max_chars} chars]")
}

fn validate_read_only_command(command: &str, args: &[String], allowlist: &[String]) -> Result<()> {
    if !allowlist.iter().any(|allowed| allowed == command) {
        bail!("command is not allowed by the read-only bash tool: {command}");
    }

    match command {
        "sed" => {
            if args
                .iter()
                .any(|arg| arg == "-i" || arg == "--in-place" || arg.starts_with("-i"))
            {
                bail!("sed in-place edits are not allowed");
            }
        }
        "find" => {
            if args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "-delete"
                        | "-exec"
                        | "-execdir"
                        | "-ok"
                        | "-okdir"
                        | "-fprint"
                        | "-fprintf"
                        | "-fls"
                )
            }) {
                bail!("find arguments that can write or execute are not allowed");
            }
        }
        "git" => validate_git_args(args)?,
        _ => {}
    }

    Ok(())
}

fn validate_git_args(args: &[String]) -> Result<()> {
    let subcommand = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow!("git requires a read-only subcommand"))?;

    match subcommand {
        "status" | "diff" | "show" | "log" | "ls-files" | "grep" | "branch" | "rev-parse" => Ok(()),
        other => bail!("git subcommand is not allowed by the read-only bash tool: {other}"),
    }
}

fn default_max_list_entries() -> usize {
    DEFAULT_MAX_LIST_ENTRIES
}

fn default_max_file_chars() -> usize {
    DEFAULT_MAX_FILE_CHARS
}

fn default_max_matches() -> usize {
    DEFAULT_MAX_MATCHES
}

fn default_command_timeout_secs() -> u64 {
    DEFAULT_COMMAND_TIMEOUT_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::ensure_schemas;

    #[test]
    fn toolset_presets_have_expected_counts() {
        assert_eq!(ToolSet::readonly().native_tools().len(), 5);
        assert_eq!(
            ToolSet::readwrite(std::env::temp_dir())
                .native_tools()
                .len(),
            8
        );
        assert_eq!(ToolSet::meta_only().native_tools().len(), 0);
    }

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn read_file_returns_numbered_contents() {
        let root = temp_root("defra-agent-read-file");
        let file = root.join("notes.txt");
        std::fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();
        let tool = ReadFileTool::new(
            ToolContext::new(root, false).unwrap(),
            DEFAULT_MAX_FILE_CHARS,
        );

        let output = rig::tool::Tool::call(
            &tool,
            ReadFileArgs {
                path: "notes.txt".to_string(),
                start_line: Some(2),
                end_line: Some(3),
                max_chars: DEFAULT_MAX_FILE_CHARS,
            },
        )
        .await
        .unwrap();

        assert!(output.contains("2: beta"));
        assert!(output.contains("3: gamma"));
    }

    #[tokio::test]
    async fn write_and_edit_file_work_under_root() {
        let root = temp_root("defra-agent-write-edit");
        let context = ToolContext::new(root.clone(), true).unwrap();
        let writer = WriteFileTool::new(context.clone());
        let editor = EditFileTool::new(context);

        rig::tool::Tool::call(
            &writer,
            WriteFileArgs {
                path: "nested/file.txt".to_string(),
                content: "hello world".to_string(),
            },
        )
        .await
        .unwrap();
        rig::tool::Tool::call(
            &editor,
            EditFileArgs {
                path: "nested/file.txt".to_string(),
                old_text: "world".to_string(),
                new_text: "amy".to_string(),
                replace_all: false,
            },
        )
        .await
        .unwrap();

        let content = std::fs::read_to_string(root.join("nested/file.txt")).unwrap();
        assert_eq!(content, "hello amy");
    }

    #[test]
    fn read_only_bash_rejects_write_commands() {
        assert!(validate_read_only_command(
            "git",
            &[String::from("commit")],
            &default_read_only_commands()
        )
        .is_err());
    }

    #[tokio::test]
    async fn delegate_to_agent_round_trip_waits_for_response() {
        let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
        ensure_schemas(node.as_ref()).await.unwrap();
        let tool = DelegateToAgentTool::new(node.clone());

        let call = tokio::spawn(async move {
            rig::tool::Tool::call(
                &tool,
                DelegateToAgentArgs {
                    target_did: "did:defra-agent:amy-code".to_string(),
                    content: "Write a test".to_string(),
                    wait: true,
                },
            )
            .await
            .unwrap()
        });

        #[derive(Deserialize)]
        struct RequestRow {
            request_id: String,
            agent_did: String,
            session_id: String,
            content: String,
            retry_count: i64,
            max_retries: i64,
        }

        let request = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let resp = node
                    .execute(
                        r#"{
                            AgentRequest(limit: 1) {
                                request_id
                                agent_did
                                session_id
                                content
                                retry_count
                                max_retries
                            }
                        }"#,
                    )
                    .await;
                if !resp.has_errors() {
                    let rows: Vec<RequestRow> = resp
                        .data
                        .as_ref()
                        .and_then(|data| data.get("AgentRequest"))
                        .and_then(|value| serde_json::from_value(value.clone()).ok())
                        .unwrap_or_default();
                    if let Some(row) = rows.into_iter().next() {
                        break row;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();

        assert_eq!(request.agent_did, "did:defra-agent:amy-code");
        assert_eq!(request.content, "Write a test");
        assert_eq!(request.retry_count, 0);
        assert_eq!(request.max_retries, DEFAULT_REQUEST_MAX_RETRIES as i64);

        let now = chrono::Utc::now().to_rfc3339();
        let mutation = format!(
            r#"mutation {{
                create_AgentResponse(
                    input: {{
                        response_key: "{request_id}",
                        request_id: "{request_id}",
                        agent_did: "{agent_did}",
                        session_id: "{session_id}",
                        content: "delegated result",
                        status: "complete",
                        token_count: 2,
                        progress_seq: 1,
                        created_at: "{created_at}",
                        completed_at: "{created_at}"
                    }}
                ) {{ _docID }}
            }}"#,
            request_id = escape_graphql_string(&request.request_id),
            agent_did = escape_graphql_string(&request.agent_did),
            session_id = escape_graphql_string(&request.session_id),
            created_at = escape_graphql_string(&now),
        );
        let resp = node.execute(&mutation).await;
        assert!(!resp.has_errors(), "{:?}", resp.errors);

        let result = call.await.unwrap();
        assert_eq!(result, "delegated result");
    }
}
