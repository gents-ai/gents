use gents_protocol::operations::install_repair::{IncompatibleStore, IncompatibleStoreKind};
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
    #[error("opening {} to inspect its schema", path.display())]
    InspectionOpen {
        path: PathBuf,
        #[source]
        source: Box<defra_node::Error>,
    },
    #[error("inspecting the schema of {}", path.display())]
    InspectionSchema {
        path: PathBuf,
        #[source]
        source: gents_migration::Error,
    },
}

impl StorageBackendError {
    pub fn incompatible(&self) -> Option<&IncompatibleStore> {
        match self {
            Self::Incompatible(store) => Some(store),
            Self::Io(_)
            | Self::NoExistingDatabase { .. }
            | Self::InspectionOpen { .. }
            | Self::InspectionSchema { .. } => None,
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
        .block_cache_size(block_cache_size(
            std::env::var("GENTS_BLOCK_CACHE_MB").ok().as_deref(),
        ));
    options
}

/// Default Regolith block cache per store. Live stores hold single blocks of
/// tens of megabytes, which a 64 MiB cache cannot keep resident: a busy node
/// spent most of its worker time re-reading, decrypting and decompressing
/// evicted blocks.
pub const DEFAULT_BLOCK_CACHE_BYTES: usize = 512 * 1024 * 1024;

/// The block cache size `GENTS_BLOCK_CACHE_MB` asks for (whole MiB, at least
/// one), else [`DEFAULT_BLOCK_CACHE_BYTES`].
fn block_cache_size(mib: Option<&str>) -> usize {
    mib.and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|mib| *mib > 0)
        .and_then(|mib| mib.checked_mul(1024 * 1024))
        .unwrap_or(DEFAULT_BLOCK_CACHE_BYTES)
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
pub fn require_existing_store(data_path: &Path) -> Result<()> {
    reject_legacy_store(data_path)?;
    if !data_path.join("MANIFEST").is_file() {
        return Err(StorageBackendError::NoExistingDatabase {
            path: data_path.to_path_buf(),
        });
    }
    Ok(())
}

/// Refuses an existing store another Gents version wrote, before anything
/// writes to it. The store is opened bare (no projector, P2P or HTTP, so
/// nothing is served) and the migration owner's read-only validators decide.
/// Any other failure is returned unclassified.
pub async fn refuse_other_version_schema(
    data_path: &Path,
    key: Option<&crate::local::store_key::StoreKey>,
) -> Result<()> {
    require_existing_store(data_path)?;
    let opened = |source| StorageBackendError::InspectionOpen {
        path: data_path.to_path_buf(),
        source: Box::new(source),
    };
    let builder = defra_node::Node::builder()
        .data_path(data_path)
        .with_storage_backend(defra_node::StorageBackend::Regolith)
        .with_regolith_options(regolith_options());
    let builder = match key {
        Some(key) => key.encrypt(builder),
        None => builder,
    };
    let node = builder.build().await.map_err(opened)?;
    let validated = gents_migration::validate_existing_schema(&node).await;
    node.shutdown().await.map_err(opened)?;
    validated.map_err(|source| match incompatible_store(&source, data_path) {
        Some(store) => StorageBackendError::Incompatible(store),
        None => StorageBackendError::InspectionSchema {
            path: data_path.to_path_buf(),
            source,
        },
    })
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
    match find_source::<super::backend_protection::BackendProtectionError>(error) {
        Some(super::backend_protection::BackendProtectionError::MissingProvenance) => {
            return Some(IncompatibleStore {
                kind: IncompatibleStoreKind::UnknownLineage,
                data_path: data_path.to_path_buf(),
            });
        }
        Some(super::backend_protection::BackendProtectionError::InvalidBinding) => {
            return Some(IncompatibleStore {
                kind: IncompatibleStoreKind::OtherVersionSchema,
                data_path: data_path.to_path_buf(),
            });
        }
        _ => {}
    }
    find_source::<gents_migration::Error>(error)
        .and_then(|cause| {
            if cause.is_unknown_lineage() {
                Some(IncompatibleStoreKind::UnknownLineage)
            } else if cause.is_foreign_version() {
                Some(IncompatibleStoreKind::ForeignVersion)
            } else if cause.is_other_version_schema() {
                Some(IncompatibleStoreKind::OtherVersionSchema)
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
                    .downcast_ref::<crate::local::store_key::StoreKeyError>()?
                    .incompatible()
            })
            .or_else(|| {
                cause
                    .downcast_ref::<crate::local::store_key::upgrade::UpgradeError>()?
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
                    .downcast_ref::<crate::local::store_key::StoreKeyError>()?
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
        use crate::local::store_key::upgrade::UpgradeError;
        use crate::local::store_key::StoreKeyError;
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
        use crate::local::store_key::{store_key_unavailable, StoreKeyError, StoreKeyUnavailable};
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
    fn block_cache_size_defaults_and_honors_override() {
        assert_eq!(block_cache_size(None), DEFAULT_BLOCK_CACHE_BYTES);
        assert_eq!(block_cache_size(Some("2048")), 2048 * 1024 * 1024);
        assert_eq!(block_cache_size(Some(" 128 ")), 128 * 1024 * 1024);
        for invalid in ["", "0", "-1", "lots", "1.5"] {
            assert_eq!(block_cache_size(Some(invalid)), DEFAULT_BLOCK_CACHE_BYTES);
        }
    }

    #[test]
    fn in_process_budget_preserves_server_acceptance_and_commit_settings() {
        let server = storage::RegolithStoreOptions::default();
        let local = regolith_options();
        assert_eq!(local.engine.get_write_buffer_size(), 32 * 1024 * 1024);
        assert_eq!(local.engine.get_max_write_buffer_number(), 2);
        assert_eq!(
            local.engine.get_block_cache_size(),
            block_cache_size(std::env::var("GENTS_BLOCK_CACHE_MB").ok().as_deref())
        );
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

        assert!(error
            .to_string()
            .contains("older version of Gents (a RocksDB store)"));
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

        assert!(error
            .to_string()
            .contains("older version of Gents (a Lark store)"));
    }

    #[test]
    fn rejects_unknown_manifest_format() {
        let tempdir = tempfile::tempdir().unwrap();
        std::fs::write(tempdir.path().join("MANIFEST"), "unknown").unwrap();

        let error = reject_legacy_store(tempdir.path()).unwrap_err();

        assert!(error
            .to_string()
            .contains("is not a store this version can read"));
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
            crate::test_support::node_builder()
                .data_path(&data)
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .build()
                .await
                .unwrap(),
        );
        // The older collection keeps its identity policy binding, which the
        // migration owner checks before lineage, so only its schema lineage is
        // unknown.
        crate::test_support::acp_policies::install_identity_acp_policies(&node)
            .await
            .unwrap();
        node.add_collection(&format!(
            r#"type {} @policy(id: "{}", resource: "secret_configuration") {{ backend_id: String }}"#,
            gents_protocol::schemas::INFERENCE_BACKEND_NAME,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
        ))
        .await
        .unwrap();
        assert_eq!(incompatible_store_kind(&data).unwrap(), None);

        let error = crate::schema::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .expect_err("an older lineage must not open");
        let store = incompatible_store(&error, &data).unwrap_or_else(|| {
            panic!(
                "typed refusal: {}",
                gents_protocol::error_chain::ErrorChain(&error)
            )
        });
        assert_eq!(store.kind, IncompatibleStoreKind::UnknownLineage);
        assert!(
            node.get_collection(gents_protocol::schemas::NODE_NAME)
                .unwrap()
                .is_none(),
            "the refusal must precede baseline registration"
        );
        node.shutdown().await.expect("node shutdown");
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
        assert!(!store.kind.made_by_older_version());
        assert!(store.to_string().contains("chmod 600"));
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o644,
            "the refused key is never repaired in place"
        );
    }

    /// A store from before backend protection (v1.0.0-rc1 and earlier) has
    /// no provenance marker; it is refused as an older home, with the
    /// start-fresh message, not as an internal fault.
    #[test]
    fn a_store_without_backend_provenance_is_an_older_home() {
        let path = Path::new("/tmp/gents-home/data");
        let error = anyhow::Error::new(
            crate::local::backend_protection::BackendProtectionError::MissingProvenance,
        )
        .context("opening the home");
        let store = incompatible_store(error.as_ref(), path).expect("typed refusal");
        assert_eq!(store.kind, IncompatibleStoreKind::UnknownLineage);
        assert!(store.kind.made_by_older_version());
        assert!(
            store.to_string().contains("older version of Gents")
                && store.to_string().contains("Nothing in it was changed"),
            "{store}"
        );
        let tampered = anyhow::Error::new(
            crate::local::backend_protection::BackendProtectionError::InvalidProvenance,
        );
        assert!(incompatible_store(tampered.as_ref(), path).is_none());
    }

    #[test]
    fn classifies_another_versions_schema_refusals_by_type() {
        let path = Path::new("/tmp/gents-home/data");
        for refused in [
            gents_migration::Error::RuntimeLocalityMismatch {
                collection: "CompactionConfig".into(),
            },
            gents_migration::Error::IncompatibleIdentityPolicy {
                collection: "NetworkEnrollmentRequest".into(),
            },
        ] {
            let classified = classify_store_error(refused, path);
            let store = incompatible_store(&classified, path).expect("typed refusal");
            assert_eq!(store.kind, IncompatibleStoreKind::OtherVersionSchema);
            assert!(store.kind.made_by_older_version());
            assert!(store.to_string().contains("older version of Gents"));
        }
    }

    async fn plaintext_node(data: &Path) -> defra_node::Node {
        crate::test_support::node_builder()
            .data_path(data)
            .with_storage_backend(defra_node::StorageBackend::Regolith)
            .with_regolith_options(regolith_options())
            .build()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_bare_inspection_refuses_another_versions_store_and_takes_this_builds() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let node = plaintext_node(&data).await;
        // CompactionConfig as v0.19.0 and v1.0.0-rc1 declared it.
        node.add_collection(
            r#"type CompactionConfig @branchable { compaction_id: String agent_did: String @index }"#,
        )
        .await
        .unwrap();
        node.shutdown().await.unwrap();
        drop(node);

        let error = refuse_other_version_schema(&data, None)
            .await
            .expect_err("another version's store must be refused");
        assert_eq!(
            error.incompatible().map(|store| store.kind),
            Some(IncompatibleStoreKind::OtherVersionSchema)
        );
        let reopened = plaintext_node(&data).await;
        assert!(reopened
            .get_collection(gents_protocol::schemas::NODE_NAME)
            .unwrap()
            .is_none());
        reopened.shutdown().await.unwrap();

        let current = temp.path().join("current");
        let node = std::sync::Arc::new(plaintext_node(&current).await);
        crate::schema::migration::ensure_all_runtime_migrations(node.clone())
            .await
            .unwrap();
        node.shutdown().await.unwrap();
        drop(node);
        refuse_other_version_schema(&current, None).await.unwrap();

        let missing = refuse_other_version_schema(&temp.path().join("absent"), None)
            .await
            .unwrap_err();
        assert!(matches!(
            missing,
            StorageBackendError::NoExistingDatabase { .. }
        ));
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
            ClassifiedStoreError::Refused { store, .. } if store.to_string().contains("older version of Gents")
        ));

        let foreign = gents_migration::Error::ForeignVersion {
            collection: "AgentRequest".into(),
            version_id: "bafy-foreign".into(),
        };
        let foreign = incompatible_store(&foreign, path).expect("typed refusal");
        assert_eq!(foreign.kind, IncompatibleStoreKind::ForeignVersion);
        assert!(!foreign.kind.is_older());
        assert!(foreign.to_string().contains("newer version of Gents"));
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
