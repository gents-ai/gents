use super::*;
use crate::engine::{
    internal_key::{VALUE_TYPE_VALUE, encode_internal_key, user_key_of},
    manifest::VersionSet,
    sstable::{SsTableMeta, SsTableReader},
};
use crate::options::{CompactionDecision, CompactionFilter, CompressionType};
use std::sync::atomic::{AtomicUsize, Ordering};

struct CacheInspector {
    cache: Arc<BlockCache>,
    calls: AtomicUsize,
}

impl CompactionFilter for CacheInspector {
    fn filter(&self, _level: usize, _key: &[u8], _value: &[u8]) -> CompactionDecision {
        assert_eq!(self.cache.usage(), 0, "compaction populated the read cache");
        self.calls.fetch_add(1, Ordering::Relaxed);
        CompactionDecision::Keep
    }

    fn name(&self) -> &'static str {
        "cache-inspector"
    }
}

#[test]
fn compaction_does_not_populate_the_foreground_cache() {
    for partitioned in [false, true] {
        for compression in [CompressionType::None, CompressionType::Lz4] {
            let dir = tempfile::tempdir().unwrap();
            let sst_dir = dir.path().join("sst");
            std::fs::create_dir_all(&sst_dir).unwrap();
            let versions = Arc::new(VersionStore::new(
                VersionSet::open(dir.path(), &sst_dir).unwrap(),
            ));
            let path = sst_dir.join(sst_filename(1));
            let mut writer =
                SsTableWriter::new(&path, 64, 10, compression, None, partitioned, 96).unwrap();
            let mut expected = Vec::new();
            for i in 0..256 {
                let key = format!("k{i:04}").into_bytes();
                let value = vec![(i % 251) as u8; 256];
                writer
                    .add(&encode_internal_key(&key, 1, VALUE_TYPE_VALUE), &value)
                    .unwrap();
                expected.push((key, value));
            }
            let summary = writer.finish().unwrap().unwrap();
            let input = LiveSst::new(
                SsTableMeta {
                    file_id: 1,
                    smallest_key: summary.smallest_user_key,
                    largest_key: summary.largest_user_key,
                    file_size: std::fs::metadata(&path).unwrap().len(),
                    num_entries: summary.num_entries,
                    global_seq: None,
                    deletions: Some(summary.deletions),
                },
                Arc::new(SsTableReader::open(&path, 1).unwrap()),
                summary.newest_seq,
            );
            versions
                .run(|versions| {
                    versions.apply(&[
                        VersionEdit::SetNextFileId(2),
                        VersionEdit::SetLastSeq(summary.newest_seq),
                        VersionEdit::AddFile {
                            level: 0,
                            file: Arc::clone(&input),
                        },
                    ])
                })
                .unwrap();
            let cache = Arc::new(BlockCache::new(1024 * 1024));
            let inspector = Arc::new(CacheInspector {
                cache: Arc::clone(&cache),
                calls: AtomicUsize::new(0),
            });
            let opts = CompactionOptions {
                compaction_filter: Some(inspector.clone()),
                compression,
                partitioned_index: partitioned,
                metadata_block_size: 96,
                ..CompactionOptions::default()
            };
            perform_compaction(
                &versions,
                &sst_dir,
                &cache,
                &opts,
                0,
                vec![input],
                Vec::new(),
                &SnapshotRegistry::new(),
            )
            .unwrap();
            assert_eq!(inspector.calls.load(Ordering::Relaxed), expected.len());
            let version = versions.current();
            assert!(version.levels[0].is_empty());
            let read_cache = BlockCache::new(0);
            let actual: Vec<_> = version.levels[1]
                .iter()
                .flat_map(|file| file.reader.iter_internal(&read_cache).unwrap())
                .map(|(key, value)| (user_key_of(&key).to_vec(), value))
                .collect();
            assert_eq!(actual, expected);
        }
    }
}
