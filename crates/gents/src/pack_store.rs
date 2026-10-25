use std::collections::BTreeMap;
use std::convert::Infallible;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::pack::is_valid_pack_name;
use crate::pack_archive::format::PackFormatError;
use crate::pack_archive::{
    digest_hex, peek_header, read_pack, Bounds, PackArchive, PackHeader, VerifiedPack, EXTENSION,
};

type Result<T, E = PackStoreError> = std::result::Result<T, E>;

const NAME_INDEX_DIR_NAME: &str = "by-name";

const UNPACKED_HEADER: &str = ".pack-header.json";

#[derive(Debug, thiserror::Error)]
pub enum PackStoreError {
    #[error(transparent)]
    Format(#[from] PackFormatError),
    #[error(transparent)]
    Pack(#[from] crate::pack::PackError),
    #[error(transparent)]
    Archive(#[from] crate::pack_archive::PackArchiveError),
    #[error("creating the pack store {}", path.display())]
    StoreDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("staging a pack in {}", path.display())]
    StagePack {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("writing the staged pack")]
    WriteStaged(#[source] io::Error),
    #[error("syncing the staged pack")]
    SyncStaged(#[source] io::Error),
    #[error("asked for pack {expected} but received {received}; nothing was stored")]
    DigestMismatch { expected: String, received: String },
    #[error("storing the pack at {}", path.display())]
    StorePack {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the unpacked pack's header is not valid")]
    UnpackedHeader(#[source] serde_json::Error),
    #[error("reading manifest.json")]
    UnpackedManifestRead(#[source] io::Error),
    #[error("the unpacked manifest is not valid")]
    UnpackedManifest(#[source] serde_json::Error),
    #[error("the unpacked pack at {} holds {held}, not {digest}", path.display())]
    UnpackedDigest {
        path: PathBuf,
        held: String,
        digest: String,
    },
    #[error("the unpacked pack at {} does not match {digest} even after unpacking the stored archive again", path.display())]
    UnpackedRecomputeMismatch { path: PathBuf, digest: String },
    #[error("unpacked path has no parent")]
    UnpackedPathNoParent,
    #[error("creating {}", path.display())]
    CreateUnpackedDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("staging an unpacked pack in {}", path.display())]
    StageUnpacked {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("pack {digest} is not in the store at {}", root.display())]
    PackNotInStore {
        digest: String,
        root: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the stored pack {} is damaged", path.display())]
    StoredPackDamaged {
        path: PathBuf,
        #[source]
        source: Box<PackFormatError>,
    },
    #[error("the stored pack {} holds {held}, not {digest}", path.display())]
    StoredDigest {
        path: PathBuf,
        held: String,
        digest: String,
    },
    #[error("writing the unpacked manifest")]
    WriteUnpackedManifest(#[source] io::Error),
    #[error(transparent)]
    Json(serde_json::Error),
    #[error("writing the unpacked header")]
    WriteUnpackedHeader(#[source] io::Error),
    #[error("unpacking into {}", path.display())]
    RenameUnpacked {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("a name index entry has no directory")]
    IndexEntryNoDirectory,
    #[error("creating the pack name index at {}", path.display())]
    CreateIndexDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("staging a pack name index entry in {}", path.display())]
    StageIndexEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("writing a pack name index entry")]
    WriteIndexEntry(#[source] io::Error),
    #[error("syncing a pack name index entry")]
    SyncIndexEntry(#[source] io::Error),
    #[error("storing the pack name index entry at {}", path.display())]
    StoreIndexEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("reading {}", path.display())]
    ReadPath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("opening {}", path.display())]
    OpenPath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("removing {}", path.display())]
    RemovePath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a regular file", path.display())]
    NotRegularFile { path: PathBuf },
    #[error("reading the unpacked {path}")]
    ReadUnpacked {
        path: String,
        #[source]
        source: Box<PackStoreError>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PackImportError<E>
where
    E: std::error::Error + 'static,
{
    #[error(transparent)]
    Store(#[from] PackStoreError),
    #[error(transparent)]
    Accept(E),
}

#[derive(Debug, thiserror::Error)]
pub enum PackFileImportError<E>
where
    E: std::error::Error + 'static,
{
    #[error(transparent)]
    Store(#[from] PackStoreError),
    #[error("{} is not a valid pack", path.display())]
    NotAValidPack {
        path: PathBuf,
        #[source]
        source: Box<PackImportError<E>>,
    },
}

#[derive(Debug, Clone)]
pub struct PackStore {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct StoredPack {
    pub header: PackHeader,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredName {
    pub version: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum VersionKey {
    Other(String),
    Semver(semver::Version),
}

impl VersionKey {
    fn parse(version: &str) -> Self {
        semver::Version::parse(version)
            .map(VersionKey::Semver)
            .unwrap_or_else(|_| VersionKey::Other(version.to_owned()))
    }
}

pub(crate) fn is_valid_index_version(version: &str) -> bool {
    version != "."
        && version != ".."
        && !version.is_empty()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
}

fn encode_version(version: &str) -> String {
    let mut out = String::with_capacity(version.len());
    for ch in version.chars() {
        if ch.is_ascii_uppercase() {
            out.push('_');
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn decode_version(file_name: &str) -> Option<String> {
    let mut out = String::with_capacity(file_name.len());
    let mut chars = file_name.chars();
    while let Some(ch) = chars.next() {
        if ch == '_' {
            out.push(
                chars
                    .next()
                    .filter(char::is_ascii_lowercase)?
                    .to_ascii_uppercase(),
            );
        } else {
            out.push(ch);
        }
    }
    is_valid_index_version(&out).then_some(out)
}

fn read_directory(path: &Path) -> Result<std::fs::ReadDir> {
    std::fs::read_dir(path).map_err(|source| PackStoreError::ReadPath {
        path: path.to_path_buf(),
        source,
    })
}

impl PackStore {
    pub fn new(home: &Path) -> Self {
        Self {
            root: home
                .join(crate::home::PACKS_DIR_NAME)
                .join("store")
                .join("sha256"),
        }
    }

    pub fn path(&self, digest: &str) -> Result<PathBuf> {
        Ok(self
            .root
            .join(format!("{}.{EXTENSION}", digest_hex(digest)?)))
    }

    pub fn contains(&self, digest: &str) -> Result<bool> {
        Ok(self.path(digest)?.is_file())
    }

    pub fn import(&self, input: impl Read, expected: Option<&str>) -> Result<StoredPack> {
        let (staged, verified) = self.stage(input, expected)?;
        self.commit(staged, verified, expected)
    }

    pub fn import_accepting<E>(
        &self,
        input: impl Read,
        expected: Option<&str>,
        accept: impl FnOnce(&VerifiedPack) -> std::result::Result<(), E>,
    ) -> std::result::Result<StoredPack, PackImportError<E>>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        let (staged, verified) = self.stage(input, expected)?;
        accept(&verified).map_err(PackImportError::Accept)?;
        Ok(self.commit(staged, verified, expected)?)
    }

    pub fn import_file(
        &self,
        file: &Path,
        expected: Option<&str>,
    ) -> std::result::Result<StoredPack, PackFileImportError<Infallible>> {
        self.import_file_accepting(file, expected, |_| Ok::<(), Infallible>(()))
    }

    pub fn import_file_no_follow(
        &self,
        file: &Path,
        expected: Option<&str>,
    ) -> std::result::Result<StoredPack, PackFileImportError<Infallible>> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let input = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(file)
                .map_err(|source| PackStoreError::OpenPath {
                    path: file.to_path_buf(),
                    source,
                })?;
            self.import_accepting(io::BufReader::new(input), expected, |_| {
                Ok::<(), Infallible>(())
            })
            .map_err(|source| PackFileImportError::NotAValidPack {
                path: file.to_path_buf(),
                source: Box::new(source),
            })
        }
        #[cfg(not(unix))]
        {
            let metadata =
                std::fs::symlink_metadata(file).map_err(|source| PackStoreError::OpenPath {
                    path: file.to_path_buf(),
                    source,
                })?;
            if !metadata.file_type().is_file() {
                return Err(PackStoreError::NotRegularFile {
                    path: file.to_path_buf(),
                }
                .into());
            }
            self.import_file(file, expected)
        }
    }

    pub fn import_file_accepting<E>(
        &self,
        file: &Path,
        expected: Option<&str>,
        accept: impl FnOnce(&VerifiedPack) -> std::result::Result<(), E>,
    ) -> std::result::Result<StoredPack, PackFileImportError<E>>
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        let input = std::fs::File::open(file).map_err(|source| PackStoreError::OpenPath {
            path: file.to_path_buf(),
            source,
        })?;
        self.import_accepting(io::BufReader::new(input), expected, accept)
            .map_err(|source| PackFileImportError::NotAValidPack {
                path: file.to_path_buf(),
                source: Box::new(source),
            })
    }

    fn stage(
        &self,
        input: impl Read,
        expected: Option<&str>,
    ) -> Result<(tempfile::NamedTempFile, VerifiedPack)> {
        if let Some(expected) = expected {
            digest_hex(expected)?;
        }
        std::fs::create_dir_all(&self.root).map_err(|source| PackStoreError::StoreDir {
            path: self.root.clone(),
            source,
        })?;
        let mut staged = tempfile::Builder::new()
            .prefix(".staging-")
            .tempfile_in(&self.root)
            .map_err(|source| PackStoreError::StagePack {
                path: self.root.clone(),
                source,
            })?;
        let verified = {
            let mut tee = TeeReader {
                inner: input,
                copy: io::BufWriter::new(staged.as_file_mut()),
            };
            let verified = read_pack(&mut tee, Bounds::default(), |_, _| Ok(()))?;
            tee.copy.flush().map_err(PackStoreError::WriteStaged)?;
            verified
        };
        Ok((staged, verified))
    }

    fn commit(
        &self,
        staged: tempfile::NamedTempFile,
        verified: VerifiedPack,
        expected: Option<&str>,
    ) -> Result<StoredPack> {
        let digest = &verified.header.digest;
        if let Some(expected) = expected {
            if digest != expected {
                return Err(PackStoreError::DigestMismatch {
                    expected: expected.to_owned(),
                    received: digest.clone(),
                });
            }
        }
        staged
            .as_file()
            .sync_all()
            .map_err(PackStoreError::SyncStaged)?;
        let path = self.path(digest)?;
        match staged.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(PackStoreError::StorePack {
                    path,
                    source: error.error,
                })
            }
        }
        self.index(&verified.header)?;
        Ok(StoredPack {
            header: verified.header,
            path,
        })
    }

    pub fn open(&self, digest: &str) -> Result<PackArchive> {
        let hex = digest_hex(digest)?;
        let unpacked = self.unpacked_root().join(hex);
        if !unpacked.is_dir() {
            self.unpack(digest, &unpacked)?;
        }
        let header: PackHeader =
            serde_json::from_slice(&std::fs::read(unpacked.join(UNPACKED_HEADER)).map_err(
                |source| PackStoreError::ReadPath {
                    path: unpacked.clone(),
                    source,
                },
            )?)
            .map_err(PackStoreError::UnpackedHeader)?;
        if header.digest != digest {
            return Err(PackStoreError::UnpackedDigest {
                path: unpacked,
                held: header.digest.clone(),
                digest: digest.to_owned(),
            });
        }
        let manifest: crate::pack::PackManifest = serde_json::from_slice(
            &std::fs::read(unpacked.join("manifest.json"))
                .map_err(PackStoreError::UnpackedManifestRead)?,
        )
        .map_err(PackStoreError::UnpackedManifest)?;
        let contents = Self::declared_file_contents(&unpacked, &manifest)?;
        let computed =
            crate::pack::digest_declared_assets(&manifest, |path| Some(contents[path].as_slice()))?;
        if computed != digest {
            tracing::warn!(
                digest,
                unpacked = %unpacked.display(),
                "the unpacked pack's contents no longer hash to their digest; unpacking the stored archive again"
            );
            self.unpack(digest, &unpacked)?;
            let reread: crate::pack::PackManifest = serde_json::from_slice(
                &std::fs::read(unpacked.join("manifest.json"))
                    .map_err(PackStoreError::UnpackedManifestRead)?,
            )
            .map_err(PackStoreError::UnpackedManifest)?;
            let contents = Self::declared_file_contents(&unpacked, &reread)?;
            let recomputed = crate::pack::digest_declared_assets(&reread, |path| {
                Some(contents[path].as_slice())
            })?;
            if recomputed != digest {
                return Err(PackStoreError::UnpackedRecomputeMismatch {
                    path: unpacked,
                    digest: digest.to_owned(),
                });
            }
        }
        Ok(PackArchive::from_unpacked(&unpacked, header, manifest)?)
    }

    fn unpacked_root(&self) -> PathBuf {
        self.root.parent().and_then(Path::parent).map_or_else(
            || self.root.join("unpacked"),
            |packs| packs.join("unpacked"),
        )
    }

    /// Refuses final symlinks so digest verification cannot be redirected by the unpacked tree.
    fn declared_file_contents(
        unpacked: &Path,
        manifest: &crate::pack::PackManifest,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        crate::pack::declared_paths(manifest)
            .into_iter()
            .map(|path| -> Result<(String, Vec<u8>)> {
                let bytes = no_follow_read(&unpacked.join(&path)).map_err(|source| {
                    PackStoreError::ReadUnpacked {
                        path: path.clone(),
                        source: Box::new(source),
                    }
                })?;
                Ok((path, bytes))
            })
            .collect()
    }

    fn by_name_root(&self) -> PathBuf {
        self.root.parent().map_or_else(
            || self.root.join(NAME_INDEX_DIR_NAME),
            |store| store.join(NAME_INDEX_DIR_NAME),
        )
    }

    fn index_entry_path(&self, header: &PackHeader) -> Option<PathBuf> {
        let (namespace, name) = header.coordinate.split_once('/')?;
        (is_valid_pack_name(namespace)
            && is_valid_pack_name(name)
            && is_valid_index_version(&header.version))
        .then(|| {
            self.by_name_root()
                .join(namespace)
                .join(name)
                .join(encode_version(&header.version))
        })
    }

    pub(crate) fn index(&self, header: &PackHeader) -> Result<()> {
        let Some(target) = self.index_entry_path(header) else {
            tracing::warn!(
                coordinate = %header.coordinate,
                version = %header.version,
                "pack coordinate or version cannot be indexed by name; not indexing it"
            );
            return Ok(());
        };
        if std::fs::read_to_string(&target).is_ok_and(|recorded| recorded.trim() == header.digest) {
            return Ok(());
        }
        let dir = target
            .parent()
            .ok_or(PackStoreError::IndexEntryNoDirectory)?;
        std::fs::create_dir_all(dir).map_err(|source| PackStoreError::CreateIndexDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let mut staged = tempfile::Builder::new()
            .prefix(".staging-")
            .tempfile_in(dir)
            .map_err(|source| PackStoreError::StageIndexEntry {
                path: dir.to_path_buf(),
                source,
            })?;
        staged
            .write_all(header.digest.as_bytes())
            .map_err(PackStoreError::WriteIndexEntry)?;
        staged
            .as_file()
            .sync_all()
            .map_err(PackStoreError::SyncIndexEntry)?;
        staged
            .persist(&target)
            .map_err(|error| PackStoreError::StoreIndexEntry {
                path: target.clone(),
                source: error.error,
            })?;
        Ok(())
    }

    fn read_indexed_versions(&self, dir: &Path) -> Result<Vec<StoredName>> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(PackStoreError::ReadPath {
                    path: dir.to_path_buf(),
                    source,
                })
            }
        };
        let mut versions = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| PackStoreError::ReadPath {
                path: dir.to_path_buf(),
                source,
            })?;
            let Some(version) = entry.file_name().to_str().and_then(decode_version) else {
                continue;
            };
            let digest = match std::fs::read_to_string(entry.path()) {
                Ok(digest) => digest.trim().to_owned(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(PackStoreError::ReadPath {
                        path: entry.path(),
                        source,
                    })
                }
            };
            let present = match self.contains(&digest) {
                Ok(present) => present,
                Err(error) => {
                    tracing::warn!(
                        entry = %entry.path().display(),
                        %error,
                        "pack name index entry has a malformed digest; skipping it"
                    );
                    false
                }
            };
            if present {
                versions.push(StoredName { version, digest });
            }
        }
        Ok(versions)
    }

    pub fn lookup(
        &self,
        namespace: &str,
        name: &str,
        version: Option<&str>,
    ) -> Result<Option<StoredName>> {
        if !is_valid_pack_name(namespace) || !is_valid_pack_name(name) {
            return Ok(None);
        }
        let dir = self.by_name_root().join(namespace).join(name);
        if let Some(version) = version {
            return self.lookup_exact_version(&dir, version);
        }
        Ok(self
            .read_indexed_versions(&dir)?
            .into_iter()
            .max_by_key(|stored| VersionKey::parse(&stored.version)))
    }

    fn lookup_exact_version(&self, dir: &Path, version: &str) -> Result<Option<StoredName>> {
        if !is_valid_index_version(version) {
            return Ok(None);
        }
        let path = dir.join(encode_version(version));
        let digest = match std::fs::read_to_string(&path) {
            Ok(digest) => digest.trim().to_owned(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(PackStoreError::ReadPath { path, source }),
        };
        let present = match self.contains(&digest) {
            Ok(present) => present,
            Err(error) => {
                tracing::warn!(
                    entry = %path.display(),
                    %error,
                    "pack name index entry has a malformed digest; skipping it"
                );
                false
            }
        };
        Ok(present.then(|| StoredName {
            version: version.to_owned(),
            digest,
        }))
    }

    pub fn names(&self) -> Result<Vec<(String, Vec<StoredName>)>> {
        let root = self.by_name_root();
        let namespace_entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(PackStoreError::ReadPath { path: root, source }),
        };
        let mut names = Vec::new();
        for namespace_entry in namespace_entries {
            let namespace_entry = namespace_entry.map_err(|source| PackStoreError::ReadPath {
                path: root.clone(),
                source,
            })?;
            if !namespace_entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Some(namespace) = namespace_entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let namespace_dir = namespace_entry.path();
            for name_entry in read_directory(&namespace_dir)? {
                let name_entry = name_entry.map_err(|source| PackStoreError::ReadPath {
                    path: namespace_dir.clone(),
                    source,
                })?;
                if !name_entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let Some(name) = name_entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let mut versions = self.read_indexed_versions(&name_entry.path())?;
                if versions.is_empty() {
                    continue;
                }
                versions.sort_by_cached_key(|stored| {
                    std::cmp::Reverse(VersionKey::parse(&stored.version))
                });
                names.push((format!("{namespace}/{name}"), versions));
            }
        }
        names.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(names)
    }

    fn stored_header(&self, archive: &Path, unpacked: &Path) -> Option<PackHeader> {
        let header_path = unpacked.join(UNPACKED_HEADER);
        if let Ok(bytes) = std::fs::read(&header_path) {
            match serde_json::from_slice(&bytes) {
                Ok(header) => return Some(header),
                Err(error) => tracing::warn!(
                    path = %header_path.display(),
                    %error,
                    "the unpacked pack header is not valid; scanning the name index instead"
                ),
            }
        }
        let file = match std::fs::File::open(archive) {
            Ok(file) => file,
            Err(error) => {
                if error.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %archive.display(),
                        %error,
                        "cannot open the stored pack; scanning the name index instead"
                    );
                }
                return None;
            }
        };
        match peek_header(io::BufReader::new(file), Bounds::default()) {
            Ok(header) => Some(header),
            Err(error) => {
                tracing::warn!(
                    path = %archive.display(),
                    %error,
                    "cannot read the stored pack header; scanning the name index instead"
                );
                None
            }
        }
    }

    fn remove_entry_if_records(path: &Path, digest: &str) -> Result<()> {
        let recorded = match std::fs::read_to_string(path) {
            Ok(recorded) => recorded,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(PackStoreError::ReadPath {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        if recorded.trim() != digest {
            return Ok(());
        }
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(PackStoreError::RemovePath {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn remove_name_entries_for_digest(&self, digest: &str) -> Result<()> {
        let root = self.by_name_root();
        let namespace_entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(PackStoreError::ReadPath { path: root, source }),
        };
        for namespace_entry in namespace_entries {
            let namespace_dir = namespace_entry
                .map_err(|source| PackStoreError::ReadPath {
                    path: root.clone(),
                    source,
                })?
                .path();
            if !namespace_dir.is_dir() {
                continue;
            }
            for name_entry in read_directory(&namespace_dir)? {
                let name_dir = name_entry
                    .map_err(|source| PackStoreError::ReadPath {
                        path: namespace_dir.clone(),
                        source,
                    })?
                    .path();
                if !name_dir.is_dir() {
                    continue;
                }
                for version_entry in read_directory(&name_dir)? {
                    let path = version_entry
                        .map_err(|source| PackStoreError::ReadPath {
                            path: name_dir.clone(),
                            source,
                        })?
                        .path();
                    Self::remove_entry_if_records(&path, digest)?;
                }
            }
        }
        Ok(())
    }

    fn unpack(&self, digest: &str, target: &Path) -> Result<()> {
        let path = self.path(digest)?;
        let file = std::fs::File::open(&path).map_err(|source| PackStoreError::PackNotInStore {
            digest: digest.to_owned(),
            root: self.root.clone(),
            source,
        })?;
        let parent = target
            .parent()
            .ok_or(PackStoreError::UnpackedPathNoParent)?;
        std::fs::create_dir_all(parent).map_err(|source| PackStoreError::CreateUnpackedDir {
            path: parent.to_path_buf(),
            source,
        })?;
        let staging = tempfile::Builder::new()
            .prefix(".unpacking-")
            .tempdir_in(parent)
            .map_err(|source| PackStoreError::StageUnpacked {
                path: parent.to_path_buf(),
                source,
            })?;
        let verified = read_pack(
            io::BufReader::new(file),
            Bounds::default(),
            |entry, content| {
                let out = staging.path().join(entry);
                if let Some(dir) = out.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                io::copy(
                    content,
                    &mut io::BufWriter::new(std::fs::File::create(&out)?),
                )?;
                Ok(())
            },
        )
        .map_err(|source| PackStoreError::StoredPackDamaged {
            path: path.clone(),
            source: Box::new(source),
        })?;
        if verified.header.digest != digest {
            return Err(PackStoreError::StoredDigest {
                path,
                held: verified.header.digest.clone(),
                digest: digest.to_owned(),
            });
        }
        std::fs::write(
            staging.path().join("manifest.json"),
            &verified.manifest_bytes,
        )
        .map_err(PackStoreError::WriteUnpackedManifest)?;
        std::fs::write(
            staging.path().join(UNPACKED_HEADER),
            serde_json::to_vec(&verified.header).map_err(PackStoreError::Json)?,
        )
        .map_err(PackStoreError::WriteUnpackedHeader)?;
        match std::fs::rename(staging.path(), target) {
            Ok(()) => {
                let _ = staging.keep();
                Ok(())
            }
            Err(_) if target.is_dir() => Ok(()),
            Err(source) => Err(PackStoreError::RenameUnpacked {
                path: target.to_path_buf(),
                source,
            }),
        }
    }

    /// Unpacked copies containing `runs/` retain operator history when a pack is released.
    pub fn release(&self, digest: &str) -> Result<bool> {
        let hex = digest_hex(digest)?;
        let archive = self.path(digest)?;
        let unpacked = self.unpacked_root().join(hex);
        // Read before deleting: once both copies are gone there is no
        // cheap way left to reconstruct this digest's name-index entry.
        let header = self.stored_header(&archive, &unpacked);
        let mut released = false;
        match std::fs::remove_file(&archive) {
            Ok(()) => released = true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(PackStoreError::RemovePath {
                    path: archive,
                    source,
                })
            }
        }
        let retains_runs = unpacked
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir())
            && unpacked.join("runs").exists();
        if !retains_runs {
            match std::fs::remove_dir_all(&unpacked) {
                Ok(()) => released = true,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(PackStoreError::RemovePath {
                        path: unpacked,
                        source,
                    })
                }
            }
        }
        match header {
            Some(header) => {
                if let Some(path) = self.index_entry_path(&header) {
                    Self::remove_entry_if_records(&path, digest)?;
                }
            }
            None => self.remove_name_entries_for_digest(digest)?,
        }
        Ok(released)
    }

    pub fn verify(&self, digest: &str) -> Result<PackHeader> {
        let path = self.path(digest)?;
        let file = std::fs::File::open(&path).map_err(|source| PackStoreError::PackNotInStore {
            digest: digest.to_owned(),
            root: self.root.clone(),
            source,
        })?;
        let verified = read_pack(io::BufReader::new(file), Bounds::default(), |_, _| Ok(()))
            .map_err(|source| PackStoreError::StoredPackDamaged {
                path: path.clone(),
                source: Box::new(source),
            })?;
        if verified.header.digest != digest {
            return Err(PackStoreError::StoredDigest {
                path,
                held: verified.header.digest.clone(),
                digest: digest.to_owned(),
            });
        }
        Ok(verified.header)
    }
}

pub(crate) fn no_follow_read(path: &Path) -> Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|source| PackStoreError::OpenPath {
                path: path.to_path_buf(),
                source,
            })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| PackStoreError::ReadPath {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|source| PackStoreError::OpenPath {
                path: path.to_path_buf(),
                source,
            })?;
        if !metadata.file_type().is_file() {
            return Err(PackStoreError::NotRegularFile {
                path: path.to_path_buf(),
            });
        }
        std::fs::read(path).map_err(|source| PackStoreError::ReadPath {
            path: path.to_path_buf(),
            source,
        })
    }
}

struct TeeReader<R, W> {
    inner: R,
    copy: W,
}

impl<R, W> Read for TeeReader<R, W>
where
    R: Read,
    W: Write,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.copy.write_all(&buf[..n])?;
        Ok(n)
    }
}

#[cfg(test)]
pub(crate) fn test_pack_named(name: &str, version: &str) -> (Vec<u8>, PackHeader) {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/packs/assets_fixture");
    let manifest: crate::pack::PackManifest =
        serde_json::from_slice(&std::fs::read(source.join("manifest.json")).unwrap()).unwrap();
    let dir = tempfile::tempdir().expect("tempdir");
    for path in crate::pack::declared_paths(&manifest) {
        let target = dir.path().join(&path);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
        std::fs::copy(source.join(&path), &target).expect("copy");
    }
    let manifest_path = dir.path().join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["namespace"] = serde_json::json!("gents");
    manifest["name"] = serde_json::json!(name);
    manifest["version"] = serde_json::json!(version);
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    crate::pack_archive::pack_dir(dir.path()).expect("packing")
}

#[cfg(test)]
mod tests;
