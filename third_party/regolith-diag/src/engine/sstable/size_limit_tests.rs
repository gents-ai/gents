use super::*;
use crate::engine::internal_key::{VALUE_TYPE_VALUE, encode_internal_key};
use tempfile::TempDir;

fn table(codec: CompressionType) -> (TempDir, SsTableReader, BlockHandle, Vec<u8>) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("limit.sst");
    let key = encode_internal_key(b"key", 1, VALUE_TYPE_VALUE);
    let mut writer = SsTableWriter::new(&path, 4096, 10, codec, None, false, 4096).unwrap();
    writer.add(&key, &vec![b'x'; 8192]).unwrap();
    writer.finish().unwrap();
    let reader = SsTableReader::open(&path, 1).unwrap();
    let handle = reader
        .find_block_handle(&key, &BlockCache::new(0))
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
    (dir, reader, handle, frame)
}

fn is_limit<T>(result: io::Result<T>, limit: usize) {
    let err = result.err().expect("must reject");
    assert!(
        matches!(crate::Error::from(err), crate::Error::DataBlockLimitExceeded { max_data_block_bytes } if max_data_block_bytes == limit)
    );
}

#[test]
fn combined_boundary_and_warm_cache_all_codecs() {
    for codec in [
        CompressionType::None,
        CompressionType::Lz4,
        CompressionType::Snappy,
    ] {
        let (_dir, reader, handle, frame) = table(codec);
        let decoded = reader
            .read_block(handle, &BlockCache::new(0))
            .unwrap()
            .decoded_buffer_capacity();
        let charge = frame.len() + decoded;
        let cache = BlockCache::new(1024 * 1024);
        is_limit(
            reader.read_block_with_limit(handle, &cache, Some(charge - 1)),
            charge - 1,
        );
        assert!(cache.get(reader.file_id, handle.offset).is_none());
        reader
            .read_block_with_limit(handle, &cache, Some(charge))
            .unwrap();
        assert!(cache.get(reader.file_id, handle.offset).is_some());
        is_limit(reader.read_block_with_limit(handle, &cache, Some(1)), 1);
        is_limit(
            reader.read_block_with_limit(handle, &cache, Some(charge - 1)),
            charge - 1,
        );
        reader
            .read_block_with_limit(handle, &cache, Some(charge))
            .unwrap();
    }
}

// Supplies the small original prefix, but substitutes a checksummed bomb on the full read.
struct ChangingFile {
    prefix: Vec<u8>,
    frame: Vec<u8>,
    full_reads: Arc<AtomicUsize>,
}

impl ReadFile for ChangingFile {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let source = if buf.len() > 5 {
            self.full_reads.fetch_add(1, Ordering::SeqCst);
            &self.frame
        } else {
            &self.prefix
        };
        buf.copy_from_slice(&source[offset as usize..offset as usize + buf.len()]);
        Ok(())
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.frame.len() as u64)
    }
}

fn set_length(frame: &mut [u8], len: u32) {
    frame[1..5].copy_from_slice(&len.to_le_bytes());
    let end = frame.len() - 4;
    let sum = checksum::sst_block(frame[0], &frame[1..end]);
    frame[end..].copy_from_slice(&sum.to_le_bytes());
}

#[test]
fn compressed_bomb_preflight_and_actual_frame_recheck() {
    for codec in [CompressionType::Lz4, CompressionType::Snappy] {
        for changed in [false, true] {
            let (_dir, mut reader, handle, mut frame) = table(codec);
            assert_eq!(handle.offset, 0);
            let original = frame.clone();
            set_length(&mut frame, u32::MAX);
            let reads = Arc::new(AtomicUsize::new(0));
            reader.file = Box::new(ChangingFile {
                prefix: if changed { original } else { frame.clone() },
                frame,
                full_reads: reads.clone(),
            });
            is_limit(
                reader.read_block_with_limit(handle, &BlockCache::new(0), Some(32768)),
                32768,
            );
            assert_eq!(reads.load(Ordering::SeqCst), usize::from(changed));
        }
    }
}

#[test]
fn guarded_corruption_and_lz4_overdeclared_cache_charge() {
    for codec in [
        CompressionType::None,
        CompressionType::Lz4,
        CompressionType::Snappy,
    ] {
        let (_dir, reader, handle, mut frame) = table(codec);
        frame[5] ^= 1;
        let err = reader
            .decode_block_frame_with_limit(handle, &frame, &BlockCache::new(0), Some(32768))
            .err()
            .unwrap();
        assert!(matches!(
            crate::Error::from(err),
            crate::Error::Corruption(_)
        ));
    }
    assert!(check_data_block_header(8, &[COMPRESSION_LZ4, 0, 0, 0, 0], Some(100)).is_err());
    assert!(check_data_block_header(10, &[255], Some(100)).is_err());
    is_limit(
        check_data_block_limit(u64::MAX, 1, Some(usize::MAX)),
        usize::MAX,
    );

    let (_dir, reader, handle, mut frame) = table(CompressionType::Lz4);
    let declared = 16384;
    set_length(&mut frame, declared);
    let cache = BlockCache::new(1024 * 1024);
    let charge = frame.len() + declared as usize;
    is_limit(
        reader.decode_block_frame_with_limit(handle, &frame, &cache, Some(charge - 1)),
        charge - 1,
    );
    reader
        .decode_block_frame_with_limit(handle, &frame, &BlockCache::new(0), Some(charge))
        .unwrap();
    // An ordinary read must keep its existing semantics, even for an overdeclared LZ4 buffer.
    reader.decode_block_frame(handle, &frame, &cache).unwrap();
    is_limit(
        reader.read_block_with_limit(handle, &cache, Some(charge - 1)),
        charge - 1,
    );
    reader
        .read_block_with_limit(handle, &cache, Some(charge))
        .unwrap();
}

#[test]
fn snappy_inner_and_outer_lengths_must_agree() {
    let (_dir, reader, handle, original) = table(CompressionType::Snappy);
    let raw_len = u32::from_le_bytes(original[1..5].try_into().unwrap());
    for declared in [raw_len - 1, raw_len + 1] {
        let mut frame = original.clone();
        set_length(&mut frame, declared);
        let cache = BlockCache::new(1024 * 1024);
        let err = reader
            .decode_block_frame_with_limit(handle, &frame, &cache, Some(32768))
            .err()
            .unwrap();
        assert!(matches!(
            crate::Error::from(err),
            crate::Error::Corruption(_)
        ));
        assert!(cache.get(reader.file_id, handle.offset).is_none());
    }
}
