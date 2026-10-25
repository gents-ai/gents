use std::path::{Path, PathBuf};

use gents::identity::{
    load_file_identity, load_or_create_file_identity, register_ed25519_signing_identity,
    IdentityError, NodeIdentity, ServiceAccount,
};
use identity::{FullIdentity as _, Identity as _, RawIdentity};
use serde::{Deserialize, Serialize};

use super::paths::DesktopPaths;

#[derive(Debug, thiserror::Error)]
pub enum PrincipalIdentityError {
    #[error("node identity metadata {} is not a regular file", path.display())]
    MetadataNotRegularFile { path: PathBuf },
    #[error("inspecting node identity metadata {}", path.display())]
    InspectMetadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("joining node identity load")]
    JoinIdentityLoad(#[source] tokio::task::JoinError),
    #[error(transparent)]
    IdentityKey(#[from] IdentityError),
    #[error(transparent)]
    Did(#[from] identity::Error),
    #[error("parsing node identity metadata {}", path.display())]
    ParseMetadata {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("node identity metadata mismatch at {}", path.display())]
    MetadataMismatch { path: PathBuf },
    #[error("reading {}", path.display())]
    ReadMetadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("writing {}", path.display())]
    WriteTemp {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("persisting {}", path.display())]
    Persist {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

type Result<T, E = PrincipalIdentityError> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
pub struct PrincipalIdentity {
    did: String,
    public_key_bytes: Vec<u8>,
    private_key_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct NodeIdentityMetadata {
    did: String,
    public_key_bytes: Vec<u8>,
}

impl PrincipalIdentity {
    pub async fn load_or_create(paths: &DesktopPaths) -> Result<Self> {
        let key_path = paths.identity_key_path().to_path_buf();
        let metadata_path = paths.principal_metadata_path();
        let metadata_exists = match tokio::fs::symlink_metadata(metadata_path).await {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(PrincipalIdentityError::MetadataNotRegularFile {
                        path: metadata_path.to_path_buf(),
                    });
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(source) => {
                return Err(PrincipalIdentityError::InspectMetadata {
                    path: metadata_path.to_path_buf(),
                    source,
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
        .map_err(PrincipalIdentityError::JoinIdentityLoad)??;

        let did = identity.did()?.to_string();
        let public_key_bytes = identity.public_key_bytes();
        let private_key_bytes = identity.private_key_bytes().to_vec();
        let metadata = NodeIdentityMetadata {
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

    pub fn recorded_did(path: &Path) -> Option<String> {
        if !std::fs::symlink_metadata(path).ok()?.is_file() {
            return None;
        }
        let metadata: NodeIdentityMetadata =
            serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        (metadata.did.starts_with("did:key:") && !metadata.public_key_bytes.is_empty())
            .then_some(metadata.did)
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

    pub(crate) fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, IdentityError> {
        let identity = RawIdentity::from_bytes(crypto::KeyType::Ed25519, &self.private_key_bytes)
            .map_err(|source| IdentityError::LoadNodeIdentity {
            did: self.did.clone(),
            source,
        })?;
        identity
            .sign(payload)
            .map_err(|source| IdentityError::SignPayloadAs {
                did: self.did.clone(),
                source,
            })
    }

    fn verify_signature(
        &self,
        did: &str,
        payload: &[u8],
        signature: &[u8],
    ) -> Result<bool, IdentityError> {
        let (key_type, public_key_bytes) = if did == self.did {
            (crypto::KeyType::Ed25519, self.public_key_bytes.clone())
        } else if did.starts_with("did:key:") {
            crypto::parse_did_key(did).map_err(|source| IdentityError::ParseDidKey {
                did: did.to_owned(),
                source,
            })?
        } else {
            return Err(IdentityError::NoPublicKey {
                did: did.to_owned(),
            });
        };

        let public_key = crypto::public_key_from_bytes(key_type, &public_key_bytes)
            .map_err(IdentityError::Crypto)?;
        public_key
            .verify(payload, signature)
            .map_err(|source| IdentityError::VerifyPayload {
                did: did.to_owned(),
                source,
            })
    }
}

#[async_trait::async_trait]
impl NodeIdentity for PrincipalIdentity {
    fn did(&self) -> &str {
        &self.did
    }

    async fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, IdentityError> {
        PrincipalIdentity::sign(self, payload)
    }

    async fn verify(
        &self,
        did: &str,
        payload: &[u8],
        signature: &[u8],
    ) -> Result<bool, IdentityError> {
        self.verify_signature(did, payload, signature)
    }

    fn service_account(&self) -> Option<&ServiceAccount> {
        None
    }
}

async fn validate_or_persist_metadata(path: &Path, expected: &NodeIdentityMetadata) -> Result<()> {
    match tokio::fs::read(path).await {
        Ok(bytes) => {
            let stored: NodeIdentityMetadata =
                serde_json::from_slice(&bytes).map_err(|source| {
                    PrincipalIdentityError::ParseMetadata {
                        path: path.to_path_buf(),
                        source,
                    }
                })?;
            if stored != *expected {
                return Err(PrincipalIdentityError::MetadataMismatch {
                    path: path.to_path_buf(),
                });
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_json_atomically(path, expected).await
        }
        Err(source) => Err(PrincipalIdentityError::ReadMetadata {
            path: path.to_path_buf(),
            source,
        }),
    }
}

async fn write_json_atomically<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    let bytes = serde_json::to_vec_pretty(value)?;
    let tmp_path = path.with_extension("tmp");

    tokio::fs::write(&tmp_path, bytes).await.map_err(|source| {
        PrincipalIdentityError::WriteTemp {
            path: tmp_path.clone(),
            source,
        }
    })?;
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(path).await;
    }
    tokio::fs::rename(&tmp_path, path)
        .await
        .map_err(|source| PrincipalIdentityError::Persist {
            path: path.to_path_buf(),
            source,
        })?;
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

    #[cfg(unix)]
    #[tokio::test]
    async fn new_node_identity_key_and_directory_are_private() {
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
    async fn node_identity_rejects_symlinked_key_without_touching_target() {
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
    async fn node_identity_rejects_insecure_existing_key_without_rewriting_it() {
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
