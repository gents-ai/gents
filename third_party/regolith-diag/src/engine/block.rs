//! SSTable data blocks: prefix-compressed key-value entries with restart points.
//!
//! Block format:
//! ```text
//! [entry_0][entry_1]...[entry_n][restart_0][restart_1]...[restart_m][num_restarts: u32]
//! ```
//!
//! Each entry is: `[shared: varint][unshared: varint][value_len: varint][key_suffix][value]`,
//! where `shared` is the length of the prefix shared with the previous key. Every
//! `RESTART_INTERVAL` entries we reset `shared = 0` and record the entry's byte
//! offset as a restart point, enabling binary search within the block.

use std::cmp::Ordering;
use std::io;
use std::ops::ControlFlow;

use super::internal_key::INTERNAL_KEY_SUFFIX_LEN;
#[cfg(test)]
use super::internal_key::compare_internal_keys;

mod stamp;

/// Entries per restart point. Smaller = faster lookups, larger = better compression.
pub(crate) const RESTART_INTERVAL: usize = 16;

/// Offset and size of a block within an SSTable file.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BlockHandle {
    pub(crate) offset: u64,
    pub(crate) size: u64,
}

/// A decoded data block containing sorted key-value entries.
pub(crate) struct Block {
    data: Vec<u8>,
    restarts: Vec<u32>,
    entries_end: usize,
    /// For a block of an ingested table, the `!seq` bytes every entry's key
    /// reads with in place of the ones it stores (see the `stamp` module).
    stamp: Option<[u8; 8]>,
}

impl Block {
    pub(crate) fn decoded_buffer_capacity(&self) -> usize {
        // LZ4 can decode fewer bytes than its declared buffer length.
        self.data.capacity()
    }

    /// Decode a block whose entry keys are opaque bytes.
    ///
    /// Only unit tests build blocks this way. Every production path
    /// reads SSTable data blocks and must go through
    /// [`Block::decode_data_block`], which additionally enforces the
    /// internal-key shape that the decoders downstream assume.
    #[cfg(test)]
    pub(crate) fn decode(data: Vec<u8>) -> io::Result<Self> {
        Self::decode_inner(data, false)
    }

    /// Decode an SSTable data block.
    ///
    /// Rejects any entry whose key is shorter than
    /// [`INTERNAL_KEY_SUFFIX_LEN`], so a truncated, tampered, or
    /// foreign-produced file cannot hand a short key to
    /// `decode_internal_key`, which indexes the trailer directly.
    pub(crate) fn decode_data_block(data: Vec<u8>) -> io::Result<Self> {
        Self::decode_inner(data, true)
    }

    fn decode_inner(data: Vec<u8>, require_internal_keys: bool) -> io::Result<Self> {
        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "block too small",
            ));
        }

        let num_restarts = u32::from_le_bytes(data[data.len() - 4..].try_into().unwrap()) as usize;
        if num_restarts == 0 {
            return Err(invalid_data("block has no restart points"));
        }
        let restart_bytes = num_restarts
            .checked_mul(4)
            .ok_or_else(|| invalid_data("block restart array size overflows"))?;
        let entries_end = data
            .len()
            .checked_sub(4)
            .and_then(|len| len.checked_sub(restart_bytes))
            .ok_or_else(|| invalid_data("block restart array exceeds block length"))?;

        let mut restarts = Vec::with_capacity(num_restarts);
        for i in 0..num_restarts {
            let offset = entries_end + i * 4;
            restarts.push(u32::from_le_bytes(
                data[offset..offset + 4].try_into().unwrap(),
            ));
        }
        validate_restarts_and_entries(&data[..entries_end], &restarts, require_internal_keys)?;

        Ok(Self {
            data,
            restarts,
            entries_end,
            stamp: None,
        })
    }

    /// This block, every entry of it reading at `seq` whatever sequence
    /// its key stores: a block of an ingested table. Its entries must hold
    /// at most one key each, which the ingest checks.
    pub(crate) fn stamped(mut self, seq: u64) -> Self {
        self.stamp = Some((!seq).to_be_bytes());
        self
    }

    /// The stamp its entries read with, for a block of an ingested table.
    pub(crate) fn stamp(&self) -> Option<[u8; 8]> {
        self.stamp
    }

    /// How `key`, an entry of this block as stored, orders against
    /// `target` as this block reads it.
    #[inline]
    pub(crate) fn compare_entry(&self, key: &[u8], target: &[u8]) -> Ordering {
        stamp::compare(key, self.stamp, target)
    }

    /// `key`, an entry of this block as stored, as an owned copy a reader
    /// sees.
    pub(crate) fn owned_key(&self, key: &[u8]) -> Vec<u8> {
        let mut owned = key.to_vec();
        if let Some(stamp) = self.stamp {
            stamp::swap(&mut owned, stamp);
        }
        owned
    }

    /// `key`, an entry of this block as stored, as a reader sees it: the
    /// stored key itself, or for a stamped block a copy in `out` that
    /// carries the stamp.
    pub(crate) fn read_key<'a>(&self, key: &'a [u8], out: &'a mut Vec<u8>) -> &'a [u8] {
        match self.stamp {
            Some(stamp) => {
                stamp::read_into(key, stamp, out);
                out
            }
            None => key,
        }
    }

    /// Approximate heap bytes held by this block. Used by the
    /// block cache to charge accurate sizes against its capacity
    /// budget. Includes the backing `Vec` allocations plus the
    /// struct itself; excludes any amortized allocator overhead,
    /// which is typically small and not worth modeling.
    pub(crate) fn charge(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.data.capacity()
            + self.restarts.capacity() * std::mem::size_of::<u32>()
    }

    /// The entry region of the block (everything before the restart
    /// array and the trailing `num_restarts` u32).
    pub(crate) fn entry_data(&self) -> &[u8] {
        &self.data[..self.entries_end]
    }

    pub(crate) fn restart_count(&self) -> usize {
        self.restarts.len()
    }

    pub(crate) fn restart_offset(&self, idx: usize) -> usize {
        self.restarts[idx] as usize
    }

    /// Borrow `offset..offset + len` of the block's entry region.
    ///
    /// Returns `None` when the range escapes that region, so a value
    /// handle recovered from a corrupt block can never widen into the
    /// restart array or past the buffer.
    pub(crate) fn entry_bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        if end > self.entries_end {
            return None;
        }
        self.data.get(offset..end)
    }

    /// Scan forward from the restart point covering `target`, invoking
    /// `f` with each entry's reconstructed key and the position of its
    /// **borrowed** value inside the entry region.
    ///
    /// Returns as soon as `f` yields [`ControlFlow::Break`], or `None`
    /// when the block is exhausted. Keys are reconstructed into
    /// `key_buf`, which the caller owns and reuses across blocks, so a
    /// scan allocates nothing once that buffer has grown.
    ///
    /// The closure receives `(key, value_offset, value_len)` rather than
    /// a value slice so it can build an owning view over the whole block
    /// (see [`crate::DbSlice`]) without holding a borrow of it. A stamped
    /// block hands it each key with the stamp, put in place for the call and
    /// taken out before the next key is reconstructed from it.
    pub(crate) fn scan_from<F, R>(&self, target: &[u8], key_buf: &mut Vec<u8>, f: F) -> Option<R>
    where
        F: FnMut(&[u8], usize, usize) -> ControlFlow<R>,
    {
        // Decided once per scan, so a block of a table regolith wrote walks
        // exactly as it would with no stamps in the engine at all.
        match self.stamp {
            None => self.walk_from::<false, F, R>(target, key_buf, [0; 8], f),
            Some(stamp) => self.walk_from::<true, F, R>(target, key_buf, stamp, f),
        }
    }

    /// [`Block::scan_from`], with `STAMPED` saying whether `stamp` applies.
    #[inline(always)]
    fn walk_from<const STAMPED: bool, F, R>(
        &self,
        target: &[u8],
        key_buf: &mut Vec<u8>,
        stamp: [u8; 8],
        mut f: F,
    ) -> Option<R>
    where
        F: FnMut(&[u8], usize, usize) -> ControlFlow<R>,
    {
        let data = self.entry_data();
        let mut pos = self.restart_start_for(target);
        // Restart entries never share a prefix, so an empty buffer is
        // the correct starting state for the walk.
        key_buf.clear();
        let mut below_target = true;
        while pos < data.len() {
            let (consumed, value_offset, value_len) = decode_entry_at(data, pos, key_buf);
            pos += consumed;
            if below_target {
                if stamp::compare(key_buf, STAMPED.then_some(stamp), target).is_lt() {
                    continue;
                }
                below_target = false;
            }
            if !STAMPED {
                if let ControlFlow::Break(result) = f(key_buf, value_offset, value_len) {
                    return Some(result);
                }
                continue;
            }
            let stored = stamp::swap(key_buf, stamp);
            if let ControlFlow::Break(result) = f(key_buf, value_offset, value_len) {
                return Some(result);
            }
            stamp::swap(key_buf, stored);
        }
        None
    }

    /// Iterate all entries in this block in sorted order.
    #[cfg(test)]
    pub(crate) fn iter(&self) -> BlockIterator<'_> {
        BlockIterator {
            data: self.entry_data(),
            pos: 0,
            current_key: Vec::new(),
            lower_bound: None,
        }
    }

    /// Iterate entries starting at the first key `>= target`.
    ///
    /// The iterator begins at the restart point immediately before
    /// `target` and skips entries until the lower bound is reached.
    ///
    /// Copies every key and value it yields. The read paths use
    /// [`Block::scan_from`] instead, which borrows; this is retained for
    /// the block round-trip tests.
    #[cfg(test)]
    pub(crate) fn iter_from<'a>(&'a self, target: &'a [u8]) -> BlockIterator<'a> {
        BlockIterator {
            data: self.entry_data(),
            pos: self.restart_start_for(target),
            current_key: Vec::new(),
            lower_bound: Some(target),
        }
    }

    /// Return the first `(key, value)` entry with `key >= target`, if any.
    /// Binary-search restart points for the first entry whose key could be
    /// `>= target`; returns the byte offset to start the linear walk from.
    fn restart_start_for(&self, target: &[u8]) -> usize {
        let data = self.entry_data();
        if data.is_empty() {
            return 0;
        }
        let stamp = self.stamp;
        let mut left = 0;
        let mut right = self.restarts.len();
        while left < right {
            let mid = left + (right - left) / 2;
            let restart_pos = self.restarts[mid] as usize;
            let header =
                decode_entry_header(data, restart_pos).expect("block entry validated at decode");
            // `Block::decode` rejects a restart entry that shares a key
            // prefix, so the unshared bytes are the whole key and the
            // probe needs no reconstruction buffer.
            debug_assert_eq!(header.shared, 0);
            let key = &data[header.key_offset..header.key_offset + header.unshared];
            if stamp::compare(key, stamp, target).is_lt() {
                left = mid + 1;
            } else {
                right = mid;
            }
        }
        if left > 0 {
            self.restarts[left - 1] as usize
        } else {
            0
        }
    }
}

#[cfg(test)]
pub(crate) struct BlockIterator<'a> {
    data: &'a [u8],
    pos: usize,
    current_key: Vec<u8>,
    lower_bound: Option<&'a [u8]>,
}

#[cfg(test)]
impl<'a> Iterator for BlockIterator<'a> {
    type Item = (Vec<u8>, Vec<u8>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.pos >= self.data.len() {
                return None;
            }

            let (consumed, val_off, val_len) =
                decode_entry_at(self.data, self.pos, &mut self.current_key);
            self.pos += consumed;

            if let Some(target) = self.lower_bound {
                if compare_internal_keys(&self.current_key, target).is_lt() {
                    continue;
                }
                self.lower_bound = None;
            }

            let value = self.data[val_off..val_off + val_len].to_vec();
            return Some((self.current_key.clone(), value));
        }
    }
}

/// Block builder: accumulates sorted entries and emits a data block.
pub(crate) struct BlockBuilder {
    buffer: Vec<u8>,
    restarts: Vec<u32>,
    entry_count: usize,
    last_key: Vec<u8>,
    restart_interval: usize,
}

impl BlockBuilder {
    pub(crate) fn new(restart_interval: usize) -> Self {
        Self {
            buffer: Vec::new(),
            restarts: vec![0], // The first entry is always a restart point.
            entry_count: 0,
            last_key: Vec::new(),
            restart_interval,
        }
    }

    pub(crate) fn add(&mut self, key: &[u8], value: &[u8]) {
        let shared =
            if self.entry_count.is_multiple_of(self.restart_interval) && self.entry_count > 0 {
                self.restarts.push(self.buffer.len() as u32);
                0 // Restart point: no prefix sharing.
            } else {
                self.last_key
                    .iter()
                    .zip(key.iter())
                    .take_while(|(a, b)| a == b)
                    .count()
            };

        let unshared = key.len() - shared;

        encode_varint(&mut self.buffer, shared as u64);
        encode_varint(&mut self.buffer, unshared as u64);
        encode_varint(&mut self.buffer, value.len() as u64);
        self.buffer.extend_from_slice(&key[shared..]);
        self.buffer.extend_from_slice(value);

        self.last_key = key.to_vec();
        self.entry_count += 1;
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        for restart in &self.restarts {
            self.buffer.extend_from_slice(&restart.to_le_bytes());
        }
        self.buffer
            .extend_from_slice(&(self.restarts.len() as u32).to_le_bytes());
        self.buffer
    }

    pub(crate) fn estimated_size(&self) -> usize {
        self.buffer.len() + self.restarts.len() * 4 + 4
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entry_count == 0
    }
}

/// Decode one entry at `pos`. Reconstructs the key in-place into
/// `prev_key` (truncate to shared prefix + extend with unshared).
/// Returns `(bytes_consumed, value_offset_in_data, value_len)`.
/// The value lives at `data[value_offset..value_offset+value_len]`.
pub(crate) fn decode_entry_at(
    data: &[u8],
    pos: usize,
    prev_key: &mut Vec<u8>,
) -> (usize, usize, usize) {
    let header = decode_entry_header(data, pos).expect("block entry validated at decode");
    assert!(
        header.shared <= prev_key.len(),
        "block entry validated at decode"
    );
    let key_end = header.key_offset + header.unshared;
    prev_key.truncate(header.shared);
    prev_key.extend_from_slice(&data[header.key_offset..key_end]);
    (header.consumed, key_end, header.value_len)
}

pub(crate) fn encoded_entry_size(data: &[u8], pos: usize) -> usize {
    decode_entry_header(data, pos)
        .expect("block entry validated at decode")
        .consumed
}

pub(crate) fn encode_varint(buf: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buf.push((value as u8) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

#[cfg(test)]
pub(crate) fn decode_varint(data: &[u8]) -> (u64, usize) {
    let mut result: u64 = 0;
    for (i, &byte) in data.iter().take(10).enumerate() {
        let shift = i * 7;
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return (result, i + 1);
        }
    }
    (result, data.len().min(10))
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct DecodedEntry {
    key: Vec<u8>,
    consumed: usize,
}

struct EntryHeader {
    shared: usize,
    unshared: usize,
    value_len: usize,
    key_offset: usize,
    consumed: usize,
}

fn validate_restarts_and_entries(
    data: &[u8],
    restarts: &[u32],
    require_internal_keys: bool,
) -> io::Result<()> {
    let mut entry_offsets = Vec::new();
    let mut pos = 0usize;
    let mut prev_key = Vec::new();
    while pos < data.len() {
        entry_offsets.push(pos);
        let entry = decode_block_entry_checked(data, pos, &prev_key)?;
        if require_internal_keys && entry.key.len() < INTERNAL_KEY_SUFFIX_LEN {
            return Err(invalid_data(
                "block entry key is shorter than the internal-key trailer",
            ));
        }
        pos += entry.consumed;
        prev_key = entry.key;
    }

    if restarts.first().copied() != Some(0) {
        return Err(invalid_data("block first restart offset must be zero"));
    }

    let mut previous_restart = 0usize;
    for (idx, &restart) in restarts.iter().enumerate() {
        let restart = restart as usize;
        if idx > 0 && restart <= previous_restart {
            return Err(invalid_data(
                "block restart offsets are not strictly increasing",
            ));
        }
        previous_restart = restart;

        if data.is_empty() {
            if restart != 0 {
                return Err(invalid_data("empty block restart offset must be zero"));
            }
            continue;
        }
        if entry_offsets.binary_search(&restart).is_err() {
            return Err(invalid_data(
                "block restart offset is not an entry boundary",
            ));
        }
        if decode_entry_header(data, restart)?.shared != 0 {
            return Err(invalid_data(
                "block restart entry must not share a key prefix",
            ));
        }
    }

    Ok(())
}

fn decode_block_entry_checked(
    data: &[u8],
    pos: usize,
    prev_key: &[u8],
) -> io::Result<DecodedEntry> {
    let header = decode_entry_header(data, pos)?;
    if header.shared > prev_key.len() {
        return Err(invalid_data(
            "block entry shared prefix exceeds previous key",
        ));
    }

    let key_len = header
        .shared
        .checked_add(header.unshared)
        .ok_or_else(|| invalid_data("block entry key length overflows"))?;
    let mut key = Vec::with_capacity(key_len);
    key.extend_from_slice(&prev_key[..header.shared]);
    key.extend_from_slice(&data[header.key_offset..header.key_offset + header.unshared]);

    Ok(DecodedEntry {
        key,
        consumed: header.consumed,
    })
}

fn decode_entry_header(data: &[u8], pos: usize) -> io::Result<EntryHeader> {
    if pos >= data.len() {
        return Err(invalid_data("block entry offset is outside entry data"));
    }

    let mut offset = pos;
    let shared = read_varint(data, &mut offset)?;
    let unshared = read_varint(data, &mut offset)?;
    let value_len = read_varint(data, &mut offset)?;

    let key_end = offset
        .checked_add(unshared)
        .ok_or_else(|| invalid_data("block entry key length overflows"))?;
    if key_end > data.len() {
        return Err(invalid_data("block entry key extends past block"));
    }
    let value_end = key_end
        .checked_add(value_len)
        .ok_or_else(|| invalid_data("block entry value length overflows"))?;
    if value_end > data.len() {
        return Err(invalid_data("block entry value extends past block"));
    }

    Ok(EntryHeader {
        shared,
        unshared,
        value_len,
        key_offset: offset,
        consumed: value_end - pos,
    })
}

pub(crate) fn read_varint(data: &[u8], offset: &mut usize) -> io::Result<usize> {
    let mut result: u64 = 0;
    for i in 0..10 {
        let Some(&byte) = data.get(*offset) else {
            return Err(invalid_data("unterminated block entry varint"));
        };
        *offset += 1;
        if i == 9 && byte > 1 {
            return Err(invalid_data("block entry varint overflows u64"));
        }
        result |= ((byte & 0x7F) as u64) << (i * 7);
        if byte & 0x80 == 0 {
            return usize::try_from(result)
                .map_err(|_| invalid_data("block entry varint overflows usize"));
        }
    }
    Err(invalid_data("unterminated block entry varint"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect `(key, value)` pairs the way the read paths do, through
    /// the borrowing scan.
    fn scan_pairs(block: &Block, target: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut out = Vec::new();
        let mut key_buf = Vec::new();
        let done: Option<()> = block.scan_from(target, &mut key_buf, |key, off, len| {
            out.push((
                key.to_vec(),
                block
                    .entry_bytes(off, len)
                    .expect("value in range")
                    .to_vec(),
            ));
            ControlFlow::Continue(())
        });
        assert!(done.is_none(), "a Continue-only scan runs to the end");
        out
    }

    #[test]
    fn scan_from_matches_the_copying_iterator() {
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..64)
            .map(|i| {
                (
                    format!("key_{i:04}").into_bytes(),
                    format!("value_{i}").into_bytes(),
                )
            })
            .collect();
        let refs: Vec<(&[u8], &[u8])> = pairs
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
            .collect();
        let block = build_block(&refs);

        for target in [
            b"".as_ref(),
            b"key_0000",
            b"key_0001",
            b"key_0016",
            b"key_0017_suffix",
            b"key_0063",
            b"zzzz",
        ] {
            let expected: Vec<_> = block.iter_from(target).collect();
            assert_eq!(scan_pairs(&block, target), expected, "target {target:?}");
        }
    }

    #[test]
    fn scan_from_reuses_the_key_buffer_across_calls() {
        let block = build_block(&[(b"aaaa", b"1"), (b"aaab", b"2"), (b"bbbb", b"3")]);
        let mut key_buf = Vec::new();
        for target in [b"aaaa".as_ref(), b"bbbb", b"aaab"] {
            let first: Option<Vec<u8>> = block.scan_from(target, &mut key_buf, |key, _, _| {
                ControlFlow::Break(key.to_vec())
            });
            assert_eq!(first.as_deref(), Some(target));
        }
    }

    #[test]
    fn entry_bytes_rejects_a_range_past_the_entry_region() {
        let block = build_block(&[(b"k", b"v")]);
        assert!(block.entry_bytes(0, block.entry_data().len()).is_some());
        assert!(block.entry_bytes(0, block.entry_data().len() + 1).is_none());
        assert!(block.entry_bytes(usize::MAX, 1).is_none());
    }

    fn build_block(pairs: &[(&[u8], &[u8])]) -> Block {
        let mut builder = BlockBuilder::new(4);
        for (k, v) in pairs {
            builder.add(k, v);
        }
        Block::decode(builder.finish()).unwrap()
    }

    fn expect_block_decode_err(data: Vec<u8>) -> io::Error {
        match Block::decode(data) {
            Err(e) => e,
            Ok(_) => panic!("expected invalid block"),
        }
    }

    // ── varint encoding ──────────────────────────────────────────

    #[test]
    fn test_varint_roundtrip() {
        let test_values = [0u64, 1, 127, 128, 16383, 16384, u64::MAX];
        for val in test_values {
            let mut buf = Vec::new();
            encode_varint(&mut buf, val);
            let (decoded, _) = decode_varint(&buf);
            assert_eq!(val, decoded);
        }
    }

    #[test]
    fn varint_zero_and_127_fit_in_one_byte() {
        for v in [0u64, 42, 127] {
            let mut buf = Vec::new();
            encode_varint(&mut buf, v);
            assert_eq!(buf.len(), 1, "value {v} should encode to 1 byte");
            assert_eq!(decode_varint(&buf), (v, 1));
        }
    }

    #[test]
    fn varint_128_crosses_into_two_bytes() {
        let mut buf = Vec::new();
        encode_varint(&mut buf, 128);
        assert_eq!(buf, vec![0x80, 0x01]);
        assert_eq!(decode_varint(&buf), (128, 2));
    }

    #[test]
    fn varint_u64_max_fits_in_ten_bytes() {
        let mut buf = Vec::new();
        encode_varint(&mut buf, u64::MAX);
        assert_eq!(buf.len(), 10);
        assert_eq!(decode_varint(&buf), (u64::MAX, 10));
    }

    #[test]
    fn decode_varint_does_not_panic_on_long_unterminated_input() {
        let buf = vec![0x80; 32];
        assert_eq!(decode_varint(&buf).1, 10);
    }

    // ── block encode/decode ─────────────────────────────────────

    #[test]
    fn empty_block_iterates_nothing() {
        let builder = BlockBuilder::new(16);
        let block = Block::decode(builder.finish()).unwrap();
        assert_eq!(block.iter().count(), 0);
    }

    #[test]
    fn single_entry_block_round_trips() {
        let block = build_block(&[(b"only", b"one")]);
        let entries: Vec<_> = block.iter().collect();
        assert_eq!(entries, vec![(b"only".to_vec(), b"one".to_vec())]);
    }

    #[test]
    fn iter_preserves_insert_order() {
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..20)
            .map(|i| {
                (
                    format!("key_{:04}", i).into_bytes(),
                    format!("val_{}", i).into_bytes(),
                )
            })
            .collect();
        let mut b = BlockBuilder::new(4);
        for (k, v) in &pairs {
            b.add(k, v);
        }
        let block = Block::decode(b.finish()).unwrap();
        let got: Vec<_> = block.iter().collect();
        assert_eq!(got, pairs);
    }

    #[test]
    fn iter_from_starts_at_first_entry_ge_target() {
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..20)
            .map(|i| {
                (
                    format!("key_{:04}", i).into_bytes(),
                    format!("val_{}", i).into_bytes(),
                )
            })
            .collect();
        let mut b = BlockBuilder::new(4);
        for (k, v) in &pairs {
            b.add(k, v);
        }
        let block = Block::decode(b.finish()).unwrap();

        let got: Vec<_> = block.iter_from(b"key_0010").collect();
        assert_eq!(got, pairs[10..].to_vec());

        let got: Vec<_> = block.iter_from(b"key_0010_suffix").collect();
        assert_eq!(got, pairs[11..].to_vec());

        assert_eq!(block.iter_from(b"zzzz").next(), None);
    }

    #[test]
    fn prefix_compression_round_trips_shared_prefix_keys() {
        let block = build_block(&[(b"zebra_a", b"1"), (b"zebra_b", b"2"), (b"zebra_c", b"3")]);
        let got: Vec<_> = block.iter().collect();
        assert_eq!(
            got,
            vec![
                (b"zebra_a".to_vec(), b"1".to_vec()),
                (b"zebra_b".to_vec(), b"2".to_vec()),
                (b"zebra_c".to_vec(), b"3".to_vec()),
            ]
        );
    }

    #[test]
    fn restart_points_occur_every_interval_entries() {
        let n = 32usize;
        let mut b = BlockBuilder::new(8);
        for i in 0..n {
            let key = format!("k_{:04}", i);
            b.add(key.as_bytes(), b"v");
        }
        let block = Block::decode(b.finish()).unwrap();
        // Entry 0 is always a restart; then 8, 16, 24 → 4 total.
        assert_eq!(block.restart_count(), 4);
        assert_eq!(block.restart_offset(0), 0);
    }

    #[test]
    fn decode_rejects_buffer_smaller_than_footer() {
        assert!(Block::decode(vec![]).is_err());
        assert!(Block::decode(vec![0u8; 3]).is_err());
    }

    #[test]
    fn decode_rejects_restart_array_that_exceeds_block() {
        let mut data = vec![0u8; 4];
        data.extend_from_slice(&2u32.to_le_bytes());
        let err = expect_block_decode_err(data);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_restart_offset_outside_entry_data() {
        let mut builder = BlockBuilder::new(16);
        builder.add(b"k", b"v");
        let mut data = builder.finish();
        let restart_offset = data.len() - 8;
        data[restart_offset] = 0xFF;
        let err = expect_block_decode_err(data);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_first_restart_offset_that_is_not_zero() {
        let mut data = Vec::new();
        encode_varint(&mut data, 0);
        encode_varint(&mut data, 1);
        encode_varint(&mut data, 0);
        data.push(b'a');
        let second_offset = data.len() as u32;
        encode_varint(&mut data, 0);
        encode_varint(&mut data, 1);
        encode_varint(&mut data, 0);
        data.push(b'b');
        data.extend_from_slice(&second_offset.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());

        let err = expect_block_decode_err(data);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_truncated_entry_payload() {
        let mut data = Vec::new();
        encode_varint(&mut data, 0);
        encode_varint(&mut data, 2);
        encode_varint(&mut data, 1);
        data.push(b'a');
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());

        let err = expect_block_decode_err(data);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_rejects_shared_prefix_at_restart() {
        let mut data = Vec::new();
        encode_varint(&mut data, 0);
        encode_varint(&mut data, 1);
        encode_varint(&mut data, 0);
        data.push(b'a');
        let second_offset = data.len() as u32;
        encode_varint(&mut data, 1);
        encode_varint(&mut data, 1);
        encode_varint(&mut data, 0);
        data.push(b'b');
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&second_offset.to_le_bytes());
        data.extend_from_slice(&2u32.to_le_bytes());

        let err = expect_block_decode_err(data);
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    // ── seek_ge edge cases ──────────────────────────────────────

    // ── cache accounting / builder internals ───────────────────

    #[test]
    fn charge_scales_with_data_bytes() {
        let small = build_block(&[(b"k", b"v")]);
        let big_pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..500)
            .map(|i| (format!("key_{:04}", i).into_bytes(), vec![0u8; 128]))
            .collect();
        let mut b = BlockBuilder::new(16);
        for (k, v) in &big_pairs {
            b.add(k, v);
        }
        let big = Block::decode(b.finish()).unwrap();
        assert!(big.charge() > small.charge() * 100);
    }

    #[test]
    fn builder_is_empty_flag_reflects_entry_count() {
        let mut b = BlockBuilder::new(16);
        assert!(b.is_empty());
        b.add(b"k", b"v");
        assert!(!b.is_empty());
    }

    #[test]
    fn builder_estimated_size_grows_monotonically() {
        let mut b = BlockBuilder::new(16);
        let s0 = b.estimated_size();
        b.add(b"a", b"1");
        let s1 = b.estimated_size();
        b.add(b"b", b"2");
        let s2 = b.estimated_size();
        assert!(s1 >= s0);
        assert!(s2 >= s1);
    }

    #[test]
    fn entry_data_length_matches_iter_consumption() {
        let pairs: Vec<(&[u8], &[u8])> = vec![(b"abc", b"1"), (b"abcd", b"22"), (b"xyz", b"333")];
        let mut b = BlockBuilder::new(16);
        for (k, v) in &pairs {
            b.add(k, v);
        }
        let block = Block::decode(b.finish()).unwrap();
        let data_len = block.entry_data().len();

        let mut consumed = 0usize;
        let mut pos = 0usize;
        let data = block.entry_data();
        while pos < data_len {
            let size = encoded_entry_size(data, pos);
            pos += size;
            consumed += size;
        }
        assert_eq!(consumed, data_len);
    }
}

#[cfg(test)]
mod scan_properties {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// The borrowing scan must visit exactly the entries the
        /// copying iterator does, in the same order, with the same
        /// bytes, for any block shape and any seek target.
        #[test]
        fn scan_from_agrees_with_iter_from(
            mut keys in proptest::collection::vec(
                proptest::collection::vec(any::<u8>(), 1..12), 1..48),
            target in proptest::collection::vec(any::<u8>(), 0..12),
        ) {
            keys.sort();
            keys.dedup();
            let mut builder = BlockBuilder::new(RESTART_INTERVAL);
            for (i, key) in keys.iter().enumerate() {
                builder.add(key, format!("v{i}").as_bytes());
            }
            let block = Block::decode(builder.finish())?;

            let expected: Vec<(Vec<u8>, Vec<u8>)> = block.iter_from(&target).collect();

            let mut got = Vec::new();
            let mut key_buf = Vec::new();
            let stopped: Option<()> = block.scan_from(&target, &mut key_buf, |key, off, len| {
                let value = block.entry_bytes(off, len).expect("value in range");
                got.push((key.to_vec(), value.to_vec()));
                ControlFlow::Continue(())
            });
            prop_assert!(stopped.is_none());
            prop_assert_eq!(got, expected);
        }
    }
}
