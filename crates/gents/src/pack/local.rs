//! Packs named by content or by path: `sha256:{hex}`, `./x.pack`, `./dir`.
//!
//! A local pack is admitted exactly like a downloaded one: a `.pack` file is
//! verified into the home's [`PackStore`] and opened from there, and a
//! directory is packed with the same writer `gents pack build` uses first. A
//! bare name is never a path, so `mailbox` always means the stored or
//! registry pack even when a `mailbox/` directory sits in the working
//! directory; a local directory is named `./mailbox`.

use std::path::Path;

use crate::pack_archive::{write_pack, PackArchive, EXTENSION};
use crate::pack_store::PackStore;
use anyhow::{Context, Result};

/// How a pack argument names a local pack, if it does.
#[derive(Debug, PartialEq, Eq)]
pub enum LocalName<'a> {
    Digest(&'a str),
    Path(&'a Path),
}

pub fn classify(argument: &str) -> Option<LocalName<'_>> {
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
///
/// A path argument is opened once, no-follow, and that handle is carried
/// into the store import: an admission-to-read window where the named path
/// is swapped for a symlink or a different file cannot change which bytes
/// are verified and stored.
pub fn open(name: &LocalName<'_>, home: &Path) -> Result<PackArchive> {
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
            let stored = store.import_file_no_follow(path, None)?;
            store.open(&stored.header.digest)
        }
    }
}
