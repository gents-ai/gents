use anyhow::{Context, Result};

/// Writes each declared file of `pack` into `root` without ever following a
/// symlink outside it.
///
/// Parent directories are created one component at a time, each component
/// refused when it is a symlink, and the existing-file branch re-opens the
/// destination no-follow so a swapped-in symlink cannot be read, blessed or
/// chmodded through. The staging temp file and the published destination
/// both live in the same parent directory, so the publish never traverses a
/// path an attacker can substitute.
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

/// `mkdir -p` for every component of `dir` below `root`, refusing any
/// component that is a symlink, so the cache cannot be redirected through
/// one and staged writes never traverse attacker-controlled links.
fn confined_create_dir_all(root: &std::path::Path, dir: &std::path::Path) -> Result<()> {
    let relative = dir
        .strip_prefix(root)
        .with_context(|| format!("asset path escapes the pack cache root {}", root.display()))?;
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
