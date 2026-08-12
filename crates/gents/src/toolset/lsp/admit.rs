use std::path::{Path, PathBuf};

use crate::toolset::denial::DenialReason;

/// Admit a language-server command: host PATH or an absolute path outside the
/// tool root. Never workspace-local bins. Returns the canonical executable.
pub fn admit_command(command: &str, tool_root: &Path) -> Result<PathBuf, DenialReason> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Err(DenialReason::WorkspaceExecutable);
    }

    let candidate = if trimmed.contains('/') || trimmed.contains('\\') || Path::new(trimmed).is_absolute()
    {
        PathBuf::from(trimmed)
    } else {
        which_on_host_path(trimmed).ok_or(DenialReason::WorkspaceExecutable)?
    };

    let canonical = std::fs::canonicalize(&candidate).unwrap_or(candidate);
    let root = std::fs::canonicalize(tool_root).unwrap_or_else(|_| tool_root.to_path_buf());
    if canonical.starts_with(&root) {
        return Err(DenialReason::WorkspaceExecutable);
    }
    if !canonical.is_file() {
        return Err(DenialReason::WorkspaceExecutable);
    }
    Ok(canonical)
}

fn which_on_host_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        // Never search workspace-style local bins even if they appear on PATH
        // after a project-local prepend — callers must not prepend those.
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_root_absolute_is_denied() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("evil-ls");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).unwrap();
        }
        let err = admit_command(&bin.to_string_lossy(), root.path()).unwrap_err();
        assert_eq!(err.to_contract(), "workspaceExecutable");
    }

    #[test]
    fn host_path_binary_is_admitted() {
        let root = tempfile::tempdir().unwrap();
        let admitted = admit_command("true", root.path()).or_else(|_| admit_command("/bin/true", root.path()));
        assert!(admitted.is_ok(), "{admitted:?}");
        let path = admitted.unwrap();
        assert!(!path.starts_with(root.path()));
    }
}
