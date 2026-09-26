//! The `.pack` file, format version 1.
//!
//! A `.pack` is a gzip-compressed POSIX ustar that any archive tool can list.
//! Its first entry is `pack.json`, the [`PackHeader`] naming what the file
//! claims to be; every other entry is `manifest.json` or a declared asset, in
//! [`declared_paths`] order, which is the order the pack digest is computed
//! over. That ordering is what lets [`read_pack`] verify a pack in one pass
//! while holding one entry's buffer at a time: it hashes each entry as it
//! streams past and compares the result with the header at the end.
//!
//! Headers are pinned (regular files, mode `0644`, uid and gid `0`, mtime
//! `0`) and gzip carries no name or timestamp, so the same directory packs to
//! the same bytes with the same build on any machine.

use std::io::{self, BufRead, Read, Write};
use std::path::Path;

use anyhow::{ensure, Context, Result};
use flate2::bufread::GzDecoder;
use flate2::{Compression, GzBuilder};
use serde::{Deserialize, Serialize};

use crate::pack::{
    declared_paths, is_distributable_asset_path, validate_manifest, PackDigester, PackKind,
    PackManifest,
};

use super::{MAX_DECOMPRESSED_BYTES, MAX_ENTRIES, MAX_PACK_BYTES};

/// The `format` every `.pack` header carries.
pub const FORMAT: &str = "gents-pack";
/// The container version this build reads and writes.
pub const FORMAT_VERSION: u32 = 1;
/// The media type a `.pack` is served as.
pub const MEDIA_TYPE: &str = "application/vnd.gents.pack";
/// The file extension, without the dot.
pub const EXTENSION: &str = "pack";
/// The reserved first entry. It is not an asset and is not part of the digest.
pub const HEADER_ENTRY: &str = "pack.json";
/// A header is a handful of short strings; anything larger is not one.
const MAX_HEADER_BYTES: u64 = 64 * 1024;

/// What a `.pack` claims to be. Every field is checked against the contents
/// before the pack is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackHeader {
    pub format: String,
    pub format_version: u32,
    /// The pack digest, `sha256:{hex}`: the pack's identity.
    pub digest: String,
    /// `{namespace}/{name}`.
    pub coordinate: String,
    pub version: String,
    pub kind: PackKind,
}

impl PackHeader {
    fn describe(manifest: &PackManifest, digest: String) -> Self {
        Self {
            format: FORMAT.to_owned(),
            format_version: FORMAT_VERSION,
            digest,
            coordinate: format!("{}/{}", manifest.metadata.namespace, manifest.name),
            version: manifest.version.clone(),
            kind: manifest.metadata.kind.clone(),
        }
    }

    /// `{namespace}.{name}-{version}.pack`, for people. Never an identity.
    pub fn file_name(&self) -> String {
        format!(
            "{}-{}.{EXTENSION}",
            self.coordinate.replace('/', "."),
            self.version
        )
    }
}

/// The hex of a `sha256:{hex}` pack digest, refusing anything else.
pub fn digest_hex(digest: &str) -> Result<&str> {
    let hex = digest
        .strip_prefix("sha256:")
        .with_context(|| format!("{digest:?} is not a pack digest; expected sha256:<64 hex>"))?;
    ensure!(
        hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
        "{digest:?} is not a pack digest; expected sha256:<64 lowercase hex>"
    );
    Ok(hex)
}

/// Size and count limits a read or write is held to.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub compressed: u64,
    pub decompressed: u64,
    pub entries: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            compressed: MAX_PACK_BYTES as u64,
            decompressed: MAX_DECOMPRESSED_BYTES,
            entries: MAX_ENTRIES,
        }
    }
}

/// A pack that passed [`read_pack`].
#[derive(Debug)]
pub struct VerifiedPack {
    pub header: PackHeader,
    pub manifest: PackManifest,
    pub manifest_bytes: Vec<u8>,
}

/// Reads and verifies a `.pack` stream in one pass.
///
/// Refuses, before reading further, anything over `bounds`; an entry that is
/// not a regular file or whose path a pack may not carry; entries out of
/// digest order or repeated; a missing or unknown header; a manifest that
/// fails the rules a bundled pack is held to; any difference between the
/// entries and the manifest's declared paths; and a header whose digest,
/// coordinate, version or kind differ from the contents.
///
/// `asset` is offered each asset's content as it streams past; whatever it
/// leaves unread is still hashed. `manifest.json` is returned, not offered.
pub fn read_pack(
    input: impl Read,
    bounds: Bounds,
    mut asset: impl FnMut(&str, &mut dyn Read) -> Result<()>,
) -> Result<VerifiedPack> {
    let compressed = io::BufReader::new(BoundedReader::new(input, bounds.compressed, "compressed"));
    let decompressed = BoundedReader::new(
        GzDecoder::new(compressed),
        bounds.decompressed,
        "decompressed",
    );
    let mut archive = tar::Archive::new(decompressed);

    let mut header: Option<PackHeader> = None;
    let mut manifest_bytes: Option<Vec<u8>> = None;
    let mut paths: Vec<String> = Vec::new();
    let mut digest = PackDigester::default();
    let mut count = 0usize;
    for entry in archive.entries().context("reading the pack")? {
        let mut entry = entry.context("reading a pack entry")?;
        count += 1;
        ensure!(
            count <= bounds.entries,
            "pack carries more than {} entries",
            bounds.entries
        );
        ensure!(
            entry.header().entry_type().is_file(),
            "pack member {:?} is not a regular file",
            entry.path().ok().map(|path| path.display().to_string())
        );
        let path = entry
            .path()
            .context("reading a pack member's path")?
            .to_str()
            .context("a pack member path is not valid UTF-8")?
            .to_owned();

        if header.is_none() {
            ensure!(
                path == HEADER_ENTRY,
                "this file does not start with {HEADER_ENTRY}, so it is not a .{EXTENSION} file"
            );
            header = Some(read_header(&mut entry)?);
            continue;
        }
        ensure!(
            path != HEADER_ENTRY && is_distributable_asset_path(&path),
            "pack member {path:?} is not a path a pack may carry"
        );
        if let Some(previous) = paths.last() {
            ensure!(
                path.as_str() > previous.as_str(),
                "pack member {path:?} is out of order or repeated after {previous:?}"
            );
        }
        let len = entry
            .header()
            .size()
            .context("reading a pack member's size")?;
        digest.begin_entry(&path, len);
        let mut hashing = HashingReader {
            inner: &mut entry,
            digest: &mut digest,
        };
        if path == "manifest.json" {
            let mut bytes = Vec::new();
            hashing
                .read_to_end(&mut bytes)
                .context("reading manifest.json")?;
            manifest_bytes = Some(bytes);
        } else {
            asset(&path, &mut hashing).with_context(|| format!("reading pack member {path:?}"))?;
            io::copy(&mut hashing, &mut io::sink())
                .with_context(|| format!("reading pack member {path:?}"))?;
        }
        paths.push(path);
    }
    // Read to the end: the gzip trailer carries the CRC that proves the
    // stream intact, and a copy of the input must hold every byte of it.
    let mut decompressed = archive.into_inner();
    io::copy(&mut decompressed, &mut io::sink()).context("reading the end of the pack")?;
    let mut compressed = decompressed.inner.into_inner();
    ensure!(
        compressed
            .fill_buf()
            .context("reading the end of the pack")?
            .is_empty(),
        "the file has data after the end of the pack"
    );

    let header =
        header.with_context(|| format!("this file is empty, so it is not a .{EXTENSION} file"))?;
    let manifest_bytes =
        manifest_bytes.context("this pack carries no manifest.json, so it is not a pack")?;
    let manifest: PackManifest =
        serde_json::from_slice(&manifest_bytes).context("the pack manifest is not valid JSON")?;
    validate_manifest(&manifest.name.clone(), &manifest)?;

    let declared = declared_paths(&manifest);
    for path in &declared {
        ensure!(
            paths.binary_search(path).is_ok(),
            "the pack declares {path:?} and does not carry it"
        );
    }
    for path in &paths {
        ensure!(
            declared.binary_search(path).is_ok(),
            "the pack carries {path:?}, which its manifest does not declare"
        );
    }

    let computed = digest.finish();
    ensure!(
        computed == header.digest,
        "{HEADER_ENTRY} claims digest {} but the contents hash to {computed}",
        header.digest
    );
    let described = PackHeader::describe(&manifest, computed);
    ensure!(
        described == header,
        "{HEADER_ENTRY} describes {} {} ({:?}) but the manifest is {} {} ({:?})",
        header.coordinate,
        header.version,
        header.kind,
        described.coordinate,
        described.version,
        described.kind
    );
    Ok(VerifiedPack {
        header,
        manifest,
        manifest_bytes,
    })
}

fn read_header(entry: &mut impl Read) -> Result<PackHeader> {
    let mut bytes = Vec::new();
    entry
        .take(MAX_HEADER_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {HEADER_ENTRY}"))?;
    ensure!(
        bytes.len() as u64 <= MAX_HEADER_BYTES,
        "{HEADER_ENTRY} is larger than {MAX_HEADER_BYTES} bytes"
    );
    // The version is read before the rest, so a newer file is named as such
    // rather than reported as a malformed one.
    #[derive(Deserialize)]
    struct Probe {
        format: Option<String>,
        format_version: Option<u32>,
    }
    let probe: Probe = serde_json::from_slice(&bytes)
        .with_context(|| format!("{HEADER_ENTRY} is not valid JSON"))?;
    ensure!(
        probe.format.as_deref() == Some(FORMAT),
        "{HEADER_ENTRY} does not describe a gents pack"
    );
    let version = probe.format_version.unwrap_or_default();
    ensure!(
        version == FORMAT_VERSION,
        "this pack uses format version {version}; this gents reads version {FORMAT_VERSION}, \
         so update gents to install it"
    );
    serde_json::from_slice(&bytes).with_context(|| format!("{HEADER_ENTRY} is malformed"))
}

/// Packs the directory `dir` into `out` as a `.pack`, returning its header.
///
/// The manifest decides what travels: a declared asset the directory lacks is
/// an error, and an undeclared file is not packed. Files are streamed twice,
/// once to compute the digest the header must carry and once to write them,
/// so memory stays one buffer regardless of the pack's size; the second pass
/// is hashed again, so a file changed between the passes fails the build
/// instead of producing a pack whose header lies.
pub fn write_pack(dir: &Path, out: impl Write) -> Result<PackHeader> {
    write_pack_bounded(dir, out, Bounds::default())
}

fn write_pack_bounded(dir: &Path, out: impl Write, bounds: Bounds) -> Result<PackHeader> {
    let manifest_path = dir.join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: PackManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    validate_manifest(&manifest.name.clone(), &manifest)?;
    let paths = declared_paths(&manifest);
    ensure!(
        paths.len() < bounds.entries,
        "the pack declares {} files, over the {} entry bound",
        paths.len(),
        bounds.entries
    );

    let mut total = 0u64;
    let mut digest = PackDigester::default();
    for path in &paths {
        let (mut file, len) = open_declared(dir, path)?;
        total += len;
        ensure!(
            total <= bounds.decompressed,
            "the pack's files exceed the {} byte bound",
            bounds.decompressed
        );
        digest.begin_entry(path, len);
        copy_exact(&mut file, len, &mut DigestWriter(&mut digest), path)?;
    }
    let header = PackHeader::describe(&manifest, digest.finish());
    let header_bytes = serde_json::to_vec(&header).context("encoding pack.json")?;

    let counted = CountingWriter {
        inner: out,
        written: 0,
    };
    let gz = GzBuilder::new()
        .mtime(0)
        .write(counted, Compression::default());
    let mut builder = tar::Builder::new(gz);
    append_entry(
        &mut builder,
        HEADER_ENTRY,
        header_bytes.len() as u64,
        header_bytes.as_slice(),
    )?;
    let mut again = PackDigester::default();
    for path in &paths {
        let (file, len) = open_declared(dir, path)?;
        again.begin_entry(path, len);
        let hashing = HashingReader {
            inner: file.take(len),
            digest: &mut again,
        };
        append_entry(&mut builder, path, len, hashing)?;
    }
    ensure!(
        again.finish() == header.digest,
        "a file in {} changed while the pack was being written; build it again",
        dir.display()
    );
    let counted = builder
        .into_inner()
        .context("finishing the pack")?
        .finish()
        .context("finishing pack compression")?;
    ensure!(
        counted.written <= bounds.compressed,
        "the pack is {} bytes compressed, over the {} byte bound",
        counted.written,
        bounds.compressed
    );
    Ok(header)
}

/// Packs `dir` in memory. For small packs and tests; [`write_pack`] streams.
pub fn pack_dir(dir: &Path) -> Result<(Vec<u8>, PackHeader)> {
    let mut bytes = Vec::new();
    let header = write_pack(dir, &mut bytes)?;
    Ok((bytes, header))
}

fn open_declared(dir: &Path, path: &str) -> Result<(std::fs::File, u64)> {
    let source = dir.join(path);
    let file = std::fs::File::open(&source).with_context(|| {
        format!(
            "the pack declares {path:?} and {} is unreadable; a plugin's artifact has to be \
             built before the pack is packed",
            source.display()
        )
    })?;
    let metadata = file
        .metadata()
        .with_context(|| format!("reading {}", source.display()))?;
    ensure!(
        metadata.is_file(),
        "the pack declares {path:?} and {} is not a regular file",
        source.display()
    );
    Ok((file, metadata.len()))
}

fn copy_exact(from: &mut impl Read, len: u64, to: &mut impl Write, path: &str) -> Result<()> {
    let copied = io::copy(&mut from.take(len), to).with_context(|| format!("reading {path:?}"))?;
    ensure!(
        copied == len,
        "{path:?} changed while the pack was being written; build it again"
    );
    Ok(())
}

pub(super) fn append_entry(
    builder: &mut tar::Builder<impl Write>,
    path: &str,
    len: u64,
    data: impl Read,
) -> Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_size(len);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Regular);
    builder
        .append_data(&mut header, path, data)
        .with_context(|| format!("writing pack member {path:?}"))
}

struct HashingReader<'a, R> {
    inner: R,
    digest: &'a mut PackDigester,
}

impl<R: Read> Read for HashingReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.digest.update(&buf[..n]);
        Ok(n)
    }
}

struct DigestWriter<'a>(&'a mut PackDigester);

impl Write for DigestWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct CountingWriter<W> {
    inner: W,
    written: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A [`Read`] that fails the moment more than `limit` bytes come out of it,
/// so neither a large file nor a gzip stream whose trailer lies about its
/// size can make a read spend unbounded memory or time.
struct BoundedReader<R> {
    inner: R,
    limit: u64,
    read: u64,
    what: &'static str,
}

impl<R> BoundedReader<R> {
    fn new(inner: R, limit: u64, what: &'static str) -> Self {
        Self {
            inner,
            limit,
            read: 0,
            what,
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        if self.read > self.limit {
            return Err(io::Error::other(format!(
                "pack is more than {} {} bytes",
                self.limit, self.what
            )));
        }
        Ok(n)
    }
}

#[cfg(test)]
pub(super) fn write_pack_with(dir: &Path, bounds: Bounds) -> Result<(Vec<u8>, PackHeader)> {
    let mut bytes = Vec::new();
    let header = write_pack_bounded(dir, &mut bytes, bounds)?;
    Ok((bytes, header))
}
