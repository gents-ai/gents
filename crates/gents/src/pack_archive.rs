//! How a pack travels: as a `.pack` file (see [`format`]).
//!
//! A pack is a directory of documents, schemas, prompts and plugins with a
//! manifest on top. To publish it and install it elsewhere it becomes one
//! `.pack` file: a plain gzip-compressed tar any archive tool can list,
//! addressed by its pack digest.
//!
//! Two properties are the point.
//!
//! A pack installed from a file or a registry is the same pack as the one
//! compiled into a binary. The digest is [`crate::pack::digest_declared_assets`]
//! over the declared contents in every case, never over the container, so
//! neither the route a pack took nor its compression changes what it is.
//!
//! And a pack's plugins travel inside it. A plugin's compiled `.afb` is a
//! declared asset, so the pack digest covers it: an artifact cannot be
//! swapped underneath the name it was admitted under without changing the
//! pack.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use anyhow::{ensure, Context, Result};

use crate::pack::{is_distributable_asset_path, PackManifest, PackPlugin};

pub mod format;
pub use format::{
    digest_hex, pack_dir, read_pack, write_pack, Bounds, PackHeader, VerifiedPack, EXTENSION,
    FORMAT, FORMAT_VERSION, HEADER_ENTRY, MEDIA_TYPE,
};

/// The namespace a pack is published under when its manifest names none.
pub const DEFAULT_NAMESPACE: &str = "gents";

/// Hard cap on a pack's compressed size. A bundled pack today tops out
/// under a megabyte (the largest, `grok_tui_port`, carries 139 assets in
/// under a megabyte); a plugin's compiled `.afb` is usually a WASI command
/// module of a few hundred kilobytes, and at the top end a Python plugin's
/// self-contained pyodide bundle runs to a few megabytes, so 64 MiB is
/// generous headroom, not a tight fit, and is checked before a single byte
/// is decompressed.
pub const MAX_PACK_BYTES: usize = 64 * 1024 * 1024;

/// Hard cap on total decompressed bytes, enforced by counting bytes as they
/// come out of the decoder rather than trusted from a header: gzip's own
/// trailer records the uncompressed size mod 2^32, which a crafted stream
/// can make lie, and a tar entry's declared size is just as easy to forge.
/// Matches `afterburner_afb`'s own `MAX_DECOMPRESSED_BYTES` for the
/// identical reason (zip-bomb defense at the same order of magnitude).
pub const MAX_DECOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

/// Hard cap on the number of tar entries. A pack bundles a manifest, docs,
/// schemas and plugin artifacts, not an operator's whole workspace; the
/// largest bundled pack today carries 139. Four figures is headroom, not a
/// design target, and it bounds the cost of the entry-count check itself.
pub const MAX_ENTRIES: usize = 4096;

/// A pack read out of a `.pack` and verified.
///
/// Held in memory: every consumer wants random access to declared assets,
/// and the bounds in [`format::Bounds`] cap what that costs. The store
/// verifies without holding assets at all.
pub struct PackArchive {
    header: PackHeader,
    manifest: PackManifest,
    assets: BTreeMap<String, Vec<u8>>,
}

impl std::fmt::Debug for PackArchive {
    /// Names the pack and what it carries, never the bytes.
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
    /// Reads and verifies a `.pack` held in memory. Every check is
    /// [`read_pack`]'s, so a pack is admitted by the same rules whichever
    /// way it arrived.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Self::from_reader(bytes, Bounds::default())
    }

    /// The same, from any reader and under explicit bounds.
    pub fn from_reader(input: impl Read, bounds: Bounds) -> Result<Self> {
        let mut assets = BTreeMap::new();
        let verified = read_pack(input, bounds, |path, content| {
            let mut data = Vec::new();
            content.read_to_end(&mut data)?;
            assets.insert(path.to_owned(), data);
            Ok(())
        })?;
        assets.insert("manifest.json".to_owned(), verified.manifest_bytes);
        Ok(Self {
            header: verified.header,
            manifest: verified.manifest,
            assets,
        })
    }

    /// What the file claimed to be, now verified.
    pub fn header(&self) -> &PackHeader {
        &self.header
    }

    pub fn manifest(&self) -> &PackManifest {
        &self.manifest
    }

    /// The plugins this pack ships.
    pub fn plugins(&self) -> &[PackPlugin] {
        &self.manifest.metadata.plugins
    }

    /// One plugin's declaration, by the name it was declared under.
    pub fn plugin(&self, name: &str) -> Result<&PackPlugin> {
        self.manifest
            .metadata
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .with_context(|| format!("this pack ships no plugin called {name:?}"))
    }

    /// One plugin's compiled `.afb`, by the name it was declared under.
    pub fn plugin_artifact(&self, name: &str) -> Result<&[u8]> {
        let plugin = self.plugin(name)?;
        self.asset(&plugin.artifact)
    }

    /// One asset's bytes. `manifest.json` is addressable like any other.
    pub fn asset(&self, path: &str) -> Result<&[u8]> {
        self.assets
            .get(path)
            .map(Vec::as_slice)
            .with_context(|| format!("this pack carries no asset {path:?}"))
    }

    /// The pack's identity, `sha256:{hex}`: the digest it would have if it
    /// had been compiled into a binary instead.
    pub fn digest(&self) -> &str {
        &self.header.digest
    }

    /// Writes the pack out as a directory, creating parents as needed.
    ///
    /// Paths were checked when the pack was read; the check is repeated
    /// because a caller must not be able to write outside the directory it
    /// named, whatever it managed to construct.
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
            std::fs::write(&target, bytes)
                .with_context(|| format!("writing {}", target.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
