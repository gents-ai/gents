use std::path::{Path, PathBuf};

use gents_protocol::operations::install_repair::{IncompatibleStore, IncompatibleStoreKind};
use sha2::{Digest as _, Sha256};

use crate::local::backend_protection::{BackendProtectionError, PROVENANCE_MARKER};
use crate::local::home::{default_data_dir, keys_dir, read_init_config, HomeError};
use crate::local::storage_backend::{incompatible_store, incompatible_store_kind};
use crate::local::store_key::{home_key_file, StoreKeyError};

/// Written by a node that opened and validated its store with this build's
/// collections, policies and migrations. Its content is [`fingerprint`]; a
/// store whose stamp matches is not checked again.
pub const COMPATIBILITY_STAMP: &str = ".gents-store-compatibility";

/// What a read-only look at a home's store found. `Absent` is a home with no
/// store yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreCompatibility {
    Absent,
    Compatible,
    Refused(IncompatibleStore),
}

#[derive(Debug, thiserror::Error)]
pub enum StoreCompatibilityError {
    #[error("reading {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Home(#[from] HomeError),
    #[error("{0}")]
    Key(#[from] StoreKeyError),
    #[error("opening a copy of {} to check it", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: Box<defra_node::Error>,
    },
    #[error("checking a copy of {}", path.display())]
    Check {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// The collections, migrations and access policies this build serves. Any
/// change to them changes the fingerprint, and the next start checks the
/// store again.
pub fn fingerprint() -> String {
    use gents_protocol::schemas::{
        AGENT_OWNERSHIP_POLICY_ID, CLIENT_JOURNAL_POLICY_ID, MAILBOX_POLICY_ID,
        TOOL_FILE_CHANGE_POLICY_ID,
    };
    let registry = &*gents_migration::DEFAULT_REGISTRY;
    let mut digest = Sha256::new();
    for policy in [
        AGENT_OWNERSHIP_POLICY_ID,
        CLIENT_JOURNAL_POLICY_ID,
        MAILBOX_POLICY_ID,
        TOOL_FILE_CHANGE_POLICY_ID,
    ] {
        digest.update(policy.as_bytes());
        digest.update([0]);
    }
    for collection in registry.baseline {
        digest.update(collection.name.as_bytes());
        digest.update([0]);
        digest.update(collection.sdl.as_bytes());
        digest.update([0]);
        digest.update(collection.expected_version.unwrap_or_default().as_bytes());
        digest.update([0]);
    }
    for step in registry.steps {
        digest.update(step.id().as_bytes());
        digest.update([0]);
    }
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn stamp_path(data: &Path) -> PathBuf {
    data.join(COMPATIBILITY_STAMP)
}

pub(crate) fn stamped(data: &Path) -> bool {
    std::fs::read_to_string(stamp_path(data)).is_ok_and(|stamp| stamp.trim() == fingerprint())
}

/// Records that this build opened and validated the store at `data`. Called
/// only by the owner holding the store's lock after its own checks passed.
pub fn record_compatible(data: &Path) -> std::io::Result<()> {
    let path = stamp_path(data);
    let staging = data.join(format!("{COMPATIBILITY_STAMP}.tmp"));
    std::fs::write(&staging, fingerprint())?;
    std::fs::rename(&staging, &path)
}

fn refused(kind: IncompatibleStoreKind, data_path: PathBuf) -> StoreCompatibility {
    StoreCompatibility::Refused(IncompatibleStore { kind, data_path })
}

/// Decides, without writing to `home`, whether this build can open its
/// store: the backend's own markers, the lineage marker, the encryption
/// record, the modes of its key files, then the collections' policies and
/// schema on a copy of the store taken outside the home. A store this build
/// already stamped is not copied. A copy that cannot be opened is returned as
/// an error: it does not show that the store is incompatible.
pub async fn check_home_store(home: &Path) -> Result<StoreCompatibility, StoreCompatibilityError> {
    if let Some(key) = insecure_key_file(home)? {
        return Ok(refused(IncompatibleStoreKind::InsecureKey, key));
    }
    let data = default_data_dir(home);
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| StoreCompatibilityError::Io { path, source }
    };
    if let Some(kind) = incompatible_store_kind(&data).map_err(|error| match error {
        crate::local::storage_backend::StorageBackendError::Io(source) => {
            StoreCompatibilityError::Io {
                path: data.clone(),
                source,
            }
        }
        error => StoreCompatibilityError::Check {
            path: data.clone(),
            source: Box::new(error),
        },
    })? {
        return Ok(refused(kind, data));
    }
    if !data.join("MANIFEST").is_file() {
        return Ok(StoreCompatibility::Absent);
    }
    let marker = data.join(PROVENANCE_MARKER);
    if !marker.try_exists().map_err(io(&marker))? {
        return Ok(refused(IncompatibleStoreKind::UnknownLineage, data));
    }
    if stamped(&data) {
        return Ok(StoreCompatibility::Compatible);
    }
    let Some(config) = read_init_config::<serde_json::Value, serde_json::Value>(home)? else {
        return Ok(StoreCompatibility::Compatible);
    };
    let Some(record) = config.store_encryption.as_ref() else {
        return Ok(refused(IncompatibleStoreKind::UnencryptedStore, data));
    };
    let key = match record.load(&home_key_file(home), &data) {
        Ok(key) => key,
        Err(error) => {
            return match error.incompatible() {
                Some(store) => Ok(StoreCompatibility::Refused(store.clone())),
                None => Err(error.into()),
            }
        }
    };
    Ok(match check_copy(&data, &key).await? {
        None => StoreCompatibility::Compatible,
        Some(kind) => refused(kind, data),
    })
}

/// Opens a copy of the store at `data`, taken outside its home, and asks the
/// owners of the collection bindings and the schema whether this build can
/// open it. The store itself is never opened.
pub(crate) async fn check_copy(
    data: &Path,
    key: &crate::local::store_key::StoreKey,
) -> Result<Option<IncompatibleStoreKind>, StoreCompatibilityError> {
    let data = data.to_path_buf();
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| StoreCompatibilityError::Io { path, source }
    };
    let copy = tempfile::Builder::new()
        .prefix("gents-store-check-")
        .tempdir()
        .map_err(io(&std::env::temp_dir()))?;
    let copied = copy.path().join("data");
    let (from, to) = (data.clone(), copied.clone());
    tokio::task::spawn_blocking(move || copy_tree(&from, &to))
        .await
        .map_err(|source| StoreCompatibilityError::Check {
            path: data.clone(),
            source: Box::new(source),
        })?
        .map_err(io(&data))?;
    let opened = |source| StoreCompatibilityError::Open {
        path: data.clone(),
        source: Box::new(source),
    };
    let node = crate::local::store_key::persistent_builder(&copied, key)?
        .build()
        .await
        .map_err(opened)?;
    let verdict = match crate::local::backend_protection::validate_policy_binding(&node).await {
        Err(BackendProtectionError::InvalidBinding) => {
            Ok(Some(IncompatibleStoreKind::OtherVersionSchema))
        }
        Err(error) => Err(StoreCompatibilityError::Check {
            path: data.clone(),
            source: Box::new(error),
        }),
        Ok(()) => match gents_migration::validate_existing_schema(&node).await {
            Ok(()) => Ok(None),
            Err(source) => match incompatible_store(&source, &data) {
                Some(store) => Ok(Some(store.kind)),
                None => Err(StoreCompatibilityError::Check {
                    path: data.clone(),
                    source: Box::new(source),
                }),
            },
        },
    };
    if let Err(error) = node.shutdown().await {
        tracing::debug!(error = %gents_protocol::error_chain::ErrorChain(&error), "closing the store check's copy");
    }
    drop(copy);
    verdict
}

/// A key file in the home's key folder that other users can read: a node
/// refuses to load one, and the user restores its mode.
fn insecure_key_file(home: &Path) -> Result<Option<PathBuf>, StoreCompatibilityError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let keys = keys_dir(home);
        let entries = match std::fs::read_dir(&keys) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(StoreCompatibilityError::Io { path: keys, source }),
        };
        let mut found: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| StoreCompatibilityError::Io {
                path: keys.clone(),
                source,
            })?;
            let metadata = entry
                .metadata()
                .map_err(|source| StoreCompatibilityError::Io {
                    path: entry.path(),
                    source,
                })?;
            if metadata.is_file() && metadata.permissions().mode() & 0o077 != 0 {
                found.push(entry.path());
            }
        }
        found.sort();
        Ok(found.into_iter().next())
    }
    #[cfg(not(unix))]
    {
        let _ = home;
        Ok(None)
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
