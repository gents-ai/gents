use std::path::Path;

use std::path::PathBuf;

use crate::pack_archive::format::PackFormatError;
use crate::pack_archive::{write_pack, PackArchive, EXTENSION};
use crate::pack_store::{PackFileImportError, PackStore, PackStoreError};

#[derive(Debug, thiserror::Error)]
pub enum LocalPackError {
    #[error("{digest} is not in the pack store of {}", home.display())]
    NotInStore {
        digest: String,
        home: PathBuf,
        #[source]
        source: Box<PackStoreError>,
    },
    #[error("staging the pack")]
    Staging(#[source] std::io::Error),
    #[error("packing {}", path.display())]
    Pack {
        path: PathBuf,
        #[source]
        source: PackFormatError,
    },
    #[error("writing the pack")]
    WritePack(#[source] std::io::Error),
    #[error(transparent)]
    Import(#[from] PackFileImportError<std::convert::Infallible>),
    #[error(transparent)]
    Store(#[from] PackStoreError),
}

type Result<T, E = LocalPackError> = std::result::Result<T, E>;

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
        LocalName::Digest(digest) => {
            store
                .open(digest)
                .map_err(|source| LocalPackError::NotInStore {
                    digest: (*digest).to_owned(),
                    home: home.to_path_buf(),
                    source: Box::new(source),
                })
        }
        LocalName::Path(path) if path.is_dir() => {
            let staged = tempfile::NamedTempFile::new().map_err(LocalPackError::Staging)?;
            let header = {
                let mut writer = std::io::BufWriter::new(staged.as_file());
                let header =
                    write_pack(path, &mut writer).map_err(|source| LocalPackError::Pack {
                        path: path.to_path_buf(),
                        source,
                    })?;
                std::io::Write::flush(&mut writer).map_err(LocalPackError::WritePack)?;
                header
            };
            store.import_file(staged.path(), Some(&header.digest))?;
            Ok(store.open(&header.digest)?)
        }
        LocalName::Path(path) => {
            let stored = store.import_file_no_follow(path, None)?;
            Ok(store.open(&stored.header.digest)?)
        }
    }
}
