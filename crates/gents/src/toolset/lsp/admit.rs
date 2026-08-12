use std::path::{Path, PathBuf};

use crate::toolset::admit_host_executable;
use crate::toolset::denial::DenialReason;

/// Admit a language-server command: host PATH or an absolute path outside the
/// tool root. Never workspace-local bins. Returns the canonical executable.
pub fn admit_command(command: &str, tool_root: &Path) -> Result<PathBuf, DenialReason> {
    admit_host_executable(command, tool_root)
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
        let admitted =
            admit_command("true", root.path()).or_else(|_| admit_command("/bin/true", root.path()));
        assert!(admitted.is_ok(), "{admitted:?}");
        let path = admitted.unwrap();
        assert!(!path.starts_with(root.path()));
    }
}
