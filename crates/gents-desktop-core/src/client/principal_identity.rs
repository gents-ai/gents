use std::path::Path;

use anyhow::{Context, Result};
use gents::store::identity::{
    load_file_identity, load_or_create_file_identity, register_ed25519_signing_identity,
    IdentityError, NodeIdentity, ServiceAccount,
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
                anyhow::ensure!(
                    metadata.file_type().is_file(),
                    "node identity metadata {} is not a regular file",
                    metadata_path.display()
                );
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(anyhow::Error::from(error)).with_context(|| {
                    format!(
                        "inspecting node identity metadata {}",
                        metadata_path.display()
                    )
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
        .context("joining node identity load")??;

        let did = identity.did().map_err(anyhow::Error::from)?.to_string();
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

    /// Synchronous callers may load an existing client key, but must never substitute
    /// a co-hosted runtime key when client identity is unavailable.
    pub fn load_existing(paths: &DesktopPaths) -> Result<Self> {
        let identity = load_file_identity(paths.identity_key_path())?;
        let did = identity.did().map_err(anyhow::Error::from)?.to_string();
        let public_key_bytes = identity.public_key_bytes();
        let private_key_bytes = identity.private_key_bytes().to_vec();
        let metadata: NodeIdentityMetadata =
            serde_json::from_slice(&std::fs::read(paths.principal_metadata_path())?)?;
        anyhow::ensure!(
            metadata.did == did && metadata.public_key_bytes == public_key_bytes,
            "client identity metadata does not match its signing key"
        );
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
}

#[async_trait::async_trait]
impl NodeIdentity for PrincipalIdentity {
    fn client_signing_capability(
        &self,
    ) -> std::result::Result<
        gents::store::config_client::ClientSigningCapability,
        gents::store::config_client::ConfigClientError,
    > {
        gents::store::config_client::ClientSigningCapability::from_ed25519_key(
            self.private_key_bytes(),
        )
    }

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

    fn service_account(&self) -> Option<&ServiceAccount> {
        None
    }
}

/// Requires the stored metadata to match `expected`, persisting it when absent.
///
/// Two first-run writers can share one home — a `gents` invocation beside the
/// desktop app, or two CLI invocations — so both may observe the file missing
/// and both persist. Their signing key is already single-winner, so the loser's
/// own re-read finds equal metadata and succeeds; only genuinely divergent
/// metadata is an error.
async fn validate_or_persist_metadata(path: &Path, expected: &NodeIdentityMetadata) -> Result<()> {
    match read_metadata(path).await? {
        Some(stored) => ensure_metadata_matches(path, &stored, expected),
        None => {
            write_json_atomically(path, expected).await?;
            match read_metadata(path).await? {
                Some(stored) => ensure_metadata_matches(path, &stored, expected),
                None => Ok(()),
            }
        }
    }
}

async fn read_metadata(path: &Path) -> Result<Option<NodeIdentityMetadata>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("parsing node identity metadata {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(anyhow::Error::from(error)).with_context(|| format!("reading {}", path.display()))
        }
    }
}

fn ensure_metadata_matches(
    path: &Path,
    stored: &NodeIdentityMetadata,
    expected: &NodeIdentityMetadata,
) -> Result<()> {
    anyhow::ensure!(
        stored == expected,
        "node identity metadata mismatch at {}",
        path.display()
    );
    Ok(())
}

/// Replaces `path` in one `rename`, staged under a name no other writer can
/// pick, so concurrent writers cannot consume each other's staging file and no
/// reader ever observes the destination missing.
async fn write_json_atomically<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    use std::io::Write as _;

    let bytes = serde_json::to_vec_pretty(value)?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut staged = tempfile::NamedTempFile::new_in(parent)
            .with_context(|| format!("staging {}", path.display()))?;
        staged
            .write_all(&bytes)
            .with_context(|| format!("writing staged {}", path.display()))?;
        staged
            .persist(&path)
            .with_context(|| format!("persisting {}", path.display()))?;
        Ok(())
    })
    .await
    .context("joining node identity metadata write")?
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_load_or_create_agrees_on_one_identity() {
        let tempdir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        paths.ensure_root_dirs().await.unwrap();

        let loads: Vec<_> = (0..8)
            .map(|_| {
                let paths = paths.clone();
                tokio::spawn(async move { PrincipalIdentity::load_or_create(&paths).await })
            })
            .collect();

        let mut dids = Vec::new();
        for load in loads {
            let identity = load.await.unwrap().expect("concurrent load_or_create");
            dids.push(identity.did().to_owned());
        }
        assert!(dids.windows(2).all(|pair| pair[0] == pair[1]), "{dids:?}");
    }

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
