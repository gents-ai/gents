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

    #[test]
    fn rustup_proxy_admits_the_selected_toolchain_binary() {
        let Some(proxy) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("rust-analyzer"))
                .find(|candidate| candidate.is_file())
        }) else {
            return;
        };
        let proxy_target = std::fs::canonicalize(&proxy).unwrap_or(proxy);
        if proxy_target
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.trim_end_matches(".exe"))
            != Some("rustup")
        {
            return;
        }

        let workspace = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        // rustup is the authority on whether the SELECTED toolchain actually
        // carries the component; a shim on PATH proves nothing (rustup
        // installs proxies for every known tool, and minimal profiles — this
        // repo pins one — omit rust-analyzer). Executing rustup here is fine:
        // the production constraint is only that ADMISSION never executes a
        // helper.
        let Ok(oracle) = std::process::Command::new(&proxy_target)
            .args(["which", "rust-analyzer"])
            .current_dir(&workspace)
            .output()
        else {
            return;
        };
        if !oracle.status.success() {
            // The selected toolchain has no rust-analyzer binary: the shim
            // would fail at spawn, so admission must fail closed instead of
            // resolving a nonexistent path.
            let err = admit_command("rust-analyzer", &workspace)
                .expect_err("missing toolchain component must deny, not resolve");
            assert_eq!(err.to_contract(), "workspaceExecutable");
            return;
        }
        let expected =
            std::path::PathBuf::from(String::from_utf8_lossy(&oracle.stdout).trim().to_string());
        let expected = std::fs::canonicalize(&expected).unwrap_or(expected);

        let admitted = admit_command("rust-analyzer", &workspace)
            .expect("installed rustup rust-analyzer proxy must resolve without executing a helper");
        assert_eq!(
            admitted, expected,
            "admission's side-effect-free resolution must agree with `rustup which`"
        );
        assert!(admitted.is_file(), "{}", admitted.display());
        assert_ne!(admitted, proxy_target);
        assert_eq!(
            admitted.file_name().and_then(|name| name.to_str()),
            Some(if cfg!(windows) {
                "rust-analyzer.exe"
            } else {
                "rust-analyzer"
            })
        );
    }
}
