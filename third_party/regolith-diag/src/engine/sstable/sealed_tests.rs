use std::sync::Arc;

use tempfile::TempDir;

use super::super::*;
use super::*;
use crate::encryption::{KeyMaterial, KeyProvider};
use crate::engine::internal_key::{VALUE_TYPE_DELETION, VALUE_TYPE_VALUE, encode_internal_key};
use crate::engine::seal::test_keys::{TestKeys, keyring};

/// A value no table may hold in plaintext.
const MARKER: &[u8] = b"PLAINTEXT-MARKER";

fn std_env() -> Arc<dyn Env> {
    crate::env::std_env()
}

fn value(i: usize) -> Vec<u8> {
    let mut v = MARKER.to_vec();
    v.extend_from_slice(format!("-{i:05}").as_bytes());
    v
}

fn key(i: usize) -> Vec<u8> {
    format!("key_{i:05}").into_bytes()
}

/// Write `n` keys, a tombstone and a range tombstone into a table at
/// `path`, sealed under `ring` when given.
fn write(path: &Path, ring: Option<&Keyring>, codec: CompressionType, partitioned: bool, n: usize) {
    let extractor: Arc<dyn PrefixExtractor> = Arc::new(crate::FixedLengthPrefix(4));
    let mut writer = SsTableWriter::new_in(
        &std_env(),
        path,
        512,
        10,
        codec,
        Some(extractor),
        partitioned,
        256,
        ring,
    )
    .unwrap();
    for i in 0..n {
        writer
            .add(
                &encode_internal_key(&key(i), 7, VALUE_TYPE_VALUE),
                &value(i),
            )
            .unwrap();
    }
    writer
        .add(
            &encode_internal_key(b"zz_deleted", 8, VALUE_TYPE_DELETION),
            b"",
        )
        .unwrap();
    writer.add_range_tombstone(b"key_00010", b"key_00020", 9);
    writer.finish().unwrap().unwrap();
}

fn open(path: &Path, ring: Option<&Keyring>) -> io::Result<SsTableReader> {
    SsTableReader::open_with(&std_env(), path, 1, MetadataPolicy::Pinned, ring)
}

fn read_all(reader: &SsTableReader) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    reader.iter_internal(&BlockCache::new(0))
}

#[test]
fn a_sealed_table_round_trips_in_every_layout_and_holds_no_plaintext() {
    let ring = keyring(&[3]);
    for partitioned in [false, true] {
        for codec in [
            CompressionType::None,
            CompressionType::Lz4,
            CompressionType::Snappy,
        ] {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("t.sst");
            write(&path, Some(&ring), codec, partitioned, 200);
            let bytes = std::fs::read(&path).unwrap();
            let magic = u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap());
            assert_eq!(magic, if partitioned { MAGIC_V8 } else { MAGIC_V7 });
            assert!(
                !bytes.windows(MARKER.len()).any(|w| w == MARKER),
                "a value is in the file in plaintext ({codec:?}, partitioned {partitioned})"
            );
            assert!(!bytes.windows(9).any(|w| w == b"key_00042"));

            let reader = open(&path, Some(&ring)).unwrap();
            assert_eq!(reader.seal_key(), Some(KeyId(3)));
            let entries = read_all(&reader).unwrap();
            assert_eq!(entries.len(), 201);
            for (i, (k, v)) in entries.iter().take(200).enumerate() {
                assert_eq!(user_key_of(k), key(i).as_slice());
                assert_eq!(v, &value(i));
            }
            assert_eq!(reader.range_tombstones().len(), 1);
            let cache = BlockCache::new(1 << 20);
            let lk = LookupKey::from_prefixed(&key(150), u64::MAX);
            let found = reader.get(&lk, &mut Vec::new(), &cache).unwrap();
            assert!(matches!(found, LookupResult::Found { seq: 7, .. }));
            // The first four bytes of these keys stand where a family id
            // does, so a prefix is the four bytes after them.
            let extractor = crate::FixedLengthPrefix(4);
            let held = PrefixProbe::new(&extractor, b"key_0001");
            assert!(reader.may_have_prefix(&held, &cache).unwrap());
            let absent = PrefixProbe::new(&extractor, b"key_9999");
            assert!(!reader.may_have_prefix(&absent, &cache).unwrap());
        }
    }
}

#[test]
fn every_byte_of_a_sealed_table_is_authenticated() {
    let ring = keyring(&[1]);
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.sst");
    write(&path, Some(&ring), CompressionType::Lz4, true, 40);
    let clean = std::fs::read(&path).unwrap();
    let damaged = dir.path().join("d.sst");
    for at in 0..clean.len() {
        let mut bytes = clean.clone();
        bytes[at] ^= 0x04;
        std::fs::write(&damaged, &bytes).unwrap();
        let outcome = open(&damaged, Some(&ring)).and_then(|reader| read_all(&reader));
        assert!(
            outcome.is_err(),
            "a flip at byte {at} of {} read back",
            clean.len()
        );
    }
}

#[test]
fn a_frame_opens_only_in_its_own_table_at_its_own_offset() {
    let ring = keyring(&[1]);
    let dir = TempDir::new().unwrap();
    let (a, b) = (dir.path().join("a.sst"), dir.path().join("b.sst"));
    write(&a, Some(&ring), CompressionType::None, false, 60);
    write(&b, Some(&ring), CompressionType::None, false, 60);
    let reader = open(&a, Some(&ring)).unwrap();
    let cache = BlockCache::new(0);
    let first = reader
        .cursor_handle(&reader.first_block_cursor(&cache).unwrap().unwrap(), &cache)
        .unwrap();
    // Same content, same key, same offsets: only the table's salt differs.
    let mut spliced = std::fs::read(&a).unwrap();
    let other = std::fs::read(&b).unwrap();
    let span = first.offset as usize..(first.offset + first.size) as usize;
    spliced[span.clone()].copy_from_slice(&other[span]);
    std::fs::write(&a, &spliced).unwrap();
    let err = open(&a, Some(&ring))
        .unwrap()
        .read_block(first, &cache)
        .err()
        .expect("a block from another table opened");
    assert!(err.to_string().contains("does not verify"), "{err}");

    let seal = TableSeal::fresh(&ring).unwrap();
    let mut frame = Vec::new();
    seal.seal_region(checksum::META_KIND_INDEX, 100, b"index", &mut frame)
        .unwrap();
    assert!(
        seal.open_region(checksum::META_KIND_INDEX, 101, frame.clone(), 1, "index")
            .is_err()
    );
    assert!(
        seal.open_region(checksum::META_KIND_BLOOM, 100, frame.clone(), 1, "index")
            .is_err()
    );
    assert_eq!(
        seal.open_region(checksum::META_KIND_INDEX, 100, frame, 1, "index")
            .unwrap(),
        b"index"
    );
}

/// Key 1 under other bytes than the ones the table was sealed with.
struct WrongKeys;

impl KeyProvider for WrongKeys {
    fn current(&self) -> KeyId {
        KeyId(1)
    }
    fn key(&self, _: KeyId) -> Option<KeyMaterial> {
        Some(KeyMaterial::new([0x5A; 32]))
    }
}

#[test]
fn a_wrong_key_an_unknown_key_and_no_key_each_refuse_at_the_footer() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.sst");
    write(&path, Some(&keyring(&[1])), CompressionType::Lz4, false, 10);

    let wrong = Keyring::new(Arc::new(WrongKeys));
    let err = open(&path, Some(&wrong)).err().unwrap();
    assert!(
        err.to_string()
            .contains("table footer does not verify under key id 1"),
        "{err}"
    );
    assert!(err.to_string().contains("t.sst"), "{err}");

    let err = open(&path, Some(&keyring(&[2]))).err().unwrap();
    assert!(matches!(
        crate::Error::from(err),
        crate::Error::UnknownKey { id: KeyId(1) }
    ));
    let err = open(&path, None).err().unwrap();
    assert!(matches!(
        crate::Error::from(err),
        crate::Error::KeyProviderRequired
    ));
    assert!(table_carries_data(&*std_env(), &path, None).is_err());
    assert!(table_carries_data(&*std_env(), &path, Some(&keyring(&[1]))).unwrap());
}

#[test]
fn an_unsealed_table_still_opens_through_a_keyring() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.sst");
    write(&path, None, CompressionType::Snappy, false, 30);
    let reader = open(&path, Some(&keyring(&[1]))).unwrap();
    assert_eq!(reader.seal_key(), None);
    assert_eq!(read_all(&reader).unwrap().len(), 31);
}

#[test]
fn a_rotated_key_seals_new_tables_while_old_ones_stay_readable() {
    let keys = TestKeys::new(&[1, 2]);
    let ring = Keyring::new(keys.clone());
    let dir = TempDir::new().unwrap();
    let (old, new) = (dir.path().join("old.sst"), dir.path().join("new.sst"));
    write(&old, Some(&ring), CompressionType::Lz4, false, 10);
    keys.set_current(2);
    write(&new, Some(&ring), CompressionType::Lz4, false, 10);
    assert_eq!(open(&old, Some(&ring)).unwrap().seal_key(), Some(KeyId(1)));
    assert_eq!(open(&new, Some(&ring)).unwrap().seal_key(), Some(KeyId(2)));
    assert_eq!(
        read_all(&open(&old, Some(&ring)).unwrap()).unwrap().len(),
        11
    );
}

#[test]
fn the_block_size_limit_bounds_sealed_blocks() {
    let ring = keyring(&[1]);
    for codec in [
        CompressionType::None,
        CompressionType::Lz4,
        CompressionType::Snappy,
    ] {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("limit.sst");
        let mut writer = SsTableWriter::new_in(
            &std_env(),
            &path,
            4096,
            10,
            codec,
            None,
            false,
            4096,
            Some(&ring),
        )
        .unwrap();
        let key = encode_internal_key(b"key", 1, VALUE_TYPE_VALUE);
        writer.add(&key, &vec![b'x'; 8192]).unwrap();
        writer.finish().unwrap();
        let reader = open(&path, Some(&ring)).unwrap();
        let handle = reader
            .find_block_handle(&key, &BlockCache::new(0))
            .unwrap()
            .unwrap();
        let decoded = reader
            .read_block(handle, &BlockCache::new(0))
            .unwrap()
            .decoded_buffer_capacity();
        let charge = handle.size as usize + decoded;
        for limit in [1, handle.size as usize, charge - 1] {
            let err = reader
                .read_block_with_limit(handle, &BlockCache::new(0), Some(limit))
                .err()
                .expect("over the limit");
            assert!(matches!(
                crate::Error::from(err),
                crate::Error::DataBlockLimitExceeded { max_data_block_bytes } if max_data_block_bytes == limit
            ));
        }
        reader
            .read_block_with_limit(handle, &BlockCache::new(0), Some(charge))
            .unwrap();
    }
}

#[test]
fn a_sealed_footer_carrying_unknown_flags_is_refused() {
    let ring = keyring(&[1]);
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("t.sst");
    write(&path, Some(&ring), CompressionType::None, false, 5);
    let mut bytes = std::fs::read(&path).unwrap();
    let footer_at = bytes.len() - SEALED_FOOTER_SIZE;
    bytes[footer_at + KEY_ID_AT + 4] = 1;
    std::fs::write(&path, &bytes).unwrap();
    let err = open(&path, Some(&ring)).err().unwrap();
    assert!(err.to_string().contains("flags"), "{err}");
}
