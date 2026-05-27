use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};

#[derive(Clone)]
pub(crate) struct ToolContext {
    root: Arc<PathBuf>,
    base: Arc<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct ToolError(pub(crate) anyhow::Error);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
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
    pub(crate) fn from_default_read_root() -> Result<Self> {
        let root = resolve_default_read_root(std::env::current_dir().ok(), dirs::home_dir())?;
        Self::new(root, false)
    }

    pub(crate) fn new(root: PathBuf, create_missing: bool) -> Result<Self> {
        Self::new_with_base(root, std::env::current_dir().ok(), create_missing)
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
        })
    }

    pub(crate) fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub(crate) fn base(&self) -> &Path {
        self.base.as_path()
    }

    pub(crate) fn resolve_path_allow_create(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            normalize_for_creation(candidate)?
        } else {
            normalize_for_creation(&self.effective_base().join(candidate))?
        };
        self.ensure_allowed(resolved)
    }

    pub(crate) fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            std::fs::canonicalize(candidate)
                .with_context(|| format!("resolving path {}", candidate.display()))?
        } else {
            let joined = self.effective_base().join(candidate);
            std::fs::canonicalize(&joined)
                .with_context(|| format!("resolving path {}", joined.display()))?
        };
        self.ensure_allowed(resolved)
    }

    pub(crate) fn resolve_existing_dir(&self, path: Option<&str>) -> Result<PathBuf> {
        let resolved = match path {
            Some(path) if !path.trim().is_empty() => self.resolve_path(path)?,
            _ => self.effective_base(),
        };

        if !resolved.is_dir() {
            bail!("path is not a directory: {}", resolved.display());
        }

        Ok(resolved)
    }

    pub(crate) fn resolve_existing_file(&self, path: &str) -> Result<PathBuf> {
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

    fn effective_base(&self) -> PathBuf {
        let runtime_base = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .and_then(|runtime| runtime.workspace_cwd)
            .and_then(|base| resolve_base_dir(self.root.as_path(), Some(base)).ok());
        runtime_base.unwrap_or_else(|| (*self.base).clone())
    }

    pub(crate) fn display_path(&self, path: &Path) -> String {
        for prefix in [self.effective_base(), (*self.root).clone()] {
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
) -> Result<PathBuf> {
    current_dir
        .or(home_dir)
        .ok_or_else(|| anyhow!("unable to determine a tool root directory"))
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
