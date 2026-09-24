//! Packs named by content or by path: `sha256:{hex}`, `./x.pack`, `./dir`.
//!
//! A local pack is admitted exactly like a downloaded one: a `.pack` file is
//! verified into the home's [`PackStore`] and opened from there, and a
//! directory is packed with the same writer `gents pack build` uses first. A
//! bare name is never a path, so `mailbox` always means the bundled or
//! registry pack even when a `mailbox/` directory sits in the working
//! directory; a local directory is named `./mailbox`.

use std::path::Path;

use anyhow::{Context, Result};
use gents::pack_archive::{write_pack, PackArchive, EXTENSION};
use gents::pack_store::PackStore;

/// How a pack argument names a local pack, if it does.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LocalName<'a> {
    Digest(&'a str),
    Path(&'a Path),
}

pub(super) fn classify(argument: &str) -> Option<LocalName<'_>> {
    if argument.starts_with("sha256:") {
        return Some(LocalName::Digest(argument));
    }
    let path = Path::new(argument);
    let explicit = argument.starts_with("./")
        || argument.starts_with("../")
        || path.is_absolute()
        || path.extension().is_some_and(|ext| ext == EXTENSION);
    explicit.then_some(LocalName::Path(path))
}

/// Opens the local pack `name` names, storing it in `home`'s store first.
pub(super) fn open(name: &LocalName<'_>, home: &Path) -> Result<PackArchive> {
    let store = PackStore::new(home);
    match name {
        LocalName::Digest(digest) => store
            .open(digest)
            .with_context(|| format!("{digest} is not in the pack store of {}", home.display())),
        LocalName::Path(path) if path.is_dir() => {
            let staged = tempfile::NamedTempFile::new().context("staging the pack")?;
            let header = {
                let mut writer = std::io::BufWriter::new(staged.as_file());
                let header = write_pack(path, &mut writer)
                    .with_context(|| format!("packing {}", path.display()))?;
                std::io::Write::flush(&mut writer).context("writing the pack")?;
                header
            };
            store.import_file(staged.path(), Some(&header.digest))?;
            store.open(&header.digest)
        }
        LocalName::Path(path) => {
            let stored = store.import_file(path, None)?;
            store.open(&stored.header.digest)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_path_or_digest_is_local() {
        assert_eq!(classify("mailbox"), None);
        assert_eq!(classify("acme/mailbox"), None);
        assert_eq!(
            classify("sha256:abc"),
            Some(LocalName::Digest("sha256:abc"))
        );
        for path in [
            "./mailbox",
            "../mailbox",
            "/tmp/mailbox",
            "x.pack",
            "dir/x.pack",
        ] {
            assert_eq!(
                classify(path),
                Some(LocalName::Path(Path::new(path))),
                "{path}"
            );
        }
    }

    fn mailbox_dir() -> tempfile::TempDir {
        let pack = gents::pack::resolve_pack("mailbox").unwrap();
        let dir = tempfile::tempdir().unwrap();
        for path in gents::pack::declared_paths(&pack.manifest) {
            let target = dir.path().join(&path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, pack.asset(&path).unwrap()).unwrap();
        }
        dir
    }

    #[test]
    fn a_directory_a_pack_file_and_its_digest_open_as_the_same_pack() {
        let bundled = gents::pack::resolve_pack("mailbox").unwrap();
        let home = tempfile::tempdir().unwrap();
        let dir = mailbox_dir();

        let from_dir = open(&LocalName::Path(dir.path()), home.path()).unwrap();
        assert_eq!(from_dir.digest(), bundled.digest);

        let file = home.path().join("mailbox.pack");
        let (bytes, _) = gents::pack_archive::pack_dir(dir.path()).unwrap();
        std::fs::write(&file, bytes).unwrap();
        let from_file = open(&LocalName::Path(&file), home.path()).unwrap();
        assert_eq!(from_file.digest(), bundled.digest);

        let from_digest = open(&LocalName::Digest(&bundled.digest), home.path()).unwrap();
        assert_eq!(from_digest.digest(), bundled.digest);
    }

    #[test]
    fn an_unknown_digest_names_the_store_it_looked_in() {
        let home = tempfile::tempdir().unwrap();
        let digest = format!("sha256:{}", "0".repeat(64));
        let error = open(&LocalName::Digest(&digest), home.path()).unwrap_err();
        assert!(
            format!("{error:#}").contains("is not in the pack store"),
            "{error:#}"
        );
    }
}
