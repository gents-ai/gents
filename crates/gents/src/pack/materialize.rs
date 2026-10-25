use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum MaterializeError {
    #[error(transparent)]
    Archive(#[from] crate::pack_archive::PackArchiveError),
    #[error(transparent)]
    Digest(#[from] crate::pack_archive::format::PackFormatError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("asset parent")]
    AssetParent,
    #[error("asset path escapes the pack cache root {}", root.display())]
    EscapesRoot {
        root: PathBuf,
        #[source]
        source: std::path::StripPrefixError,
    },
    #[error("installed asset was modified: {}", path.display())]
    AssetModified { path: PathBuf },
    #[error("pack cache component {} is a symlink", path.display())]
    ComponentSymlink { path: PathBuf },
    #[error("creating pack cache dir {}", path.display())]
    CreateCacheDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("inspecting pack cache root {}", path.display())]
    InspectRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("pack cache root {} is a symlink; remove it or choose a different cache root", path.display())]
    RootSymlink { path: PathBuf },
    #[error("pack cache root {} exists and is not a directory; remove it or choose a different cache root", path.display())]
    RootNotDirectory { path: PathBuf },
    #[error("creating pack cache root {}", path.display())]
    CreateRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("opening {}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("installed asset {} is not a regular file", path.display())]
    NotRegularFile { path: PathBuf },
    #[error("invalid pack coordinate {namespace}/{name}")]
    InvalidCoordinate { namespace: String, name: String },
    #[error("pack cache parent")]
    CacheParent,
    #[error("pack cache is in use; stop active pack operations and retry")]
    CacheInUse,
    #[error("locking pack cache")]
    LockCache(#[source] std::io::Error),
    #[error("removing {}", path.display())]
    RemoveCacheRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

type Result<T, E = MaterializeError> = std::result::Result<T, E>;

/// Writes each declared file of `pack` into `root`.
///
/// Premise: the ancestors of every path written here — everything from the
/// filesystem root through `root`'s parent — are stable and trusted for the
/// duration of the call. Only final components are checked no-follow:
/// components created below `root` are refused when they already exist as
/// symlinks, and an existing destination is re-opened no-follow so a
/// swapped-in symlink fails instead of being followed. There is no dirfd
/// confinement and no TOCTOU closure: a component checked here can still be
/// swapped before a later use. The staging temp file and the published
/// destination both live in the same parent directory, so the publish does
/// not traverse a path outside that directory.
///
/// The caller holds `FileLock::shared(cache_lock(root.parent()))` so prune and
/// remove cannot delete `root` mid-write.
pub fn materialize(pack: &crate::pack_archive::PackArchive, root: &Path) -> Result<()> {
    use std::io::Write;
    for path in std::iter::once("manifest.json")
        .chain(pack.manifest().metadata.assets.iter().map(String::as_str))
    {
        let destination = root.join(path);
        confined_create_dir_all(
            root,
            destination.parent().ok_or(MaterializeError::AssetParent)?,
        )?;
        let bytes = pack.asset(path)?;
        // Stage beside the destination and publish without replacing it. Readers
        // never see a partial body, and an interrupted staging write cannot
        // poison the digest-addressed destination or overwrite operator edits.
        let parent = destination.parent().ok_or(MaterializeError::AssetParent)?;
        let mut staged = tempfile::Builder::new()
            .prefix(".staging-")
            .tempfile_in(parent)?;
        staged.write_all(bytes)?;
        set_distribution_permissions(staged.as_file())?;
        staged.as_file().sync_all()?;
        match staged.persist_noclobber(&destination) {
            Ok(_) => {
                #[cfg(unix)]
                std::fs::File::open(destination.parent().unwrap())?.sync_all()?;
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                // The destination may have been swapped for a symlink after
                // validation; re-read it through a no-follow open so a
                // modified or replaced cache cannot be blessed here.
                let file = no_follow_open(&destination)?;
                let mut mapped = Vec::new();
                std::io::Read::read_to_end(&mut &file, &mut mapped)?;
                if mapped != bytes {
                    return Err(MaterializeError::AssetModified { path: destination });
                }
                set_distribution_permissions(&file)?;
            }
            Err(error) => return Err(error.error.into()),
        }
    }
    Ok(())
}

/// Creates each component of `dir` below `root`, refusing any component that
/// already exists as a symlink. The symlink check is `symlink_metadata` on the
/// accumulated path — a no-follow check of the final component only, not
/// dirfd confinement of the walked prefix. `root`'s ancestors are trusted by
/// premise (see `materialize`), and a component accepted here can still be
/// swapped before a later use, so this is creation-time symlink refusal, not
/// TOCTOU closure.
fn confined_create_dir_all(root: &Path, dir: &Path) -> Result<()> {
    let relative = dir
        .strip_prefix(root)
        .map_err(|source| MaterializeError::EscapesRoot {
            root: root.to_path_buf(),
            source,
        })?;
    ensure_root_dir(root)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match std::fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::symlink_metadata(&current)?
                    .file_type()
                    .is_symlink()
                {
                    return Err(MaterializeError::ComponentSymlink { path: current });
                }
            }
            Err(source) => {
                return Err(MaterializeError::CreateCacheDir {
                    path: current,
                    source,
                })
            }
        }
    }
    Ok(())
}

/// Creates `root` when absent; when it already exists, requires it to be a
/// directory (any regular file is rejected) and refuses it when it is a
/// symlink. The refusal covers the final component only: it is a
/// `symlink_metadata` check on `root`, not a no-follow walk of its ancestors,
/// which are trusted by premise (see `materialize`). No directory mode is
/// guaranteed here — the umask decides. `root`'s parent is pre-created by
/// `cached`.
fn ensure_root_dir(root: &Path) -> Result<()> {
    match std::fs::create_dir(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(root).map_err(|source| {
                MaterializeError::InspectRoot {
                    path: root.to_path_buf(),
                    source,
                }
            })?;
            if metadata.file_type().is_symlink() {
                return Err(MaterializeError::RootSymlink {
                    path: root.to_path_buf(),
                });
            }
            if !metadata.is_dir() {
                return Err(MaterializeError::RootNotDirectory {
                    path: root.to_path_buf(),
                });
            }
            Ok(())
        }
        Err(source) => Err(MaterializeError::CreateRoot {
            path: root.to_path_buf(),
            source,
        }),
    }
}

/// Opens `path` without following a final symlink (O_NOFOLLOW on unix).
fn no_follow_open(path: &Path) -> Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|source| MaterializeError::Open {
                path: path.to_path_buf(),
                source,
            })
    }
    #[cfg(not(unix))]
    {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|source| MaterializeError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        if !metadata.file_type().is_file() {
            return Err(MaterializeError::NotRegularFile {
                path: path.to_path_buf(),
            });
        }
        std::fs::File::open(path).map_err(|source| MaterializeError::Open {
            path: path.to_path_buf(),
            source,
        })
    }
}

pub fn set_distribution_permissions(
    #[cfg(unix)] file: &std::fs::File,
    #[cfg(not(unix))] _file: &std::fs::File,
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

pub fn cache_root(home: &Path, namespace: &str, name: &str, digest: &str) -> Result<PathBuf> {
    if !(super::is_valid_pack_name(namespace) && super::is_valid_pack_name(name)) {
        return Err(MaterializeError::InvalidCoordinate {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
        });
    }
    Ok(home
        .join(crate::home::PACKS_DIR_NAME)
        .join(".materialized")
        .join(namespace)
        .join(name)
        .join(crate::pack_archive::digest_hex(digest)?))
}

pub fn cached(
    home: &Path,
    pack: &crate::pack_archive::PackArchive,
) -> Result<(PathBuf, crate::file_lock::FileLock)> {
    let root = cache_root(
        home,
        &pack.manifest().metadata.namespace,
        &pack.manifest().name,
        pack.digest(),
    )?;
    let parent = root.parent().ok_or(MaterializeError::CacheParent)?;
    std::fs::create_dir_all(parent)?;
    let lock = crate::file_lock::FileLock::shared(
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(parent.join(".cache.lock"))?,
    )?;
    materialize(pack, &root)?;
    let marker = root.join(".gents-pack-cache-v1");
    let staged = tempfile::NamedTempFile::new_in(&root)?;
    set_distribution_permissions(staged.as_file())?;
    match staged.persist_noclobber(marker) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.error.into()),
    }
    Ok((root, lock))
}

const CACHE_MARKER: &str = ".gents-pack-cache-v1";
const CACHE_LOCK: &str = ".cache.lock";

/// The caller holds `FileLock::shared(cache_lock(root.parent()))`.
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

pub fn cache_lock(parent: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(parent)?;
    Ok(std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(parent.join(CACHE_LOCK))?)
}

/// The caller holds [`lock_exclusive`] on `parent`, so no scenario can acquire
/// or use a sibling cache root while the probe and removal occur.
pub fn prune_stale_asset_cache(parent: &Path, current: &Path) -> anyhow::Result<Vec<String>> {
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
        if let CacheRelease::Removed = release_cache_root(&root, || Ok(()))? {
            removed.push(name.to_owned());
        }
    }
    removed.sort();
    Ok(removed)
}

pub fn lock_exclusive(parent: &Path) -> Result<crate::file_lock::FileLock> {
    match crate::file_lock::FileLock::try_exclusive(cache_lock(parent)?) {
        Ok(lock) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => Err(MaterializeError::CacheInUse),
        Err(std::fs::TryLockError::Error(error)) => Err(MaterializeError::LockCache(error)),
    }
}

pub enum CacheRelease {
    Removed,
    Retained(&'static str),
}

/// The caller holds [`lock_exclusive`] on `root.parent()`.
fn release_cache_root(
    root: &Path,
    check_owner: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<CacheRelease> {
    match cache_root_disposition(root, check_owner)? {
        CacheRelease::Removed => {
            std::fs::remove_dir_all(root).map_err(|source| MaterializeError::RemoveCacheRoot {
                path: root.to_path_buf(),
                source,
            })?;
            Ok(CacheRelease::Removed)
        }
        retained => Ok(retained),
    }
}

pub fn cache_root_disposition(
    root: &Path,
    check_owner: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<CacheRelease> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn assets_archive() -> crate::pack_archive::PackArchive {
        let (_guard, dir) =
            crate::test_support::fixture_pack_copy("assets_fixture", &serde_json::json!({}));
        let (bytes, _) = crate::pack_archive::pack_dir(&dir).unwrap();
        crate::pack_archive::PackArchive::from_bytes(&bytes).unwrap()
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
        let (root, lease) = cached(home.path(), &pack).unwrap();
        assert_eq!(
            root,
            cache_root(
                home.path(),
                &pack.manifest().metadata.namespace,
                &pack.manifest().name,
                pack.digest(),
            )
            .unwrap()
        );
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

    #[cfg(unix)]
    #[test]
    fn existing_root_symlink_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir_in(&temp).unwrap();
        let root = temp.path().join("root-link");
        std::os::unix::fs::symlink(target.path(), &root).unwrap();
        let error = ensure_root_dir(&root).unwrap_err();
        assert!(
            error.to_string().contains("is a symlink"),
            "unexpected error: {error}"
        );
    }
}
