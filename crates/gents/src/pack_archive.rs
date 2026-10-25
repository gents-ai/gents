use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::pack::{is_distributable_asset_path, PackManifest, PackPlugin};

pub mod format;
pub use format::{
    digest_hex, pack_dir, peek_header, read_pack, write_pack, Bounds, PackHeader, VerifiedPack,
    EXTENSION, FORMAT, FORMAT_VERSION, HEADER_ENTRY, MEDIA_TYPE,
};

pub const DEFAULT_NAMESPACE: &str = "gents";

#[derive(Debug, thiserror::Error)]
pub enum PackArchiveError {
    #[error(transparent)]
    Format(#[from] format::PackFormatError),
    #[error("refusing to read pack asset {path:?}")]
    UnpackedRefused { path: String },
    #[error("the unpacked pack is missing {path}")]
    UnpackedMissing {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Metadata(std::io::Error),
    #[error("mapping {}", path.display())]
    Map {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("this pack ships no plugin called {name:?}")]
    NoPlugin { name: String },
    #[error("this pack carries no asset {path:?}")]
    NoAsset { path: String },
    #[error("refusing to write pack asset {path:?}")]
    WriteRefused { path: String },
    #[error("creating {}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub const MAX_PACK_BYTES: usize = 1024 * 1024 * 1024;

pub const MAX_DECOMPRESSED_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub const MAX_ENTRIES: usize = 4096;

pub struct PackArchive {
    header: PackHeader,
    manifest: PackManifest,
    assets: BTreeMap<String, AssetBytes>,
}

enum AssetBytes {
    Owned(Vec<u8>),
    Mapped(memmap2::Mmap),
    Empty,
}

impl AssetBytes {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Mapped(map) => map,
            Self::Empty => &[],
        }
    }
}

impl std::fmt::Debug for PackArchive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackArchive")
            .field("name", &self.manifest.name)
            .field("version", &self.manifest.version)
            .field("assets", &self.assets.len())
            .field("plugins", &self.manifest.metadata.plugins.len())
            .finish()
    }
}

impl PackArchive {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PackArchiveError> {
        Self::from_reader(bytes, Bounds::default())
    }

    pub fn from_reader(input: impl Read, bounds: Bounds) -> Result<Self, PackArchiveError> {
        let mut assets = BTreeMap::new();
        let verified = read_pack(input, bounds, |path, content| {
            let mut data = Vec::new();
            content.read_to_end(&mut data)?;
            assets.insert(path.to_owned(), AssetBytes::Owned(data));
            Ok(())
        })?;
        assets.insert(
            "manifest.json".to_owned(),
            AssetBytes::Owned(verified.manifest_bytes),
        );
        Ok(Self {
            header: verified.header,
            manifest: verified.manifest,
            assets,
        })
    }

    pub fn from_unpacked(
        dir: &Path,
        header: PackHeader,
        manifest: PackManifest,
    ) -> Result<Self, PackArchiveError> {
        let mut assets = BTreeMap::new();
        for path in crate::pack::declared_paths(&manifest) {
            if !is_distributable_asset_path(&path) {
                return Err(PackArchiveError::UnpackedRefused { path });
            }
            let file = std::fs::File::open(dir.join(&path)).map_err(|source| {
                PackArchiveError::UnpackedMissing {
                    path: path.clone(),
                    source,
                }
            })?;
            let bytes = if file.metadata().map_err(PackArchiveError::Metadata)?.len() == 0 {
                AssetBytes::Empty
            } else {
                // SAFETY: the store writes an unpacked pack once, into a staging
                // directory renamed into place under its digest, and never
                // writes to it again, so the mapped file does not change.
                AssetBytes::Mapped(unsafe { memmap2::Mmap::map(&file) }.map_err(|source| {
                    PackArchiveError::Map {
                        path: dir.join(&path),
                        source,
                    }
                })?)
            };
            assets.insert(path, bytes);
        }
        Ok(Self {
            header,
            manifest,
            assets,
        })
    }

    pub fn header(&self) -> &PackHeader {
        &self.header
    }

    pub fn manifest(&self) -> &PackManifest {
        &self.manifest
    }

    pub fn plugins(&self) -> &[PackPlugin] {
        &self.manifest.metadata.plugins
    }

    pub fn plugin(&self, name: &str) -> Result<&PackPlugin, PackArchiveError> {
        self.manifest
            .metadata
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .ok_or_else(|| PackArchiveError::NoPlugin {
                name: name.to_owned(),
            })
    }

    pub fn plugin_artifact(&self, name: &str) -> Result<&[u8], PackArchiveError> {
        let plugin = self.plugin(name)?;
        self.asset(&plugin.artifact)
    }

    pub fn asset(&self, path: &str) -> Result<&[u8], PackArchiveError> {
        self.assets
            .get(path)
            .map(AssetBytes::as_slice)
            .ok_or_else(|| PackArchiveError::NoAsset {
                path: path.to_owned(),
            })
    }

    pub fn digest(&self) -> &str {
        &self.header.digest
    }

    pub fn write_to(&self, dir: &Path) -> Result<(), PackArchiveError> {
        for (path, bytes) in &self.assets {
            if !is_distributable_asset_path(path) {
                return Err(PackArchiveError::WriteRefused { path: path.clone() });
            }
            let target = dir.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|source| PackArchiveError::CreateDir {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::write(&target, bytes.as_slice()).map_err(|source| {
                PackArchiveError::Write {
                    path: target.clone(),
                    source,
                }
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
