use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

#[derive(Clone)]
pub(crate) struct RunnerContext {
    root: PathBuf,
}

impl RunnerContext {
    pub(crate) fn new(root: PathBuf) -> Result<Self> {
        let root = std::fs::canonicalize(&root)
            .with_context(|| format!("canonicalizing runner root {}", root.display()))?;
        Ok(Self { root })
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

    pub(crate) fn resolve_existing_dir(&self, path: Option<&str>) -> Result<PathBuf> {
        let resolved = match path {
            Some(path) if !path.trim().is_empty() => self.resolve_path(path)?,
            _ => self.root.clone(),
        };
        if !resolved.is_dir() {
            bail!("path is not a directory: {}", resolved.display());
        }
        Ok(resolved)
    }

    fn ensure_allowed(&self, path: PathBuf) -> Result<PathBuf> {
        if path.starts_with(&self.root) {
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
        path.strip_prefix(&self.root)
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
