use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPaths {
    root: PathBuf,
    node_data_dir: PathBuf,
    runtime_directory_path: PathBuf,
    principal_metadata_path: PathBuf,
    identity_key_path: PathBuf,
    store_encryption_path: PathBuf,
    store_key_path: PathBuf,
}

impl DesktopPaths {
    /// The desktop client state a process was given with `--desktop-home`,
    /// else the platform's application data folder.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        match explicit {
            Some(root) => Ok(Self::from_root(root)),
            None => Self::platform_default(),
        }
    }

    /// The platform's application data folder's `gents/desktop`. A test build
    /// refuses it outside the temporary directory, so a test passes its
    /// desktop home explicitly.
    pub fn platform_default() -> Result<Self> {
        let root = dirs::data_local_dir()
            .ok_or_else(|| {
                anyhow!("unable to resolve a local application data directory; pass --desktop-home")
            })?
            .join("gents")
            .join("desktop");
        crate::local::home::refuse_real_gents_home(&root, "--desktop-home");
        Ok(Self::from_root(root))
    }

    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let node_data_dir = root.join("node");
        Self {
            runtime_directory_path: root.join("connections.json"),
            principal_metadata_path: root.join("principal.json"),
            identity_key_path: root.join("principal.ed25519.key"),
            store_encryption_path: root.join("store-encryption.json"),
            store_key_path: root.join("store.key"),
            node_data_dir,
            root,
        }
    }

    pub async fn ensure_root_dirs(&self) -> Result<()> {
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&self.root).await?;
        builder.create(&self.node_data_dir).await?;
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The CLI's own client principal under this desktop root: a separate User
    /// key, which `gents init` makes the native admin of the homes it provisions.
    pub fn cli_client(&self) -> Self {
        Self::from_root(self.root.join("cli"))
    }

    pub fn node_data_dir(&self) -> &Path {
        &self.node_data_dir
    }

    pub fn runtime_directory_path(&self) -> &Path {
        &self.runtime_directory_path
    }

    pub fn principal_metadata_path(&self) -> &Path {
        &self.principal_metadata_path
    }

    pub fn identity_key_path(&self) -> &Path {
        &self.identity_key_path
    }

    pub fn store_encryption_path(&self) -> &Path {
        &self.store_encryption_path
    }

    pub fn store_key_path(&self) -> &Path {
        &self.store_key_path
    }

    pub fn client_state_exists(&self) -> bool {
        self.principal_metadata_path.is_file()
            && self.identity_key_path.is_file()
            && self.node_data_dir.join("MANIFEST").is_file()
    }

    pub fn client_records(&self) -> [&Path; 5] {
        [
            &self.principal_metadata_path,
            &self.identity_key_path,
            &self.store_encryption_path,
            &self.store_key_path,
            &self.runtime_directory_path,
        ]
    }

    pub fn client_state_entries(&self) -> Vec<PathBuf> {
        vec![
            self.node_data_dir.clone(),
            self.store_encryption_path.clone(),
            self.store_key_path.clone(),
            self.runtime_directory_path.clone(),
            self.runtime_directory_path.with_extension("lock"),
            self.principal_metadata_path.clone(),
            self.identity_key_path.clone(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn new_desktop_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let tempdir = tempfile::tempdir().expect("tempdir");
        let paths = DesktopPaths::from_root(tempdir.path().join("desktop"));

        paths.ensure_root_dirs().await.expect("desktop directories");

        for path in [paths.root(), paths.node_data_dir()] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", path.display());
        }
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

        assert!(paths.client_state_exists());
    }
}
