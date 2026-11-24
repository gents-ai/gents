//! Format 1 of the write-ahead log, as 0.1.x wrote it: one record per
//! write, `[len u32][type u8][payload][checksum u32]` after a 12-byte
//! stamp. Nothing writes it any more; replay still reads it by the rules it
//! always was read by (see [`super::wal`]), and the tests of those rules
//! write it with the test-only writer at the end of this file.

use std::io::{self, Read};
use std::path::Path;

use super::checksum;
use super::wal::{
    RECORD_BATCH, RECORD_DELETE, RECORD_DELETE_RANGE, RECORD_MERGE, RECORD_PUT, TailVerdict,
    WalEntry, parse_delete_range_record, parse_delete_record, parse_merge_record, parse_put_record,
};
#[cfg(test)]
use super::wal::{
    WAL_MAGIC, WAL_STAMP_LEN, encode_delete_payload, encode_op_record, encode_put_payload,
};
#[cfg(test)]
use crate::WriteBatchOp;

/// The on-disk format 0.1.x wrote, still read. Only tests write it.
#[cfg(test)]
const WAL_FORMAT_V1: u16 = 1;

/// A format 1 record header: length and type.
const WAL_HEADER_LEN: usize = 5;
/// Trailing 4-byte little-endian checksum of every format 1 record.
const CHECKSUM_LEN: usize = 4;

/// One record framed inside a WAL file.
struct Frame<'a> {
    record_type: u8,
    data: &'a [u8],
    stored_checksum: u32,
    /// Offset one past this record's checksum: where the next record
    /// starts.
    end: usize,
}

impl Frame<'_> {
    /// Whether the stored checksum matches the bytes of this record.
    fn checksum_matches(&self) -> bool {
        let len = self.data.len() as u32;
        self.stored_checksum == checksum::wal_record(len, self.record_type, self.data)
    }
}

/// Frame the record starting at `offset`, or `None` when the file ends
/// inside it.
///
/// The length field is untrusted input, so nothing is sized from it until
/// the bytes it promises are known to be present: a five-byte header left
/// by a torn write cannot make recovery ask the allocator for 4 GiB.
fn frame_at(bytes: &[u8], offset: usize) -> Option<Frame<'_>> {
    let data_start = offset.checked_add(WAL_HEADER_LEN)?;
    let header = bytes.get(offset..data_start)?;
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let data_end = data_start.checked_add(len)?;
    let end = data_end.checked_add(CHECKSUM_LEN)?;
    let data = bytes.get(data_start..data_end)?;
    let stored = bytes.get(data_end..end)?;

    Some(Frame {
        record_type: header[4],
        data,
        stored_checksum: u32::from_le_bytes([stored[0], stored[1], stored[2], stored[3]]),
        end,
    })
}

/// Decide whether the incomplete record beginning at `record_start` is a
/// torn tail or real damage, for a reader that streams and therefore
/// cannot see the bytes past it. Format 1 only.
///
/// Reached at most once per file, and only when the log is already
/// damaged, so the whole-file read it needs is not on any healthy path.
/// A WAL is bounded by `write_buffer_size` plus the one record that
/// crossed it, so this bounds nothing the engine did not already hold.
///
/// `Ok` means torn: the records before it stand and the tail is
/// discarded, which recovery reports. `Err` means whole records follow
/// the damage, so the tail is loss rather than a torn write, and the open
/// is refused.
pub(super) fn classify_incomplete_record(
    env: &dyn crate::env::Env,
    path: &Path,
    record_start: u64,
) -> io::Result<TailVerdict> {
    // Through `Env`, like every other read on the recovery path. A
    // `std::fs::read` here would look on the real filesystem, so a
    // database on any other backend could not reopen the moment its
    // newest log had a partial tail: the ordinary shape of a crash.
    let bytes = env.read(path)?;
    let pos = usize::try_from(record_start)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    if let Some(next) = resync_after(&bytes, pos) {
        // The second offset is evidence, and the comment on
        // `resync_after` says why: whole records cannot follow a torn
        // write.
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "write-ahead log {} is damaged at offset {pos}, and whole records follow \
                 it from offset {next}; refusing to open rather than lose them",
                path.display()
            ),
        ));
    }
    Ok(TailVerdict::discarded(&bytes, pos))
}

/// Decide a record that framed cleanly but carries an unusable type or a
/// failing checksum. Format 1 only.
///
/// Bytes a crash never wrote read back as zeros, and a power cut can
/// zero a region that was already written. Either way an all-zero tail
/// is the end of the log, not damage: there is nothing after it to lose.
/// A bad record with anything non-zero behind it is real corruption and
/// refuses the open.
pub(super) fn classify_unusable_record(
    env: &dyn crate::env::Env,
    path: &Path,
    record_start: u64,
) -> io::Result<TailVerdict> {
    let bytes = env.read(path)?;
    let pos = usize::try_from(record_start)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    if !tail_is_unwritten(&bytes, pos) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "write-ahead log {} is damaged at offset {pos}; refusing to open",
                path.display()
            ),
        ));
    }
    Ok(TailVerdict::discarded(&bytes, pos))
}

/// Whether every byte from `pos` on is zero, which is how both a crash
/// that never wrote them and a power cut that zeroed them read back.
fn tail_is_unwritten(bytes: &[u8], pos: usize) -> bool {
    bytes[pos..].iter().all(|b| *b == 0)
}

/// Offset of the first whole, checksum-valid, known-type record after the
/// incomplete one at `pos` from which the rest of the file parses as
/// nothing but whole records, ending exactly at the last byte.
///
/// `Some` means real records lie beyond the damage, so the length field of
/// the record at `pos` was mangled rather than its write being torn.
///
/// Requiring the whole remainder to tile is what makes this affordable.
/// Testing each offset on its own would checksum every candidate payload,
/// and a torn tail whose bytes read as plausible lengths (a large value of
/// repeated `0x01` bytes, say) would put gigabytes through the hash per
/// megabyte of tail. The tiling is computed once, backwards, in a single
/// linear pass: each offset is `Some` frame plus one lookup of the answer
/// already computed for the offset the frame ends at. Only the handful of
/// offsets that survive that are ever checksummed. It also sharpens the
/// evidence, because a chance 32-bit checksum match inside a partly
/// written payload now has to land on a tiling as well before it can
/// refuse an open.
fn resync_after(bytes: &[u8], pos: usize) -> Option<usize> {
    let scan_start = pos + 1;
    if scan_start >= bytes.len() {
        return None;
    }

    let mut tiles = vec![false; bytes.len() - scan_start];
    let mut first = None;

    for offset in (scan_start..bytes.len()).rev() {
        let Some(frame) = frame_at(bytes, offset) else {
            continue;
        };
        if frame.end != bytes.len() && !tiles[frame.end - scan_start] {
            continue;
        }
        tiles[offset - scan_start] = true;

        if matches!(
            frame.record_type,
            RECORD_PUT | RECORD_DELETE | RECORD_DELETE_RANGE | RECORD_MERGE | RECORD_BATCH
        ) && frame.checksum_matches()
        {
            first = Some(offset);
        }
    }

    first
}

pub(super) fn read_wal_header(reader: &mut impl Read) -> io::Result<Option<[u8; 5]>> {
    let mut header = [0u8; 5];
    let mut read = 0;

    while read < header.len() {
        match reader.read(&mut header[read..]) {
            Ok(0) if read == 0 => return Ok(None),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated WAL record header",
                ));
            }
            Ok(n) => read += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }

    Ok(Some(header))
}

pub(super) fn parse_batch_record(data: &[u8]) -> io::Result<Vec<WalEntry>> {
    if data.len() < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "batch record too short",
        ));
    }

    let mut pos = 0;
    let count = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    pos += 4;

    let mut entries = Vec::new();
    for _ in 0..count {
        if pos + 5 > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "batch entry header overflow",
            ));
        }

        let record_type = data[pos];
        pos += 1;

        let payload_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        if payload_len > data.len().saturating_sub(pos) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "batch entry payload overflow",
            ));
        }

        let payload = &data[pos..pos + payload_len];
        pos += payload_len;

        let entry = match record_type {
            RECORD_PUT => parse_put_record(payload)?,
            RECORD_DELETE => parse_delete_record(payload)?,
            RECORD_DELETE_RANGE => parse_delete_range_record(payload)?,
            RECORD_MERGE => parse_merge_record(payload)?,
            RECORD_BATCH => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "nested batch records are not supported",
                ));
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown WAL batch entry type {record_type}"),
                ));
            }
        };
        entries.push(entry);
    }

    if pos != data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "batch record has trailing bytes",
        ));
    }

    Ok(entries)
}

/// Encode a format 1 stamp, as 0.1.x began every log.
#[cfg(test)]
fn encode_wal_stamp() -> [u8; WAL_STAMP_LEN] {
    let mut out = [0u8; WAL_STAMP_LEN];
    out[0..4].copy_from_slice(&WAL_MAGIC);
    out[4..6].copy_from_slice(&WAL_FORMAT_V1.to_le_bytes());
    out[6..8].copy_from_slice(&0u16.to_le_bytes());
    let checksum = checksum::wal_stamp(&WAL_MAGIC, WAL_FORMAT_V1, 0);
    out[8..12].copy_from_slice(&checksum.to_le_bytes());
    out
}

/// Frame one format 1 record: `[len u32][type u8][payload][checksum
/// u32]`, the checksum covering the length and type too.
#[cfg(test)]
pub(crate) fn encode_record(out: &mut Vec<u8>, record_type: u8, payload: &[u8]) {
    let len = payload.len() as u32;
    out.extend_from_slice(&len.to_le_bytes());
    out.push(record_type);
    out.extend_from_slice(payload);
    out.extend_from_slice(&checksum::wal_record(len, record_type, payload).to_le_bytes());
}

/// A format 1 put record.
#[cfg(test)]
pub(crate) fn put(key: &[u8], value: &[u8], seq: u64) -> Vec<u8> {
    let mut payload = Vec::new();
    encode_put_payload(&mut payload, key, value, seq);
    let mut out = Vec::new();
    encode_record(&mut out, RECORD_PUT, &payload);
    out
}

/// A format 1 delete record.
#[cfg(test)]
pub(crate) fn delete(key: &[u8], seq: u64) -> Vec<u8> {
    let mut payload = Vec::new();
    encode_delete_payload(&mut payload, key, seq);
    let mut out = Vec::new();
    encode_record(&mut out, RECORD_DELETE, &payload);
    out
}

/// A format 1 batch record holding `ops` from `base_seq`.
#[cfg(test)]
pub(crate) fn batch(ops: &[WriteBatchOp], base_seq: u64) -> Vec<u8> {
    let mut payload = (ops.len() as u32).to_le_bytes().to_vec();
    for (i, op) in ops.iter().enumerate() {
        let mut entry = Vec::new();
        encode_op_record(&mut entry, op, base_seq + i as u64);
        // A group entry is `[type][payload]`; a batch entry carries the
        // payload length between them.
        payload.push(entry[0]);
        payload.extend_from_slice(&((entry.len() - 1) as u32).to_le_bytes());
        payload.extend_from_slice(&entry[1..]);
    }
    let mut out = Vec::new();
    encode_record(&mut out, RECORD_BATCH, &payload);
    out
}

/// The format 1 stamp.
#[cfg(test)]
pub(crate) fn stamp() -> [u8; WAL_STAMP_LEN] {
    encode_wal_stamp()
}

/// A format 1 log at `path` holding `records`, as 0.1.x wrote it.
#[cfg(test)]
pub(crate) fn write_log(path: &Path, records: &[Vec<u8>]) {
    let mut bytes = stamp().to_vec();
    for record in records {
        bytes.extend_from_slice(record);
    }
    std::fs::write(path, bytes).unwrap();
}

#[cfg(test)]
#[path = "wal_v1_tests.rs"]
mod tests;
