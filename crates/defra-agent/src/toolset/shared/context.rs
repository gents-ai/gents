use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};

#[derive(Clone)]
pub(crate) struct ToolContext {
    root: Arc<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct ToolError(pub(crate) anyhow::Error);

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
    pub(crate) fn from_home() -> Result<Self> {
        let root = dirs::home_dir()
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| anyhow!("unable to determine a tool root directory"))?;
        Self::new(root, false)
    }

    pub(crate) fn new(root: PathBuf, create_missing: bool) -> Result<Self> {
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

    pub(crate) fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub(crate) fn resolve_path_allow_create(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            normalize_for_creation(candidate)?
        } else {
            normalize_for_creation(&self.root.join(candidate))?
        };
        self.ensure_allowed(resolved)
    }

    pub(crate) fn resolve_path(&self, path: &str) -> Result<PathBuf> {
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

    pub(crate) fn resolve_existing_dir(&self, path: Option<&str>) -> Result<PathBuf> {
        let resolved = match path {
            Some(path) if !path.trim().is_empty() => self.resolve_path(path)?,
            _ => (*self.root).clone(),
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

    pub(crate) fn display_path(&self, path: &Path) -> String {
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
