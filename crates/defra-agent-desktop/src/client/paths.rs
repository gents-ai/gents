use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

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
        let root = dirs::data_local_dir()
            .ok_or_else(|| anyhow!("unable to resolve a local application data directory"))?
            .join("defra-agent")
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

    pub fn principal_metadata_path(&self) -> &Path {
        &self.principal_metadata_path
    }

    pub fn identity_key_path(&self) -> &Path {
        &self.identity_key_path
    }

    pub fn iroh_secret_key_path(&self) -> &Path {
        &self.iroh_secret_key_path
    }
}
