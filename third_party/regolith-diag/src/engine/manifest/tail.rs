//! The end of a manifest whose replay stopped short of it (E29), and the
//! one reader of a batch, which replay and the judgment share.
//!
//! # The rule
//!
//! [`VersionSet::apply`](super::VersionSet::apply) syncs each batch it
//! appends before it returns, unless every record in it only reserves file
//! ids or retires logs ([`ManifestRecord::requires_sync`]), and one apply
//! runs at a time. So at a power cut, the bytes no completed sync covers
//! are a run of batches that needed no sync, then at most one batch whose
//! sync was under way, written last. A crash damages only those bytes.
//!
//! Replay stops at the first batch that does not read back whole, at
//! offset O. Whether O lay in that unsynced run is decided by the bytes
//! after it, the way plan 4.2 decides it for a write-ahead log:
//!
//! - A batch after O that reads back whole, needed a sync, and has bytes
//!   after it proves O durable: those bytes were written only once its sync
//!   had completed, and that sync covered every byte before it. The open
//!   refuses, naming the file, O, and the end P of that batch. No crash
//!   leaves this state.
//! - With no such batch, a crash can have left every byte from O on, and
//!   the open drops them as an unsynced tail and reports it.
//!
//! A crash may cut that tail short, zero it, fill it with unrelated bytes or
//! tear it at a sector. A checksum that holds by chance in unrelated bytes,
//! about one test in 2^32, can only refuse an open that could have
//! succeeded, never open one that should not.
//!
//! # Sealed manifests
//!
//! On an encrypted database every batch is sealed (`sealed.rs`), and its
//! records are noise until the batch is opened under the key it names.
//! Noise decodes as nothing this build knows, which [`needs_sync`] must count
//! as a batch that needed a sync, so a judge reading sealed batches as plain
//! ones would take every whole reservation after a torn one as proof, and
//! refuse a crash's tail. Each batch is therefore opened first
//! (`ManifestRecovery.tla`, RED JudgeCiphertext). One that does not open, a
//! key the provider lacks or a tag that fails, still counts as needing a
//! sync: a crash leaves only batches the writer sealed, under its key.
//!
//! # What it cannot see
//!
//! Damage to the last batch that needed a sync, with no batch needing a
//! sync after it, reads exactly like that batch's sync never completing:
//! the open drops it and reports it, never silently. A write-ahead log has
//! the same residual in its last synced group (plan 4.2, item 5).
//!
//! The judgment also assumes a crash never leaves a whole batch of an
//! older manifest where the unsynced tail was: such a batch would verify
//! wherever it landed, because a batch does not carry its offset. No tear
//! the fault shim models does that, and neither does ext4 in its default
//! ordered mode.
//!
//! # Cost
//!
//! Nothing for a manifest that replays whole. For a damaged one, every
//! offset after O is tested once; a test reads a 4-byte length and stops
//! there unless the batch it claims fits in the file, which on zeros, text
//! and random bytes it almost never does.

use std::io;
use std::path::Path;

use super::ManifestRecord;
use crate::engine::checksum;
use crate::engine::seal::Keyring;

/// Bytes of a batch's framing: its length in front, its checksum behind.
const FRAMING: usize = 8;

/// A batch that reads back whole.
pub(super) struct Batch<'a> {
    /// The encoded records it carries.
    pub(super) records: &'a [u8],
    /// The offset just past it, where the next batch begins.
    pub(super) end: usize,
}

/// The batch at `at` of `data`, or `None` when the bytes there are not one
/// whole batch: a length that runs past the end of the file, or a checksum
/// that does not match.
///
/// ```text
/// batch   [len u32][records: len bytes][checksum u32]
/// ```
pub(super) fn batch_at(data: &[u8], at: usize) -> Option<Batch<'_>> {
    let rest = data.get(at..)?;
    let len = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?);
    let records_end = 4usize.checked_add(len as usize)?;
    let end = records_end.checked_add(4)?;
    let records = rest.get(4..records_end)?;
    let stored = u32::from_le_bytes(rest.get(records_end..end)?.try_into().ok()?);
    (stored == checksum::manifest_record(len, records)).then_some(Batch {
        records,
        end: at + end,
    })
}

/// What a sealed manifest's batches are opened with: the database's keyring
/// and the manifest's salt, which every batch's tag is bound to.
#[derive(Clone, Copy)]
pub(super) struct Opener<'a> {
    pub(super) keyring: &'a Keyring,
    pub(super) salt: &'a [u8; 16],
}

/// The end of a manifest an open dropped as a crash's unsynced tail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DroppedTail {
    /// Where the first batch that did not read back whole began.
    pub(crate) offset: u64,
    /// Bytes from there to the end of the file.
    pub(crate) bytes: u64,
}

/// Judge the bytes of `data` from `damage_at`, where replay stopped, to the
/// end: the tail to drop when a crash can have left them, or an error naming
/// the file and both offsets when a later batch proves them synced. A sealed
/// manifest's batches are opened through `sealed` before they are read.
pub(super) fn judge(
    data: &[u8],
    damage_at: usize,
    path: &Path,
    sealed: Option<Opener<'_>>,
) -> io::Result<DroppedTail> {
    match proof_past(data, damage_at, sealed) {
        Some(proven) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is damaged at offset {damage_at}, below offset {proven} that a later batch \
                 proves durable; the database is left untouched",
                path.display()
            ),
        )),
        None => Ok(DroppedTail {
            offset: damage_at as u64,
            bytes: (data.len() - damage_at) as u64,
        }),
    }
}

/// The end of the first batch after `damage_at` that proves `damage_at`
/// durable: it reads back whole, it needed a sync, and bytes follow it.
///
/// The damaged batch's own length cannot be trusted, so every offset after
/// it is tested; once a batch reads back whole, the next test is where it
/// ends.
// vertexia: a tail crafted so that many offsets claim a batch that fits costs
// a checksum over each, quadratic in the tail's length; a batch header that
// carries its own offset and check would make each test constant.
fn proof_past(data: &[u8], damage_at: usize, sealed: Option<Opener<'_>>) -> Option<usize> {
    let mut at = damage_at + 1;
    while at + FRAMING <= data.len() {
        match batch_at(data, at) {
            Some(batch) if batch.end < data.len() && needs_sync(batch.records, at, sealed) => {
                return Some(batch.end);
            }
            Some(batch) => at = batch.end,
            None => at += 1,
        }
    }
    None
}

/// Whether the batch at `at` holding `records` was synced before the next
/// was written. A sealed batch is opened first, and one that does not open
/// counts as synced, as does one this build cannot decode: nothing shows it
/// needed no sync.
fn needs_sync(records: &[u8], at: usize, sealed: Option<Opener<'_>>) -> bool {
    let opened;
    let records = match sealed {
        Some(opener) => {
            match super::sealed::open_batch(opener.keyring, opener.salt, at as u64, records) {
                Ok((_, plain)) => {
                    opened = plain;
                    opened.as_slice()
                }
                Err(_) => return true,
            }
        }
        None => records,
    };
    let mut pos = 0;
    loop {
        match ManifestRecord::decode(records, &mut pos) {
            Ok(Some(record)) if record.requires_sync() => return true,
            Ok(Some(_)) => {}
            Ok(None) => return false,
            Err(_) => return true,
        }
    }
}

#[cfg(test)]
#[path = "tail_tests.rs"]
mod tests;
