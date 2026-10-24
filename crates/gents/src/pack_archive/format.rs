use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use flate2::bufread::GzDecoder;
use flate2::{Compression, GzBuilder};
use serde::{Deserialize, Serialize};

use crate::pack::{
    declared_paths, is_distributable_asset_path, validate_manifest, PackDigester, PackKind,
    PackManifest,
};

use super::{MAX_DECOMPRESSED_BYTES, MAX_ENTRIES, MAX_PACK_BYTES};

pub const FORMAT: &str = "gents-pack";
pub const FORMAT_VERSION: u32 = 1;
pub const MEDIA_TYPE: &str = "application/vnd.gents.pack";
pub const EXTENSION: &str = "pack";
pub const HEADER_ENTRY: &str = "pack.json";
const MAX_HEADER_BYTES: u64 = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PackFormatError {
    #[error("{digest:?} is not a pack digest; expected sha256:<64 hex>")]
    DigestPrefix { digest: String },
    #[error("{digest:?} is not a pack digest; expected sha256:<64 lowercase hex>")]
    DigestHex { digest: String },
    #[error("reading the pack")]
    ReadEntries(#[source] io::Error),
    #[error("reading a pack entry")]
    ReadEntry(#[source] io::Error),
    #[error("pack carries more than {limit} entries")]
    TooManyEntries { limit: usize },
    #[error("this file does not start with {HEADER_ENTRY}, so it is not a .{EXTENSION} file")]
    NotPackFile,
    #[error("this file is empty, so it is not a .{EXTENSION} file")]
    EmptyPack,
    #[error("pack member {path:?} is not a regular file")]
    MemberNotRegular { path: Option<String> },
    #[error("reading a pack member's path")]
    MemberPath(#[source] io::Error),
    #[error("a pack member path is not valid UTF-8")]
    MemberPathNotUtf8,
    #[error("reading a pack member's size")]
    MemberSize(#[source] io::Error),
    #[error("pack member {path:?} is not a path a pack may carry")]
    MemberNotDistributable { path: String },
    #[error("pack member {path:?} is out of order or repeated after {previous:?}")]
    MemberOutOfOrder { path: String, previous: String },
    #[error("reading {HEADER_ENTRY}")]
    ReadHeader(#[source] io::Error),
    #[error("{HEADER_ENTRY} is larger than {MAX_HEADER_BYTES} bytes")]
    HeaderTooLarge,
    #[error("{HEADER_ENTRY} is not valid JSON")]
    HeaderNotJson(#[source] serde_json::Error),
    #[error("{HEADER_ENTRY} does not describe a gents pack")]
    HeaderNotGents,
    #[error("this pack uses format version {version}; this gents reads version {FORMAT_VERSION}, so update gents to install it")]
    FormatVersion { version: u32 },
    #[error("{HEADER_ENTRY} is malformed")]
    HeaderMalformed(#[source] serde_json::Error),
    #[error("the pack declares {path:?} and it is unreadable; a plugin's artifact has to be built before the pack is packed")]
    DeclaredUnreadable {
        path: String,
        #[source]
        source: crate::pack::PackAssetError,
    },
    #[error("reading declared pack asset {path:?}")]
    DeclaredMetadata {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("the pack declares {path:?} and it is not a regular file")]
    DeclaredNotRegular { path: String },
    #[error("reading {path:?}")]
    CopyRead {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("{path:?} changed while the pack was being written; build it again")]
    CopyChanged { path: String },
    #[error("writing pack member {path:?}")]
    AppendMember {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("reading manifest.json")]
    ReadManifest(#[source] io::Error),
    #[error("reading the end of the pack")]
    ReadEnd(#[source] io::Error),
    #[error("the file has data after the end of the pack")]
    TrailingData,
    #[error("this pack carries no manifest.json, so it is not a pack")]
    NoManifest,
    #[error("the pack manifest is not valid JSON")]
    ManifestNotJson(#[source] serde_json::Error),
    #[error("the pack declares {path:?} and does not carry it")]
    DeclaredMissing { path: String },
    #[error("the pack carries {path:?}, which its manifest does not declare")]
    UndeclaredMember { path: String },
    #[error("reading pack member {path:?}")]
    ReadMember {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("{HEADER_ENTRY} claims digest {claimed} but the contents hash to {computed}")]
    DigestMismatch { claimed: String, computed: String },
    #[error(
        "{HEADER_ENTRY} describes {} {} ({:?}) but the manifest is {} {} ({:?})",
        .header.coordinate,
        .header.version,
        .header.kind,
        .described.coordinate,
        .described.version,
        .described.kind
    )]
    HeaderMismatch {
        header: Box<PackHeader>,
        described: Box<PackHeader>,
    },
    #[error("parsing manifest.json")]
    ParseManifest(#[source] serde_json::Error),
    #[error("the pack declares {count} files, over the {limit} entry bound")]
    TooManyDeclared { count: usize, limit: usize },
    #[error("the pack's files exceed the {limit} byte bound")]
    TooLarge { limit: u64 },
    #[error("a file in {} changed while the pack was being written; build it again", dir.display())]
    SourceChanged { dir: PathBuf },
    #[error("finishing the pack")]
    FinishPack(#[source] io::Error),
    #[error("finishing pack compression")]
    FinishCompression(#[source] io::Error),
    #[error("encoding pack.json")]
    EncodeHeader(#[source] serde_json::Error),
    #[error("the pack is {written} bytes compressed, over the {limit} byte bound")]
    TooCompressed { written: u64, limit: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackHeader {
    pub format: String,
    pub format_version: u32,
    pub digest: String,
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

    pub fn file_name(&self) -> String {
        format!(
            "{}-{}.{EXTENSION}",
            self.coordinate.replace('/', "."),
            self.version
        )
    }
}

pub fn digest_hex(digest: &str) -> Result<&str, PackFormatError> {
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| PackFormatError::DigestPrefix {
            digest: digest.to_owned(),
        })?;
    if !(hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))) {
        return Err(PackFormatError::DigestHex {
            digest: digest.to_owned(),
        });
    }
    Ok(hex)
}

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

#[derive(Debug)]
pub struct VerifiedPack {
    pub header: PackHeader,
    pub manifest: PackManifest,
    pub manifest_bytes: Vec<u8>,
}

pub fn read_pack(
    input: impl Read,
    bounds: Bounds,
    mut asset: impl FnMut(&str, &mut dyn Read) -> io::Result<()>,
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
    for entry in archive.entries().map_err(PackFormatError::ReadEntries)? {
        let mut entry = entry.map_err(PackFormatError::ReadEntry)?;
        count += 1;
        if count > bounds.entries {
            return Err(PackFormatError::TooManyEntries {
                limit: bounds.entries,
            }
            .into());
        }
        if !entry.header().entry_type().is_file() {
            return Err(PackFormatError::MemberNotRegular {
                path: entry.path().ok().map(|path| path.display().to_string()),
            }
            .into());
        }
        let path = entry
            .path()
            .map_err(PackFormatError::MemberPath)?
            .to_str()
            .ok_or(PackFormatError::MemberPathNotUtf8)?
            .to_owned();

        if header.is_none() {
            if path != HEADER_ENTRY {
                return Err(PackFormatError::NotPackFile.into());
            }
            header = Some(read_header(&mut entry)?);
            continue;
        }
        if path == HEADER_ENTRY || !is_distributable_asset_path(&path) {
            return Err(PackFormatError::MemberNotDistributable { path }.into());
        }
        if let Some(previous) = paths.last() {
            if path.as_str() <= previous.as_str() {
                return Err(PackFormatError::MemberOutOfOrder {
                    path,
                    previous: previous.clone(),
                }
                .into());
            }
        }
        let len = entry.header().size().map_err(PackFormatError::MemberSize)?;
        digest.begin_entry(&path, len);
        let mut hashing = HashingReader {
            inner: &mut entry,
            digest: &mut digest,
        };
        if path == "manifest.json" {
            let mut bytes = Vec::new();
            hashing
                .read_to_end(&mut bytes)
                .map_err(PackFormatError::ReadManifest)?;
            manifest_bytes = Some(bytes);
        } else {
            asset(&path, &mut hashing).map_err(|source| PackFormatError::ReadMember {
                path: path.clone(),
                source,
            })?;
            io::copy(&mut hashing, &mut io::sink()).map_err(|source| {
                PackFormatError::ReadMember {
                    path: path.clone(),
                    source,
                }
            })?;
        }
        paths.push(path);
    }
    let mut decompressed = archive.into_inner();
    io::copy(&mut decompressed, &mut io::sink()).map_err(PackFormatError::ReadEnd)?;
    let mut compressed = decompressed.inner.into_inner();
    if !compressed
        .fill_buf()
        .map_err(PackFormatError::ReadEnd)?
        .is_empty()
    {
        return Err(PackFormatError::TrailingData.into());
    }

    let header = header.ok_or(PackFormatError::EmptyPack)?;
    let manifest_bytes = manifest_bytes.ok_or(PackFormatError::NoManifest)?;
    let manifest: PackManifest =
        serde_json::from_slice(&manifest_bytes).map_err(PackFormatError::ManifestNotJson)?;
    validate_manifest(&manifest.name.clone(), &manifest)?;

    let declared = declared_paths(&manifest);
    for path in &declared {
        if paths.binary_search(path).is_err() {
            return Err(PackFormatError::DeclaredMissing { path: path.clone() }.into());
        }
    }
    for path in &paths {
        if declared.binary_search(path).is_err() {
            return Err(PackFormatError::UndeclaredMember { path: path.clone() }.into());
        }
    }

    let computed = digest.finish();
    if computed != header.digest {
        return Err(PackFormatError::DigestMismatch {
            claimed: header.digest.clone(),
            computed,
        }
        .into());
    }
    let described = PackHeader::describe(&manifest, computed);
    if described != header {
        return Err(PackFormatError::HeaderMismatch {
            header: Box::new(header),
            described: Box::new(described),
        }
        .into());
    }
    Ok(VerifiedPack {
        header,
        manifest,
        manifest_bytes,
    })
}

pub fn peek_header(input: impl Read, bounds: Bounds) -> Result<PackHeader, PackFormatError> {
    let compressed = io::BufReader::new(BoundedReader::new(input, bounds.compressed, "compressed"));
    let decompressed = BoundedReader::new(
        GzDecoder::new(compressed),
        bounds.decompressed,
        "decompressed",
    );
    let mut archive = tar::Archive::new(decompressed);
    let mut entries = archive.entries().map_err(PackFormatError::ReadEntries)?;
    let mut entry = entries
        .next()
        .ok_or(PackFormatError::EmptyPack)?
        .map_err(PackFormatError::ReadEntry)?;
    let path = entry
        .path()
        .map_err(PackFormatError::MemberPath)?
        .to_str()
        .ok_or(PackFormatError::MemberPathNotUtf8)?
        .to_owned();
    if path != HEADER_ENTRY {
        return Err(PackFormatError::NotPackFile);
    }
    read_header(&mut entry)
}

fn read_header(entry: &mut impl Read) -> Result<PackHeader, PackFormatError> {
    let mut bytes = Vec::new();
    entry
        .take(MAX_HEADER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(PackFormatError::ReadHeader)?;
    if bytes.len() as u64 > MAX_HEADER_BYTES {
        return Err(PackFormatError::HeaderTooLarge);
    }
    #[derive(Deserialize)]
    struct Probe {
        format: Option<String>,
        format_version: Option<u32>,
    }
    let probe: Probe = serde_json::from_slice(&bytes).map_err(PackFormatError::HeaderNotJson)?;
    if probe.format.as_deref() != Some(FORMAT) {
        return Err(PackFormatError::HeaderNotGents);
    }
    let version = probe.format_version.unwrap_or_default();
    if version != FORMAT_VERSION {
        return Err(PackFormatError::FormatVersion { version });
    }
    serde_json::from_slice(&bytes).map_err(PackFormatError::HeaderMalformed)
}

pub fn write_pack(dir: &Path, out: impl Write) -> Result<PackHeader> {
    write_pack_bounded(dir, out, Bounds::default())
}

fn write_pack_bounded(dir: &Path, out: impl Write, bounds: Bounds) -> Result<PackHeader> {
    let mut manifest_file = crate::pack::open_pack_asset(dir, "manifest.json")?;
    let mut manifest_bytes = Vec::new();
    manifest_file
        .read_to_end(&mut manifest_bytes)
        .map_err(PackFormatError::ReadManifest)?;
    let manifest: PackManifest =
        serde_json::from_slice(&manifest_bytes).map_err(PackFormatError::ParseManifest)?;
    validate_manifest(&manifest.name.clone(), &manifest)?;
    let paths = declared_paths(&manifest);
    if paths.len() >= bounds.entries {
        return Err(PackFormatError::TooManyDeclared {
            count: paths.len(),
            limit: bounds.entries,
        }
        .into());
    }

    let mut total = 0u64;
    let mut digest = PackDigester::default();
    for path in &paths {
        let (mut file, len) = open_declared(dir, path)?;
        total += len;
        if total > bounds.decompressed {
            return Err(PackFormatError::TooLarge {
                limit: bounds.decompressed,
            }
            .into());
        }
        digest.begin_entry(path, len);
        copy_exact(&mut file, len, &mut DigestWriter(&mut digest), path)?;
    }
    let header = PackHeader::describe(&manifest, digest.finish());
    let header_bytes = serde_json::to_vec(&header).map_err(PackFormatError::EncodeHeader)?;

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
    if again.finish() != header.digest {
        return Err(PackFormatError::SourceChanged {
            dir: dir.to_path_buf(),
        }
        .into());
    }
    let counted = builder
        .into_inner()
        .map_err(PackFormatError::FinishPack)?
        .finish()
        .map_err(PackFormatError::FinishCompression)?;
    if counted.written > bounds.compressed {
        return Err(PackFormatError::TooCompressed {
            written: counted.written,
            limit: bounds.compressed,
        }
        .into());
    }
    Ok(header)
}

pub fn pack_dir(dir: &Path) -> Result<(Vec<u8>, PackHeader)> {
    let mut bytes = Vec::new();
    let header = write_pack(dir, &mut bytes)?;
    Ok((bytes, header))
}

fn open_declared(dir: &Path, path: &str) -> Result<(std::fs::File, u64), PackFormatError> {
    let file = crate::pack::open_pack_asset(dir, path).map_err(|source| {
        PackFormatError::DeclaredUnreadable {
            path: path.to_owned(),
            source,
        }
    })?;
    let metadata = file
        .metadata()
        .map_err(|source| PackFormatError::DeclaredMetadata {
            path: path.to_owned(),
            source,
        })?;
    if !metadata.is_file() {
        return Err(PackFormatError::DeclaredNotRegular {
            path: path.to_owned(),
        });
    }
    Ok((file, metadata.len()))
}

fn copy_exact(
    from: &mut impl Read,
    len: u64,
    to: &mut impl Write,
    path: &str,
) -> Result<(), PackFormatError> {
    let copied = io::copy(&mut from.take(len), to).map_err(|source| PackFormatError::CopyRead {
        path: path.to_owned(),
        source,
    })?;
    if copied != len {
        return Err(PackFormatError::CopyChanged {
            path: path.to_owned(),
        });
    }
    Ok(())
}

pub(super) fn append_entry(
    builder: &mut tar::Builder<impl Write>,
    path: &str,
    len: u64,
    data: impl Read,
) -> Result<(), PackFormatError> {
    let mut header = tar::Header::new_ustar();
    header.set_size(len);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Regular);
    builder
        .append_data(&mut header, path, data)
        .map_err(|source| PackFormatError::AppendMember {
            path: path.to_owned(),
            source,
        })
}

struct HashingReader<'a, R> {
    inner: R,
    digest: &'a mut PackDigester,
}

impl<R> Read for HashingReader<'_, R>
where
    R: Read,
{
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

impl<W> Write for CountingWriter<W>
where
    W: Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

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

impl<R> Read for BoundedReader<R>
where
    R: Read,
{
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
