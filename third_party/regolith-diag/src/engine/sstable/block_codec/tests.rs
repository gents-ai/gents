use super::*;
use crate::engine::block_cache::BlockCache;
use crate::engine::internal_key::{VALUE_TYPE_VALUE, encode_internal_key};
use crate::engine::lookup_key::LookupKey;
use crate::engine::sstable::{
    COMPRESSION_LZ4, COMPRESSION_NONE, LookupResult, SsTableReader, SsTableWriter, read_file_region,
};
use tempfile::TempDir;

#[test]
fn a_block_too_long_for_a_compressed_frame_is_written_uncompressed() {
    use CompressionType::{Lz4, None, Snappy};
    for codec in [None, Lz4, Snappy] {
        assert_eq!(block_codec(codec, 0), codec);
        assert_eq!(block_codec(codec, 1 << 30), codec);
    }
    #[cfg(target_pointer_width = "64")]
    {
        let most = u32::MAX as usize;
        assert_eq!(block_codec(Lz4, most), Lz4);
        assert_eq!(block_codec(Lz4, most + 1), None);
        assert_eq!(block_codec(Snappy, most + 1), None);
        // Snappy's bound on its output is a sixth over the input.
        assert_eq!(block_codec(Snappy, most / 7 * 6 - 64), Snappy);
        assert_eq!(block_codec(Snappy, most / 7 * 6 + 64), None);
        assert_eq!(block_codec(None, most + 1), None);
    }
}

/// The codec byte of the data block that holds `key`, and what a read of
/// `key` finds.
fn block_of(reader: &SsTableReader, key: &[u8]) -> (u8, LookupResult) {
    let cache = BlockCache::new(0);
    let internal = encode_internal_key(key, 1, VALUE_TYPE_VALUE);
    let handle = reader
        .find_block_handle(&internal, &cache)
        .unwrap()
        .unwrap();
    let frame = read_file_region(
        &*reader.file,
        handle.offset,
        handle.size,
        reader.data_end,
        "test",
    )
    .unwrap();
    let lookup = LookupKey::from_prefixed(key, u64::MAX);
    let found = reader.get(&lookup, &mut Vec::new(), &cache).unwrap();
    (frame[0], found)
}

#[test]
fn a_table_holds_an_uncompressed_block_among_compressed_ones() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mixed.sst");
    let big = vec![0u8; 8192];
    frames_up_to(6000, || {
        let mut writer =
            SsTableWriter::new(&path, 4096, 10, CompressionType::Lz4, None, false, 4096).unwrap();
        writer
            .add(&encode_internal_key(b"a", 1, VALUE_TYPE_VALUE), b"small")
            .unwrap();
        writer
            .add(&encode_internal_key(b"b", 1, VALUE_TYPE_VALUE), &big)
            .unwrap();
        writer
            .add(&encode_internal_key(b"c", 1, VALUE_TYPE_VALUE), b"after")
            .unwrap();
        writer.finish().unwrap().unwrap();
    });
    let reader = SsTableReader::open(&path, 1).unwrap();
    let found = |value: &[u8]| LookupResult::Found {
        seq: 1,
        value: crate::DbSlice::from_vec(value.to_vec()),
    };
    // The block holding the long value crossed the limit; the one after it
    // did not.
    assert_eq!(block_of(&reader, b"a"), (COMPRESSION_NONE, found(b"small")));
    assert_eq!(block_of(&reader, b"b"), (COMPRESSION_NONE, found(&big)));
    assert_eq!(block_of(&reader, b"c"), (COMPRESSION_LZ4, found(b"after")));
}
