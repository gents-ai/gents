//! The asset-cache directory tree, `<home>/packs/<name>/<digest>/`, and its
//! lock and prune/release rules. `gents pack prune` and `gents pack remove`
//! share [`release_cache_root`] for the marker/`runs/` retention rule, and
//! [`lock_exclusive`] for the per-pack cache lock, so the two cannot diverge
//! on when a cache version is safe to touch or delete.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use gents::file_lock::FileLock;
use gents::pack::PackKind;

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
        // The caller holds the exclusive per-pack lock, so no Gents scenario
        // can acquire or use any sibling cache root while the probe and
        // removal occur. Shares `release_cache_root`'s marker/`runs/` rule so
        // the two cannot diverge on when a cache version is safe to delete.
        if let CacheRelease::Removed = release_cache_root(&root)? {
            removed.push(name.to_owned());
        }
    }
    removed.sort();
    Ok(removed)
}

/// [`super::asset_cache_root`] from a pack's name and digest directly, for a
/// caller (`gents pack remove`) that only has a [`gents::pack::HomePackInstall`]
/// record, not a resolved [`PackSource`].
pub(super) fn asset_cache_root_for(
    home: &Path,
    name: &str,
    digest: &str,
) -> Result<std::path::PathBuf> {
    // Keep the shared sha256: digest representation out of filesystem names.
    let hash = digest
        .strip_prefix("sha256:")
        .context("invalid pack digest")?;
    Ok(home.join(gents::home::PACKS_DIR_NAME).join(name).join(hash))
}

/// Takes the exclusive lock on a pack's cache parent directory, the same
/// lock [`prune`] takes, failing loudly rather than blocking when another
/// pack operation already holds it.
pub(super) fn lock_exclusive(parent: &Path) -> Result<FileLock> {
    match FileLock::try_exclusive(cache_lock(parent)?) {
        Ok(lock) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => {
            anyhow::bail!("pack cache is in use; stop active pack operations and retry")
        }
        Err(std::fs::TryLockError::Error(error)) => Err(error).context("locking pack cache"),
    }
}

/// What [`release_cache_root`] did with one cache version directory.
pub(super) enum CacheRelease {
    Removed,
    Retained(&'static str),
}

/// Removes `root` when it exists, carries gents' own cache marker, and holds
/// no `runs/` (operator-owned run history). [`prune_stale_asset_cache`] calls
/// this directly so the two cannot diverge on when a cache version is safe to
/// delete. A digest directory that is already gone (for example `gents pack
/// prune` removed it after a newer version became current) is reported as
/// already absent, never as "not created by gents".
pub(super) fn release_cache_root(root: &Path) -> Result<CacheRelease> {
    if !root.is_dir() {
        return Ok(CacheRelease::Retained("already absent"));
    }
    if !root.join(CACHE_MARKER).is_file() {
        return Ok(CacheRelease::Retained("not created by gents"));
    }
    if root.join("runs").exists() {
        return Ok(CacheRelease::Retained("holds run history"));
    }
    std::fs::remove_dir_all(root).with_context(|| format!("removing {}", root.display()))?;
    Ok(CacheRelease::Removed)
}

/// Resolves through the same path `install` does: an explicit local pack,
/// else an installed record's digest if the store still holds it, else the
/// store's name index, else the registry. No bundled fallback.
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
    use super::*;

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
        // Store the fixture first, the way an install would have.
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
            .join("review_graph")
            .exists());
    }
}
