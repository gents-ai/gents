use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("canonicalizing runner root {}", root.display())]
    CanonicalizeRoot {
        root: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("canonicalizing runner base {}", base.display())]
    CanonicalizeBase {
        base: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "runner base {} is outside root {} or is not a directory",
        base.display(),
        root.display()
    )]
    BaseOutsideRoot { base: PathBuf, root: PathBuf },
    #[error("resolving path {}", path.display())]
    Resolve {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("path is not a directory: {}", path.display())]
    NotADirectory { path: PathBuf },
    #[error("path is outside the allowed tool root {}: {}", root.display(), path.display())]
    OutsideRoot { root: PathBuf, path: PathBuf },
}

type Result<T, E = PathError> = std::result::Result<T, E>;

#[derive(Clone)]
pub(crate) struct RunnerContext {
    root: PathBuf,
    base: PathBuf,
}

impl RunnerContext {
    pub(crate) fn new_with_base(root: PathBuf, base: Option<PathBuf>) -> Result<Self> {
        let root = std::fs::canonicalize(&root)
            .map_err(|source| PathError::CanonicalizeRoot { root, source })?;
        let base = match base {
            Some(base) => {
                let base = std::fs::canonicalize(&base)
                    .map_err(|source| PathError::CanonicalizeBase { base, source })?;
                if !base.is_dir() || !base.starts_with(&root) {
                    return Err(PathError::BaseOutsideRoot { base, root });
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
            std::fs::canonicalize(candidate).map_err(|source| PathError::Resolve {
                path: candidate.to_path_buf(),
                source,
            })?
        } else {
            let joined = self.base.join(candidate);
            std::fs::canonicalize(&joined).map_err(|source| PathError::Resolve {
                path: joined,
                source,
            })?
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
            return Err(PathError::NotADirectory { path: resolved });
        }
        Ok(resolved)
    }

    fn ensure_allowed(&self, path: PathBuf) -> Result<PathBuf> {
        if path.starts_with(&self.root) {
            Ok(path)
        } else {
            Err(PathError::OutsideRoot {
                root: self.root.clone(),
                path,
            })
        }
    }

    pub(crate) fn base_dir(&self) -> &Path {
        &self.base
    }

    pub(crate) fn resolve_prune_subdir(&self, components: &[&str]) -> Option<PathBuf> {
        let mut candidate = self.base.clone();
        for component in components {
            candidate.push(component);
        }
        let resolved = std::fs::canonicalize(&candidate).ok()?;
        if resolved == candidate && resolved.is_dir() && resolved.starts_with(&self.root) {
            Some(resolved)
        } else {
            None
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
