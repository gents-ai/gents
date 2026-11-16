use std::path::Path;

use crate::pack::archive::{write_pack, PackArchive, EXTENSION};
use crate::pack::store::PackStore;
use anyhow::{Context, Result};

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
