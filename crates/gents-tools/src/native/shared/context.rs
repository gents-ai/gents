use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};

use crate::native::{CommandPolicyDenial, NativeExecutables};
use gents_fs_runner::beneath::{BeneathError, Intent, Located, RootDir};
use gents_loop::tool_call_lifecycle::FailureClass;
use gents_pack::pack::rooted_paths::ensure_unprotected;

/// The configured tool root and base, the call whose workspace root and
/// working directory override them while it is served, and the executables
/// its native filesystem runners start.
#[derive(Clone)]
pub struct ToolContext {
    root: Arc<PathBuf>,
    base: Arc<PathBuf>,
    call: gents_loop::tool::ToolCallEnvelope,
    native_executables: Arc<NativeExecutables>,
    #[cfg(test)]
    runner_blocker: Option<Arc<gents_fs_runner::TraversalBlocker>>,
}

#[derive(Debug)]
pub enum ToolError {
    Other(anyhow::Error),
    PolicyDenial(CommandPolicyDenial),
    ReportedFailure { class: FailureClass, text: String },
}

impl ToolError {
    pub(crate) fn policy_denial(denial: CommandPolicyDenial) -> Self {
        Self::PolicyDenial(denial)
    }

    pub(crate) fn reported_failure(class: FailureClass, text: String) -> Self {
        Self::ReportedFailure { class, text }
    }

    pub(crate) fn into_dispatch_error(self) -> gents_loop::tool::ToolError {
        match self {
            Self::PolicyDenial(denial) => gents_loop::tool::ToolError::ReportedFailure {
                class: FailureClass::PolicyDenied,
                text: denial.tool_error_payload(),
            },
            Self::ReportedFailure { class, text } => {
                gents_loop::tool::ToolError::ReportedFailure { class, text }
            }
            other => gents_loop::tool::ToolError::ToolCallError(Box::new(other)),
        }
    }

    #[cfg(test)]
    pub(crate) fn command_policy_denial(&self) -> Option<&CommandPolicyDenial> {
        match self {
            Self::PolicyDenial(denial) => Some(denial),
            Self::Other(_) | Self::ReportedFailure { .. } => None,
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Other(error) => write!(f, "{error:#}"),
            Self::PolicyDenial(denial) => write!(f, "{}", denial.tool_error_payload()),
            Self::ReportedFailure { text, .. } => f.write_str(text),
        }
    }
}

impl std::error::Error for ToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Other(error) => Some(error.root_cause()),
            Self::PolicyDenial(_) | Self::ReportedFailure { .. } => None,
        }
    }
}

impl From<anyhow::Error> for ToolError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

impl From<std::io::Error> for ToolError {
    fn from(error: std::io::Error) -> Self {
        Self::Other(error.into())
    }
}

impl ToolContext {
    pub(crate) fn from_default_read_root() -> Result<Self> {
        let root = resolve_default_read_root(std::env::current_dir().ok(), dirs::home_dir())?;
        Self::new(root, false)
    }

    pub fn new(root: PathBuf, create_missing: bool) -> Result<Self> {
        Self::new_with_base(root, None, create_missing)
    }

    pub(crate) fn new_with_base(
        root: PathBuf,
        base: Option<PathBuf>,
        create_missing: bool,
    ) -> Result<Self> {
        if create_missing && !root.exists() {
            std::fs::create_dir_all(&root)
                .with_context(|| format!("creating tool root {}", root.display()))?;
        }

        let canonical = std::fs::canonicalize(&root)
            .with_context(|| format!("canonicalizing tool root {}", root.display()))?;

        let base = resolve_base_dir(&canonical, base)?;

        Ok(Self {
            root: Arc::new(canonical),
            base: Arc::new(base),
            call: gents_loop::tool::ToolCallEnvelope::default(),
            native_executables: Arc::default(),
            #[cfg(test)]
            runner_blocker: None,
        })
    }

    pub(crate) fn with_native_executables(mut self, executables: NativeExecutables) -> Self {
        self.native_executables = Arc::new(executables);
        self
    }

    pub(crate) fn native_executables(&self) -> Arc<NativeExecutables> {
        Arc::clone(&self.native_executables)
    }

    /// Native filesystem runners spawned from this context pause their scan
    /// of the blocker's directory.
    #[cfg(test)]
    pub(crate) fn with_runner_blocker(
        mut self,
        blocker: gents_fs_runner::TraversalBlocker,
    ) -> Self {
        self.runner_blocker = Some(Arc::new(blocker));
        self
    }

    #[cfg(test)]
    pub(crate) fn runner_blocker(&self) -> Option<Arc<gents_fs_runner::TraversalBlocker>> {
        self.runner_blocker.clone()
    }

    /// This context while it serves `call`.
    pub(crate) fn for_call(&self, call: &gents_loop::tool::ToolCallEnvelope) -> Self {
        Self {
            call: call.clone(),
            ..self.clone()
        }
    }

    /// The call this context serves.
    pub(crate) fn call(&self) -> &gents_loop::tool::ToolCallEnvelope {
        &self.call
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.effective_root()
    }

    pub(crate) fn base(&self) -> PathBuf {
        self.effective_base()
    }

    /// Resolves `path` beneath the effective root by directory handle: the
    /// capability every file operation of the native tools acts through.
    pub(crate) fn locate(&self, path: &str, intent: Intent) -> Result<Located> {
        let root = RootDir::open(&self.effective_root())?;
        let located = root.locate(Path::new(path), &self.effective_base(), intent)?;
        ensure_unprotected(&located.path())?;
        Ok(located)
    }

    /// An existing file beneath the effective root.
    pub(crate) fn locate_existing_file(&self, path: &str) -> Result<Located> {
        let located = self.locate(path, Intent::Existing)?;
        if located.is_dir() {
            bail!("path is not a file: {}", located.path().display());
        }
        Ok(located)
    }

    /// The content of an existing file, read through its resolved handle off
    /// the runtime's threads.
    pub(crate) async fn read_existing(&self, path: &Path) -> Result<Vec<u8>> {
        let path = path.to_str().context("path is not valid UTF-8")?;
        let located = self.locate_existing_file(path)?;
        on_blocking(move || located.read()).await
    }

    /// The path `path` resolves to, for callers that hand it to another
    /// program or subsystem.
    pub(crate) fn resolve_path_allow_create(&self, path: &str) -> Result<PathBuf> {
        Ok(self.locate(path, Intent::Create)?.path())
    }

    pub(crate) fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        Ok(self.locate(path, Intent::Existing)?.path())
    }

    pub(crate) fn resolve_existing_dir(&self, path: Option<&str>) -> Result<PathBuf> {
        let resolved = match path {
            Some(path) if !path.trim().is_empty() => {
                let located = self.locate(path, Intent::Existing)?;
                if !located.is_dir() {
                    bail!("path is not a directory: {}", located.path().display());
                }
                located.path()
            }
            _ => self.effective_base(),
        };
        Ok(resolved)
    }

    pub(crate) fn resolve_existing_file(&self, path: &str) -> Result<PathBuf> {
        Ok(self.locate_existing_file(path)?.path())
    }

    fn effective_root(&self) -> PathBuf {
        crate::runtime_context::tool_runtime_context(&self.call)
            .and_then(|runtime| runtime.workspace_root)
            .unwrap_or_else(|| (*self.root).clone())
    }

    fn effective_base(&self) -> PathBuf {
        let root = self.effective_root();
        let runtime_base = crate::runtime_context::tool_runtime_context(&self.call)
            .and_then(|runtime| runtime.workspace_cwd)
            .and_then(|base| resolve_base_dir(&root, Some(base)).ok());
        runtime_base
            .unwrap_or_else(|| resolve_base_dir(&root, Some((*self.base).clone())).unwrap_or(root))
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

/// Runs a filesystem operation on the blocking pool.
pub(crate) async fn on_blocking<T>(
    operation: impl FnOnce() -> Result<T, BeneathError> + Send + 'static,
) -> Result<T>
where
    T: Send + 'static,
{
    Ok(tokio::task::spawn_blocking(operation)
        .await
        .context("the filesystem task did not finish")??)
}

fn resolve_default_read_root(
    current_dir: Option<PathBuf>,
    home_dir: Option<PathBuf>,
) -> Result<PathBuf> {
    current_dir
        .or(home_dir)
        .ok_or_else(|| anyhow!("unable to determine a tool root directory"))
}

fn resolve_base_dir(root: &Path, base: Option<PathBuf>) -> Result<PathBuf> {
    let Some(base) = base else {
        return Ok(root.to_path_buf());
    };
    let canonical = match std::fs::canonicalize(&base) {
        Ok(base) => base,
        Err(_) => return Ok(root.to_path_buf()),
    };
    if canonical.is_dir() && canonical.starts_with(root) {
        Ok(canonical)
    } else {
        Ok(root.to_path_buf())
    }
}

#[cfg(test)]
mod tests;
