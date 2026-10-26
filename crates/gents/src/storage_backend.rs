use std::io::Read;
use std::path::{Path, PathBuf};

use gents_protocol::error_chain::{find_map_source, find_source};

#[derive(Debug, thiserror::Error)]
pub enum StorageBackendError {
    #[error(transparent)]
    Incompatible(#[from] IncompatibleStore),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{} has no existing database", path.display())]
    NoExistingDatabase { path: PathBuf },
}

impl StorageBackendError {
    pub fn incompatible(&self) -> Option<&IncompatibleStore> {
        match self {
            Self::Incompatible(store) => Some(store),
            Self::Io(_) | Self::NoExistingDatabase { .. } => None,
        }
    }
}

type Result<T, E = StorageBackendError> = std::result::Result<T, E>;

/// In-process hosts keep the server's value/key limits, isolation and
/// durability; the upstream embedded profile restricts document sizes and
/// disables background compaction. These are per-store working-set targets,
/// not a process RSS limit.
pub fn regolith_options() -> storage::RegolithStoreOptions {
    let mut options = storage::RegolithStoreOptions::default();
    options.engine = options
        .engine
        .write_buffer_size(32 * 1024 * 1024)
        .max_write_buffer_number(2)
        .block_cache_size(64 * 1024 * 1024);
    options
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncompatibleStoreKind {
    LegacyRocksDb,
    LegacyLark,
    UnsupportedOrCorrupt,
    /// A Regolith store whose schema lineage this build does not know: it was
    /// written by an older (or foreign) Gents build. This release has no
    /// migration from it, so it cannot be opened.
    UnknownLineage,
    /// A Regolith store that descends from this build's baseline but carries
    /// schema versions another build (for example a newer one) added.
    ForeignVersion,
    /// An identity key an older build wrote with group/other access. The
    /// identity owner refuses it rather than trusting a possibly exposed
    /// key; the home it belongs to needs a fresh start.
    InsecureKey,
    /// A store with no recorded at-rest key: written unencrypted by an
    /// earlier release (or left by an interrupted initialization). Encryption
    /// cannot be turned on in place.
    UnencryptedStore,
    /// An encrypted store whose recorded key is definitely gone (its Keychain
    /// item or key file no longer exists), so its data cannot be read.
    MissingStoreKey,
}

impl IncompatibleStoreKind {
    pub fn is_older(self) -> bool {
        !matches!(self, Self::ForeignVersion)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", describe(*kind, data_path))]
pub struct IncompatibleStore {
    pub kind: IncompatibleStoreKind,
    pub data_path: PathBuf,
}

fn describe(kind: IncompatibleStoreKind, data_path: &Path) -> String {
    let path = data_path.display();
    match kind {
        IncompatibleStoreKind::LegacyRocksDb => format!(
            "{path} contains a legacy RocksDB Gents store; this release uses Regolith and cannot open it. Reset the runtime state or use an older Gents release to export any data you need first"
        ),
        IncompatibleStoreKind::LegacyLark => format!(
            "{path} contains a legacy Lark Gents store; this release uses Regolith and cannot open it. Reset the runtime state or use an older Gents release to export any data you need first"
        ),
        IncompatibleStoreKind::UnsupportedOrCorrupt => format!(
            "{path} contains an unsupported or corrupt Gents store; this release uses Regolith and cannot open it. Reset the runtime state or use an older Gents release to export any data you need first"
        ),
        IncompatibleStoreKind::UnknownLineage => format!(
            "{path} was created by an older Gents version. This release is a breaking release with no migration from it, so it cannot open that store. Back it up or delete it to start fresh, or use the release that created it to export any data you need first"
        ),
        IncompatibleStoreKind::InsecureKey => format!(
            "This home's keys were created by an older Gents version with unsafe file permissions ({path} is readable by other users). This release does not load a key that may have been exposed. Back it up or delete it to start fresh with a new identity"
        ),
        IncompatibleStoreKind::ForeignVersion => format!(
            "{path} was written by a different Gents version whose schema this release does not know, so it cannot open that store. Use the version that wrote it, or back it up to start fresh"
        ),
        IncompatibleStoreKind::UnencryptedStore => format!(
            "{path} is not encrypted at rest: it was created by an older Gents version (or by an initialization that did not finish). This release opens only encrypted stores and cannot convert one, so re-initialize the home to start fresh (`gents init --dangerously-overwrite`), backing it up first if you need its data"
        ),
        IncompatibleStoreKind::MissingStoreKey => format!(
            "{path} is encrypted, but the key this home recorded for it no longer exists (its Keychain item or key file was removed), so its data cannot be read. Re-initialize the home to start fresh (`gents init --dangerously-overwrite`)"
        ),
    }
}

/// Inspect the exact on-disk markers owned by the storage backend without
/// opening or mutating the store.
pub fn incompatible_store_kind(data_path: &Path) -> Result<Option<IncompatibleStoreKind>> {
    if data_path.join("CURRENT").is_file() {
        return Ok(Some(IncompatibleStoreKind::LegacyRocksDb));
    }
    if data_path.join("data.lark").exists() {
        return Ok(Some(IncompatibleStoreKind::LegacyLark));
    }
    let manifest_path = data_path.join("MANIFEST");
    if manifest_path.is_file() {
        let mut manifest = std::fs::File::open(&manifest_path)?;
        let mut magic = [0_u8; 7];
        let read = manifest.read(&mut magic)?;
        if read != magic.len() || magic != *b"REGOMAN" {
            return Ok(Some(IncompatibleStoreKind::UnsupportedOrCorrupt));
        }
    }
    Ok(None)
}

/// Reject a data directory created by a retired runtime storage backend.
/// Regolith, Lark, and RocksDB use incompatible on-disk formats. Regolith
/// would otherwise try to open legacy files as its own store, so callers must
/// fail before opening the directory.
pub fn reject_legacy_store(data_path: &Path) -> Result<()> {
    match incompatible_store_kind(data_path)? {
        Some(kind) => Err(StorageBackendError::Incompatible(IncompatibleStore {
            kind,
            data_path: data_path.to_path_buf(),
        })),
        None => Ok(()),
    }
}

/// Refuse to open a new store when a caller only intends to inspect retained
/// data. The backend otherwise creates an empty database in a key-only home.
pub(crate) fn require_existing_store(data_path: &Path) -> Result<()> {
    reject_legacy_store(data_path)?;
    if !data_path.join("MANIFEST").is_file() {
        return Err(StorageBackendError::NoExistingDatabase {
            path: data_path.to_path_buf(),
        });
    }
    Ok(())
}

pub fn incompatible_store(
    error: &(dyn std::error::Error + 'static),
    data_path: &Path,
) -> Option<IncompatibleStore> {
    if let Some(store) = find_incompatible(error) {
        return Some(store.clone());
    }
    if let Some(key) = find_insecure_key(error) {
        return Some(IncompatibleStore {
            kind: IncompatibleStoreKind::InsecureKey,
            data_path: key.path.clone(),
        });
    }
    find_source::<gents_migration::Error>(error)
        .and_then(|cause| {
            if cause.is_unknown_lineage() {
                Some(IncompatibleStoreKind::UnknownLineage)
            } else if cause.is_foreign_version() {
                Some(IncompatibleStoreKind::ForeignVersion)
            } else {
                None
            }
        })
        .map(|kind| IncompatibleStore {
            kind,
            data_path: data_path.to_path_buf(),
        })
}

/// Each owner is asked by its own type too, since transparent wrappers
/// forward `source()` past the sentinel they carry.
fn find_incompatible<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a IncompatibleStore> {
    find_map_source(error, |cause| {
        cause
            .downcast_ref::<IncompatibleStore>()
            .or_else(|| cause.downcast_ref::<StorageBackendError>()?.incompatible())
            .or_else(|| {
                cause
                    .downcast_ref::<crate::store_key::StoreKeyError>()?
                    .incompatible()
            })
            .or_else(|| {
                cause
                    .downcast_ref::<crate::store_key::upgrade::UpgradeError>()?
                    .incompatible()
            })
    })
}

fn find_insecure_key<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a crate::identity::InsecureKeyPermissions> {
    find_map_source(error, |cause| {
        cause
            .downcast_ref::<crate::identity::InsecureKeyPermissions>()
            .or_else(|| {
                cause
                    .downcast_ref::<crate::identity::IdentityError>()?
                    .insecure_key()
            })
            .or_else(|| {
                cause
                    .downcast_ref::<crate::store_key::StoreKeyError>()?
                    .insecure_key()
            })
            .or_else(|| {
                cause
                    .downcast_ref::<crate::config_client::ConfigClientError>()?
                    .insecure_key()
            })
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ClassifiedStoreError<E>
where
    E: std::error::Error + 'static,
{
    #[error("{store}")]
    Refused {
        store: IncompatibleStore,
        #[source]
        source: E,
    },
    #[error(transparent)]
    Unclassified(E),
}

pub fn classify_store_error<E>(error: E, data_path: &Path) -> ClassifiedStoreError<E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    if find_incompatible(&error).is_some() {
        return ClassifiedStoreError::Unclassified(error);
    }
    match incompatible_store(&error, data_path) {
        Some(store) => ClassifiedStoreError::Refused {
            store,
            source: error,
        },
        None => ClassifiedStoreError::Unclassified(error),
    }
}

/// Drives a future that opens or holds an embedded node, whose build and
/// handles are not `Send`, on a blocking-pool thread of the supplied runtime so
/// no async worker waits on it. That thread owns the future until it
/// finishes, even if the caller drops the returned future.
#[expect(
    clippy::disallowed_methods,
    reason = "the sole owner of block_on on a blocking-pool thread; no async worker is held"
)]
pub async fn drive_unsendable<T, F, Fut>(
    runtime: &tokio::runtime::Handle,
    operation: F,
) -> Result<T, tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T>,
{
    let blocking = runtime.clone();
    runtime
        .spawn_blocking(move || blocking.block_on(operation()))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_refusals_classify_through_every_owner_wrapper() {
        use crate::identity::{IdentityError, InsecureKeyPermissions};
        use crate::store_key::upgrade::UpgradeError;
        use crate::store_key::StoreKeyError;
        let store = || IncompatibleStore {
            kind: IncompatibleStoreKind::LegacyLark,
            data_path: "/data".into(),
        };
        let kind = |error: anyhow::Error| {
            incompatible_store(error.context("opening").as_ref(), Path::new("/other"))
                .map(|store| (store.kind, store.data_path))
        };
        let refused = Some((IncompatibleStoreKind::LegacyLark, PathBuf::from("/data")));
        for error in [
            anyhow::Error::new(store()),
            StorageBackendError::from(store()).into(),
            StoreKeyError::from(store()).into(),
            StoreKeyError::Storage(store().into()).into(),
            StoreKeyError::Upgrade(UpgradeError::StorageBackend(store().into())).into(),
            UpgradeError::StorageBackend(store().into()).into(),
        ] {
            assert_eq!(kind(error), refused);
        }
        let key = || InsecureKeyPermissions {
            path: "/key".into(),
            mode: 0o644,
        };
        let insecure = Some((IncompatibleStoreKind::InsecureKey, PathBuf::from("/key")));
        for error in [
            anyhow::Error::new(key()),
            IdentityError::from(key()).into(),
            StoreKeyError::Identity(key().into()).into(),
            crate::config_client::ConfigClientError::from(IdentityError::from(key())).into(),
        ] {
            assert_eq!(kind(error), insecure);
        }
        assert_eq!(
            kind(StorageBackendError::NoExistingDatabase { path: "/d".into() }.into()),
            None
        );
    }

    #[test]
    fn unavailable_store_keys_classify_through_their_owner() {
        use crate::store_key::{store_key_unavailable, StoreKeyError, StoreKeyUnavailable};
        let unavailable = || StoreKeyUnavailable {
            data_path: "/data".into(),
            code: -25293,
        };
        for error in [
            anyhow::Error::new(unavailable()),
            StoreKeyError::from(unavailable()).into(),
        ] {
            let error = error.context("opening");
            assert_eq!(
                store_key_unavailable(error.as_ref()).map(|key| key.code),
                Some(-25293)
            );
        }
        let other: anyhow::Error = StoreKeyError::KeychainUnsupported.into();
        assert!(store_key_unavailable(other.as_ref()).is_none());
    }

    #[test]
    fn in_process_budget_preserves_server_acceptance_and_commit_settings() {
        let server = storage::RegolithStoreOptions::default();
        let local = regolith_options();
        assert_eq!(local.engine.get_write_buffer_size(), 32 * 1024 * 1024);
        assert_eq!(local.engine.get_max_write_buffer_number(), 2);
        assert_eq!(local.engine.get_block_cache_size(), 64 * 1024 * 1024);
        assert_eq!(
            local.engine.get_max_key_size(),
            server.engine.get_max_key_size()
        );
        assert_eq!(
            local.engine.get_max_value_size(),
            server.engine.get_max_value_size()
        );
        assert_eq!(
            local.engine.get_durability(),
            server.engine.get_durability()
        );
        assert_eq!(local.isolation, server.isolation);
        assert_eq!(local.close_timeout, server.close_timeout);
        assert_eq!(
            local.engine.get_max_background_compactions(),
            server.engine.get_max_background_compactions()
        );
    }
    #[test]
    fn rejects_rocksdb_current_marker() {
        let tempdir = tempfile::tempdir().unwrap();
        std::fs::write(tempdir.path().join("CURRENT"), "MANIFEST-000005\n").unwrap();

        let error = reject_legacy_store(tempdir.path()).unwrap_err();

        assert_eq!(
            incompatible_store_kind(tempdir.path()).unwrap(),
            Some(IncompatibleStoreKind::LegacyRocksDb)
        );

        assert!(error.to_string().contains("legacy RocksDB Gents store"));
        assert_eq!(
            incompatible_store(&error, tempdir.path()).map(|store| store.kind),
            Some(IncompatibleStoreKind::LegacyRocksDb)
        );
    }

    #[test]
    fn rejects_lark_manifest() {
        let tempdir = tempfile::tempdir().unwrap();
        std::fs::create_dir(tempdir.path().join("data.lark")).unwrap();

        let error = reject_legacy_store(tempdir.path()).unwrap_err();

        assert!(error.to_string().contains("legacy Lark Gents store"));
    }

    #[test]
    fn rejects_unknown_manifest_format() {
        let tempdir = tempfile::tempdir().unwrap();
        std::fs::write(tempdir.path().join("MANIFEST"), "unknown").unwrap();

        let error = reject_legacy_store(tempdir.path()).unwrap_err();

        assert!(error.to_string().contains("unsupported or corrupt"));
    }

    #[test]
    fn accepts_empty_or_regolith_data_directory() {
        let tempdir = tempfile::tempdir().unwrap();
        reject_legacy_store(tempdir.path()).unwrap();

        std::fs::write(tempdir.path().join("MANIFEST"), b"REGOMAN\x01rest").unwrap();
        reject_legacy_store(tempdir.path()).unwrap();
    }

    #[test]
    fn inspection_requires_an_existing_compatible_store() {
        let tempdir = tempfile::tempdir().unwrap();
        let error = require_existing_store(tempdir.path()).unwrap_err();
        assert!(error.to_string().contains("has no existing database"));
        assert_eq!(std::fs::read_dir(tempdir.path()).unwrap().count(), 0);

        std::fs::write(tempdir.path().join("MANIFEST"), b"REGOMAN\x01rest").unwrap();
        require_existing_store(tempdir.path()).unwrap();

        std::fs::write(tempdir.path().join("CURRENT"), "MANIFEST-000005\n").unwrap();
        let error = require_existing_store(tempdir.path()).unwrap_err();
        assert_eq!(
            incompatible_store(&error, tempdir.path()).map(|store| store.kind),
            Some(IncompatibleStoreKind::LegacyRocksDb)
        );
    }

    #[tokio::test]
    async fn older_regolith_lineage_is_refused_by_type_without_writing_the_baseline() {
        let tempdir = tempfile::tempdir().unwrap();
        let data = tempdir.path().join("data");
        let node = std::sync::Arc::new(
            defra_node::EmbeddedNode::builder()
                .data_path(&data)
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .build()
                .await
                .unwrap(),
        );
        node.add_schema(&format!(
            "type {} {{ backend_id: String }}",
            gents_protocol::schemas::INFERENCE_BACKEND_NAME
        ))
        .await
        .unwrap();
        assert_eq!(incompatible_store_kind(&data).unwrap(), None);

        let error = crate::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .expect_err("an older lineage must not open");
        let store = incompatible_store(&error, &data).expect("typed refusal");
        assert_eq!(store.kind, IncompatibleStoreKind::UnknownLineage);
        assert!(
            node.get_collection(gents_protocol::schemas::NODE_NAME)
                .unwrap()
                .is_none(),
            "the refusal must precede baseline registration"
        );
        node.shutdown().await;
    }

    #[cfg(unix)]
    #[test]
    fn an_older_home_key_with_open_permissions_is_classified_insecure() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let key = temp.path().join("keys/local.key");
        crate::identity::load_or_create_file_identity(&key).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = crate::identity::KeyIdentity::load_existing(&key, None)
            .map(|_| ())
            .unwrap_err();
        let store = incompatible_store(&error, &temp.path().join("data")).expect("typed");
        assert_eq!(store.kind, IncompatibleStoreKind::InsecureKey);
        assert_eq!(store.data_path, key);
        assert!(store.kind.is_older());
        assert!(store.to_string().contains("unsafe file permissions"));
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o644,
            "the refused key is never repaired in place"
        );
    }

    #[test]
    fn classifies_lineage_refusals_by_type() {
        let path = Path::new("/tmp/gents-home/data");
        let refused = gents_migration::Error::UnknownLineage {
            collection: "AgentRequest".into(),
            versions: "bafy-old".into(),
        };
        let classified = classify_store_error(refused, path);
        let store = incompatible_store(&classified, path).expect("typed refusal");
        assert_eq!(store.kind, IncompatibleStoreKind::UnknownLineage);
        assert_eq!(store.data_path, path);
        assert!(matches!(
            &classified,
            ClassifiedStoreError::Refused { store, .. } if store.to_string().contains("older Gents version")
        ));

        let foreign = gents_migration::Error::ForeignVersion {
            collection: "AgentRequest".into(),
            version_id: "bafy-foreign".into(),
        };
        let foreign = incompatible_store(&foreign, path).expect("typed refusal");
        assert_eq!(foreign.kind, IncompatibleStoreKind::ForeignVersion);
        assert!(!foreign.kind.is_older());
        assert!(foreign.to_string().contains("different Gents version"));
        assert!(!foreign.to_string().contains("older"));

        let unrelated = gents_migration::Error::CollectionMissing {
            collection: "AgentRequest".into(),
        };
        assert!(incompatible_store(&unrelated, path).is_none());
        let unrelated = classify_store_error(unrelated, path);
        assert!(matches!(
            unrelated,
            ClassifiedStoreError::Unclassified(gents_migration::Error::CollectionMissing { .. })
        ));
    }
}
