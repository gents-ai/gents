//! How a pack travels: as a gzip-compressed tar.
//!
//! A pack used to exist only compiled into this binary, so `gents pack
//! list` could show nothing that did not ship with the build. To publish a
//! pack and install it somewhere else it has to become bytes, and those
//! bytes have to be checkable by whoever receives them.
//!
//! The container is the plain one, not Afterburner's own `.afb`: a pack is
//! a directory of documents, schemas, and prompts with a manifest on top,
//! not a program with an entry point, so it needs nothing an `.afb`
//! provides that a tar does not. A `.tar.gz` holds:
//!
//! ```text
//! manifest.json                     the pack manifest
//! <asset>                           every asset the manifest declares,
//!                                   including each plugin's compiled
//!                                   artifact under plugins/
//! ```
//!
//! A plugin is different: it is a complete, first-class Afterburner `.afb`,
//! publishable and installable on its own, and also carried inside a pack
//! as one of its declared assets (see [`crate::plugin`] for how one runs).
//! Nesting an `.afb` inside a `.tar.gz` costs nothing extra here, because
//! this file never looks inside a plugin's bytes - it just carries them
//! like any other declared asset.
//!
//! Two properties are the point.
//!
//! A pack installed from a registry is the same pack as the one compiled
//! into a binary. The digest is [`crate::pack::digest_declared_assets`]
//! over the declared contents in both cases, never over the container, so
//! neither the route a pack took nor the compression it arrived under can
//! change what it is.
//!
//! And a pack's plugins travel inside it. A plugin's compiled artifact is a
//! declared asset, so the pack's own digest covers it: an artifact cannot
//! be swapped underneath the name it was admitted under without changing
//! the pack.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{ensure, Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::pack::{
    declared_paths, is_distributable_asset_path, validate_manifest, PackManifest, PackPlugin,
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

/// A pack read out of a `.tar.gz`.
///
/// Held in memory rather than streamed: a pack is small by this module's
/// own bounds, every consumer wants random access to declared assets, and
/// the alternative is a temporary directory the caller has to clean up.
pub struct PackArchive {
    manifest: PackManifest,
    assets: BTreeMap<String, Vec<u8>>,
    /// SHA-256 of the exact `.tar.gz` bytes this was parsed from.
    artifact_digest: [u8; 32],
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
    /// Reads and fully validates a pack `.tar.gz`.
    ///
    /// Hostile-input safe: the compressed size is checked before anything
    /// is decompressed, the decompressed byte count and entry count are
    /// bounded while streaming rather than trusted from a header, every
    /// member path is checked against the same rule a distributable asset
    /// has to satisfy everywhere else (no escape, no symlink, no
    /// non-regular entry), and duplicate members are refused. Once the
    /// bytes are in, the pack's own contract is checked: the manifest is
    /// held to exactly the rules a bundled pack is held to, every declared
    /// asset is present, and no member is present that the manifest did
    /// not declare.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Self::from_bytes_bounded(bytes, MAX_PACK_BYTES, MAX_DECOMPRESSED_BYTES, MAX_ENTRIES)
    }

    /// The same, with the size/count bounds passed explicitly instead of
    /// this module's own constants - the seam the bound-enforcement tests
    /// use, so they can prove a cap actually fires against a small fixture
    /// instead of needing a multi-hundred-megabyte one to reach the real
    /// limit.
    fn from_bytes_bounded(
        bytes: &[u8],
        max_compressed: usize,
        max_decompressed: u64,
        max_entries: usize,
    ) -> Result<Self> {
        ensure!(
            bytes.len() <= max_compressed,
            "pack is {} bytes, over the {} byte compressed bound",
            bytes.len(),
            max_compressed
        );

        let gz = GzDecoder::new(bytes);
        let bounded = BoundedReader::new(gz, max_decompressed);
        let mut archive = tar::Archive::new(bounded);

        let mut assets: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut entry_count = 0usize;
        for entry in archive.entries().context("reading the pack tar stream")? {
            let mut entry = entry.context("reading a pack tar entry")?;
            entry_count += 1;
            ensure!(
                entry_count <= max_entries,
                "pack carries more than {max_entries} entries"
            );
            ensure!(
                entry.header().entry_type().is_file(),
                "pack member {:?} is not a regular file",
                entry.path().ok().map(|p| p.display().to_string())
            );
            let path = entry
                .path()
                .context("reading a pack member's path")?
                .to_str()
                .context("a pack member path is not valid UTF-8")?
                .to_owned();
            ensure!(
                is_distributable_asset_path(&path),
                "pack member {path:?} is not a path a pack may carry"
            );
            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .with_context(|| format!("reading pack member {path:?}"))?;
            ensure!(
                assets.insert(path.clone(), data).is_none(),
                "pack carries {path:?} twice"
            );
        }

        let manifest_bytes = assets
            .remove("manifest.json")
            .context("this archive carries no manifest.json, so it is not a pack")?;
        let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
            .context("the pack manifest is not valid JSON")?;
        validate_manifest(&manifest.name.clone(), &manifest)?;

        for path in &manifest.metadata.assets {
            ensure!(
                assets.contains_key(path),
                "the pack declares {path:?} and does not carry it"
            );
        }
        // The manifest is the whole description of a pack. A member it
        // does not declare would be invisible to every check the pack
        // system runs and to the pack's own digest, so it is a refusal
        // rather than something to ignore.
        let declared = declared_paths(&manifest);
        for path in assets.keys() {
            ensure!(
                declared.iter().any(|known| known == path),
                "the pack carries {path:?}, which its manifest does not declare"
            );
        }
        assets.insert("manifest.json".to_owned(), manifest_bytes);

        Ok(Self {
            manifest,
            assets,
            artifact_digest: sha256(bytes),
        })
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

    /// The pack's identity: the digest this pack would have if it had been
    /// compiled into a binary instead of published.
    pub fn digest(&self) -> Result<String> {
        crate::pack::digest_declared_assets(&self.manifest, |path| self.asset(path))
    }

    /// SHA-256 of the exact `.tar.gz` bytes this was read from. What a
    /// registry addresses the artifact by, and what a download is checked
    /// against before it is opened.
    pub fn artifact_digest(&self) -> String {
        hex32(self.artifact_digest)
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

/// A [`Read`] that errors the moment more than `limit` bytes have come out
/// of it, so a decompressed stream (whose true size a gzip trailer's
/// 32-bit, attacker-controlled ISIZE cannot be trusted to report) can never
/// make [`PackArchive::from_bytes`] spend unbounded memory before this file
/// gets a chance to refuse it.
struct BoundedReader<R> {
    inner: R,
    limit: u64,
    read: u64,
}

impl<R> BoundedReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            limit,
            read: 0,
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        if self.read > self.limit {
            return Err(std::io::Error::other(format!(
                "pack decompresses to more than {} decompressed bytes",
                self.limit
            )));
        }
        Ok(n)
    }
}

/// Packs a pack directory into `.tar.gz` bytes, returning them with the
/// artifact's own digest.
///
/// The manifest decides what travels: an asset it declares and the
/// directory does not have is an error, and a file the directory has and
/// the manifest does not declare is simply not packed. What ships is
/// exactly what the pack says it is, which is also what its digest covers.
/// Entries are written via [`crate::pack::declared_paths`] - the same
/// sorted, deduplicated path list the pack's own digest is computed over -
/// so packing the same directory twice, on any machine, gives byte-identical
/// output: every tar header pins `mtime`/`uid`/`gid`/`mode`, and gzip's own
/// header carries no filename, comment, or timestamp either.
///
/// Compiling a pack's plugins is not this function's job. It packs what is
/// on disk, so an artifact a plugin declares must already be built; `gents
/// pack build` is what compiles first and then calls this.
pub fn pack_dir(dir: &Path) -> Result<(Vec<u8>, String)> {
    let manifest_path = dir.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    validate_manifest(&manifest.name.clone(), &manifest)?;

    let mut builder = tar::Builder::new(Vec::new());
    for path in declared_paths(&manifest) {
        let bytes = if path == "manifest.json" {
            manifest_bytes.clone()
        } else {
            let source = dir.join(&path);
            std::fs::read(&source).with_context(|| {
                format!(
                    "the pack declares {path:?} and {} is unreadable; a plugin's artifact has to \
                     be built before the pack is packed",
                    source.display()
                )
            })?
        };
        append_entry(&mut builder, &path, &bytes)?;
    }
    let tar_bytes = builder
        .into_inner()
        .context("finishing the pack tar stream")?;

    let mut gz = GzEncoder::new(Vec::new(), Compression::best());
    gz.write_all(&tar_bytes).context("compressing the pack")?;
    let bytes = gz.finish().context("finishing pack compression")?;
    let digest = hex32(sha256(&bytes));

    Ok((bytes, digest))
}

/// Appends one member with every reproducibility-affecting header field
/// pinned: fixed mode, zero mtime/uid/gid, and a plain regular-file type,
/// so nothing about the machine or moment a pack was built leaks into its
/// bytes.
fn append_entry(builder: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Regular);
    builder
        .append_data(&mut header, path, data)
        .with_context(|| format!("writing pack member {path:?}"))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}

fn hex32(bytes: [u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes one of this build's own bundled packs out as a directory, so
    /// the tests work on a real pack rather than a fixture that could
    /// drift from what a pack actually looks like.
    fn bundled_pack_dir(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let pack = crate::pack::resolve_pack(name).expect("a bundled pack");
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(name);
        for path in declared_paths(&pack.manifest) {
            let target = root.join(&path);
            std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
            std::fs::write(&target, pack.asset(&path).expect("asset")).expect("write");
        }
        (dir, root)
    }

    #[test]
    fn a_packed_pack_reads_back_as_the_same_pack() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, _) = pack_dir(&root).expect("packing");
        let packed = PackArchive::from_bytes(&bytes).expect("reading back");

        let bundled = crate::pack::resolve_pack("mailbox").expect("a bundled pack");
        assert_eq!(packed.manifest().name, bundled.manifest.name);
        assert_eq!(packed.manifest().version, bundled.manifest.version);
        for path in declared_paths(&bundled.manifest) {
            assert_eq!(
                packed.asset(&path).expect("packed asset"),
                bundled.asset(&path).expect("bundled asset"),
                "{path} differs"
            );
        }
    }

    #[test]
    fn a_pack_has_the_same_identity_however_it_arrived() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, _) = pack_dir(&root).expect("packing");
        let packed = PackArchive::from_bytes(&bytes).expect("reading");
        let bundled = crate::pack::resolve_pack("mailbox").expect("a bundled pack");
        assert_eq!(
            packed.digest().expect("packed digest"),
            bundled.digest,
            "a pack installed from a registry must be the same pack as the one compiled in"
        );
    }

    #[test]
    fn packing_twice_gives_the_same_bytes() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (first, first_digest) = pack_dir(&root).expect("first");
        let (second, second_digest) = pack_dir(&root).expect("second");
        assert_eq!(
            first, second,
            "a rebuild that changes nothing changes no bytes"
        );
        assert_eq!(first_digest, second_digest);
    }

    #[test]
    fn a_pack_is_a_tar_gz_any_archive_tool_can_read() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, digest) = pack_dir(&root).expect("packing");
        // Decoded with the same crates any other consumer would use, not
        // with anything of ours, so this is the check that a pack really
        // is a plain tar.gz without the registry learning a second format.
        let mut archive = tar::Archive::new(GzDecoder::new(bytes.as_slice()));
        let names: Vec<String> = archive
            .entries()
            .expect("entries")
            .map(|entry| {
                entry
                    .expect("entry")
                    .path()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert!(names.contains(&"manifest.json".to_owned()));
        let packed = PackArchive::from_bytes(&bytes).expect("and it is a pack");
        assert_eq!(
            packed.digest().unwrap(),
            crate::pack::resolve_pack("mailbox").unwrap().digest
        );
        assert_eq!(
            packed.artifact_digest(),
            digest,
            "pack_dir's returned digest is the same artifact digest PackArchive computes"
        );
    }

    #[test]
    fn an_archive_with_no_manifest_is_refused() {
        let mut builder = tar::Builder::new(Vec::new());
        append_entry(&mut builder, "README.md", b"# not a pack").expect("append");
        let tar_bytes = builder.into_inner().expect("tar bytes");
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tar_bytes).expect("write");
        let bytes = gz.finish().expect("finish");

        let error = PackArchive::from_bytes(&bytes).expect_err("must be refused");
        assert!(format!("{error:#}").contains("not a pack"), "{error:#}");
    }

    #[test]
    fn a_member_the_manifest_does_not_declare_is_refused() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        std::fs::write(root.join("stowaway.md"), b"not declared").expect("write");
        let (bytes, _) = pack_dir(&root).expect("packing");
        // Packing ignores it, because the manifest is the description.
        assert!(PackArchive::from_bytes(&bytes)
            .expect("reading")
            .asset("stowaway.md")
            .is_err());

        // And an archive built to carry it anyway is refused on the way in.
        let manifest_bytes = std::fs::read(root.join("manifest.json")).expect("manifest");
        let manifest: PackManifest = serde_json::from_slice(&manifest_bytes).expect("parse");
        let mut builder = tar::Builder::new(Vec::new());
        append_entry(&mut builder, "manifest.json", &manifest_bytes).expect("append");
        append_entry(&mut builder, "stowaway.md", b"not declared").expect("append");
        for path in &manifest.metadata.assets {
            append_entry(
                &mut builder,
                path,
                &std::fs::read(root.join(path)).expect("asset"),
            )
            .expect("append");
        }
        let tar_bytes = builder.into_inner().expect("tar bytes");
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tar_bytes).expect("write");
        let smuggled = gz.finish().expect("finish");

        let error = PackArchive::from_bytes(&smuggled).expect_err("must be refused");
        assert!(
            format!("{error:#}").contains("does not declare"),
            "{error:#}"
        );
    }

    #[test]
    fn a_missing_declared_asset_is_refused_when_the_pack_is_built() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let manifest: PackManifest =
            serde_json::from_slice(&std::fs::read(root.join("manifest.json")).expect("manifest"))
                .expect("parse");
        let dropped = manifest.metadata.assets[0].clone();
        std::fs::remove_file(root.join(&dropped)).expect("remove");
        let error = pack_dir(&root).expect_err("must be refused");
        assert!(format!("{error:#}").contains(&dropped), "{error:#}");
    }

    #[test]
    fn a_path_that_escapes_the_pack_is_refused() {
        // `tar::Header::set_path` refuses a `..` component outright (its
        // own defense against the classic archive-extraction escape), so a
        // hostile entry has to be written at the raw-byte level to prove
        // this file's own, independent check refuses it too.
        let mut header = tar::Header::new_ustar();
        header.set_size(4);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        let name = b"../escape.txt";
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_cksum();

        let mut builder = tar::Builder::new(Vec::new());
        append_entry(&mut builder, "manifest.json", b"{}").expect("append");
        builder
            .append(&header, &b"nope"[..])
            .expect("append escaping entry");
        let tar_bytes = builder.into_inner().expect("tar bytes");
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&tar_bytes).expect("write");
        let bytes = gz.finish().expect("finish");

        let error = PackArchive::from_bytes(&bytes).expect_err("must be refused");
        assert!(
            format!("{error:#}").contains("is not a path a pack may carry"),
            "{error:#}"
        );
    }

    #[test]
    fn a_pack_over_the_compressed_size_bound_is_refused() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, _) = pack_dir(&root).expect("packing");
        let error = PackArchive::from_bytes_bounded(
            &bytes,
            bytes.len() - 1,
            MAX_DECOMPRESSED_BYTES,
            MAX_ENTRIES,
        )
        .expect_err("must be refused");
        assert!(
            format!("{error:#}").contains("compressed bound"),
            "{error:#}"
        );
    }

    #[test]
    fn a_pack_over_the_decompressed_size_bound_is_refused() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, _) = pack_dir(&root).expect("packing");
        let error = PackArchive::from_bytes_bounded(&bytes, MAX_PACK_BYTES, 16, MAX_ENTRIES)
            .expect_err("must be refused");
        assert!(
            format!("{error:#}").contains("decompressed bytes"),
            "{error:#}"
        );
    }

    #[test]
    fn a_pack_over_the_entry_count_bound_is_refused() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, _) = pack_dir(&root).expect("packing");
        let error =
            PackArchive::from_bytes_bounded(&bytes, MAX_PACK_BYTES, MAX_DECOMPRESSED_BYTES, 1)
                .expect_err("must be refused");
        assert!(
            format!("{error:#}").contains("more than 1 entries"),
            "{error:#}"
        );
    }

    #[test]
    fn writing_a_pack_out_reproduces_the_directory_it_came_from() {
        let (_guard, root) = bundled_pack_dir("mailbox");
        let (bytes, digest) = pack_dir(&root).expect("packing");
        let packed = PackArchive::from_bytes(&bytes).expect("reading");
        let out = tempfile::tempdir().expect("tempdir");
        packed.write_to(out.path()).expect("writing out");
        let (again, again_digest) = pack_dir(out.path()).expect("repacking");
        assert_eq!(
            again, bytes,
            "a pack written to disk and packed again is the same pack"
        );
        assert_eq!(again_digest, digest);
    }

    #[test]
    fn a_plugin_pack_carries_its_artifact_and_asks_for_what_the_plugin_asks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("shipping_plugins");
        std::fs::create_dir_all(root.join("plugins")).expect("mkdir");
        std::fs::write(root.join("README.md"), b"# shipping plugins").expect("write");
        std::fs::write(root.join("plugins/format_check.afb"), b"not a real afb yet")
            .expect("write");
        let manifest = serde_json::json!({
            "manifest_version": 1,
            "name": "shipping_plugins",
            "version": "0.1.0",
            "description": "A pack that is nothing but capabilities",
            "authors": ["gents-ai contributors"],
            "tags": ["plugins"],
            "kind": "plugins",
            "assets": ["README.md", "plugins/format_check.afb"],
            "plugins": [{
                "name": "format_check",
                "description": "Checks formatting and says what is wrong",
                "artifact": "plugins/format_check.afb",
                "language": "rust",
                "input_schema": {"type": "object", "properties": {}},
                "manifold": {"fs": {"ReadOnly": ["/workspace"]}, "net": "None", "env": "None",
                             "crypto": false, "child_process": false, "allow_exit": false,
                             "http_timeout_ms": null}
            }]
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).expect("encode"),
        )
        .expect("write");

        let (bytes, _) = pack_dir(&root).expect("packing a plugins pack");
        let packed = PackArchive::from_bytes(&bytes).expect("reading it back");
        assert_eq!(packed.plugins().len(), 1);
        assert_eq!(packed.plugins()[0].name, "format_check");
        assert_eq!(
            packed
                .plugin_artifact("format_check")
                .expect("the artifact"),
            b"not a real afb yet",
            "a plugin's compiled artifact travels inside the pack"
        );
    }

    #[test]
    fn a_plugin_whose_artifact_is_not_a_declared_asset_is_refused() {
        let mut manifest: crate::pack::PackManifest = serde_json::from_value(serde_json::json!({
            "manifest_version": 1,
            "name": "shipping_plugins",
            "version": "0.1.0",
            "description": "A pack that is nothing but capabilities",
            "authors": ["gents-ai contributors"],
            "tags": ["plugins"],
            "kind": "plugins",
            "assets": ["README.md"],
            "plugins": [{
                "name": "format_check",
                "description": "Checks formatting",
                "artifact": "plugins/format_check.afb",
                "language": "rust",
                "input_schema": {"type": "object"}
            }]
        }))
        .expect("a manifest");
        let error = validate_manifest("shipping_plugins", &manifest).expect_err("must be refused");
        assert!(
            format!("{error:#}").contains("not its artifact"),
            "{error:#}"
        );

        manifest
            .metadata
            .assets
            .push("plugins/format_check.afb".to_owned());
        validate_manifest("shipping_plugins", &manifest).expect("and accepted once declared");
    }
}
