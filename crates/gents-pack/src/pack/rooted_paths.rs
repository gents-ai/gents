use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use gents_fs_runner::beneath::{Intent, RootDir};

/// A directory that resolved paths must stay beneath. Resolution walks it by
/// directory handle and follows a link only while it stays beneath.
#[derive(Clone, Debug)]
pub struct RootedPaths {
    root: PathBuf,
}

impl RootedPaths {
    pub fn new(root: &Path) -> Result<Self> {
        let root = std::fs::canonicalize(root)
            .with_context(|| format!("canonicalizing tool root {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn resolve_path_allow_create(&self, path: &str) -> Result<PathBuf> {
        let located =
            RootDir::open(&self.root)?.locate(Path::new(path), &self.root, Intent::Create)?;
        let path = located.path();
        ensure_unprotected(&path)?;
        Ok(path)
    }
}

/// Refuses the operator's plugin access settings, which no tool may use.
pub fn ensure_unprotected(path: &Path) -> Result<()> {
    if crate::plugin::allowed::is_protected(path) {
        bail!(
            "{} holds the operator's plugin access settings; tools cannot use it",
            path.display()
        );
    }
    Ok(())
}
