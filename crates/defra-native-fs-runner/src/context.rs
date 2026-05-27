use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

#[derive(Clone)]
pub(crate) struct RunnerContext {
    root: PathBuf,
    base: PathBuf,
}

impl RunnerContext {
    pub(crate) fn new_with_base(root: PathBuf, base: Option<PathBuf>) -> Result<Self> {
        let root = std::fs::canonicalize(&root)
            .with_context(|| format!("canonicalizing runner root {}", root.display()))?;
        let base = match base {
            Some(base) => {
                let base = std::fs::canonicalize(&base)
                    .with_context(|| format!("canonicalizing runner base {}", base.display()))?;
                if !base.is_dir() || !base.starts_with(&root) {
                    bail!(
                        "runner base {} is outside root {} or is not a directory",
                        base.display(),
                        root.display()
                    );
                }
                base
            }
            None => root.clone(),
        };
        Ok(Self { root, base })
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            std::fs::canonicalize(candidate)
                .with_context(|| format!("resolving path {}", candidate.display()))?
        } else {
            let joined = self.base.join(candidate);
            std::fs::canonicalize(&joined)
                .with_context(|| format!("resolving path {}", joined.display()))?
        };
        self.ensure_allowed(resolved)
    }

    pub(crate) fn resolve_existing_path(&self, path: Option<&str>) -> Result<PathBuf> {
        match path {
            Some(path) if !path.trim().is_empty() => self.resolve_path(path),
            _ => Ok(self.base.clone()),
        }
    }

    pub(crate) fn resolve_existing_dir(&self, path: Option<&str>) -> Result<PathBuf> {
        let resolved = self.resolve_existing_path(path)?;
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
        for prefix in [&self.base, &self.root] {
            if let Ok(relative) = path.strip_prefix(prefix) {
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
