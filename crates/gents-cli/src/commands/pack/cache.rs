use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

use gents::file_lock::FileLock;
use gents::pack::materialize::{
    cache_root, cache_root_disposition, lock_exclusive, prune_stale_asset_cache, CacheRelease,
};
use gents::pack::{HomePackInstall, PackKind};

use crate::cli::PackPruneArgs;

#[derive(Debug, thiserror::Error)]
pub(super) enum CacheReleaseError {
    #[error("removing {}", path.display())]
    Remove {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub(super) struct PreparedCacheRelease {
    root: std::path::PathBuf,
    disposition: CacheRelease,
    _lock: Option<FileLock>,
}

impl PreparedCacheRelease {
    pub(super) fn finish(self) -> Result<CacheRelease, CacheReleaseError> {
        match self.disposition {
            CacheRelease::Removed => {
                std::fs::remove_dir_all(&self.root).map_err(|source| {
                    CacheReleaseError::Remove {
                        path: self.root.clone(),
                        source,
                    }
                })?;
                Ok(CacheRelease::Removed)
            }
            CacheRelease::Retained(reason) => Ok(CacheRelease::Retained(reason)),
        }
    }
}

pub(super) fn prepare_recorded_cache(
    home: &Path,
    coordinate: &str,
    record: &HomePackInstall,
) -> Result<PreparedCacheRelease> {
    anyhow::ensure!(
        record.coordinate == coordinate,
        "pack receipt coordinate mismatch"
    );
    let (namespace, name) = super::split_namespace(coordinate);
    let expected = cache_root(Path::new(""), namespace, name, &record.digest)?;
    let recorded = Path::new(&record.assets);
    let unscoped = Path::new(gents::home::PACKS_DIR_NAME)
        .join(name)
        .join(gents::pack_archive::digest_hex(&record.digest)?);
    anyhow::ensure!(
        recorded == expected || recorded == unscoped,
        "{coordinate}'s recorded asset path {} does not match {}",
        recorded.display(),
        expected.display()
    );
    let root = home.join(recorded);
    let Some(parent) = root.parent().filter(|parent| parent.is_dir()) else {
        return Ok(PreparedCacheRelease {
            root,
            disposition: CacheRelease::Retained("already absent"),
            _lock: None,
        });
    };
    let lock = lock_exclusive(parent)?;
    let disposition = cache_root_disposition(&root, || {
        if recorded == unscoped {
            let archive = gents::pack_store::PackStore::new(home).open(&record.digest)?;
            anyhow::ensure!(
                archive.manifest().metadata.namespace == namespace
                    && archive.manifest().name == name,
                "{coordinate}'s recorded asset digest belongs to a different pack"
            );
            for path in gents::pack::declared_paths(archive.manifest()) {
                anyhow::ensure!(
                    std::fs::read(root.join(&path))? == archive.asset(&path)?,
                    "{coordinate}'s recorded asset {path} does not match {}",
                    record.digest
                );
            }
        }
        Ok(())
    })?;
    Ok(PreparedCacheRelease {
        root,
        disposition,
        _lock: Some(lock),
    })
}

pub(super) async fn prune(args: PackPruneArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let pack = super::resolve_pack_source(&args.package, None, &home).await?;
    anyhow::ensure!(
        pack.manifest().metadata.kind == PackKind::Assets
            || pack
                .manifest()
                .metadata
                .assets
                .iter()
                .any(|asset| asset == "experiment.json"),
        "pack {} has no materialized asset cache",
        pack.manifest().name
    );
    let current = super::asset_cache_root(&home, &pack)?;
    let parent = current.parent().context("pack cache parent")?;
    if !parent.is_dir() {
        return Ok(crate::print_json(&json!({
            "pack": pack.manifest().name,
            "current_digest": pack.digest(),
            "removed_digests": [],
        }))?);
    }
    let _lock = lock_exclusive(parent)?;
    let removed = prune_stale_asset_cache(parent, &current)?;
    Ok(crate::print_json(&json!({
        "pack": pack.manifest().name,
        "current_digest": pack.digest(),
        "removed_digests": removed,
    }))?)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{assets_pack_dir, local_pack_source};
    use super::*;
    use gents::pack::materialize::{cache_lock, write_cache_marker};

    #[tokio::test]
    async fn pruning_keeps_other_namespaces_and_recorded_unscoped_assets() {
        let home = tempfile::tempdir().unwrap();
        let mut sources = Vec::new();
        for (namespace, version) in [("acme", "1.0.0"), ("acme", "2.0.0"), ("zeta", "1.0.0")] {
            let dir = assets_pack_dir(namespace, "tools", version);
            let pack = local_pack_source(dir.path(), home.path());
            let (root, _lease) = super::super::materialize_cached_pack(home.path(), &pack).unwrap();
            sources.push((pack, root));
        }
        let unscoped = home.path().join("packs/tools").join("a".repeat(64));
        std::fs::create_dir_all(&unscoped).unwrap();
        write_cache_marker(&unscoped).unwrap();
        let _other_lease =
            FileLock::shared(cache_lock(sources[2].1.parent().unwrap()).unwrap()).unwrap();

        let report = crate::request_helpers::capture_report(prune(PackPruneArgs {
            package: "acme/tools".to_owned(),
            home: Some(home.path().to_owned()),
        }))
        .await
        .unwrap();

        assert_eq!(
            report["removed_digests"],
            json!([gents::pack_archive::digest_hex(sources[0].0.digest()).unwrap()])
        );
        assert!(!sources[0].1.exists());
        assert!(sources[1].1.exists());
        assert!(sources[2].1.exists());
        assert!(unscoped.exists());
    }

    #[test]
    fn infrastructure_names_materialize_outside_store_and_unpack_directories() {
        let home = tempfile::tempdir().unwrap();
        assert!(!gents::pack::is_valid_pack_name(".materialized"));
        for name in ["store", "unpacked", "materialized"] {
            let dir = assets_pack_dir("acme", name, "1.0.0");
            let pack = local_pack_source(dir.path(), home.path());
            let (root, _lease) = super::super::materialize_cached_pack(home.path(), &pack).unwrap();
            assert_eq!(
                root.parent().unwrap(),
                home.path().join("packs/.materialized/acme").join(name)
            );
            assert!(gents::pack_store::PackStore::new(home.path())
                .open(pack.digest())
                .is_ok());
        }
    }

    #[tokio::test]
    async fn pack_prune_rejects_without_creating_a_cache_tree() {
        let home = tempfile::tempdir().unwrap();
        let _ = super::super::test_support::fixture_pack_source("slot_fixture", home.path());
        let error = prune(PackPruneArgs {
            package: "fixture/slot_fixture".to_owned(),
            home: Some(home.path().to_path_buf()),
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("no materialized asset cache"));
        assert!(!home
            .path()
            .join(gents::home::PACKS_DIR_NAME)
            .join(".materialized")
            .exists());
    }
}
