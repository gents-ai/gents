use std::path::Path;

use crate::pack::archive::{write_built_pack, PackArchive, EXTENSION};
use crate::pack::store::PackStore;
use anyhow::{Context, Result};
use gents_fs_runner::beneath::{Access, Intent, Located};

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

/// A path argument is resolved once and every read after that goes through
/// the handle of the directory it was resolved in: a swap of the named path
/// for a link or a different file after admission cannot change which bytes
/// are verified and stored. A directory is packed as `pack build` packs it,
/// so it opens with the digest its build and check report.
pub fn open(name: &LocalName<'_>, home: &Path) -> Result<PackArchive> {
    match name {
        LocalName::Digest(digest) => PackStore::new(home)
            .open(digest)
            .with_context(|| format!("{digest} is not in the pack store of {}", home.display())),
        LocalName::Path(path) => open_located(&Located::at(path, Intent::Existing)?, home),
    }
}

/// The pack at a resolved directory or built `.pack` file, imported into the
/// pack store of `home`.
pub fn open_located(located: &Located, home: &Path) -> Result<PackArchive> {
    let store = PackStore::new(home);
    if located.is_dir() {
        let dir = located.subroot()?;
        let staged = tempfile::NamedTempFile::new().context("staging the pack")?;
        let header = {
            let mut writer = std::io::BufWriter::new(staged.as_file());
            let header = write_built_pack(
                &dir,
                &mut writer,
                &crate::pack::core_packs::runtime_release(),
            )
            .with_context(|| format!("packing {}", located.path().display()))?;
            std::io::Write::flush(&mut writer).context("writing the pack")?;
            header
        };
        store.import_file(staged.path(), Some(&header.digest))?;
        return store.open(&header.digest);
    }
    let input = located
        .open(Access::Read)
        .with_context(|| format!("opening {}", located.path().display()))?;
    let stored = store
        .import(std::io::BufReader::new(input), None)
        .with_context(|| format!("{} is not a valid pack", located.path().display()))?;
    store.open(&stored.header.digest)
}
