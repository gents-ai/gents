use anyhow::{Context, Result};

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
pub fn materialize(pack: &crate::pack_archive::PackArchive, root: &std::path::Path) -> Result<()> {
    use std::io::Write;
    for path in std::iter::once("manifest.json")
        .chain(pack.manifest().metadata.assets.iter().map(String::as_str))
    {
        let destination = root.join(path);
        confined_create_dir_all(root, destination.parent().context("asset parent")?)?;
        let bytes = pack.asset(path)?;
        // Stage beside the destination and publish without replacing it. Readers
        // never see a partial body, and an interrupted staging write cannot
        // poison the digest-addressed destination or overwrite operator edits.
        let parent = destination.parent().context("asset parent")?;
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
                anyhow::ensure!(
                    mapped == bytes,
                    "installed asset was modified: {}",
                    destination.display()
                );
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
fn confined_create_dir_all(root: &std::path::Path, dir: &std::path::Path) -> Result<()> {
    let relative = dir
        .strip_prefix(root)
        .with_context(|| format!("asset path escapes the pack cache root {}", root.display()))?;
    // The root itself is a cache component and gets the same creation-time
    // symlink/dir check as those below it (its parent is pre-created by
    // `cached`); see `ensure_root_dir` and `confined_create_dir_all` for the
    // exact guarantee, which is final-component only.
    ensure_root_dir(root)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match std::fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                anyhow::ensure!(
                    !std::fs::symlink_metadata(&current)?
                        .file_type()
                        .is_symlink(),
                    "pack cache component {} is a symlink",
                    current.display()
                );
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("creating pack cache dir {}", current.display()))
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
fn ensure_root_dir(root: &std::path::Path) -> Result<()> {
    match std::fs::create_dir(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(root)
                .with_context(|| format!("inspecting pack cache root {}", root.display()))?;
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "pack cache root {} is a symlink; remove it or choose a different cache root",
                root.display()
            );
            anyhow::ensure!(
                metadata.is_dir(),
                "pack cache root {} exists and is not a directory; remove it or choose a different cache root",
                root.display()
            );
            Ok(())
        }
        Err(error) => Err(error).with_context(|| format!("creating pack cache root {}", root.display())),
    }
}

/// Opens `path` without following a final symlink (O_NOFOLLOW on unix).
fn no_follow_open(path: &std::path::Path) -> Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .with_context(|| format!("opening {}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("opening {}", path.display()))?;
        anyhow::ensure!(
            metadata.file_type().is_file(),
            "installed asset {} is not a regular file",
            path.display()
        );
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))
    }
}

pub fn set_distribution_permissions(file: &std::fs::File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

pub fn cache_root(
    home: &std::path::Path,
    namespace: &str,
    name: &str,
    digest: &str,
) -> Result<std::path::PathBuf> {
    anyhow::ensure!(
        super::is_valid_pack_name(namespace) && super::is_valid_pack_name(name),
        "invalid pack coordinate {namespace}/{name}"
    );
    Ok(home
        .join(crate::home::PACKS_DIR_NAME)
        .join(".materialized")
        .join(namespace)
        .join(name)
        .join(crate::pack_archive::digest_hex(digest)?))
}

pub fn cached(
    home: &std::path::Path,
    pack: &crate::pack_archive::PackArchive,
) -> Result<(std::path::PathBuf, crate::file_lock::FileLock)> {
    let root = cache_root(
        home,
        &pack.manifest().metadata.namespace,
        &pack.manifest().name,
        pack.digest(),
    )?;
    let parent = root.parent().context("pack cache parent")?;
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

#[cfg(test)]
mod tests {
    use super::ensure_root_dir;

    #[test]
    fn absent_root_is_created() {
        let temp = tempfile::tempdir().unwrap();
        // `ensure_root_dir` uses a nonrecursive create, so the trusted parent
        // must already exist; only the root itself is created here.
        std::fs::create_dir(temp.path().join("a")).unwrap();
        let root = temp.path().join("a").join("b");
        ensure_root_dir(&root).unwrap();
        assert!(root.is_dir());
        // Idempotent when the root already exists as a directory.
        ensure_root_dir(&root).unwrap();
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

    #[test]
    fn existing_root_regular_file_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root-file");
        std::fs::write(&root, b"not a directory").unwrap();
        let error = ensure_root_dir(&root).unwrap_err();
        assert!(
            error.to_string().contains("is not a directory"),
            "unexpected error: {error}"
        );
    }
}
