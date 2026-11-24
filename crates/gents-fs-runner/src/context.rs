use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};

use crate::beneath::{Intent, Located, RootDir};
#[cfg(feature = "test-fixtures")]
use crate::blocker::TraversalBlocker;

#[derive(Clone)]
pub(crate) struct RunnerContext {
    root: RootDir,
    base: PathBuf,
    #[cfg(feature = "test-fixtures")]
    blocker: Option<TraversalBlocker>,
}

/// A directory the walk holds, and its path for display.
pub(crate) struct HeldDir {
    pub(crate) fd: Arc<OwnedFd>,
    pub(crate) path: PathBuf,
}

impl RunnerContext {
    pub(crate) fn new_with_base(root: RootDir, base: Option<PathBuf>) -> Result<Self> {
        let base = match base {
            Some(base) => {
                let base = std::fs::canonicalize(&base)
                    .with_context(|| format!("canonicalizing runner base {}", base.display()))?;
                if !base.is_dir() || !base.starts_with(root.path()) {
                    bail!(
                        "runner base {} is outside root {} or is not a directory",
                        base.display(),
                        root.path().display()
                    );
                }
                base
            }
            None => root.path().to_path_buf(),
        };
        Ok(Self {
            root,
            base,
            #[cfg(feature = "test-fixtures")]
            blocker: None,
        })
    }

    #[cfg(feature = "test-fixtures")]
    pub(crate) fn with_blocker(mut self, blocker: Option<TraversalBlocker>) -> Self {
        self.blocker = blocker;
        self
    }

    #[cfg(feature = "test-fixtures")]
    pub(crate) fn blocker(&self) -> Option<&TraversalBlocker> {
        self.blocker.as_ref()
    }

    pub(crate) fn root(&self) -> &RootDir {
        &self.root
    }

    pub(crate) fn resolve_existing_path(&self, path: Option<&str>) -> Result<Located> {
        let requested = match path {
            Some(path) if !path.trim().is_empty() => Path::new(path),
            _ => self.base.as_path(),
        };
        Ok(self.root.locate(requested, &self.base, Intent::Existing)?)
    }

    pub(crate) fn resolve_existing_dir(&self, path: Option<&str>) -> Result<HeldDir> {
        let located = self.resolve_existing_path(path)?;
        Ok(HeldDir {
            fd: located.open_dir()?,
            path: located.path(),
        })
    }

    pub(crate) fn base_dir(&self) -> &Path {
        &self.base
    }

    /// The directory the literal components name below the base, when no
    /// link stands on the way to it.
    pub(crate) fn resolve_prune_subdir(&self, components: &[&str]) -> Option<HeldDir> {
        let mut candidate = self.base.clone();
        candidate.extend(components);
        let located = self
            .root
            .locate(&candidate, &self.base, Intent::Existing)
            .ok()?;
        if located.path() != candidate {
            return None;
        }
        Some(HeldDir {
            fd: located.open_dir().ok()?,
            path: candidate,
        })
    }

    pub(crate) fn display_path(&self, path: &Path) -> String {
        for prefix in [&self.base, &self.root.path().to_path_buf()] {
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

    /// The path of `path` below the root, for resolving it again by handle.
    pub(crate) fn below_root<'a>(&self, path: &'a Path) -> Option<&'a Path> {
        path.strip_prefix(self.root.path()).ok()
    }
}
