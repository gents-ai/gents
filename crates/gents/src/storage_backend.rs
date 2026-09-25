use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Result;

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
}

impl IncompatibleStoreKind {
    /// Whether the store is known to come from an earlier release. A foreign
    /// version may be newer; its data is not offered for deletion by default.
    pub fn is_older(self) -> bool {
        !matches!(self, Self::ForeignVersion)
    }
}

/// A data directory this build refuses to open, and why.
///
/// Carried in the error chain so hosts can recognize the refusal by type
/// (process exit status, desktop bridge error code) instead of by message.
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
    }
}

/// Inspect the exact on-disk markers owned by the storage backend without
/// opening or mutating the store.
///
/// A Regolith store from an older build has no distinguishing marker; it is
/// recognized when opened, by [`incompatible_store`] over the migration
/// engine's lineage refusal.
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
///
/// Regolith, Lark, and RocksDB use incompatible on-disk formats. Regolith
/// would otherwise try to open legacy files as its own store, so callers must
/// fail before opening the directory.
pub fn reject_legacy_store(data_path: &Path) -> Result<()> {
    match incompatible_store_kind(data_path)? {
        Some(kind) => Err(IncompatibleStore {
            kind,
            data_path: data_path.to_path_buf(),
        }
        .into()),
        None => Ok(()),
    }
}

/// The store refusal carried by `error`, if any: a legacy-backend rejection,
/// an identity key refused for unsafe permissions (reported at the key's
/// path), or the migration engine refusing a lineage it does not know. The engine
/// refuses before it registers or patches anything, so the store is left
/// exactly as the older build wrote it.
pub fn incompatible_store(error: &anyhow::Error, data_path: &Path) -> Option<IncompatibleStore> {
    if let Some(store) = error.downcast_ref::<IncompatibleStore>() {
        return Some(store.clone());
    }
    let insecure_key = error
        .downcast_ref::<crate::identity::InsecureKeyPermissions>()
        .cloned()
        .or_else(|| {
            error.chain().find_map(|cause| {
                cause
                    .downcast_ref::<crate::identity::InsecureKeyPermissions>()
                    .cloned()
            })
        });
    if let Some(key) = insecure_key {
        return Some(IncompatibleStore {
            kind: IncompatibleStoreKind::InsecureKey,
            data_path: key.path,
        });
    }
    let kind = |cause: &gents_migration::Error| {
        if cause.is_unknown_lineage() {
            Some(IncompatibleStoreKind::UnknownLineage)
        } else if cause.is_foreign_version() {
            Some(IncompatibleStoreKind::ForeignVersion)
        } else {
            None
        }
    };
    error
        .downcast_ref::<gents_migration::Error>()
        .and_then(kind)
        .or_else(|| {
            error.chain().find_map(|cause| {
                cause
                    .downcast_ref::<gents_migration::Error>()
                    .and_then(kind)
            })
        })
        .map(|kind| IncompatibleStore {
            kind,
            data_path: data_path.to_path_buf(),
        })
}

/// Attach the typed store refusal to an error from opening `data_path`, so
/// the refusal is recognizable by type wherever the error surfaces. Other
/// errors pass through unchanged.
pub fn classify_store_error(error: anyhow::Error, data_path: &Path) -> anyhow::Error {
    if error.downcast_ref::<IncompatibleStore>().is_some() {
        return error;
    }
    match incompatible_store(&error, data_path) {
        Some(store) => error.context(store),
        None => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;

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

    /// A Regolith store written by an older build: a managed collection
    /// exists with a root this release's baseline does not pin.
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
            node.get_collection(gents_protocol::schemas::AGENT_PRINCIPAL_NAME)
                .unwrap()
                .is_none(),
            "the refusal must precede baseline registration"
        );
        node.shutdown().await;
    }

    /// A key an older build wrote with ambient (0644) permissions is refused
    /// by the identity owner and classified as a home needing a fresh start.
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
            .context("loading agent identity key")
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
    fn classifies_lineage_refusals_by_type_through_context() {
        let path = Path::new("/tmp/gents-home/data");
        let refused = anyhow::Error::new(gents_migration::Error::UnknownLineage {
            collection: "AgentRequest".into(),
            versions: "bafy-old".into(),
        })
        .context("ensure_migrations");
        let classified = classify_store_error(refused, path);
        let store = incompatible_store(&classified, path).expect("typed refusal");
        assert_eq!(store.kind, IncompatibleStoreKind::UnknownLineage);
        assert_eq!(store.data_path, path);
        assert!(classified
            .downcast_ref::<IncompatibleStore>()
            .is_some_and(|store| store.to_string().contains("older Gents version")));

        let foreign = anyhow::Error::new(gents_migration::Error::ForeignVersion {
            collection: "AgentRequest".into(),
            version_id: "bafy-foreign".into(),
        });
        let foreign = incompatible_store(&foreign, path).expect("typed refusal");
        assert_eq!(foreign.kind, IncompatibleStoreKind::ForeignVersion);
        assert!(!foreign.kind.is_older());
        assert!(foreign.to_string().contains("different Gents version"));
        assert!(!foreign.to_string().contains("older"));

        let unrelated = anyhow::Error::new(gents_migration::Error::CollectionMissing {
            collection: "AgentRequest".into(),
        });
        assert!(incompatible_store(&unrelated, path).is_none());
        let unrelated = classify_store_error(unrelated, path);
        assert!(unrelated.downcast_ref::<gents_migration::Error>().is_some());
    }
}
