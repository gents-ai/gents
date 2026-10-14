//! The asset-cache directory tree, `<home>/packs/.materialized/<namespace>/<name>/<digest>/`,
//! its writer, and its lock and prune/release rules. `gents pack prune` and
//! `gents pack remove` share [`release_cache_root`] for the marker/`runs/`
//! retention rule, and [`lock_exclusive`] for the per-pack cache lock, so the
//! two cannot diverge on when a cache version is safe to touch or delete.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::file_lock::FileLock;
use crate::pack_archive::PackArchive;

const CACHE_MARKER: &str = ".gents-pack-cache-v1";
const CACHE_LOCK: &str = ".cache.lock";

pub fn materialize(pack: &PackArchive, root: &Path) -> Result<()> {
    use std::io::Write;
    for path in std::iter::once("manifest.json")
        .chain(pack.manifest().metadata.assets.iter().map(String::as_str))
    {
        let destination = root.join(path);
        std::fs::create_dir_all(destination.parent().context("asset parent")?)?;
        let bytes = pack.asset(path)?;
        // Stage beside the destination and publish without replacing it. Readers
        // never see a partial body, and an interrupted staging write cannot
        // poison the digest-addressed destination or overwrite operator edits.
        let mut staged = tempfile::NamedTempFile::new_in(destination.parent().unwrap())?;
        staged.write_all(bytes)?;
        set_distribution_permissions(staged.as_file())?;
        staged.as_file().sync_all()?;
        match staged.persist_noclobber(&destination) {
            Ok(_) => {
                #[cfg(unix)]
                std::fs::File::open(destination.parent().unwrap())?.sync_all()?;
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                anyhow::ensure!(
                    std::fs::read(&destination)? == bytes,
                    "installed asset was modified: {}",
                    destination.display()
                );
                set_distribution_permissions(&std::fs::File::open(&destination)?)?;
            }
            Err(error) => return Err(error.error.into()),
        }
    }
    Ok(())
}

fn set_distribution_permissions(file: &std::fs::File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

pub fn write_cache_marker(root: &Path) -> Result<()> {
    let marker = root.join(CACHE_MARKER);
    if marker.exists() {
        return Ok(());
    }
    let staged = tempfile::NamedTempFile::new_in(root)?;
    set_distribution_permissions(staged.as_file())?;
    match staged.persist_noclobber(&marker) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.error.into()),
    }
}

pub fn cache_lock(parent: &Path) -> Result<File> {
    std::fs::create_dir_all(parent)?;
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(CACHE_LOCK))?)
}

pub fn prune_stale_asset_cache(parent: &Path, current: &Path) -> Result<Vec<String>> {
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
        // The caller holds the exclusive per-pack lock, so no Gents scenario
        // can acquire or use any sibling cache root while the probe and
        // removal occur. Shares `release_cache_root`'s marker/`runs/` rule so
        // the two cannot diverge on when a cache version is safe to delete.
        if let CacheRelease::Removed = release_cache_root(&root, || Ok(()))? {
            removed.push(name.to_owned());
        }
    }
    removed.sort();
    Ok(removed)
}

/// A pack's asset-cache root from its coordinate and digest, for a caller
/// (`gents pack remove`) that only has a [`crate::pack::HomePackInstall`]
/// record, not a resolved pack.
pub fn asset_cache_root_for(
    home: &Path,
    namespace: &str,
    name: &str,
    digest: &str,
) -> Result<PathBuf> {
    anyhow::ensure!(
        crate::pack::is_valid_pack_name(namespace) && crate::pack::is_valid_pack_name(name),
        "invalid pack coordinate {namespace}/{name}"
    );
    let hash = crate::pack_archive::digest_hex(digest)?;
    Ok(home
        .join(crate::home::PACKS_DIR_NAME)
        .join(".materialized")
        .join(namespace)
        .join(name)
        .join(hash))
}

/// A pack's asset-cache root, keyed by its coordinate and content digest.
pub fn asset_cache_root(home: &Path, pack: &PackArchive) -> Result<PathBuf> {
    let manifest = pack.manifest();
    asset_cache_root_for(
        home,
        &manifest.metadata.namespace,
        &manifest.name,
        pack.digest(),
    )
}

/// Publishes a pack into its asset cache: shared lease first, then the
/// assets, then the marker `release_cache_root` requires before it removes
/// anything. The caller holds the lease while it reads the cache.
pub fn materialize_cached(home: &Path, pack: &PackArchive) -> Result<(PathBuf, FileLock)> {
    let root = asset_cache_root(home, pack)?;
    let lease = FileLock::shared(cache_lock(root.parent().context("pack cache parent")?)?)?;
    materialize(pack, &root)?;
    write_cache_marker(&root)?;
    Ok((root, lease))
}

/// Takes the exclusive lock on a pack's cache parent directory, the same
/// lock `gents pack prune` takes, failing loudly rather than blocking when
/// another pack operation already holds it.
pub fn lock_exclusive(parent: &Path) -> Result<FileLock> {
    match FileLock::try_exclusive(cache_lock(parent)?) {
        Ok(lock) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => {
            anyhow::bail!("pack cache is in use; stop active pack operations and retry")
        }
        Err(std::fs::TryLockError::Error(error)) => Err(error).context("locking pack cache"),
    }
}

/// What [`release_cache_root`] did with one cache version directory.
pub enum CacheRelease {
    Removed,
    Retained(&'static str),
}

/// Removes `root` when it exists, carries gents' own cache marker, and holds
/// no `runs/` (operator-owned run history). [`prune_stale_asset_cache`] calls
/// this directly so the two cannot diverge on when a cache version is safe to
/// delete. A digest directory that is already gone (for example `gents pack
/// prune` removed it after a newer version became current) is reported as
/// already absent, never as "not created by gents".
pub fn release_cache_root(
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
    std::fs::remove_dir_all(root).with_context(|| format!("removing {}", root.display()))?;
    Ok(CacheRelease::Removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_archive::PackArchive;

    fn assets_archive() -> PackArchive {
        let (_guard, dir) =
            crate::test_support::fixture_pack_copy("assets_fixture", &serde_json::json!({}));
        let (bytes, _) = crate::pack_archive::pack_dir(&dir).unwrap();
        PackArchive::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn concurrent_materialization_publishes_complete_assets() {
        let source = assets_archive();
        let root = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    materialize(&source, root.path()).unwrap();
                });
            }
        });
        for path in std::iter::once("manifest.json")
            .chain(source.manifest().metadata.assets.iter().map(String::as_str))
        {
            assert_eq!(
                std::fs::read(root.path().join(path)).unwrap(),
                source.asset(path).unwrap()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn materialized_assets_are_readable_like_distribution_files() {
        use std::os::unix::fs::PermissionsExt;

        let source = assets_archive();
        let root = tempfile::tempdir().unwrap();
        materialize(&source, root.path()).unwrap();
        for path in std::iter::once("manifest.json")
            .chain(source.manifest().metadata.assets.iter().map(String::as_str))
        {
            assert_eq!(
                std::fs::metadata(root.path().join(path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o644,
                "{path}"
            );
        }

        let cached = root.path().join("README.md");
        std::fs::set_permissions(&cached, std::fs::Permissions::from_mode(0o600)).unwrap();
        materialize(&source, root.path()).unwrap();
        assert_eq!(
            std::fs::metadata(cached).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn abandoned_staging_file_does_not_poison_install_or_allow_overwrite() {
        use std::io::Write;
        let pack = assets_archive();
        let root = tempfile::tempdir().unwrap();
        // Model process death before publication: a partial temporary file
        // remains, but no destination has been exposed.
        let mut staged = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        staged.write_all(b"partial").unwrap();
        let (_file, abandoned) = staged.keep().unwrap();
        materialize(&pack, root.path()).unwrap();
        materialize(&pack, root.path()).unwrap();
        assert_eq!(std::fs::read(abandoned).unwrap(), b"partial");
        std::fs::write(root.path().join("README.md"), "operator edit").unwrap();
        assert!(materialize(&pack, root.path())
            .unwrap_err()
            .to_string()
            .contains("installed asset was modified"));
        assert_eq!(
            std::fs::read_to_string(root.path().join("README.md")).unwrap(),
            "operator edit"
        );
    }

    #[test]
    fn cached_materialization_leaves_a_releasable_marked_cache_under_a_shared_lease() {
        let pack = assets_archive();
        let home = tempfile::tempdir().unwrap();
        let (root, lease) = materialize_cached(home.path(), &pack).unwrap();
        assert_eq!(root, asset_cache_root(home.path(), &pack).unwrap());
        assert!(root.join("manifest.json").is_file());
        assert!(matches!(
            lock_exclusive(root.parent().unwrap()),
            Err(error) if error.to_string().contains("in use")
        ));
        drop(lease);
        assert!(matches!(
            release_cache_root(&root, || Ok(())).unwrap(),
            CacheRelease::Removed
        ));
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
}
