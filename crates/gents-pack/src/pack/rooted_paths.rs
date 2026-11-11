use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

/// A canonical directory that resolved paths must stay inside. Resolution
/// follows existing links, so a symlink in the tree cannot lead out of it.
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
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            normalize_for_creation(candidate)?
        } else {
            normalize_for_creation(&self.root.join(candidate))?
        };
        ensure_within(&self.root, resolved)
    }
}

pub fn ensure_within(root: &Path, path: PathBuf) -> Result<PathBuf> {
    if crate::plugin::allowed::is_protected(&path) {
        bail!(
            "{} holds the operator's plugin access settings; tools cannot use it",
            path.display()
        );
    }
    if path.starts_with(root) {
        Ok(path)
    } else {
        bail!(
            "path is outside the allowed tool root {}: {}",
            root.display(),
            path.display()
        );
    }
}

pub fn normalize_for_creation(path: &Path) -> Result<PathBuf> {
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
            .ok_or_else(|| anyhow!("path has no existing ancestor: {}", path.display()))?;
        suffix.push(name.to_os_string());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| anyhow!("path has no existing ancestor: {}", path.display()))?;
    }
    let mut resolved = std::fs::canonicalize(ancestor)
        .with_context(|| format!("canonicalizing path ancestor {}", ancestor.display()))?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}
