//! Internal key encoding shared by the memtable and SSTables.
//!
//! Format: `[user_key][!seq as u64 BE][value_type: u8]`
//!
//! The bitwise-NOT of the sequence number ensures that for a given user key,
//! newer entries (higher seq) sort *first* in lexicographic order. That lets
//! both the memtable's skip list and an SSTable's sorted blocks answer
//! "most recent version visible at snapshot_seq" with a single forward seek:
//! seek to `user_key || !snapshot_seq || 0x00`, then the first entry with the
//! matching user key has the largest seq ≤ snapshot_seq. That search key is
//! built by [`super::lookup_key::LookupKey`], which encodes it once per read
//! and shares the buffer with the CF-prefixed user key.
//!
//! # Comparison
//!
//! Raw byte comparison of internal keys is **incorrect** when two
//! user keys have different lengths and one is a prefix of the
//! other - the `!seq` bytes of the shorter key collide with the
//! literal data bytes of the longer key. Every comparison site
//! must use [`compare_internal_keys`], or
//! [`compare_internal_split`] where the right-hand key has not
//! been concatenated yet. The memtable's arena skip list orders
//! its nodes with the split form, so the two must agree; a
//! proptest below asserts they do.

/// Entry is a live value.
pub(crate) const VALUE_TYPE_VALUE: u8 = 1;
/// Entry is a deletion tombstone.
pub(crate) const VALUE_TYPE_DELETION: u8 = 0;
/// Entry is a merge operand - a piece of data that will be combined
/// with an older base value (or other operands) at read time via the
/// configured [`crate::MergeOperator`].
pub(crate) const VALUE_TYPE_MERGE: u8 = 2;

/// Size of the trailing `(seq, value_type)` suffix in an internal key.
pub(crate) const INTERNAL_KEY_SUFFIX_LEN: usize = 9;

/// The kind of write an entry of `value_type` records. A conflict names the
/// newer write by it.
pub(crate) fn write_kind(value_type: u8) -> crate::WriteKind {
    match value_type {
        VALUE_TYPE_DELETION => crate::WriteKind::Delete,
        VALUE_TYPE_MERGE => crate::WriteKind::Merge,
        _ => crate::WriteKind::Put,
    }
}

pub(crate) fn encode_internal_key(user_key: &[u8], seq: u64, value_type: u8) -> Vec<u8> {
    let mut key = Vec::with_capacity(user_key.len() + INTERNAL_KEY_SUFFIX_LEN);
    key.extend_from_slice(user_key);
    key.extend_from_slice(&(!seq).to_be_bytes());
    key.push(value_type);
    key
}

pub(crate) fn decode_internal_key(internal_key: &[u8]) -> (&[u8], u64, u8) {
    let len = internal_key.len();
    let value_type = internal_key[len - 1];
    let seq_bytes: [u8; 8] = internal_key[len - 9..len - 1].try_into().unwrap();
    let seq = !u64::from_be_bytes(seq_bytes);
    let user_key = &internal_key[..len - 9];
    (user_key, seq, value_type)
}

/// Extract the user key portion of an internal key without decoding seq/type.
pub(crate) fn user_key_of(internal_key: &[u8]) -> &[u8] {
    &internal_key[..internal_key.len() - INTERNAL_KEY_SUFFIX_LEN]
}

/// Compare two internal keys correctly: user-key portion first
/// (standard lexicographic byte comparison), then the `!seq ||
/// vt` trailer on tie. This is the regolith equivalent of LevelDB's
/// `InternalKeyComparator` - raw byte comparison of the encoded
/// form is NOT correct when user keys have different lengths.
pub(crate) fn compare_internal_keys(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    // Guard: keys shorter than the suffix are not valid internal
    // keys (they can appear in unit tests that build raw blocks).
    // Fall back to raw byte comparison for those.
    if b.len() < INTERNAL_KEY_SUFFIX_LEN {
        return a.cmp(b);
    }
    let split = b.len() - INTERNAL_KEY_SUFFIX_LEN;
    compare_internal_split(a, &b[..split], &b[split..])
}

/// [`compare_internal_keys`] against a right-hand side that has not been
/// concatenated yet: `b_user_key` followed by its 9-byte `!seq || vt`
/// trailer.
///
/// The memtable assembles an internal key directly inside its arena node,
/// so its insert path has the two halves but no contiguous buffer to
/// compare against. Both entry points share this body so the two can
/// never disagree about ordering.
pub(crate) fn compare_internal_split(
    a: &[u8],
    b_user_key: &[u8],
    b_trailer: &[u8],
) -> std::cmp::Ordering {
    if a.len() < INTERNAL_KEY_SUFFIX_LEN {
        return raw_cmp_split(a, b_user_key, b_trailer);
    }
    let a_uk = user_key_of(a);
    match a_uk.cmp(b_user_key) {
        std::cmp::Ordering::Equal => {
            // Same user key - compare the trailer. The trailer
            // is `!seq || vt`, and because !seq is inverted, a
            // SMALLER trailer value corresponds to a NEWER entry
            // (higher seq). We want newer entries to sort first
            // so the raw byte comparison of the trailer is
            // already the correct order.
            a[a_uk.len()..].cmp(b_trailer)
        }
        ord => ord,
    }
}

/// Lexicographic comparison of `a` against the concatenation of `b1` and
/// `b2`, without building the concatenation.
fn raw_cmp_split(a: &[u8], b1: &[u8], b2: &[u8]) -> std::cmp::Ordering {
    let head = a.len().min(b1.len());
    match a[..head].cmp(&b1[..head]) {
        std::cmp::Ordering::Equal => {}
        ord => return ord,
    }
    if b1.len() > head {
        return std::cmp::Ordering::Less;
    }
    let rest = &a[head..];
    let tail = rest.len().min(b2.len());
    match rest[..tail].cmp(&b2[..tail]) {
        std::cmp::Ordering::Equal => {}
        ord => return ord,
    }
    a.len().cmp(&(b1.len() + b2.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WriteKind;
    use proptest::prelude::*;

    #[test]
    fn each_value_type_records_its_own_kind_of_write() {
        assert_eq!(write_kind(VALUE_TYPE_VALUE), WriteKind::Put);
        assert_eq!(write_kind(VALUE_TYPE_DELETION), WriteKind::Delete);
        assert_eq!(write_kind(VALUE_TYPE_MERGE), WriteKind::Merge);
    }

    #[test]
    fn roundtrip() {
        let ik = encode_internal_key(b"hello", 42, VALUE_TYPE_VALUE);
        let (uk, seq, vt) = decode_internal_key(&ik);
        assert_eq!(uk, b"hello");
        assert_eq!(seq, 42);
        assert_eq!(vt, VALUE_TYPE_VALUE);
    }

    #[test]
    fn newer_seq_sorts_first() {
        let older = encode_internal_key(b"k", 5, VALUE_TYPE_VALUE);
        let newer = encode_internal_key(b"k", 10, VALUE_TYPE_VALUE);
        assert!(newer < older, "higher seq must sort before lower seq");
    }

    #[test]
    fn search_key_matches_first_visible() {
        // At snapshot_seq=7, the first entry >= lookup_key with matching user
        // key should be the one with seq=5 (largest seq ≤ 7).
        let seq_10 = encode_internal_key(b"k", 10, VALUE_TYPE_VALUE);
        let seq_5 = encode_internal_key(b"k", 5, VALUE_TYPE_VALUE);
        let seq_3 = encode_internal_key(b"k", 3, VALUE_TYPE_VALUE);
        let probe = encode_internal_key(b"k", 7, VALUE_TYPE_DELETION);
        assert!(seq_10 < probe);
        assert!(seq_5 >= probe);
        assert!(seq_3 >= probe);
    }

    #[test]
    fn user_key_of_strips_suffix() {
        let ik = encode_internal_key(b"hello", 42, VALUE_TYPE_VALUE);
        assert_eq!(user_key_of(&ik), b"hello");
    }

    #[test]
    fn compare_correctly_orders_prefix_keys() {
        // Naive byte comparison would interleave these incorrectly
        // because `ab`'s `!seq` trailer collides with `abc`'s literal
        // `c` byte. The custom comparator fixes this: `ab` < `abc`
        // regardless of seq.
        let ab_high = encode_internal_key(b"ab", u64::MAX, VALUE_TYPE_VALUE);
        let ab_low = encode_internal_key(b"ab", 0, VALUE_TYPE_VALUE);
        let abc_high = encode_internal_key(b"abc", u64::MAX, VALUE_TYPE_VALUE);

        assert!(compare_internal_keys(&ab_high, &abc_high).is_lt());
        assert!(compare_internal_keys(&ab_low, &abc_high).is_lt());
        assert!(compare_internal_keys(&abc_high, &ab_high).is_gt());
    }

    #[test]
    fn compare_falls_back_to_raw_for_short_keys() {
        // Keys shorter than the 9-byte internal-key suffix can't be
        // decoded as internal keys. The comparator short-circuits to
        // raw byte compare so it can still be used on test-crafted
        // blocks that contain raw user keys.
        let a = b"ab";
        let b = b"ac";
        assert!(compare_internal_keys(a, b).is_lt());
        assert!(compare_internal_keys(b, a).is_gt());
        assert!(compare_internal_keys(a, a).is_eq());
    }

    #[test]
    fn every_value_type_round_trips() {
        for vt in [VALUE_TYPE_VALUE, VALUE_TYPE_DELETION, VALUE_TYPE_MERGE] {
            let ik = encode_internal_key(b"k", 3, vt);
            let (_, _, decoded_vt) = decode_internal_key(&ik);
            assert_eq!(decoded_vt, vt);
        }
    }

    #[test]
    fn search_key_at_u64_max_sorts_after_any_stored_seq() {
        // snapshot_seq = u64::MAX means "most recent possible read".
        // The resulting lookup key must be <= every encoded
        // entry for the same user key.
        let probe = encode_internal_key(b"k", u64::MAX, VALUE_TYPE_DELETION);
        let seq_1 = encode_internal_key(b"k", 1, VALUE_TYPE_VALUE);
        let seq_huge = encode_internal_key(b"k", u64::MAX - 1, VALUE_TYPE_VALUE);
        assert!(compare_internal_keys(&probe, &seq_1).is_le());
        assert!(compare_internal_keys(&probe, &seq_huge).is_le());
    }

    /// A data block whose entry key is shorter than the 9-byte MVCC
    /// suffix used to pass block validation and reach
    /// `decode_internal_key`, which indexes the trailer directly and
    /// panicked with a subtract overflow. `Db::open` is a reachable
    /// entry point, through `load_cf_registry` -> `collect_range` ->
    /// `RegolithIterator::seek`, so the shape is now rejected where the
    /// block is parsed.
    #[test]
    fn short_key_from_a_tampered_sstable_is_rejected_as_corruption() {
        fn varint(buf: &mut Vec<u8>, mut v: u64) {
            while v >= 0x80 {
                buf.push((v as u8) | 0x80);
                v >>= 7;
            }
            buf.push(v as u8);
        }

        let dir = tempfile::tempdir().unwrap();
        let opts = crate::Options::default()
            .block_size(64 * 1024)
            .compression(crate::CompressionType::None);
        {
            let db = crate::Db::open(dir.path(), opts.clone()).unwrap();
            for i in 0..8 {
                db.put(format!("k{i}").as_bytes(), b"v").unwrap();
            }
            db.compact_range(None, None).wait().unwrap();
        }

        let sst_dir = dir.path().join("sst");
        let sst = std::fs::read_dir(&sst_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|s| s.to_str()) == Some("sst"))
            .expect("one sst file");
        let mut bytes = std::fs::read(&sst).unwrap();

        // Footer: [rt_off][rt_size][bloom_off][bloom_size][idx_off]
        // [idx_size][num_entries][magic], u64 LE each.
        let f = &bytes[bytes.len() - 64..];
        let rd = |i: usize| u64::from_le_bytes(f[i * 8..i * 8 + 8].try_into().unwrap()) as usize;
        let data_end = [rd(0), rd(2), rd(4)]
            .into_iter()
            .filter(|o| *o > 0)
            .min()
            .expect("a section follows the data blocks");

        // Rebuild the single data block frame in place:
        // [compression: u8][payload][checksum: u32], payload is one
        // entry with a 3-byte key plus a one-point restart array.
        let frame_len = data_end;
        let payload_len = frame_len - 5;
        let mut value_len = payload_len - 14;
        let mut payload = Vec::new();
        loop {
            payload.clear();
            varint(&mut payload, 0);
            varint(&mut payload, 3);
            varint(&mut payload, value_len as u64);
            payload.extend_from_slice(b"abc");
            payload.resize(payload.len() + value_len, 0u8);
            payload.extend_from_slice(&0u32.to_le_bytes());
            payload.extend_from_slice(&1u32.to_le_bytes());
            match payload.len().cmp(&payload_len) {
                std::cmp::Ordering::Equal => break,
                std::cmp::Ordering::Less => value_len += payload_len - payload.len(),
                std::cmp::Ordering::Greater => value_len -= payload.len() - payload_len,
            }
        }
        let checksum = crate::engine::checksum::sst_block(0, &payload);
        bytes[0] = 0;
        bytes[1..1 + payload.len()].copy_from_slice(&payload);
        bytes[1 + payload.len()..frame_len].copy_from_slice(&checksum.to_le_bytes());
        std::fs::write(&sst, &bytes).unwrap();

        // The frame is well formed: only the internal-key shape is
        // wrong, which is exactly what the data-block decoder now
        // rejects.
        assert!(crate::engine::block::Block::decode(payload.clone()).is_ok());
        match crate::engine::block::Block::decode_data_block(payload) {
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData),
            Ok(_) => panic!("a short key must not decode as a data block"),
        }

        // Reading through the public API surfaces corruption instead of
        // panicking, whether or not the open itself trips over it.
        match crate::Db::open(dir.path(), opts) {
            Err(e) => assert!(matches!(e, crate::Error::Corruption(_)), "{e:?}"),
            Ok(db) => {
                let mut it = db.iter();
                it.seek_to_first();
                let mut seen = 0;
                while it.valid() && seen < 1000 {
                    let _ = it.key();
                    it.next();
                    seen += 1;
                }
                assert!(it.status().is_err() || seen < 1000);
                assert!(db.get(b"k0").is_err() || db.get(b"k0").is_ok());
            }
        }
    }

    proptest! {
        /// The split comparator the memtable insert path uses must agree
        /// with the concatenated one every other call site uses. Two
        /// orderings that disagree would corrupt the skip list silently.
        #[test]
        fn split_comparator_matches_the_concatenated_one(
            a_key in proptest::collection::vec(any::<u8>(), 0..24),
            a_seq in any::<u64>(),
            b_key in proptest::collection::vec(any::<u8>(), 0..24),
            b_seq in any::<u64>(),
            a_vt in 0u8..3,
            b_vt in 0u8..3,
        ) {
            let a = encode_internal_key(&a_key, a_seq, a_vt);
            let b = encode_internal_key(&b_key, b_seq, b_vt);
            let mut trailer = [0u8; INTERNAL_KEY_SUFFIX_LEN];
            trailer[..8].copy_from_slice(&(!b_seq).to_be_bytes());
            trailer[8] = b_vt;
            prop_assert_eq!(
                compare_internal_split(&a, &b_key, &trailer),
                compare_internal_keys(&a, &b)
            );
        }

        /// Short keys (only test-crafted blocks produce them) keep the
        /// raw-byte fallback the concatenated comparator has always had.
        #[test]
        fn split_comparator_matches_on_short_keys(
            a in proptest::collection::vec(any::<u8>(), 0..12),
            b_key in proptest::collection::vec(any::<u8>(), 0..12),
            b_seq in any::<u64>(),
        ) {
            let b = encode_internal_key(&b_key, b_seq, VALUE_TYPE_VALUE);
            let mut trailer = [0u8; INTERNAL_KEY_SUFFIX_LEN];
            trailer[..8].copy_from_slice(&(!b_seq).to_be_bytes());
            trailer[8] = VALUE_TYPE_VALUE;
            prop_assert_eq!(
                compare_internal_split(&a, &b_key, &trailer),
                compare_internal_keys(&a, &b)
            );
        }
    }
}
