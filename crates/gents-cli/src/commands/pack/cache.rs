
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use gents::file_lock::FileLock;
use gents::pack::{HomePackInstall, PackKind};

use crate::cli::PackPruneArgs;

const CACHE_MARKER: &str = ".gents-pack-cache-v1";
const CACHE_LOCK: &str = ".cache.lock";

pub(super) fn write_cache_marker(root: &Path) -> Result<()> {
    let marker = root.join(CACHE_MARKER);
    if marker.exists() {
        return Ok(());
    }
    let staged = tempfile::NamedTempFile::new_in(root)?;
    super::set_distribution_permissions(staged.as_file())?;
    match staged.persist_noclobber(&marker) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.error.into()),
    }
}

pub(super) fn cache_lock(parent: &Path) -> Result<File> {
    std::fs::create_dir_all(parent)?;
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(CACHE_LOCK))?)
}

fn prune_stale_asset_cache(parent: &Path, current: &Path) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let root = entry.path();
        if root == current || !root.is_dir() {
            continue;
        }
        let Some(name) = root.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.len() != 64 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        // The caller holds the exclusive per-pack lock, so no scenario can
        // acquire or use any sibling cache root while the probe and removal
        // occur.
        if let CacheRelease::Removed = release_cache_root(&root, || Ok(()))? {
            removed.push(name.to_owned());
        }
    }
    removed.sort();
    Ok(removed)
}

pub(super) fn asset_cache_root_for(
    home: &Path,
    namespace: &str,
    name: &str,
    digest: &str,
) -> Result<std::path::PathBuf> {
    gents::pack::materialize::cache_root(home, namespace, name, digest)
}

pub(super) fn release_recorded_cache(
    home: &Path,
    coordinate: &str,
    record: &HomePackInstall,
) -> Result<CacheRelease> {
    prepare_recorded_cache(home, coordinate, record)?.finish()
}

pub(super) struct PreparedCacheRelease {
    root: std::path::PathBuf,
    disposition: CacheRelease,
    _lock: Option<FileLock>,
}

impl PreparedCacheRelease {
    pub(super) fn finish(self) -> Result<CacheRelease> {
        match self.disposition {
            CacheRelease::Removed => {
                std::fs::remove_dir_all(&self.root)
                    .with_context(|| format!("removing {}", self.root.display()))?;
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
    let expected = asset_cache_root_for(Path::new(""), namespace, name, &record.digest)?;
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

pub(super) fn lock_exclusive(parent: &Path) -> Result<FileLock> {
    match FileLock::try_exclusive(cache_lock(parent)?) {
        Ok(lock) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => {
            anyhow::bail!("pack cache is in use; stop active pack operations and retry")
        }
        Err(std::fs::TryLockError::Error(error)) => Err(error).context("locking pack cache"),
    }
}

pub(super) enum CacheRelease {
    Removed,
    Retained(&'static str),
}

fn release_cache_root(
    root: &Path,
    check_owner: impl FnOnce() -> Result<()>,
) -> Result<CacheRelease> {
    match cache_root_disposition(root, check_owner)? {
        CacheRelease::Removed => {
            std::fs::remove_dir_all(root)
                .with_context(|| format!("removing {}", root.display()))?;
            Ok(CacheRelease::Removed)
        }
        retained => Ok(retained),
    }
}

fn cache_root_disposition(
    root: &Path,
    check_owner: impl FnOnce() -> Result<()>,
) -> Result<CacheRelease> {
    if !root.is_dir() {
        return Ok(CacheRelease::Retained("already absent"));
    }
    if !root.join(CACHE_MARKER).is_file() {
        return Ok(CacheRelease::Retained("not created by gents"));
    }
    if root.join("runs").exists() {
        return Ok(CacheRelease::Retained("holds run history"));
    }
    check_owner()?;
    Ok(CacheRelease::Removed)
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
        return crate::print_json(&json!({
            "pack": pack.manifest().name,
            "current_digest": pack.digest(),
            "removed_digests": [],
        }));
    }
    let _lock = lock_exclusive(parent)?;
    let removed = prune_stale_asset_cache(parent, &current)?;
    crate::print_json(&json!({
        "pack": pack.manifest().name,
        "current_digest": pack.digest(),
        "removed_digests": removed,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{assets_pack_dir, local_pack_source};
    use super::*;

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

    #[test]
    fn cache_pruning_removes_only_owned_versions_without_runs() {
        let parent = tempfile::tempdir().unwrap();
        let current = parent.path().join("a".repeat(64));
        let stale = parent.path().join("b".repeat(64));
        let active = parent.path().join("c".repeat(64));
        let unowned = parent.path().join("d".repeat(64));
        for root in [&current, &stale, &active, &unowned] {
            std::fs::create_dir_all(root).unwrap();
        }
        write_cache_marker(&current).unwrap();
        write_cache_marker(&stale).unwrap();
        write_cache_marker(&active).unwrap();
        std::fs::create_dir(active.join("runs")).unwrap();

        prune_stale_asset_cache(parent.path(), &current).unwrap();

        assert!(current.exists());
        assert!(!stale.exists());
        assert!(active.exists(), "run artifacts retain their source version");
        assert!(
            unowned.exists(),
            "directories without our marker are not ours"
        );
    }

    #[tokio::test]
    async fn graph_pack_prune_rejects_without_creating_a_cache_tree() {
        let home = tempfile::tempdir().unwrap();
        let _ = super::super::test_support::fixture_pack_source("review_graph", home.path());
        let error = prune(PackPruneArgs {
            package: "fixture/review_graph".to_owned(),
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
