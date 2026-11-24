//! Inline-first encoding of the internal key used by every read path.
//!
//! On-disk / in-memory format (see [`super::internal_key`]):
//! `[cf_id BE(4)][user_key][!seq BE(8)][value_type(1)]`
//!
//! The CF-prefixed user key is a strict prefix of the internal key, so
//! one buffer answers both `column_family::prefix_key` and the search
//! key an MVCC seek needs. Buffers up to [`INLINE_KEY_CAP`] bytes never
//! touch the allocator, and moving to a different snapshot is an 8-byte
//! overwrite of the trailer rather than a re-encode.

use std::cell::Cell;

use super::internal_key::{INTERNAL_KEY_SUFFIX_LEN, VALUE_TYPE_DELETION};

/// Bytes held on the stack before an over-long key spills to the heap.
///
/// Two cache lines: covers a 4-byte CF prefix, a user key up to 115
/// bytes, and the 9-byte trailer.
pub(crate) const INLINE_KEY_CAP: usize = 128;

/// Byte buffer that lives on the stack until it does not fit.
///
/// Invariant: exactly one of the two storages is live. `spill` is unused
/// until the first overflow; from then on `inline` is dead and every
/// read goes through `spill`. [`InlineBuf::clear`] keeps whichever
/// storage is live so a spilled buffer retains its heap capacity and
/// never re-spills.
pub(crate) struct InlineBuf {
    inline: [u8; INLINE_KEY_CAP],
    spill: Vec<u8>,
    len: usize,
    spilled: bool,
}

impl InlineBuf {
    pub(crate) fn new() -> Self {
        Self {
            inline: [0u8; INLINE_KEY_CAP],
            spill: Vec::new(),
            len: 0,
            spilled: false,
        }
    }

    /// Drop the contents, keeping the live storage and its capacity.
    pub(crate) fn clear(&mut self) {
        self.len = 0;
        if self.spilled {
            self.spill.clear();
        }
    }

    pub(crate) fn extend_from_slice(&mut self, bytes: &[u8]) {
        if self.spilled {
            self.spill.extend_from_slice(bytes);
            self.len = self.spill.len();
            return;
        }
        let fits = self
            .len
            .checked_add(bytes.len())
            .is_some_and(|end| end <= INLINE_KEY_CAP);
        if fits {
            let end = self.len + bytes.len();
            self.inline[self.len..end].copy_from_slice(bytes);
            self.len = end;
            return;
        }
        self.spill.clear();
        self.spill.reserve(self.len.saturating_add(bytes.len()));
        self.spill.extend_from_slice(&self.inline[..self.len]);
        self.spill.extend_from_slice(bytes);
        self.spilled = true;
        self.len = self.spill.len();
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        if self.spilled {
            &self.spill
        } else {
            &self.inline[..self.len]
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// True when this buffer has spilled to the heap. The
    /// allocation-budget tests assert the common key shapes never do.
    #[cfg(test)]
    pub(crate) fn spilled(&self) -> bool {
        self.spilled
    }
}

impl Default for InlineBuf {
    fn default() -> Self {
        Self::new()
    }
}

/// The single encoded key form shared by every read path.
///
/// Built once at the public API boundary and threaded down by reference,
/// replacing the per-source `prefix_key` + `lookup_key` pair that
/// allocated twice before the first bloom bit was read.
pub(crate) struct LookupKey {
    buf: InlineBuf,
    /// Offset of the `!seq || value_type` trailer, i.e. the length of
    /// the CF-prefixed user key.
    user_end: usize,
    snapshot_seq: u64,
}

impl LookupKey {
    /// Encode `cf_id || user_key || !snapshot_seq || VALUE_TYPE_DELETION`.
    pub(crate) fn new(cf_id: u32, user_key: &[u8], snapshot_seq: u64) -> Self {
        let mut lk = Self {
            buf: InlineBuf::new(),
            user_end: 0,
            snapshot_seq,
        };
        lk.reset(cf_id, user_key, snapshot_seq);
        lk
    }

    /// Wrap a key a caller already prefixed. Used by the internal entry
    /// points that still receive a `&[u8]` whose CF prefix is baked in
    /// (`multi_get`, the CF metadata loader, the iterator seek paths).
    ///
    /// `prefixed` is stored verbatim, so a key shorter than the 4-byte
    /// CF prefix is accepted; [`super::internal_key::compare_internal_keys`]
    /// already tolerates short keys.
    pub(crate) fn from_prefixed(prefixed: &[u8], snapshot_seq: u64) -> Self {
        let mut lk = Self {
            buf: InlineBuf::new(),
            user_end: 0,
            snapshot_seq,
        };
        lk.reset_prefixed(prefixed, snapshot_seq);
        lk
    }

    /// `cf_id || user_key`. Byte-identical to `prefix_key(cf_id, key)`.
    pub(crate) fn prefixed_user_key(&self) -> &[u8] {
        &self.buf.as_slice()[..self.user_end]
    }

    /// The full internal key. Byte-identical to
    /// `encode_internal_key(prefixed_user_key, seq, VALUE_TYPE_DELETION)`.
    pub(crate) fn internal(&self) -> &[u8] {
        self.buf.as_slice()
    }

    pub(crate) fn snapshot_seq(&self) -> u64 {
        self.snapshot_seq
    }

    /// Re-point at a different user key, reusing the buffer.
    pub(crate) fn reset(&mut self, cf_id: u32, user_key: &[u8], snapshot_seq: u64) {
        self.buf.clear();
        self.buf.extend_from_slice(&cf_id.to_be_bytes());
        self.buf.extend_from_slice(user_key);
        self.finish_trailer(snapshot_seq);
    }

    /// Re-point at a different already-prefixed key, reusing the buffer.
    pub(crate) fn reset_prefixed(&mut self, prefixed: &[u8], snapshot_seq: u64) {
        self.buf.clear();
        self.buf.extend_from_slice(prefixed);
        self.finish_trailer(snapshot_seq);
    }

    fn finish_trailer(&mut self, snapshot_seq: u64) {
        self.user_end = self.buf.len();
        self.buf.extend_from_slice(&(!snapshot_seq).to_be_bytes());
        self.buf.extend_from_slice(&[VALUE_TYPE_DELETION]);
        self.snapshot_seq = snapshot_seq;
        debug_assert_eq!(self.buf.len(), self.user_end + INTERNAL_KEY_SUFFIX_LEN);
    }

    /// True when the encoding needed the heap. Test-only.
    #[cfg(test)]
    pub(crate) fn spilled(&self) -> bool {
        self.buf.spilled()
    }
}

thread_local! {
    /// Per-thread scratch for reconstructing prefix-compressed block
    /// keys during a point read. Grows to the largest internal key the
    /// thread has seen and is then reused forever, so a warm block scan
    /// reconstructs keys without touching the allocator.
    static KEY_SCRATCH: Cell<Vec<u8>> = const { Cell::new(Vec::new()) };
}

/// Run `f` with this thread's key-reconstruction scratch buffer.
///
/// The buffer is taken out of the thread-local for the duration of the
/// call rather than borrowed, so a re-entrant read (a merge operator or
/// compaction filter that calls back into the database) allocates its
/// own buffer instead of panicking on a double borrow.
pub(crate) fn with_key_scratch<R>(f: impl FnOnce(&mut Vec<u8>) -> R) -> R {
    let mut buf = KEY_SCRATCH.with(|slot| slot.take());
    let out = f(&mut buf);
    buf.clear();
    KEY_SCRATCH.with(|slot| slot.set(buf));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::column_family::prefix_key;
    use crate::engine::internal_key::encode_internal_key;
    use proptest::prelude::*;

    fn reference_encoding(cf_id: u32, user_key: &[u8], seq: u64) -> Vec<u8> {
        encode_internal_key(&prefix_key(cf_id, user_key), seq, VALUE_TYPE_DELETION)
    }

    #[test]
    fn inline_buf_does_not_spill_for_typical_keys() {
        let lk = LookupKey::new(0, &[b'k'; 32], 7);
        assert!(!lk.spilled());
        assert_eq!(lk.internal().len(), 4 + 32 + 9);
    }

    #[test]
    fn inline_buf_spills_exactly_once() {
        let mut buf = InlineBuf::new();
        buf.extend_from_slice(&[1u8; 64]);
        assert!(!buf.spilled());
        buf.extend_from_slice(&[2u8; 4096]);
        assert!(buf.spilled());
        assert_eq!(buf.len(), 64 + 4096);
        buf.extend_from_slice(&[3u8; 8]);
        assert!(buf.spilled());
        assert_eq!(buf.len(), 64 + 4096 + 8);
        assert_eq!(&buf.as_slice()[..64], &[1u8; 64]);
        assert_eq!(&buf.as_slice()[64..64 + 4096], &[2u8; 4096]);
        assert_eq!(&buf.as_slice()[64 + 4096..], &[3u8; 8]);
        // The inline region is dead after the spill; further appends go
        // straight onto the heap buffer. Asserting the *address* is
        // unchanged instead would be asserting that `realloc` grew the
        // spill vector in place, which is the allocator's choice, not an
        // invariant of this type.
        assert_ne!(buf.as_slice().as_ptr(), buf.inline.as_ptr());
    }

    #[test]
    fn clear_keeps_the_spilled_storage() {
        let mut buf = InlineBuf::new();
        buf.extend_from_slice(&[7u8; 4096]);
        assert!(buf.spilled());
        buf.clear();
        assert_eq!(buf.len(), 0);
        assert!(buf.as_slice().is_empty());
        buf.extend_from_slice(b"short");
        assert_eq!(buf.as_slice(), b"short");
    }

    #[test]
    fn from_prefixed_tolerates_a_short_prefix() {
        let lk = LookupKey::from_prefixed(b"ab", 3);
        assert_eq!(lk.prefixed_user_key(), b"ab");
        assert_eq!(lk.internal().len(), 2 + 9);
        assert_eq!(lk.snapshot_seq(), 3);
    }

    #[test]
    fn empty_user_key_round_trips() {
        let lk = LookupKey::new(4, b"", u64::MAX);
        assert_eq!(lk.prefixed_user_key(), 4u32.to_be_bytes());
        assert_eq!(
            lk.internal(),
            reference_encoding(4, b"", u64::MAX).as_slice()
        );
    }

    proptest! {
        #[test]
        fn matches_reference_encoding(
            cf_id in any::<u32>(),
            key in proptest::collection::vec(any::<u8>(), 0..4096),
            seq in any::<u64>(),
        ) {
            let lk = LookupKey::new(cf_id, &key, seq);
            let expected_internal = reference_encoding(cf_id, &key, seq);
            let expected_prefixed = prefix_key(cf_id, &key);
            prop_assert_eq!(lk.internal(), expected_internal.as_slice());
            prop_assert_eq!(lk.prefixed_user_key(), expected_prefixed.as_slice());
            prop_assert_eq!(lk.snapshot_seq(), seq);
        }

        #[test]
        fn reset_reuses_buffer_and_matches_fresh(
            long in proptest::collection::vec(any::<u8>(), 512..1024),
            short in proptest::collection::vec(any::<u8>(), 0..32),
            seq in any::<u64>(),
        ) {
            let mut lk = LookupKey::new(1, &long, seq);
            prop_assert!(lk.spilled());
            let fresh_short = LookupKey::new(2, &short, seq);
            let fresh_long = LookupKey::new(1, &long, seq);
            lk.reset(2, &short, seq);
            prop_assert_eq!(lk.internal(), fresh_short.internal());
            lk.reset(1, &long, seq);
            prop_assert_eq!(lk.internal(), fresh_long.internal());
        }

        #[test]
        fn from_prefixed_matches_new(
            cf_id in any::<u32>(),
            key in proptest::collection::vec(any::<u8>(), 0..256),
            seq in any::<u64>(),
        ) {
            let prefixed = prefix_key(cf_id, &key);
            let a = LookupKey::from_prefixed(&prefixed, seq);
            let b = LookupKey::new(cf_id, &key, seq);
            prop_assert_eq!(a.internal(), b.internal());
            prop_assert_eq!(a.prefixed_user_key(), b.prefixed_user_key());
        }
    }
}
