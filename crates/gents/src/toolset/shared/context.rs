use std::path::{Path, PathBuf};
use std::sync::Arc;

use gents_protocol::error_chain::ErrorChain;

use crate::tool_call_lifecycle::FailureClass;
use crate::toolset::cli_tool::CliToolError;
use crate::toolset::file_tools::FileToolError;
use crate::toolset::goal::GoalToolError;
use crate::toolset::lsp::LspToolError;
use crate::toolset::native_runner::NativeRunnerError;
use crate::toolset::session_message::SessionMessageError;
use crate::toolset::shared::command::CommandError;
use crate::toolset::CommandPolicyDenial;

#[derive(Clone)]
pub(crate) struct ToolContext {
    root: Arc<PathBuf>,
    base: Arc<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("creating tool root {}", root.display())]
    CreateRoot {
        root: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("canonicalizing tool root {}", root.display())]
    CanonicalizeRoot {
        root: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("resolving path {}", path.display())]
    ResolvePath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("path is not a directory: {}", path.display())]
    NotDirectory { path: PathBuf },
    #[error("path is not a file: {}", path.display())]
    NotFile { path: PathBuf },
    #[error("{} holds the operator's plugin access settings; tools cannot use it", path.display())]
    ProtectedPath { path: PathBuf },
    #[error("path is outside the allowed tool root {}: {}", root.display(), path.display())]
    OutsideRoot { root: PathBuf, path: PathBuf },
    #[error("unable to determine a tool root directory")]
    NoRootDirectory,
    #[error("path has no existing ancestor: {}", path.display())]
    NoExistingAncestor { path: PathBuf },
    #[error("canonicalizing path ancestor {}", ancestor.display())]
    CanonicalizeAncestor {
        ancestor: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub(crate) enum ToolError {
    CliTool(CliToolError),
    Command(CommandError),
    Context(ContextError),
    FileTool(FileToolError),
    Goal(GoalToolError),
    Io(std::io::Error),
    Lsp(LspToolError),
    LiveOutput(gents_loop::live_output::LiveOutputError),
    NativeRunner(NativeRunnerError),
    PolicyDenial(CommandPolicyDenial),
    ReportedFailure { class: FailureClass, text: String },
    SessionMessage(SessionMessageError),
    ToolOutput(crate::tool_output::ToolOutputError),
}

impl ToolError {
    pub(crate) fn policy_denial(denial: CommandPolicyDenial) -> Self {
        Self::PolicyDenial(denial)
    }

    pub(crate) fn reported_failure(class: FailureClass, text: String) -> Self {
        Self::ReportedFailure { class, text }
    }

    pub(crate) fn into_dispatch_error(self) -> crate::llm::tool::ToolError {
        match self {
            Self::PolicyDenial(denial) => crate::llm::tool::ToolError::ReportedFailure {
                class: FailureClass::PolicyDenied,
                text: denial.tool_error_payload(),
            },
            Self::ReportedFailure { class, text } => {
                crate::llm::tool::ToolError::ReportedFailure { class, text }
            }
            other => crate::llm::tool::ToolError::ToolCallError(Box::new(other)),
        }
    }

    #[cfg(test)]
    pub(crate) fn command_policy_denial(&self) -> Option<&CommandPolicyDenial> {
        match self {
            Self::PolicyDenial(denial) => Some(denial),
            Self::CliTool(_)
            | Self::Command(_)
            | Self::Context(_)
            | Self::FileTool(_)
            | Self::Goal(_)
            | Self::Io(_)
            | Self::Lsp(_)
            | Self::LiveOutput(_)
            | Self::NativeRunner(_)
            | Self::SessionMessage(_)
            | Self::ToolOutput(_)
            | Self::ReportedFailure { .. } => None,
        }
    }
}

fn root_cause<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> &'a (dyn std::error::Error + 'static) {
    let mut root = error;
    while let Some(next) = root.source() {
        root = next;
    }
    root
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CliTool(error) => write!(f, "{}", ErrorChain(error)),
            Self::Command(error) => write!(f, "{}", ErrorChain(error)),
            Self::Context(error) => write!(f, "{}", ErrorChain(error)),
            Self::FileTool(error) => write!(f, "{}", ErrorChain(error)),
            Self::Goal(error) => write!(f, "{}", ErrorChain(error)),
            Self::Io(error) => write!(f, "{}", ErrorChain(error)),
            Self::Lsp(error) => write!(f, "{}", ErrorChain(error)),
            Self::LiveOutput(error) => write!(f, "{}", ErrorChain(error)),
            Self::NativeRunner(error) => write!(f, "{}", ErrorChain(error)),
            Self::SessionMessage(error) => write!(f, "{}", ErrorChain(error)),
            Self::ToolOutput(error) => write!(f, "{}", ErrorChain(error)),
            Self::PolicyDenial(denial) => write!(f, "{}", denial.tool_error_payload()),
            Self::ReportedFailure { text, .. } => f.write_str(text),
        }
    }
}

impl std::error::Error for ToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CliTool(error) => Some(root_cause(error)),
            Self::Command(error) => Some(root_cause(error)),
            Self::Context(error) => Some(root_cause(error)),
            Self::FileTool(error) => Some(root_cause(error)),
            Self::Goal(error) => Some(root_cause(error)),
            Self::Io(error) => Some(root_cause(error)),
            Self::Lsp(error) => Some(root_cause(error)),
            Self::LiveOutput(error) => Some(root_cause(error)),
            Self::NativeRunner(error) => Some(root_cause(error)),
            Self::SessionMessage(error) => Some(root_cause(error)),
            Self::ToolOutput(error) => Some(root_cause(error)),
            Self::PolicyDenial(_) | Self::ReportedFailure { .. } => None,
        }
    }
}

impl From<CommandError> for ToolError {
    fn from(error: CommandError) -> Self {
        Self::Command(error)
    }
}

impl From<CliToolError> for ToolError {
    fn from(error: CliToolError) -> Self {
        Self::CliTool(error)
    }
}

impl From<ContextError> for ToolError {
    fn from(error: ContextError) -> Self {
        Self::Context(error)
    }
}

impl From<GoalToolError> for ToolError {
    fn from(error: GoalToolError) -> Self {
        Self::Goal(error)
    }
}

impl From<FileToolError> for ToolError {
    fn from(error: FileToolError) -> Self {
        Self::FileTool(error)
    }
}

impl From<LspToolError> for ToolError {
    fn from(error: LspToolError) -> Self {
        Self::Lsp(error)
    }
}

impl From<SessionMessageError> for ToolError {
    fn from(error: SessionMessageError) -> Self {
        Self::SessionMessage(error)
    }
}

impl From<NativeRunnerError> for ToolError {
    fn from(error: NativeRunnerError) -> Self {
        Self::NativeRunner(error)
    }
}

impl From<gents_loop::live_output::LiveOutputError> for ToolError {
    fn from(error: gents_loop::live_output::LiveOutputError) -> Self {
        Self::LiveOutput(error)
    }
}

impl From<crate::tool_output::ToolOutputError> for ToolError {
    fn from(error: crate::tool_output::ToolOutputError) -> Self {
        Self::ToolOutput(error)
    }
}

impl From<std::io::Error> for ToolError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl ToolContext {
    pub(crate) fn from_default_read_root() -> Result<Self, ContextError> {
        let root = resolve_default_read_root(std::env::current_dir().ok(), dirs::home_dir())?;
        Self::new(root, false)
    }

    pub(crate) fn new(root: PathBuf, create_missing: bool) -> Result<Self, ContextError> {
        Self::new_with_base(root, None, create_missing)
    }

    pub(crate) fn new_with_base(
        root: PathBuf,
        base: Option<PathBuf>,
        create_missing: bool,
    ) -> Result<Self, ContextError> {
        if create_missing && !root.exists() {
            std::fs::create_dir_all(&root).map_err(|source| ContextError::CreateRoot {
                root: root.clone(),
                source,
            })?;
        }

        let canonical = std::fs::canonicalize(&root)
            .map_err(|source| ContextError::CanonicalizeRoot { root, source })?;

        let base = resolve_base_dir(&canonical, base);

        Ok(Self {
            root: Arc::new(canonical),
            base: Arc::new(base),
        })
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.effective_root()
    }

    pub(crate) fn base(&self) -> PathBuf {
        self.effective_base()
    }

    pub(crate) fn resolve_path_allow_create(&self, path: &str) -> Result<PathBuf, ContextError> {
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            normalize_for_creation(candidate)?
        } else {
            normalize_for_creation(&self.effective_base().join(candidate))?
        };
        self.ensure_allowed(resolved)
    }

    pub(crate) fn resolve_path(&self, path: &str) -> Result<PathBuf, ContextError> {
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            std::fs::canonicalize(candidate).map_err(|source| ContextError::ResolvePath {
                path: candidate.to_path_buf(),
                source,
            })?
        } else {
            let joined = self.effective_base().join(candidate);
            std::fs::canonicalize(&joined).map_err(|source| ContextError::ResolvePath {
                path: joined,
                source,
            })?
        };
        self.ensure_allowed(resolved)
    }

    pub(crate) fn resolve_existing_dir(&self, path: Option<&str>) -> Result<PathBuf, ContextError> {
        let resolved = match path {
            Some(path) if !path.trim().is_empty() => self.resolve_path(path)?,
            _ => self.effective_base(),
        };

        if !resolved.is_dir() {
            return Err(ContextError::NotDirectory { path: resolved });
        }

        Ok(resolved)
    }

    pub(crate) fn resolve_existing_file(&self, path: &str) -> Result<PathBuf, ContextError> {
        let resolved = self.resolve_path(path)?;
        if !resolved.is_file() {
            return Err(ContextError::NotFile { path: resolved });
        }
        Ok(resolved)
    }

    fn ensure_allowed(&self, path: PathBuf) -> Result<PathBuf, ContextError> {
        if crate::plugin::allowed::is_protected(&path) {
            return Err(ContextError::ProtectedPath { path });
        }
        let root = self.effective_root();
        if path.starts_with(&root) {
            Ok(path)
        } else {
            Err(ContextError::OutsideRoot { root, path })
        }
    }

    fn effective_root(&self) -> PathBuf {
        crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .and_then(|runtime| runtime.workspace_root)
            .unwrap_or_else(|| (*self.root).clone())
    }

    fn effective_base(&self) -> PathBuf {
        let root = self.effective_root();
        let runtime_base = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .and_then(|runtime| runtime.workspace_cwd)
            .map(|base| resolve_base_dir(&root, Some(base)));
        runtime_base.unwrap_or_else(|| resolve_base_dir(&root, Some((*self.base).clone())))
    }

    pub(crate) fn display_path(&self, path: &Path) -> String {
        for prefix in [self.effective_base(), self.effective_root()] {
            if let Ok(relative) = path.strip_prefix(&prefix) {
                let display = relative.to_string_lossy().replace('\\', "/");
                return if display.is_empty() {
                    ".".to_string()
                } else {
                    display
                };
            }
        }
        path.display().to_string()
    }
}

fn resolve_default_read_root(
    current_dir: Option<PathBuf>,
    home_dir: Option<PathBuf>,
) -> Result<PathBuf, ContextError> {
    current_dir
        .or(home_dir)
        .ok_or(ContextError::NoRootDirectory)
}

fn normalize_for_creation(path: &Path) -> Result<PathBuf, ContextError> {
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
    // Canonicalize the nearest filesystem entry, then append the missing
    // suffix. `symlink_metadata` deliberately does not follow the final link:
    // a dangling symlink must fail rather than resolve to a creatable leaf.
    let mut ancestor = normalized.as_path();
    let mut suffix = Vec::new();
    while std::fs::symlink_metadata(ancestor).is_err() {
        let name = ancestor
            .file_name()
            .ok_or_else(|| ContextError::NoExistingAncestor {
                path: path.to_path_buf(),
            })?;
        suffix.push(name.to_os_string());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| ContextError::NoExistingAncestor {
                path: path.to_path_buf(),
            })?;
    }
    let mut resolved =
        std::fs::canonicalize(ancestor).map_err(|source| ContextError::CanonicalizeAncestor {
            ancestor: ancestor.to_path_buf(),
            source,
        })?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn resolve_base_dir(root: &Path, base: Option<PathBuf>) -> PathBuf {
    let Some(base) = base else {
        return root.to_path_buf();
    };
    let canonical = match std::fs::canonicalize(&base) {
        Ok(base) => base,
        Err(_) => return root.to_path_buf(),
    };
    if canonical.is_dir() && canonical.starts_with(root) {
        canonical
    } else {
        root.to_path_buf()
    }
}

#[cfg(test)]
mod tests;
