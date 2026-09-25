use std::path::Path;

use anyhow::{Context, Result};
use gents::identity::{
    load_file_identity, load_or_create_file_identity, register_ed25519_signing_identity,
    AgentIdentity, ServiceAccount,
};
use identity::{FullIdentity as _, Identity as _, RawIdentity};
use serde::{Deserialize, Serialize};

use super::paths::DesktopPaths;

#[derive(Debug, Clone)]
pub struct PrincipalIdentity {
    did: String,
    public_key_bytes: Vec<u8>,
    private_key_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PrincipalMetadata {
    did: String,
    public_key_bytes: Vec<u8>,
}

impl PrincipalIdentity {
    pub async fn load_or_create(paths: &DesktopPaths) -> Result<Self> {
        let key_path = paths.identity_key_path().to_path_buf();
        let metadata_path = paths.principal_metadata_path();
        let metadata_exists = match tokio::fs::symlink_metadata(metadata_path).await {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.file_type().is_file(),
                    "principal metadata {} is not a regular file",
                    metadata_path.display()
                );
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(anyhow::Error::from(error)).with_context(|| {
                    format!("inspecting principal metadata {}", metadata_path.display())
                });
            }
        };
        let identity = tokio::task::spawn_blocking(move || {
            if metadata_exists {
                load_file_identity(&key_path)
            } else {
                load_or_create_file_identity(&key_path)
            }
        })
        .await
        .context("joining principal identity load")??;

        let did = identity.did().map_err(anyhow::Error::from)?.to_string();
        let public_key_bytes = identity.public_key_bytes();
        let private_key_bytes = identity.private_key_bytes().to_vec();
        let metadata = PrincipalMetadata {
            did: did.clone(),
            public_key_bytes: public_key_bytes.clone(),
        };

        validate_or_persist_metadata(metadata_path, &metadata).await?;
        register_ed25519_signing_identity(&did, &private_key_bytes, &public_key_bytes)?;

        Ok(Self {
            did,
            public_key_bytes,
            private_key_bytes,
        })
    }

    pub fn did(&self) -> &str {
        &self.did
    }

    pub fn short_did(&self) -> String {
        abbreviate_did(&self.did)
    }

    pub fn public_key_bytes(&self) -> &[u8] {
        &self.public_key_bytes
    }

    pub fn private_key_bytes(&self) -> &[u8] {
        &self.private_key_bytes
    }

    pub(crate) fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        RawIdentity::from_bytes(crypto::KeyType::Ed25519, &self.private_key_bytes)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("loading principal identity for {}", self.did))?
            .sign(payload)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("signing payload as {}", self.did))
    }
}

#[async_trait::async_trait]
impl AgentIdentity for PrincipalIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        PrincipalIdentity::sign(self, payload)
    }

    async fn verify(&self, did: &str, payload: &[u8], signature: &[u8]) -> Result<bool> {
        let (key_type, public_key_bytes) = if did == self.did {
            (crypto::KeyType::Ed25519, self.public_key_bytes.clone())
        } else if did.starts_with("did:key:") {
            crypto::parse_did_key(did)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("parsing did:key public key from DID {did}"))?
        } else {
            anyhow::bail!("no public key registered for DID {did}");
        };

        let public_key = crypto::public_key_from_bytes(key_type, &public_key_bytes)
            .map_err(anyhow::Error::from)?;
        public_key
            .verify(payload, signature)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("verifying payload for {did}"))
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        None
    }
}

async fn validate_or_persist_metadata(path: &Path, expected: &PrincipalMetadata) -> Result<()> {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let stored: PrincipalMetadata = serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing principal metadata {}", path.display()))?;
            if stored != *expected {
                return Err(anyhow::anyhow!(
                    "principal metadata mismatch at {}",
                    path.display()
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_json_atomically(path, expected).await
        }
        Err(error) => {
            Err(anyhow::Error::from(error)).with_context(|| format!("reading {}", path.display()))
        }
    }
}

async fn write_json_atomically<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let tmp_path = path.with_extension("tmp");

    tokio::fs::write(&tmp_path, bytes)
        .await
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(path).await;
    }
    tokio::fs::rename(&tmp_path, path)
        .await
        .with_context(|| format!("persisting {}", path.display()))?;
    Ok(())
}

fn abbreviate_did(did: &str) -> String {
    if did.len() <= 20 {
        return did.to_string();
    }

    format!("{}..{}", &did[..16], &did[did.len() - 4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_launch_creates_principal_files() {
        let tempdir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(tempdir.path());

        let identity = PrincipalIdentity::load_or_create(&paths).await.unwrap();

        assert!(!identity.did().is_empty());
        assert!(tokio::fs::try_exists(paths.identity_key_path())
            .await
            .unwrap());
        assert!(tokio::fs::try_exists(paths.principal_metadata_path())
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn second_launch_reuses_same_identity_material() {
        let tempdir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(tempdir.path());

        let first = PrincipalIdentity::load_or_create(&paths).await.unwrap();
        let second = PrincipalIdentity::load_or_create(&paths).await.unwrap();

        assert_eq!(first.did(), second.did());
        assert_eq!(first.public_key_bytes(), second.public_key_bytes());
        assert_eq!(first.private_key_bytes(), second.private_key_bytes());
    }

    #[tokio::test]
    async fn missing_key_after_principal_metadata_does_not_create_new_identity() {
        let tempdir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        PrincipalIdentity::load_or_create(&paths).await.unwrap();
        let original_metadata = std::fs::read(paths.principal_metadata_path()).unwrap();

        std::fs::remove_file(paths.identity_key_path()).unwrap();
        assert!(PrincipalIdentity::load_or_create(&paths).await.is_err());

        assert!(!paths.identity_key_path().exists());
        assert_eq!(
            std::fs::read(paths.principal_metadata_path()).unwrap(),
            original_metadata
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dangling_metadata_symlink_does_not_create_a_key() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(root.path().join("desktop"));
        paths.ensure_root_dirs().await.unwrap();
        symlink(
            root.path().join("absent.json"),
            paths.principal_metadata_path(),
        )
        .unwrap();

        let error = PrincipalIdentity::load_or_create(&paths).await.unwrap_err();
        assert!(format!("{error:#}").contains("is not a regular file"));
        assert!(!paths.identity_key_path().exists());
        assert!(std::fs::symlink_metadata(paths.principal_metadata_path())
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn new_principal_key_and_directory_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let tempdir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(tempdir.path().join("desktop"));

        paths.ensure_root_dirs().await.unwrap();
        PrincipalIdentity::load_or_create(&paths).await.unwrap();

        let directory_mode = std::fs::metadata(paths.root())
            .unwrap()
            .permissions()
            .mode();
        let key_mode = std::fs::metadata(paths.identity_key_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(directory_mode & 0o777, 0o700);
        assert_eq!(key_mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn principal_rejects_symlinked_key_without_touching_target() {
        use std::os::unix::fs::symlink;

        let tempdir = tempfile::tempdir().unwrap();
        let target_paths = DesktopPaths::from_root(tempdir.path().join("target"));
        PrincipalIdentity::load_or_create(&target_paths)
            .await
            .unwrap();
        let target_key = target_paths.identity_key_path();
        let original = std::fs::read(target_key).unwrap();

        let linked_paths = DesktopPaths::from_root(tempdir.path().join("linked"));
        linked_paths.ensure_root_dirs().await.unwrap();
        symlink(target_key, linked_paths.identity_key_path()).unwrap();

        assert!(PrincipalIdentity::load_or_create(&linked_paths)
            .await
            .is_err());
        assert_eq!(std::fs::read(target_key).unwrap(), original);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn principal_rejects_insecure_existing_key_without_rewriting_it() {
        use std::os::unix::fs::PermissionsExt;

        let tempdir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        PrincipalIdentity::load_or_create(&paths).await.unwrap();
        let key_path = paths.identity_key_path();
        let original = std::fs::read(key_path).unwrap();
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(PrincipalIdentity::load_or_create(&paths).await.is_err());
        assert_eq!(std::fs::read(key_path).unwrap(), original);
        assert_eq!(
            std::fs::metadata(key_path).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}
