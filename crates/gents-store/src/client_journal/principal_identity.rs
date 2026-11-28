use std::path::Path;

use crate::identity::{
    load_file_identity, load_or_create_file_identity, IdentityError, NodeIdentity, ServiceAccount,
};
use anyhow::{Context, Result};
use identity::{FullIdentity as _, Identity as _, RawIdentity};
use serde::{Deserialize, Serialize};

use super::paths::DesktopPaths;

#[derive(Clone)]
pub struct PrincipalIdentity {
    did: String,
    public_key_bytes: Vec<u8>,
    private_key_bytes: Vec<u8>,
}

impl std::fmt::Debug for PrincipalIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PrincipalIdentity")
            .field("did", &self.did)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct NodeIdentityMetadata {
    did: String,
    public_key_bytes: Vec<u8>,
}

impl PrincipalIdentity {
    pub async fn load_or_create(paths: &DesktopPaths) -> Result<Self> {
        let paths = paths.clone();
        tokio::task::spawn_blocking(move || {
            Self::load_or_create_at(&paths, |step| {
                tracing::trace!(?step, "client identity metadata")
            })
        })
        .await
        .context("joining node identity load")?
    }

    fn load_or_create_at<F>(paths: &DesktopPaths, mut observe: F) -> Result<Self>
    where
        F: FnMut(MetadataStep),
    {
        let key_path = paths.identity_key_path();
        let metadata_path = paths.principal_metadata_path();
        let identity = match std::fs::symlink_metadata(metadata_path) {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.file_type().is_file(),
                    "node identity metadata {} is not a regular file",
                    metadata_path.display()
                );
                load_file_identity(key_path)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                load_or_create_file_identity(key_path)?
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "inspecting node identity metadata {}",
                        metadata_path.display()
                    )
                })
            }
        };
        let did = identity.did().map_err(anyhow::Error::from)?.to_string();
        let public_key_bytes = identity.public_key_bytes();
        let private_key_bytes = identity.private_key_bytes().to_vec();
        let metadata = NodeIdentityMetadata {
            did: did.clone(),
            public_key_bytes: public_key_bytes.clone(),
        };

        validate_or_persist_metadata(metadata_path, &metadata, &mut observe)?;

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

    pub fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, IdentityError> {
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
        crate::config_client::ClientSigningCapability,
        crate::config_client::ConfigClientError,
    > {
        crate::config_client::ClientSigningCapability::from_ed25519_key(self.private_key_bytes())
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

/// Which read of the metadata a writer is at (Lean `ClientIdentityMetadata.Read`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MetadataRead {
    First,
    /// After the writer renamed its own staging file over the destination.
    AfterInstall,
}

/// Lean `ClientIdentityMetadata.Decision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MetadataDecision {
    Accept,
    Install,
    Mismatch,
    Missing,
}

/// One writer's choice at a read (Lean `ClientIdentityMetadata.decide`). Two
/// first-run writers can share one home, a `gents` invocation beside the
/// desktop app or two CLI invocations, so both may find the metadata absent
/// and both install it. Their signing key is single-winner, so a writer that
/// lost the race finds equal metadata on its second read and accepts it.
fn decide_metadata(
    read: MetadataRead,
    stored: Option<&NodeIdentityMetadata>,
    expected: &NodeIdentityMetadata,
) -> MetadataDecision {
    match (read, stored) {
        (_, Some(stored)) if stored == expected => MetadataDecision::Accept,
        (_, Some(_)) => MetadataDecision::Mismatch,
        (MetadataRead::First, None) => MetadataDecision::Install,
        (MetadataRead::AfterInstall, None) => MetadataDecision::Missing,
    }
}

fn read_and_decide(
    path: &Path,
    read: MetadataRead,
    expected: &NodeIdentityMetadata,
) -> Result<MetadataDecision> {
    Ok(decide_metadata(
        read,
        read_metadata(path)?.as_ref(),
        expected,
    ))
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum MetadataStep {
    Read,
    Create,
    Fill,
    Rename,
    Decide,
}

/// Requires the stored metadata to match `expected`, installing it when
/// absent. The second read never installs again (Lean
/// `the_second_read_never_installs`), so this installs at most once.
fn validate_or_persist_metadata<F>(
    path: &Path,
    expected: &NodeIdentityMetadata,
    observe: &mut F,
) -> Result<()>
where
    F: FnMut(MetadataStep),
{
    let mut read = MetadataRead::First;
    loop {
        let decision = read_and_decide(path, read, expected)?;
        observe(match read {
            MetadataRead::First => MetadataStep::Read,
            MetadataRead::AfterInstall => MetadataStep::Decide,
        });
        match decision {
            MetadataDecision::Accept => return Ok(()),
            MetadataDecision::Install => {
                write_json_atomically(path, expected, observe)?;
                read = MetadataRead::AfterInstall;
            }
            MetadataDecision::Mismatch => {
                anyhow::bail!("node identity metadata mismatch at {}", path.display())
            }
            MetadataDecision::Missing => anyhow::bail!(
                "node identity metadata {} is missing after it was installed",
                path.display()
            ),
        }
    }
}

fn read_metadata(path: &Path) -> Result<Option<NodeIdentityMetadata>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("parsing node identity metadata {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(anyhow::Error::from(error)).with_context(|| format!("reading {}", path.display()))
        }
    }
}

/// Replaces `path` in one `rename`, staged under a name no other writer can
/// pick, so concurrent writers cannot consume each other's staging file and no
/// reader ever observes the destination missing.
fn write_json_atomically<T, F>(path: &Path, value: &T, observe: &mut F) -> Result<()>
where
    T: Serialize,
    F: FnMut(MetadataStep),
{
    use std::io::Write as _;

    let bytes = serde_json::to_vec_pretty(value)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut staged = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("staging {}", path.display()))?;
    observe(MetadataStep::Create);
    staged
        .write_all(&bytes)
        .with_context(|| format!("writing staged {}", path.display()))?;
    observe(MetadataStep::Fill);
    staged
        .persist(path)
        .with_context(|| format!("persisting {}", path.display()))?;
    observe(MetadataStep::Rename);
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

    #[derive(Debug, Clone, Copy, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Stored {
        Absent,
        Equal,
        Different,
    }

    /// Replays Lean `ClientIdentityMetadata.decide` against a real metadata
    /// file in each modelled state.
    #[test]
    fn metadata_decisions_match_the_lean_model() {
        let expected = NodeIdentityMetadata {
            did: "did:key:expected".to_owned(),
            public_key_bytes: vec![1, 2, 3],
        };
        let different = NodeIdentityMetadata {
            did: "did:key:different".to_owned(),
            public_key_bytes: vec![4, 5, 6],
        };
        let cases = gents_lean_vocab::families::client_identity_metadata_decisions::cases::<
            MetadataDecision,
            MetadataRead,
            Stored,
        >();
        assert_eq!(cases.len(), 6);
        for case in cases {
            let tempdir = tempfile::tempdir().unwrap();
            let path = tempdir.path().join("principal.json");
            match case.input.stored {
                Stored::Absent => {}
                Stored::Equal => write_json_atomically(&path, &expected, &mut |_| {}).unwrap(),
                Stored::Different => write_json_atomically(&path, &different, &mut |_| {}).unwrap(),
            }
            let decision = read_and_decide(&path, case.input.read, &expected).unwrap();
            case.check(decision);
        }
    }

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

#[cfg(test)]
mod trace_replay;
