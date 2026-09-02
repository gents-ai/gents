use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

pub const DESKTOP_HOME_ENV: &str = "GENTS_DESKTOP_HOME";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPaths {
    root: PathBuf,
    node_data_dir: PathBuf,
    peer_directory_path: PathBuf,
    principal_metadata_path: PathBuf,
    identity_key_path: PathBuf,
    iroh_secret_key_path: PathBuf,
}

impl DesktopPaths {
    pub fn discover() -> Result<Self> {
        Self::discover_with_env(std::env::var_os(DESKTOP_HOME_ENV))
    }

    fn discover_with_env(env_root: Option<OsString>) -> Result<Self> {
        if let Some(root) = env_root.filter(|value| !value.is_empty()) {
            return Ok(Self::from_root(root));
        }

        let root = dirs::data_local_dir()
            .ok_or_else(|| anyhow!("unable to resolve a local application data directory"))?
            .join("gents")
            .join("desktop");

        Ok(Self::from_root(root))
    }

    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let node_data_dir = root.join("node");
        Self {
            peer_directory_path: root.join("peers.json"),
            principal_metadata_path: root.join("principal.json"),
            identity_key_path: root.join("principal.ed25519.key"),
            iroh_secret_key_path: root.join("node.iroh.key"),
            node_data_dir,
            root,
        }
    }

    pub async fn ensure_root_dirs(&self) -> Result<()> {
        tokio::fs::create_dir_all(&self.root).await?;
        tokio::fs::create_dir_all(&self.node_data_dir).await?;
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn node_data_dir(&self) -> &Path {
        &self.node_data_dir
    }

    pub fn peer_directory_path(&self) -> &Path {
        &self.peer_directory_path
    }

    pub fn log_file_path(&self) -> PathBuf {
        self.root.join("logs").join("desktop.log")
    }

    pub fn principal_metadata_path(&self) -> &Path {
        &self.principal_metadata_path
    }

    pub fn identity_key_path(&self) -> &Path {
        &self.identity_key_path
    }

    pub fn iroh_secret_key_path(&self) -> &Path {
        &self.iroh_secret_key_path
    }

    /// Whether this home contains enough durable client state to require a
    /// restart, even before enrollment has materialized its first peer row.
    ///
    /// This is only a liveness hint. The restarted client still verifies every
    /// enrollment signature, authorization generation, lease, and route
    /// receipt before it can create authority or report readiness.
    pub fn client_state_exists(&self) -> bool {
        self.principal_metadata_path.is_file()
            && self.identity_key_path.is_file()
            && self.iroh_secret_key_path.is_file()
            && self.node_data_dir.join("MANIFEST").is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_uses_desktop_home_env_when_set() {
        let tempdir = tempfile::tempdir().expect("tempdir");

        let paths = DesktopPaths::discover_with_env(Some(tempdir.path().as_os_str().to_owned()))
            .expect("paths");

        assert_eq!(paths.root(), tempdir.path());
        assert_eq!(
            paths.peer_directory_path(),
            tempdir.path().join("peers.json")
        );
    }

    #[test]
    fn discover_without_override_uses_fresh_gents_desktop_root() {
        let paths = DesktopPaths::discover_with_env(None).expect("paths");

        assert_eq!(DESKTOP_HOME_ENV, "GENTS_DESKTOP_HOME");
        assert!(paths.root().ends_with(Path::new("gents").join("desktop")));
    }

    #[test]
    fn client_state_requires_the_complete_persisted_restart_boundary() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let paths = DesktopPaths::from_root(tempdir.path());
        std::fs::create_dir_all(paths.node_data_dir()).expect("node dir");
        std::fs::write(paths.node_data_dir().join("MANIFEST"), "manifest").expect("node manifest");
        assert!(!paths.client_state_exists());

        std::fs::write(paths.principal_metadata_path(), "{}").expect("principal metadata");
        std::fs::write(paths.identity_key_path(), [0_u8; 64]).expect("principal key");
        std::fs::write(paths.iroh_secret_key_path(), [0_u8; 32]).expect("iroh key");

        assert!(paths.client_state_exists());
    }
}
