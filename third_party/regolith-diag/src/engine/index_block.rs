//! Decoded SSTable index blocks.
//!
//! On-disk format, written by `sstable::encode_index_block`:
//!
//! ```text
//! [count: u32 LE]
//! then `count` times:
//!   [key_len: u32 LE][key bytes][handle.offset: u64 LE][handle.size: u64 LE]
//! ```
//!
//! Each key is the **last internal key** of the block the handle points
//! at, so a search for the first entry whose key is `>= target` names the
//! block that may contain `target`.
//!
//! One representation serves all three shapes an index takes: the flat
//! index of a V1 file, the top-level index of a V2 (partitioned) file,
//! and each leaf of a V2 file. Holding the raw bytes whole plus a table
//! of slots costs two allocations per index block instead of one per
//! entry, which is what makes a partitioned file's leaves cheap enough
//! to load through the block cache on demand.

use std::io;

use super::block::BlockHandle;
use super::internal_key::{compare_internal_keys, compare_internal_split};

/// One index entry: where its key lives inside the blob, and the data
/// block (or leaf) it points at.
#[derive(Debug, Clone, Copy)]
struct IndexSlot {
    key_offset: u32,
    key_len: u32,
    handle: BlockHandle,
}

/// A decoded index block: the raw encoded bytes plus a slot table.
///
/// Keys are borrowed out of `blob`; nothing is copied per entry.
pub(crate) struct IndexBlock {
    blob: Vec<u8>,
    slots: Vec<IndexSlot>,
}

impl IndexBlock {
    /// Decode an index block from the bytes of its file region.
    ///
    /// Takes the buffer by value so the region read from disk becomes
    /// the block's storage rather than being copied entry by entry.
    /// Rejects a truncated block, a declared entry count the bytes
    /// cannot support, and trailing bytes past the last entry.
    pub(crate) fn decode(blob: Vec<u8>) -> io::Result<Self> {
        if blob.len() < 4 {
            return Err(invalid_data("index block too short"));
        }
        if u32::try_from(blob.len()).is_err() {
            return Err(invalid_data("index block is too large to address"));
        }

        let num_entries = u32::from_le_bytes(blob[0..4].try_into().unwrap()) as usize;
        // Smallest possible entry is 4 (key_len) + 0 (key) + 16 (handle).
        let max_entries_by_size = (blob.len() - 4) / 20;
        let mut slots = Vec::with_capacity(num_entries.min(max_entries_by_size));
        let mut pos = 4usize;

        for _ in 0..num_entries {
            let key_len = read_u32(&blob, &mut pos, "index key length")? as usize;
            let key_offset = pos;
            let key_end = pos
                .checked_add(key_len)
                .ok_or_else(|| invalid_data("index key length overflows"))?;
            if key_end > blob.len() {
                return Err(invalid_data("index key is truncated"));
            }
            pos = key_end;
            let offset = read_u64(&blob, &mut pos, "index block offset")?;
            let size = read_u64(&blob, &mut pos, "index block size")?;

            slots.push(IndexSlot {
                key_offset: key_offset as u32,
                key_len: key_len as u32,
                handle: BlockHandle { offset, size },
            });
        }
        if pos != blob.len() {
            return Err(invalid_data("index block has trailing bytes"));
        }

        Ok(Self { blob, slots })
    }

    /// Number of entries.
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the block has no entries. An empty index block is legal
    /// on disk (an SSTable with only range tombstones writes one).
    pub(crate) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The internal key of entry `i`, borrowed from the block's bytes.
    ///
    /// Production searches go through [`IndexBlock::seek`]; this exists
    /// so the decode tests can check the bytes round-trip.
    #[cfg(test)]
    pub(crate) fn key(&self, i: usize) -> Option<&[u8]> {
        let slot = self.slots.get(i)?;
        let start = slot.key_offset as usize;
        let end = start + slot.key_len as usize;
        self.blob.get(start..end)
    }

    /// The handle of entry `i`.
    pub(crate) fn handle(&self, i: usize) -> Option<BlockHandle> {
        self.slots.get(i).map(|slot| slot.handle)
    }

    /// Index of the first entry whose key is `>= target`, or [`len`]
    /// when every key is strictly less than `target`.
    ///
    /// Comparison goes through [`compare_internal_keys`], never raw byte
    /// order: index keys are internal keys, whose sequence trailer sorts
    /// descending.
    ///
    /// [`len`]: IndexBlock::len
    pub(crate) fn seek(&self, target: &[u8]) -> usize {
        self.partition_point(target)
    }

    /// [`IndexBlock::seek`] for the internal key `user_key || trailer`,
    /// without concatenating it.
    pub(crate) fn seek_split(&self, user_key: &[u8], trailer: &[u8]) -> usize {
        self.partition_by(|key| compare_internal_split(key, user_key, trailer).is_lt())
    }

    /// Sum of the handle sizes of every entry whose key falls between
    /// the two probes, clamped to the last entry so a `hi` past the end
    /// still charges the final block.
    ///
    /// Both probes are internal keys built at `u64::MAX`, matching the
    /// ordering the index itself is stored in.
    pub(crate) fn approximate_size_in_range(&self, lo_probe: &[u8], hi_probe: &[u8]) -> u64 {
        if self.slots.is_empty() {
            return 0;
        }
        let first = self.partition_point(lo_probe);
        let last = self.partition_point(hi_probe);
        let end_idx = last.min(self.slots.len() - 1);
        if first > end_idx {
            return 0;
        }
        self.slots[first..=end_idx]
            .iter()
            .map(|slot| slot.handle.size)
            .sum()
    }

    /// Heap bytes held by this block, for block-cache charging.
    pub(crate) fn charge(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.blob.capacity()
            + self.slots.capacity() * std::mem::size_of::<IndexSlot>()
    }

    fn partition_point(&self, target: &[u8]) -> usize {
        self.partition_by(|key| compare_internal_keys(key, target).is_lt())
    }

    /// Index of the first entry whose key `below` rejects.
    fn partition_by(&self, mut below: impl FnMut(&[u8]) -> bool) -> usize {
        self.slots.partition_point(|slot| {
            let start = slot.key_offset as usize;
            let end = start + slot.key_len as usize;
            // The slot table is built from `blob` in `decode`, so this
            // range is always in bounds; an empty fallback keeps the
            // comparison total rather than panicking if it ever is not.
            below(self.blob.get(start..end).unwrap_or(&[]))
        })
    }
}

impl std::fmt::Debug for IndexBlock {
    /// Shape only. The blob is index keys, not payload, but printing a
    /// whole index block is noise in every context that would ask.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexBlock")
            .field("entries", &self.slots.len())
            .field("bytes", &self.blob.len())
            .finish()
    }
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_u32(data: &[u8], pos: &mut usize, field: &'static str) -> io::Result<u32> {
    let end = pos
        .checked_add(4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("{field} overflows")))?;
    if end > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{field} is truncated"),
        ));
    }
    let value = u32::from_le_bytes(data[*pos..end].try_into().unwrap());
    *pos = end;
    Ok(value)
}

fn read_u64(data: &[u8], pos: &mut usize, field: &'static str) -> io::Result<u64> {
    let end = pos
        .checked_add(8)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("{field} overflows")))?;
    if end > data.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{field} is truncated"),
        ));
    }
    let value = u64::from_le_bytes(data[*pos..end].try_into().unwrap());
    *pos = end;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::internal_key::{VALUE_TYPE_VALUE, encode_internal_key};

    fn encode(entries: &[(Vec<u8>, BlockHandle)]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (key, handle) in entries {
            data.extend_from_slice(&(key.len() as u32).to_le_bytes());
            data.extend_from_slice(key);
            data.extend_from_slice(&handle.offset.to_le_bytes());
            data.extend_from_slice(&handle.size.to_le_bytes());
        }
        data
    }

    fn ik(key: &[u8], seq: u64) -> Vec<u8> {
        encode_internal_key(key, seq, VALUE_TYPE_VALUE)
    }

    fn sample() -> Vec<(Vec<u8>, BlockHandle)> {
        vec![
            (
                ik(b"aaa", 1),
                BlockHandle {
                    offset: 0,
                    size: 10,
                },
            ),
            (
                ik(b"mmm", 2),
                BlockHandle {
                    offset: 10,
                    size: 20,
                },
            ),
            (
                ik(b"zzz", 3),
                BlockHandle {
                    offset: 30,
                    size: 30,
                },
            ),
        ]
    }

    #[test]
    fn decode_roundtrip_preserves_keys_and_handles() {
        let entries = sample();
        let block = IndexBlock::decode(encode(&entries)).unwrap();
        assert_eq!(block.len(), entries.len());
        for (i, (key, handle)) in entries.iter().enumerate() {
            assert_eq!(block.key(i).unwrap(), key.as_slice());
            let got = block.handle(i).unwrap();
            assert_eq!(got.offset, handle.offset);
            assert_eq!(got.size, handle.size);
        }
    }

    #[test]
    fn decode_rejects_short_block() {
        let err = IndexBlock::decode(vec![0u8; 3]).expect_err("too short");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let mut bytes = encode(&sample());
        bytes.push(0);
        let err = IndexBlock::decode(bytes).expect_err("trailing bytes");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_truncated_entry() {
        let mut bytes = encode(&sample());
        bytes.truncate(bytes.len() - 4);
        let err = IndexBlock::decode(bytes).expect_err("truncated");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_absurd_entry_count() {
        let mut bytes = encode(&sample());
        bytes[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        let err = IndexBlock::decode(bytes).expect_err("count overflows bytes");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn empty_index_block_decodes() {
        let block = IndexBlock::decode(encode(&[])).unwrap();
        assert!(block.is_empty());
        assert_eq!(block.seek(&ik(b"any", 1)), 0);
        assert_eq!(
            block.approximate_size_in_range(&ik(b"a", 1), &ik(b"z", 1)),
            0
        );
    }

    #[test]
    fn seek_matches_linear_scan() {
        let entries = sample();
        let block = IndexBlock::decode(encode(&entries)).unwrap();
        for probe in [
            ik(b"a", u64::MAX),
            ik(b"aaa", 1),
            ik(b"b", u64::MAX),
            ik(b"mmm", 2),
            ik(b"zzz", 3),
            ik(b"zzzz", u64::MAX),
        ] {
            let expected = entries
                .iter()
                .position(|(key, _)| !compare_internal_keys(key, &probe).is_lt())
                .unwrap_or(entries.len());
            assert_eq!(block.seek(&probe), expected, "probe {probe:?}");
        }
    }

    #[test]
    fn key_and_handle_are_none_out_of_range() {
        let block = IndexBlock::decode(encode(&sample())).unwrap();
        assert!(block.key(3).is_none());
        assert!(block.handle(3).is_none());
    }

    #[test]
    fn charge_covers_blob_and_slots() {
        let block = IndexBlock::decode(encode(&sample())).unwrap();
        assert!(block.charge() > block.len() * std::mem::size_of::<IndexSlot>());
    }

    #[test]
    fn approximate_size_sums_covered_handles() {
        let block = IndexBlock::decode(encode(&sample())).unwrap();
        let all = block.approximate_size_in_range(&ik(b"a", u64::MAX), &ik(b"zzzz", u64::MAX));
        assert_eq!(all, 60);
        let past_end =
            block.approximate_size_in_range(&ik(b"zzzz", u64::MAX), &ik(b"zzzzz", u64::MAX));
        assert_eq!(
            past_end, 0,
            "a range entirely past the last key covers nothing"
        );
        let middle = block.approximate_size_in_range(&ik(b"mmm", 2), &ik(b"nnn", u64::MAX));
        assert_eq!(middle, 20 + 30);
    }
}
