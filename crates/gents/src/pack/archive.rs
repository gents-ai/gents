use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use anyhow::{ensure, Context, Result};

use crate::pack::{is_distributable_asset_path, PackManifest, PackPlugin};

pub mod format;
pub use format::{
    digest_hex, pack_dir, peek_header, read_pack, write_pack, Bounds, PackHeader, VerifiedPack,
    EXTENSION, FORMAT, FORMAT_VERSION, HEADER_ENTRY, MEDIA_TYPE,
};

pub const DEFAULT_NAMESPACE: &str = "gents";

/// Caps compressed archive input before parsing it into retained assets.
pub const MAX_PACK_BYTES: usize = 1024 * 1024 * 1024;

/// Caps total expanded asset bytes to bound decompression work and storage.
pub const MAX_DECOMPRESSED_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Caps archive entry count to bound per-entry parsing and metadata overhead.
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
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Self::from_reader(bytes, Bounds::default())
    }

    pub fn from_reader(input: impl Read, bounds: Bounds) -> Result<Self> {
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

    pub fn from_unpacked(dir: &Path, header: PackHeader, manifest: PackManifest) -> Result<Self> {
        let mut assets = BTreeMap::new();
        for path in crate::pack::declared_paths(&manifest) {
            ensure!(
                is_distributable_asset_path(&path),
                "refusing to read pack asset {path:?}"
            );
            let file = std::fs::File::open(dir.join(&path))
                .with_context(|| format!("the unpacked pack is missing {path}"))?;
            let bytes = if file.metadata()?.len() == 0 {
                AssetBytes::Empty
            } else {
                // SAFETY: the store writes an unpacked pack once, into a staging
                // directory renamed into place under its digest, and never
                // writes to it again, so the mapped file does not change.
                AssetBytes::Mapped(
                    unsafe { memmap2::Mmap::map(&file) }
                        .with_context(|| format!("mapping {}", dir.join(&path).display()))?,
                )
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

    pub fn plugin(&self, name: &str) -> Result<&PackPlugin> {
        self.manifest
            .metadata
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .with_context(|| format!("this pack ships no plugin called {name:?}"))
    }

    pub fn plugin_artifact(&self, name: &str) -> Result<&[u8]> {
        let plugin = self.plugin(name)?;
        self.asset(&plugin.artifact)
    }

    pub fn asset(&self, path: &str) -> Result<&[u8]> {
        self.assets
            .get(path)
            .map(AssetBytes::as_slice)
            .with_context(|| format!("this pack carries no asset {path:?}"))
    }

    pub fn digest(&self) -> &str {
        &self.header.digest
    }

    pub fn write_to(&self, dir: &Path) -> Result<()> {
        for (path, bytes) in &self.assets {
            ensure!(
                is_distributable_asset_path(path),
                "refusing to write pack asset {path:?}"
            );
            let target = dir.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::write(&target, bytes.as_slice())
                .with_context(|| format!("writing {}", target.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
