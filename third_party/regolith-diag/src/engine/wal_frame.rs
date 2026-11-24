//! Format 2 of the write-ahead log: its stamp, its record frame, and the
//! scan replay uses to read the records that follow a damaged one.
//!
//! # On-disk format 2
//!
//! ```text
//! stamp   ["REGO"][format u16 = 2][reserved u16 = 0][head check u32][nonce u64]
//!         [stamp check u32]
//! record  [len u32][kind u8][synced_through u64][payload check u32][header check u32]
//!         [payload: len bytes]
//! ```
//!
//! Integers are little-endian. The stamp's first 12 bytes are laid out as
//! format 1's whole stamp is, head check included, so 0.1.x reads the
//! format field and refuses the log as a newer format instead of calling
//! it damaged. The stamp check covers the 20 bytes before it.
//!
//! A commit is one record of kind [`KIND_GROUP`]. Its payload is the
//! commit's operations back to back, each an operation type byte followed
//! by that operation's payload, which carries its own lengths and sequence
//! number. A clean close appends one record of kind [`KIND_CLOSE`], with
//! an empty payload, and nothing follows it.
//!
//! `synced_through` is the end offset the last completed sync of the log
//! had made durable when the record was written. A record therefore never
//! claims more than its own offset, and a header that does is refused.
//!
//! # Finding the records after a damaged one
//!
//! Replay of the newest log reads P, the largest `synced_through` of any
//! usable record in the file, including the records after the first
//! unusable one (`proofs/tla/WalRecovery.tla`, `P`). A damaged record's
//! length cannot be trusted to say where the next record starts, so
//! [`proof_past`] tests every offset after the damage for a header that
//! verifies. Three properties make that cheap and exact:
//!
//! - The header carries its own check, over its fields, the log's random
//!   nonce and the record's own offset, so a test costs one short hash and
//!   never the record's payload. A record copied from another log, from
//!   another position of this one, or out of a value a caller stored never
//!   verifies here. The payload check is one of the fields the header
//!   check covers, which binds the payload to its header.
//! - The kind bytes, 0xC0 and 0xC1, never occur in UTF-8 text and are not
//!   zero, so on text, zeros and random bytes almost every offset is
//!   skipped on one byte compare before any hash.
//! - Only a record that proves bytes past the damage durable can change
//!   the verdict, so the scan reads the payload of no other record, and
//!   stops at the first such record whose payload verifies.
//!
//! # Cost
//!
//! A group carries [`HEADER_LEN`] bytes of framing plus one type byte per
//! operation, where format 1 carried nine bytes per write (and five more
//! per operation of a batch), so a group of three or more writes is
//! smaller than before and a group of one put is 13 bytes larger.
//! Appending computes one hash over the payload and one over the 33 bytes
//! of the header input, and copies a group of up to 4 KiB next to its
//! frame, so it leaves in one plain write. Replay of a healthy log adds one
//! header hash per group; replay of a damaged one reads the bytes after the
//! damage once.

use std::io;

use super::checksum;
use super::seal::Sealer;
use super::wal::{
    MAX_RECORD_LEN, RECORD_DELETE, RECORD_DELETE_RANGE, RECORD_MERGE, RECORD_PUT, WAL_MAGIC,
    WalEntry, parse_delete_range_record, parse_delete_record, parse_merge_record, parse_put_record,
};
use crate::env::ReadFile;

/// The format field of a format 2 stamp.
pub(crate) const FORMAT_V2: u16 = 2;

/// Bytes in a format 2 stamp.
pub(crate) const STAMP_LEN: usize = 24;

/// Bytes of framing in front of every record's payload.
pub(crate) const HEADER_LEN: usize = 21;

/// A record holding one commit's operations.
pub(crate) const KIND_GROUP: u8 = 0xC0;

/// The record a clean close appends last.
pub(crate) const KIND_CLOSE: u8 = 0xC1;

/// Bytes the scan reads per positioned read.
const SCAN_CHUNK: usize = 64 * 1024;

/// Encode the stamp a format 2 log begins with.
pub(crate) fn encode_stamp(nonce: u64) -> [u8; STAMP_LEN] {
    let mut out = [0u8; STAMP_LEN];
    out[0..4].copy_from_slice(&WAL_MAGIC);
    out[4..6].copy_from_slice(&FORMAT_V2.to_le_bytes());
    let head = checksum::wal_stamp(&WAL_MAGIC, FORMAT_V2, 0);
    out[8..12].copy_from_slice(&head.to_le_bytes());
    out[12..20].copy_from_slice(&nonce.to_le_bytes());
    let check = checksum::wal_stamp_v2(&out[..20]);
    out[20..24].copy_from_slice(&check.to_le_bytes());
    out
}

/// The nonce of a format 2 stamp whose first 12 bytes already validated,
/// or an error when the rest of it fails its check.
pub(super) fn stamp_nonce(stamp: &[u8; STAMP_LEN]) -> io::Result<u64> {
    let stored = u32::from_le_bytes([stamp[20], stamp[21], stamp[22], stamp[23]]);
    if stored != checksum::wal_stamp_v2(&stamp[..20]) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the write-ahead log header is damaged",
        ));
    }
    let mut nonce = [0u8; 8];
    nonce.copy_from_slice(&stamp[12..20]);
    Ok(u64::from_le_bytes(nonce))
}

/// A record header that verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Header {
    pub(super) len: u32,
    pub(super) kind: u8,
    pub(super) synced_through: u64,
    payload_check: u32,
}

impl Header {
    /// Bytes the whole record occupies, header included.
    pub(super) fn record_len(&self) -> u64 {
        HEADER_LEN as u64 + u64::from(self.len)
    }

    /// Whether `payload` is the payload this header was written with.
    pub(super) fn payload_matches(&self, payload: &[u8]) -> bool {
        checksum::wal_group_payload(payload) == self.payload_check
    }

    /// The offset this record proves durable: its stamp, or, for CLOSE,
    /// the whole file of `file_len` bytes.
    pub(super) fn proves(&self, file_len: u64) -> u64 {
        if self.kind == KIND_CLOSE {
            file_len
        } else {
            self.synced_through
        }
    }

    /// The header's first 17 bytes, as they were written.
    pub(super) fn fields(&self) -> [u8; 17] {
        let mut fields = [0u8; 17];
        fields[0..4].copy_from_slice(&self.len.to_le_bytes());
        fields[4] = self.kind;
        fields[5..13].copy_from_slice(&self.synced_through.to_le_bytes());
        fields[13..17].copy_from_slice(&self.payload_check.to_le_bytes());
        fields
    }
}

/// Frame a record of `kind` whose payload is `payload`, written at
/// `offset` of the log stamped with `nonce`, when the last completed sync
/// covered `synced_through`.
pub(super) fn encode_header(
    kind: u8,
    payload: &[u8],
    synced_through: u64,
    nonce: u64,
    offset: u64,
) -> [u8; HEADER_LEN] {
    debug_assert!(
        synced_through <= offset,
        "a record claims only what is behind it"
    );
    let mut fields = [0u8; 17];
    fields[0..4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    fields[4] = kind;
    fields[5..13].copy_from_slice(&synced_through.to_le_bytes());
    fields[13..17].copy_from_slice(&checksum::wal_group_payload(payload).to_le_bytes());
    let mut out = [0u8; HEADER_LEN];
    out[..17].copy_from_slice(&fields);
    let check = checksum::wal_group_header(nonce, offset, &fields);
    out[17..21].copy_from_slice(&check.to_le_bytes());
    out
}

/// The header at the start of `bytes`, read at `offset` of the log stamped
/// with `nonce`, or `None` when those bytes are not a header this log's
/// writer produced there. `sealed` says the log's payloads are sealed
/// frames (`super::wal_seal`), which changes what lengths the writer
/// produces.
pub(super) fn decode_header(bytes: &[u8], nonce: u64, offset: u64, sealed: bool) -> Option<Header> {
    let h = bytes.get(..HEADER_LEN)?;
    let kind = h[4];
    if kind != KIND_GROUP && kind != KIND_CLOSE {
        return None;
    }
    let stored = u32::from_le_bytes([h[17], h[18], h[19], h[20]]);
    if stored != checksum::wal_group_header(nonce, offset, h[..17].try_into().ok()?) {
        return None;
    }
    let len = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
    let synced_through = u64::from_le_bytes(h[5..13].try_into().ok()?);
    let payload_check = u32::from_le_bytes([h[13], h[14], h[15], h[16]]);
    // A verified header that breaks the writer's own rules is not one the
    // writer produced: a stamp never runs ahead of its record, CLOSE
    // carries nothing, and a sealed record carries a frame and no payload
    // check.
    let shape_ok = if sealed {
        len <= MAX_RECORD_LEN + super::wal_seal::CLOSE_PAYLOAD_LEN
            && payload_check == 0
            && super::wal_seal::sealed_len_ok(kind, len)
    } else {
        len <= MAX_RECORD_LEN && (kind != KIND_CLOSE || len == 0)
    };
    if !shape_ok || synced_through > offset {
        return None;
    }
    Some(Header {
        len,
        kind,
        synced_through,
        payload_check,
    })
}

fn u32_at(bytes: &[u8], at: usize) -> Option<usize> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

/// Bytes of the operation at the head of `entries`, its type byte
/// included, or `None` when it has an unknown type or runs past the end.
fn entry_len(entries: &[u8]) -> Option<usize> {
    let (&op, rest) = entries.split_first()?;
    let payload = match op {
        RECORD_PUT | RECORD_MERGE | RECORD_DELETE_RANGE => {
            let second = 4usize.checked_add(u32_at(rest, 0)?)?;
            second
                .checked_add(4)?
                .checked_add(u32_at(rest, second)?)?
                .checked_add(8)?
        }
        RECORD_DELETE => 4usize.checked_add(u32_at(rest, 0)?)?.checked_add(8)?,
        _ => return None,
    };
    (payload <= rest.len()).then_some(payload + 1)
}

/// Whether `entries` is a sequence of whole operations and nothing else.
///
/// Replay checks a group before it yields any of its operations, so a
/// group is applied whole or not at all.
pub(super) fn entries_are_whole(entries: &[u8]) -> bool {
    let mut at = 0;
    while at < entries.len() {
        match entry_len(&entries[at..]) {
            Some(len) => at += len,
            None => return false,
        }
    }
    true
}

/// Decode the operation at the head of `entries`, returning it and its
/// length.
pub(super) fn decode_entry(entries: &[u8]) -> io::Result<(WalEntry, usize)> {
    let len = entry_len(entries).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "write-ahead log operation is malformed",
        )
    })?;
    let payload = &entries[1..len];
    let entry = match entries[0] {
        RECORD_PUT => parse_put_record(payload)?,
        RECORD_DELETE => parse_delete_record(payload)?,
        RECORD_DELETE_RANGE => parse_delete_range_record(payload)?,
        _ => parse_merge_record(payload)?,
    };
    Ok((entry, len))
}

/// The first usable record in `[from, end)` of `file` that proves a byte
/// past `damaged_at` durable, as the offset it proves through; `None` when
/// there is none.
///
/// `file` is a log stamped with `nonce` and `end` long. Every offset is
/// tested, so no record the damage hides is missed; a record that proves
/// no more than `damaged_at` cannot change the verdict and is passed over
/// without reading its payload. Holds one chunk and, for a candidate,
/// streams its payload through the hash, so memory stays bounded whatever
/// the file's length. In a log sealed under `seal` a candidate is usable
/// when its tag verifies, which takes its whole payload: memory is then
/// bounded by one record, as replay itself is.
pub(super) fn proof_past(
    file: &dyn ReadFile,
    nonce: u64,
    from: u64,
    end: u64,
    damaged_at: u64,
    seal: Option<&Sealer>,
) -> io::Result<Option<u64>> {
    let mut chunk = vec![0u8; SCAN_CHUNK + HEADER_LEN - 1];
    let mut start = from;
    while start.saturating_add(HEADER_LEN as u64) <= end {
        let want = (end - start).min(chunk.len() as u64) as usize;
        file.read_exact_at(start, &mut chunk[..want])?;
        let candidates = want - HEADER_LEN + 1;
        let mut i = 0;
        while i < candidates {
            let Some(skip) = chunk[i + 4..candidates + 4]
                .iter()
                .position(|&b| b == KIND_GROUP || b == KIND_CLOSE)
            else {
                break;
            };
            i += skip;
            let offset = start + i as u64;
            i += 1;
            let Some(header) = decode_header(&chunk[i - 1..], nonce, offset, seal.is_some()) else {
                continue;
            };
            let proves = header.proves(end);
            if proves <= damaged_at || offset + header.record_len() > end {
                continue;
            }
            let at = offset + HEADER_LEN as u64;
            let usable = match seal {
                Some(sealer) => sealed_payload_verifies(file, at, &header, sealer, nonce, offset)?,
                None => payload_verifies(file, at, &header)?,
            };
            if usable {
                return Ok(Some(proves));
            }
        }
        start += candidates as u64;
    }
    Ok(None)
}

/// Whether the payload of `header`, at `at` in `file`, verifies. Streams
/// it through the hash in chunks.
fn payload_verifies(file: &dyn ReadFile, at: u64, header: &Header) -> io::Result<bool> {
    let len = u64::from(header.len);
    let mut buf = vec![0u8; (SCAN_CHUNK as u64).min(len) as usize];
    let mut hasher = checksum::WalGroupPayloadHasher::new();
    let mut done = 0u64;
    while done < len {
        let n = (len - done).min(buf.len() as u64) as usize;
        file.read_exact_at(at + done, &mut buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
    }
    Ok(hasher.finish() == header.payload_check)
}

/// Whether the sealed payload of `header`, at `at` in `file`, opens under
/// `sealer` for the record at `offset` of the log stamped with `nonce`.
fn sealed_payload_verifies(
    file: &dyn ReadFile,
    at: u64,
    header: &Header,
    sealer: &Sealer,
    nonce: u64,
    offset: u64,
) -> io::Result<bool> {
    let mut payload = vec![0u8; header.len as usize];
    file.read_exact_at(at, &mut payload)?;
    Ok(
        super::wal_seal::open_record(sealer, &header.fields(), nonce, offset, &mut payload)
            .is_some_and(|ops| entries_are_whole(&payload[ops])),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WriteBatchOp;
    use crate::engine::wal;

    struct Bytes(Vec<u8>);

    impl ReadFile for Bytes {
        fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            let start = offset as usize;
            let src = self
                .0
                .get(start..start + buf.len())
                .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
            buf.copy_from_slice(src);
            Ok(())
        }
        fn len(&self) -> io::Result<u64> {
            Ok(self.0.len() as u64)
        }
    }

    fn entries() -> Vec<u8> {
        let mut out = Vec::new();
        wal::encode_ops_record(
            &mut out,
            &[
                WriteBatchOp::Put {
                    key: b"k".to_vec(),
                    value: b"v".to_vec(),
                },
                WriteBatchOp::Delete { key: b"d".to_vec() },
                WriteBatchOp::DeleteRange {
                    start: b"a".to_vec(),
                    end: b"b".to_vec(),
                },
                WriteBatchOp::Merge {
                    key: b"m".to_vec(),
                    operand: b"+".to_vec(),
                },
            ],
            7,
        );
        out
    }

    #[test]
    fn a_stamp_round_trips_and_any_damage_to_it_is_refused() {
        let stamp = encode_stamp(0xDEAD_BEEF_0123_4567);
        assert_eq!(stamp_nonce(&stamp).unwrap(), 0xDEAD_BEEF_0123_4567);
        for byte in 0..STAMP_LEN {
            let mut bad = stamp;
            bad[byte] ^= 0x10;
            assert!(stamp_nonce(&bad).is_err(), "damage at byte {byte} passed");
        }
    }

    #[test]
    fn a_header_verifies_only_in_its_own_log_at_its_own_offset() {
        let payload = entries();
        let header = encode_header(KIND_GROUP, &payload, 20, 99, 40);
        let decoded = decode_header(&header, 99, 40, false).expect("verifies where it was written");
        assert_eq!(decoded.len as usize, payload.len());
        assert_eq!(decoded.synced_through, 20);
        assert!(decoded.payload_matches(&payload));
        assert!(
            decode_header(&header, 98, 40, false).is_none(),
            "another log"
        );
        assert!(
            decode_header(&header, 99, 41, false).is_none(),
            "another offset"
        );
        for byte in 0..HEADER_LEN {
            let mut bad = header;
            bad[byte] ^= 0x01;
            assert!(
                decode_header(&bad, 99, 40, false).is_none(),
                "damage at byte {byte} passed"
            );
        }
    }

    #[test]
    fn a_header_that_claims_more_than_its_offset_is_refused() {
        let mut header = encode_header(KIND_GROUP, b"", 40, 5, 40);
        assert!(decode_header(&header, 5, 40, false).is_some());
        // Re-sign a claim one byte past the record, as only a broken writer
        // could produce, and check the reader still refuses it.
        header[5..13].copy_from_slice(&41u64.to_le_bytes());
        let check = checksum::wal_group_header(5, 40, header[..17].try_into().unwrap());
        header[17..21].copy_from_slice(&check.to_le_bytes());
        assert!(decode_header(&header, 5, 40, false).is_none());
    }

    #[test]
    fn close_proves_the_whole_file_and_a_group_proves_its_stamp() {
        let close =
            decode_header(&encode_header(KIND_CLOSE, b"", 30, 1, 30), 1, 30, false).unwrap();
        assert_eq!(close.proves(1000), 1000);
        let group =
            decode_header(&encode_header(KIND_GROUP, b"x", 30, 1, 30), 1, 30, false).unwrap();
        assert_eq!(group.proves(1000), 30);
    }

    #[test]
    fn every_operation_decodes_and_a_cut_anywhere_is_not_whole() {
        let bytes = entries();
        assert!(entries_are_whole(&bytes));
        let mut at = 0;
        let mut seqs = Vec::new();
        while at < bytes.len() {
            let (entry, len) = decode_entry(&bytes[at..]).unwrap();
            seqs.push(match entry {
                WalEntry::Put { seq, .. }
                | WalEntry::Delete { seq, .. }
                | WalEntry::DeleteRange { seq, .. }
                | WalEntry::Merge { seq, .. } => seq,
            });
            at += len;
        }
        assert_eq!(seqs, vec![7, 8, 9, 10]);
        for cut in 1..bytes.len() {
            let whole = entries_are_whole(&bytes[..cut]);
            // A put, a merge and a range delete of one-byte keys are 19
            // bytes each, a delete of a one-byte key 14.
            let boundary = [19, 33, 52].contains(&cut);
            assert_eq!(whole, boundary, "cut at {cut}");
        }
    }

    #[test]
    fn an_unknown_operation_type_is_not_whole() {
        assert!(!entries_are_whole(&[0x09, 0, 0, 0, 0]));
        assert!(decode_entry(&[0x09]).is_err());
    }

    /// A log of `n` groups whose stamps follow `claims`, starting at the
    /// stamp's end. Returns the bytes and each record's offset.
    fn log(nonce: u64, claims: &[u64]) -> (Vec<u8>, Vec<u64>) {
        let mut bytes = encode_stamp(nonce).to_vec();
        let mut offsets = Vec::new();
        let payload = entries();
        for &claim in claims {
            let offset = bytes.len() as u64;
            offsets.push(offset);
            bytes.extend_from_slice(&encode_header(
                KIND_GROUP,
                &payload,
                claim.min(offset),
                nonce,
                offset,
            ));
            bytes.extend_from_slice(&payload);
        }
        (bytes, offsets)
    }

    #[test]
    fn the_scan_finds_a_record_past_damage_that_proves_the_damage_synced() {
        let (mut bytes, offsets) = log(3, &[0, 0, u64::MAX]);
        // Damage the second record; the third claims everything before it.
        let damaged = offsets[1];
        bytes[damaged as usize + 2] ^= 0xFF;
        let end = bytes.len() as u64;
        let proof = proof_past(&Bytes(bytes), 3, damaged + 1, end, damaged, None).unwrap();
        assert_eq!(proof, Some(offsets[2]));
    }

    #[test]
    fn the_scan_passes_over_records_that_prove_nothing_past_the_damage() {
        let (mut bytes, offsets) = log(3, &[0, 0, 0, 0]);
        let damaged = offsets[1];
        bytes[damaged as usize + 30] ^= 0xFF;
        let end = bytes.len() as u64;
        assert_eq!(
            proof_past(&Bytes(bytes), 3, damaged + 1, end, damaged, None).unwrap(),
            None
        );
    }

    #[test]
    fn the_scan_finds_a_close_and_ignores_another_logs_records() {
        let (mut bytes, offsets) = log(3, &[0, 0]);
        let close_at = bytes.len() as u64;
        bytes.extend_from_slice(&encode_header(KIND_CLOSE, b"", close_at, 3, close_at));
        let end = bytes.len() as u64;
        let damaged = offsets[1];
        bytes[damaged as usize] ^= 0xFF;
        assert_eq!(
            proof_past(&Bytes(bytes.clone()), 3, damaged + 1, end, damaged, None).unwrap(),
            Some(end),
            "a usable CLOSE proves the whole file"
        );
        assert_eq!(
            proof_past(&Bytes(bytes), 4, damaged + 1, end, damaged, None).unwrap(),
            None,
            "records of another log never verify"
        );
    }

    #[test]
    fn the_scan_crosses_chunk_boundaries_and_rejects_a_damaged_payload() {
        let nonce = 11;
        let mut bytes = encode_stamp(nonce).to_vec();
        // Pad so the proving record straddles the first chunk boundary.
        let filler_at = bytes.len();
        bytes.resize(filler_at + SCAN_CHUNK - 7, 0x55);
        let offset = bytes.len() as u64;
        let payload = vec![0xC0; 3 * SCAN_CHUNK];
        bytes.extend_from_slice(&encode_header(KIND_GROUP, &payload, offset, nonce, offset));
        bytes.extend_from_slice(&payload);
        let end = bytes.len() as u64;
        let damaged = filler_at as u64;
        assert_eq!(
            proof_past(
                &Bytes(bytes.clone()),
                nonce,
                damaged + 1,
                end,
                damaged,
                None
            )
            .unwrap(),
            Some(offset)
        );
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert_eq!(
            proof_past(&Bytes(bytes), nonce, damaged + 1, end, damaged, None).unwrap(),
            None,
            "a record whose payload fails proves nothing"
        );
    }

    #[test]
    fn the_streamed_payload_check_matches_the_one_shot_check() {
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i * 31 % 251) as u8).collect();
        let mut hasher = checksum::WalGroupPayloadHasher::new();
        for piece in payload.chunks(4093) {
            hasher.update(piece);
        }
        assert_eq!(hasher.finish(), checksum::wal_group_payload(&payload));
    }
}
