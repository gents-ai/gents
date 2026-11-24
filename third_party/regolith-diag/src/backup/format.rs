//! The `.backup` file under `meta/`: what one backup holds.
//!
//! Every version begins `[magic "REGOMBKP"][version u32]` and ends with an
//! xxh3 checksum (`u64`) of every byte before it, checked first and with no
//! key, so accidental damage reads as damage in every version.
//!
//! **Version 3** is written for a database without a key provider, all of
//! it in plaintext:
//!
//! ```text
//! [created_at u64][next_file_id u64][last_seq u64][count u32]
//! count x [level u32][file_id u64][file_size u64][num_entries u64]
//!         [content id u128][smallest len u32][smallest]
//!         [largest len u32][largest][ingest seq u64, 0 for none]
//! ```
//!
//! Version 2 is version 3 without the ingest sequence, and still reads.
//!
//! **Version 4** is written for a database encrypted at rest. It keeps in
//! plaintext only the listing, which `shared/` shows anyway (each shared
//! object's name and size), and seals the rest under the database's
//! current key, naming that key:
//!
//! ```text
//! listing  [created_at u64][count u32] count x [content id u128][file_size u64]
//! seal     [key id u32][nonce: 12][sealed contents][tag: 16]
//! contents [next_file_id u64][last_seq u64]
//!          count x [level u32][file_id u64][num_entries u64][ingest seq u64]
//!                  [table sealed u8][table key id u32]
//!                  [smallest len u32][smallest][largest len u32][largest]
//! ```
//!
//! Listing, deleting and purging backups read only the listing, so they
//! need no key and count every backup's shared objects, sealed or not. The
//! tag binds the listing all the same: its associated data is
//! `[b'B'][backup id u64]` followed by every byte from the magic through
//! the key id. A listing changed to name other objects, a changed key id,
//! or a `.backup` file renamed to another backup's id fails the tag like a
//! wrong key, and the restore refuses rather than put back tables the
//! backup never held (`proofs/tla/BackupSeal.tla`, RED `ListingUnbound` and
//! `IdUnbound`).
//!
//! Each table's key id is recorded so a restore can refuse a provider that
//! lacks it before writing anything, as the open refuses one.

use std::io;

use crate::encryption::KeyId;
use crate::engine::checksum;
use crate::engine::seal::{DOMAIN_BACKUP, Keyring, OVERHEAD, Sealer};

use super::BackupId;

/// One table a backup captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BackupFileEntry {
    pub(super) level: u32,
    pub(super) file_id: u64,
    pub(super) file_size: u64,
    /// The content id: the table's name in `shared/`.
    pub(super) hash: u128,
    pub(super) smallest_key: Vec<u8>,
    pub(super) largest_key: Vec<u8>,
    pub(super) num_entries: u64,
    /// The sequence an ingested table's entries read at, which lives in
    /// the manifest and not in the file. Without it a restored ingested
    /// table would read at the sequences the file stores.
    pub(super) global_seq: Option<u64>,
    /// The key the table's own footer names, `None` for a plaintext table
    /// and for every table of a version 2 or 3 backup, which never
    /// recorded it.
    pub(super) seal_key: Option<KeyId>,
}

/// Everything one backup holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BackupManifest {
    pub(super) created_at_unix: u64,
    pub(super) files: Vec<BackupFileEntry>,
    pub(super) next_file_id: u64,
    pub(super) last_seq: u64,
    /// The key the metadata is sealed under, `None` when it is plaintext.
    pub(super) sealed_under: Option<KeyId>,
    /// The database it was taken from had its range deletes checked for
    /// ones an earlier release left behind older keys they cover
    /// (`engine::buried_range_deletes`), so the manifest a restore writes
    /// says so. True for versions 3 and 4, which only this release line
    /// writes, and every database it backs up was checked when it opened;
    /// false for version 2, which 0.1.x wrote, so the restored database is
    /// checked at its first open. Not stored: the version says it.
    pub(super) range_deletes_checked: bool,
}

/// What every version shows without a key: when the backup was taken, and
/// each shared object it lists as `(content id, size)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Listing {
    pub(super) created_at_unix: u64,
    pub(super) objects: Vec<(u128, u64)>,
}

/// Identifier at the head of a backup manifest: `REGOMBKP`.
pub(super) const MAGIC: [u8; 8] = *b"REGOMBKP";

/// The version written for a database without a key provider. Version 3
/// adds each table's ingest sequence, 0 for none, to version 2.
pub(super) const VERSION_PLAIN: u32 = 3;

/// The version written for a database encrypted at rest.
pub(super) const VERSION_SEALED: u32 = 4;

/// The oldest version this build reads.
const VERSION_OLDEST: u32 = 2;

/// Bytes of the magic and the version.
const HEAD_LEN: usize = MAGIC.len() + 4;

/// Bytes of one listing entry: a content id and a size.
const LISTING_ENTRY_LEN: usize = 16 + 8;

/// The fewest bytes one table takes in the plaintext body of version 2:
/// its fixed fields and two empty keys. Bounds the entry count a damaged
/// or hostile count field can make a decoder reserve room for.
const PLAIN_ENTRY_MIN_LEN: usize = 4 + 8 + 8 + 8 + 16 + 4 + 4;

/// The fewest bytes one table takes in sealed contents.
const SEALED_ENTRY_MIN_LEN: usize = 4 + 8 + 8 + 8 + 1 + 4 + 4 + 4;

/// Encode `m` as backup `id`'s metadata: sealed under `sealer` (version 4)
/// when given, plaintext (version 3) otherwise.
pub(super) fn encode(
    m: &BackupManifest,
    id: BackupId,
    sealer: Option<&Sealer>,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC);
    match sealer {
        None => {
            out.extend_from_slice(&VERSION_PLAIN.to_le_bytes());
            encode_plain(m, &mut out)?;
        }
        Some(sealer) => {
            out.extend_from_slice(&VERSION_SEALED.to_le_bytes());
            encode_sealed(m, id, sealer, &mut out)?;
        }
    }
    let checksum = checksum::backup_manifest(&out);
    out.extend_from_slice(&checksum.to_le_bytes());
    Ok(out)
}

fn encode_plain(m: &BackupManifest, out: &mut Vec<u8>) -> io::Result<()> {
    out.extend_from_slice(&m.created_at_unix.to_le_bytes());
    out.extend_from_slice(&m.next_file_id.to_le_bytes());
    out.extend_from_slice(&m.last_seq.to_le_bytes());
    out.extend_from_slice(&count_field(m.files.len())?.to_le_bytes());
    for f in &m.files {
        out.extend_from_slice(&f.level.to_le_bytes());
        out.extend_from_slice(&f.file_id.to_le_bytes());
        out.extend_from_slice(&f.file_size.to_le_bytes());
        out.extend_from_slice(&f.num_entries.to_le_bytes());
        out.extend_from_slice(&f.hash.to_le_bytes());
        put_var_bytes(out, &f.smallest_key)?;
        put_var_bytes(out, &f.largest_key)?;
        out.extend_from_slice(&f.global_seq.unwrap_or(0).to_le_bytes());
    }
    Ok(())
}

fn encode_sealed(
    m: &BackupManifest,
    id: BackupId,
    sealer: &Sealer,
    out: &mut Vec<u8>,
) -> io::Result<()> {
    out.extend_from_slice(&m.created_at_unix.to_le_bytes());
    out.extend_from_slice(&count_field(m.files.len())?.to_le_bytes());
    for f in &m.files {
        out.extend_from_slice(&f.hash.to_le_bytes());
        out.extend_from_slice(&f.file_size.to_le_bytes());
    }
    out.extend_from_slice(&sealer.id().0.to_le_bytes());

    let mut contents = Vec::new();
    contents.extend_from_slice(&m.next_file_id.to_le_bytes());
    contents.extend_from_slice(&m.last_seq.to_le_bytes());
    for f in &m.files {
        contents.extend_from_slice(&f.level.to_le_bytes());
        contents.extend_from_slice(&f.file_id.to_le_bytes());
        contents.extend_from_slice(&f.num_entries.to_le_bytes());
        contents.extend_from_slice(&f.global_seq.unwrap_or(0).to_le_bytes());
        contents.push(u8::from(f.seal_key.is_some()));
        contents.extend_from_slice(&f.seal_key.map_or(0, |k| k.0).to_le_bytes());
        put_var_bytes(&mut contents, &f.smallest_key)?;
        put_var_bytes(&mut contents, &f.largest_key)?;
    }
    let aad = aad(id, out);
    sealer.seal_frame(&aad, &contents, out)
}

/// The associated data a version 4 file's tag binds: the domain, the
/// backup id its name carries, and `head`, every byte from the magic
/// through the key id.
fn aad(id: BackupId, head: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(1 + 8 + head.len());
    aad.push(DOMAIN_BACKUP);
    aad.extend_from_slice(&id.0.to_le_bytes());
    aad.extend_from_slice(head);
    aad
}

/// Decode backup `id`'s metadata. A sealed one is opened under the key it
/// names: without a `keyring` it refuses with
/// [`crate::Error::KeyProviderRequired`], under a key the keyring lacks
/// with [`crate::Error::UnknownKey`], and when its tag fails (a wrong key,
/// or a changed byte) as damaged data.
pub(super) fn decode(
    data: &[u8],
    id: BackupId,
    keyring: Option<&Keyring>,
) -> io::Result<BackupManifest> {
    let (version, body) = checked_body(data)?;
    if version != VERSION_SEALED {
        return decode_plain(body, version);
    }
    let mut p = HEAD_LEN;
    let listing = decode_sealed_listing(body, &mut p)?;
    let key = KeyId(read_u32(body, &mut p)?);
    let keyring = keyring.ok_or_else(|| crate::Error::KeyProviderRequired.into_io_error())?;
    let sealer = keyring.sealer(key)?;
    let mut frame = body[p..].to_vec();
    let Some(plain) = sealer.open_frame(&aad(id, &body[..p]), &mut frame) else {
        return Err(invalid_data(format!(
            "backup {id}: its metadata does not verify under key id {key}; \
             the key is wrong or the file is damaged"
        )));
    };
    let contents = &frame[plain];
    let mut q = 0;
    let next_file_id = read_u64(contents, &mut q)?;
    let last_seq = read_u64(contents, &mut q)?;
    let mut files = Vec::with_capacity(
        listing
            .objects
            .len()
            .min(contents.len() / SEALED_ENTRY_MIN_LEN),
    );
    for &(hash, file_size) in &listing.objects {
        let level = read_u32(contents, &mut q)?;
        let file_id = read_u64(contents, &mut q)?;
        let num_entries = read_u64(contents, &mut q)?;
        let global_seq = Some(read_u64(contents, &mut q)?).filter(|&seq| seq != 0);
        let sealed = read_u8(contents, &mut q)?;
        let table_key = KeyId(read_u32(contents, &mut q)?);
        let seal_key = match sealed {
            0 => None,
            1 => Some(table_key),
            other => {
                return Err(invalid_data(format!(
                    "backup {id}: a table's seal flag is {other}"
                )));
            }
        };
        let smallest_key = read_var_bytes(contents, &mut q)?;
        let largest_key = read_var_bytes(contents, &mut q)?;
        files.push(BackupFileEntry {
            level,
            file_id,
            file_size,
            hash,
            smallest_key,
            largest_key,
            num_entries,
            global_seq,
            seal_key,
        });
    }
    if q != contents.len() {
        return Err(invalid_data(format!(
            "backup {id}: its metadata holds bytes after its last table"
        )));
    }
    Ok(BackupManifest {
        created_at_unix: listing.created_at_unix,
        files,
        next_file_id,
        last_seq,
        sealed_under: Some(key),
        range_deletes_checked: true,
    })
}

/// The listing of a backup's metadata, which every version holds where no
/// key is needed to read it.
pub(super) fn decode_listing(data: &[u8]) -> io::Result<Listing> {
    let (version, body) = checked_body(data)?;
    if version == VERSION_SEALED {
        let mut p = HEAD_LEN;
        let listing = decode_sealed_listing(body, &mut p)?;
        // The seal must be there whole, even though nothing here opens it.
        if body.len().saturating_sub(p) < 4 + OVERHEAD {
            return Err(invalid_data("backup manifest ends before its seal"));
        }
        return Ok(listing);
    }
    let m = decode_plain(body, version)?;
    Ok(Listing {
        created_at_unix: m.created_at_unix,
        objects: m.files.iter().map(|f| (f.hash, f.file_size)).collect(),
    })
}

/// The version and the bytes before the checksum, once the checksum, the
/// magic and the version check out.
fn checked_body(data: &[u8]) -> io::Result<(u32, &[u8])> {
    let Some((body, stored)) = data.split_last_chunk::<8>() else {
        return Err(invalid_data("short backup"));
    };
    if checksum::backup_manifest(body) != u64::from_le_bytes(*stored) {
        return Err(invalid_data("backup manifest checksum mismatch"));
    }
    if body.len() < MAGIC.len() || body[..MAGIC.len()] != MAGIC {
        return Err(invalid_data("backup manifest bad magic"));
    }
    let mut p = MAGIC.len();
    let version = read_u32(body, &mut p)?;
    if !(VERSION_OLDEST..=VERSION_SEALED).contains(&version) {
        return Err(invalid_data(format!(
            "unsupported backup manifest version {version}"
        )));
    }
    Ok((version, body))
}

fn decode_sealed_listing(body: &[u8], p: &mut usize) -> io::Result<Listing> {
    let created_at_unix = read_u64(body, p)?;
    let count = read_u32(body, p)? as usize;
    let mut objects = Vec::with_capacity(count.min(body.len() / LISTING_ENTRY_LEN));
    for _ in 0..count {
        let hash = read_u128(body, p)?;
        let size = read_u64(body, p)?;
        objects.push((hash, size));
    }
    Ok(Listing {
        created_at_unix,
        objects,
    })
}

fn decode_plain(body: &[u8], version: u32) -> io::Result<BackupManifest> {
    let mut p = HEAD_LEN;
    let created_at_unix = read_u64(body, &mut p)?;
    let next_file_id = read_u64(body, &mut p)?;
    let last_seq = read_u64(body, &mut p)?;
    let count = read_u32(body, &mut p)? as usize;
    let mut files = Vec::with_capacity(count.min(body.len() / PLAIN_ENTRY_MIN_LEN));
    for _ in 0..count {
        let level = read_u32(body, &mut p)?;
        let file_id = read_u64(body, &mut p)?;
        let file_size = read_u64(body, &mut p)?;
        let num_entries = read_u64(body, &mut p)?;
        let hash = read_u128(body, &mut p)?;
        let smallest_key = read_var_bytes(body, &mut p)?;
        let largest_key = read_var_bytes(body, &mut p)?;
        // Sequences start at 1, so 0 is the encoding of "none".
        let global_seq = match version {
            VERSION_OLDEST => None,
            _ => Some(read_u64(body, &mut p)?).filter(|&seq| seq != 0),
        };
        files.push(BackupFileEntry {
            level,
            file_id,
            file_size,
            hash,
            smallest_key,
            largest_key,
            num_entries,
            global_seq,
            seal_key: None,
        });
    }
    Ok(BackupManifest {
        created_at_unix,
        files,
        next_file_id,
        last_seq,
        sealed_under: None,
        range_deletes_checked: version >= VERSION_PLAIN,
    })
}

fn count_field(n: usize) -> io::Result<u32> {
    u32::try_from(n).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a backup holds more than 4294967295 tables",
        )
    })
}

fn put_var_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> io::Result<()> {
    let len = u32::try_from(bytes.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a table's key range is longer than 4 GiB",
        )
    })?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn short() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "short")
}

fn read_array<const N: usize>(data: &[u8], p: &mut usize) -> io::Result<[u8; N]> {
    let bytes = data
        .get(*p..)
        .and_then(|rest| rest.first_chunk::<N>())
        .ok_or_else(short)?;
    *p += N;
    Ok(*bytes)
}

fn read_u8(data: &[u8], p: &mut usize) -> io::Result<u8> {
    Ok(read_array::<1>(data, p)?[0])
}

fn read_u32(data: &[u8], p: &mut usize) -> io::Result<u32> {
    read_array(data, p).map(u32::from_le_bytes)
}

fn read_u64(data: &[u8], p: &mut usize) -> io::Result<u64> {
    read_array(data, p).map(u64::from_le_bytes)
}

fn read_u128(data: &[u8], p: &mut usize) -> io::Result<u128> {
    read_array(data, p).map(u128::from_le_bytes)
}

fn read_var_bytes(data: &[u8], p: &mut usize) -> io::Result<Vec<u8>> {
    let len = read_u32(data, p)? as usize;
    let bytes = data
        .get(*p..)
        .and_then(|rest| rest.get(..len))
        .ok_or_else(short)?;
    *p += len;
    Ok(bytes.to_vec())
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
