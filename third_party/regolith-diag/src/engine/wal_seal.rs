//! Sealed write-ahead logs: format 2 with every record's payload an AEAD
//! frame, and a stamp sealed under the key and durable before the log
//! exists.
//!
//! ```text
//! stamp   ["REGO"][format u16 = 2][flags u16 = 1][head check u32][nonce u64]
//!         [key id u32][stamp check u32][aead nonce: 12][tag: 16]
//! record  [len u32][kind u8][synced_through u64][payload check u32 = 0]
//!         [header check u32][aead nonce: 12][sealed operations][tag: 16]
//! ```
//!
//! The first twelve bytes are laid out as in every format, with flag bit 0
//! saying the log is sealed, so 0.1.x refuses the log as format 2. The
//! stamp check covers the 24 bytes before it, as in an unsealed stamp, so
//! a stamp damaged in place is told from one under a wrong key; the tag
//! seals the 28 bytes before the AEAD nonce under the key the stamp names.
//!
//! # The header keeps its check; the tag replaces the payload check
//!
//! Replay finds the records after a damaged one by testing every offset
//! for a header that verifies (`wal_frame::proof_past`). That test has to
//! stay one short hash, so the header check stays. Everything the payload
//! check did, the tag does: it covers the payload, and its associated data
//! is the header check's whole input (the log's nonce, the record's offset
//! and the header's fields), so it also binds the payload to its header,
//! its position and its log. The payload check field is written as zero.
//! `len` counts the frame, so a sealed record's payload is `len - 28`
//! bytes of operations; CLOSE seals an empty payload.
//!
//! # The stamp is sealed and durable at creation
//!
//! `proofs/tla/WalRecovery.tla` (RED `StampNotSealed`): with a wrong key
//! every record's tag fails, so no record can be read to prove anything
//! synced, P is 0, and the O < P rule would drop the whole log as a torn
//! tail. The stamp is therefore sealed under the log's key and made
//! durable before the log takes a record: a stamp whose tag fails refuses
//! the open instead. Creation writes the stamp to `wal_N.tmp`, syncs it,
//! renames it to `wal_N.log` and syncs the directory, so a crash leaves
//! either no log or a log whose whole stamp is durable, never one whose
//! stamp a crash tore and a wrong key cannot be told from.

use std::io;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use super::checksum;
use super::seal::{DOMAIN_WAL, Keyring, NONCE_LEN, OVERHEAD, Sealer, TAG_LEN};
use super::wal::WAL_MAGIC;
use super::wal_frame::{FORMAT_V2, HEADER_LEN, KIND_CLOSE};
use crate::encryption::KeyId;
use crate::env::{Env, WriteFile, WriteMode};

/// The stamp's flag bit saying the log is sealed.
pub(crate) const FLAG_SEALED: u16 = 1;

/// Bytes in a sealed stamp.
pub(crate) const SEALED_STAMP_LEN: usize = 56;

const KEY_ID_AT: usize = 20;
const CHECK_AT: usize = 24;
const AEAD_NONCE_AT: usize = 28;
const TAG_AT: usize = AEAD_NONCE_AT + NONCE_LEN;

/// Associated data kinds within the write-ahead log's domain.
const AAD_STAMP: u8 = 0;
const AAD_RECORD: u8 = 1;

/// The sealed stamp of a log whose header nonce is `nonce`, sealed under
/// `sealer`.
pub(crate) fn encode_stamp(nonce: u64, sealer: &Sealer) -> io::Result<[u8; SEALED_STAMP_LEN]> {
    let mut out = [0u8; SEALED_STAMP_LEN];
    out[0..4].copy_from_slice(&WAL_MAGIC);
    out[4..6].copy_from_slice(&FORMAT_V2.to_le_bytes());
    out[6..8].copy_from_slice(&FLAG_SEALED.to_le_bytes());
    let head = checksum::wal_stamp(&WAL_MAGIC, FORMAT_V2, FLAG_SEALED);
    out[8..12].copy_from_slice(&head.to_le_bytes());
    out[12..20].copy_from_slice(&nonce.to_le_bytes());
    out[KEY_ID_AT..CHECK_AT].copy_from_slice(&sealer.id().0.to_le_bytes());
    let check = checksum::wal_stamp_v2(&out[..CHECK_AT]);
    out[CHECK_AT..AEAD_NONCE_AT].copy_from_slice(&check.to_le_bytes());
    let (aead_nonce, tag) = sealer.seal(&stamp_aad(&out), &mut [])?;
    out[AEAD_NONCE_AT..TAG_AT].copy_from_slice(&aead_nonce);
    out[TAG_AT..].copy_from_slice(&tag);
    Ok(out)
}

fn stamp_aad(stamp: &[u8; SEALED_STAMP_LEN]) -> [u8; 2 + AEAD_NONCE_AT] {
    let mut aad = [0u8; 2 + AEAD_NONCE_AT];
    aad[0] = DOMAIN_WAL;
    aad[1] = AAD_STAMP;
    aad[2..].copy_from_slice(&stamp[..AEAD_NONCE_AT]);
    aad
}

/// Whether the 24 bytes a sealed stamp's check covers verify, which says
/// the stamp was written whole whatever its tag says.
pub(crate) fn stamp_check_holds(stamp: &[u8]) -> bool {
    stamp.len() >= AEAD_NONCE_AT
        && u32::from_le_bytes([
            stamp[CHECK_AT],
            stamp[CHECK_AT + 1],
            stamp[CHECK_AT + 2],
            stamp[CHECK_AT + 3],
        ]) == checksum::wal_stamp_v2(&stamp[..CHECK_AT])
}

/// The header nonce of the log at `path` whose sealed stamp is `stamp`,
/// and the sealer its records open under.
///
/// Refuses, naming the file, when the stamp is damaged or its tag fails
/// under the key it names; with [`crate::Error::KeyProviderRequired`]
/// when there is no keyring, and with [`crate::Error::UnknownKey`] when
/// the keyring does not provide the key.
pub(crate) fn open_stamp(
    stamp: &[u8; SEALED_STAMP_LEN],
    keyring: Option<&Keyring>,
    path: &Path,
) -> io::Result<(u64, Sealer)> {
    if !stamp_check_holds(stamp) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: the write-ahead log header is damaged", path.display()),
        ));
    }
    let keyring = keyring.ok_or_else(|| crate::Error::KeyProviderRequired.into_io_error())?;
    let id = KeyId(u32::from_le_bytes([
        stamp[KEY_ID_AT],
        stamp[KEY_ID_AT + 1],
        stamp[KEY_ID_AT + 2],
        stamp[KEY_ID_AT + 3],
    ]));
    let sealer = keyring.sealer(id)?;
    let mut aead_nonce = [0u8; NONCE_LEN];
    aead_nonce.copy_from_slice(&stamp[AEAD_NONCE_AT..TAG_AT]);
    let mut tag = [0u8; TAG_LEN];
    tag.copy_from_slice(&stamp[TAG_AT..]);
    if !sealer.open(&aead_nonce, &stamp_aad(stamp), &mut [], &tag) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{}: the write-ahead log header does not verify under key id {id}; the key is \
                 wrong or the header is damaged",
                path.display()
            ),
        ));
    }
    let mut nonce = [0u8; 8];
    nonce.copy_from_slice(&stamp[12..20]);
    Ok((u64::from_le_bytes(nonce), sealer))
}

/// What a sealed record's tag binds besides its payload: the header check's
/// whole input.
fn record_aad(nonce: u64, offset: u64, fields: &[u8; 17]) -> [u8; 2 + 33] {
    let mut aad = [0u8; 2 + 33];
    aad[0] = DOMAIN_WAL;
    aad[1] = AAD_RECORD;
    aad[2..].copy_from_slice(&checksum::wal_group_header_input(nonce, offset, fields));
    aad
}

/// Frame a record of `kind` whose operations are `payload`, written at
/// `offset` of the log stamped with `nonce` when the last completed sync
/// covered `synced_through`, sealed under `sealer`, into `out`, header
/// included. `out` is cleared first.
pub(crate) fn frame_record(
    sealer: &Sealer,
    kind: u8,
    payload: &[u8],
    synced_through: u64,
    nonce: u64,
    offset: u64,
    out: &mut Vec<u8>,
) -> io::Result<()> {
    out.clear();
    out.reserve(HEADER_LEN + payload.len() + OVERHEAD);
    out.resize(SEALED_PREFIX_LEN, 0);
    out.extend_from_slice(payload);
    let aead_nonce = super::seal::fresh_nonce()?;
    frame_in_place(
        sealer,
        kind,
        synced_through,
        nonce,
        offset,
        &aead_nonce,
        out,
    )
}

/// Bytes a sealed record puts in front of its operations: the header and the
/// AEAD nonce.
pub(crate) const SEALED_PREFIX_LEN: usize = HEADER_LEN + NONCE_LEN;

/// Frame and seal, in place, the record whose operations sit in `buf` after
/// [`SEALED_PREFIX_LEN`] bytes left for the header and the AEAD nonce. The
/// record is of `kind`, written at `offset` of the log stamped with `nonce`
/// when the last completed sync covered `synced_through`, and sealed under
/// `sealer` with `aead_nonce`. Given the same arguments it produces the same
/// bytes, which is what lets any thread write a commit's record.
pub(crate) fn frame_in_place(
    sealer: &Sealer,
    kind: u8,
    synced_through: u64,
    nonce: u64,
    offset: u64,
    aead_nonce: &[u8; NONCE_LEN],
    buf: &mut Vec<u8>,
) -> io::Result<()> {
    debug_assert!(
        synced_through <= offset,
        "a record claims only what is behind it"
    );
    let payload_len = buf.len().checked_sub(SEALED_PREFIX_LEN).ok_or_else(|| {
        io::Error::other("a sealed record was framed without room for its header")
    })?;
    let len = u32::try_from(payload_len + OVERHEAD)
        .map_err(|_| io::Error::other("a write-ahead log record is too large to seal"))?;
    let mut fields = [0u8; 17];
    fields[0..4].copy_from_slice(&len.to_le_bytes());
    fields[4] = kind;
    fields[5..13].copy_from_slice(&synced_through.to_le_bytes());
    let check = checksum::wal_group_header(nonce, offset, &fields);
    buf[..17].copy_from_slice(&fields);
    buf[17..HEADER_LEN].copy_from_slice(&check.to_le_bytes());
    buf[HEADER_LEN..SEALED_PREFIX_LEN].copy_from_slice(aead_nonce);
    let tag = sealer.seal_with(
        aead_nonce,
        &record_aad(nonce, offset, &fields),
        &mut buf[SEALED_PREFIX_LEN..],
    )?;
    buf.extend_from_slice(&tag);
    Ok(())
}

/// Open the sealed payload `payload` of the record whose header fields are
/// `fields`, read at `offset` of the log stamped with `nonce`, in place.
/// Where its operations now sit, or `None` when the tag fails.
pub(crate) fn open_record(
    sealer: &Sealer,
    fields: &[u8; 17],
    nonce: u64,
    offset: u64,
    payload: &mut [u8],
) -> Option<Range<usize>> {
    sealer.open_frame(&record_aad(nonce, offset, fields), payload)
}

/// Create the log at `path` holding only `stamp`, durably, and open it for
/// appending: the stamp is written to `path` with a `tmp` extension, which
/// recovery never reads as a log, synced, renamed into place, and the
/// directory synced. A crash anywhere in here leaves no log at `path` or
/// one whose whole stamp is durable.
pub(crate) fn create_durably(
    env: &Arc<dyn Env>,
    path: &Path,
    stamp: &[u8],
) -> io::Result<Box<dyn WriteFile>> {
    stage_durably(env, path, stamp)?;
    env.open_write(path, WriteMode::Append)
}

/// The creation half of [`create_durably`]: afterwards the log at `path`
/// holds `stamp` and nothing else, durably, and is not open.
pub(crate) fn stage_durably(env: &Arc<dyn Env>, path: &Path, stamp: &[u8]) -> io::Result<()> {
    let staged = path.with_extension("tmp");
    {
        let mut file = env.open_write(&staged, WriteMode::Truncate)?;
        file.write_all(stamp)?;
        file.sync_data()?;
    }
    env.rename(&staged, path)?;
    crate::env::sync_parent_dir(&**env, path)
}

/// Whether `path` is a staging file [`create_durably`] leaves when a crash
/// comes before the rename: no log was ever named by one, so nothing in it
/// is needed.
pub(crate) fn is_staged(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "tmp")
}

/// Remove the staging file at `path` ([`is_staged`]). A read-write open
/// removes each one its walk of the log directory reaches
/// (`wal_scan::live_removing_staged`).
pub(crate) fn remove_staged(env: &dyn Env, path: &Path) -> io::Result<()> {
    match env.remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Bytes a sealed CLOSE record's payload holds: an empty sealed frame.
pub(crate) const CLOSE_PAYLOAD_LEN: u32 = OVERHEAD as u32;

/// Whether `kind` and `len` are a shape a sealed log's writer produces.
pub(crate) fn sealed_len_ok(kind: u8, len: u32) -> bool {
    if kind == KIND_CLOSE {
        len == CLOSE_PAYLOAD_LEN
    } else {
        len > CLOSE_PAYLOAD_LEN
    }
}
