use regolith::{CompressionType, Db, Error, MergeOperator, Options};
use std::sync::Arc;
use tempfile::TempDir;

#[test]
fn guarded_public_reads_preserve_snapshot_tombstone_and_cf_visibility() {
    for compression in [
        CompressionType::None,
        CompressionType::Lz4,
        CompressionType::Snappy,
    ] {
        let dir = TempDir::new().unwrap();
        let db = Db::open(
            dir.path(),
            Options::default()
                .compression(compression)
                .l0_compaction_trigger(100),
        )
        .unwrap();
        let cf = db.create_column_family("other").unwrap();
        for key in [b"point".as_slice(), b"range", b"live", b"empty"] {
            let value = if key == b"empty" {
                b"".as_slice()
            } else {
                b"old"
            };
            db.put(key, value).unwrap();
            db.put_cf(&cf, key, value).unwrap();
        }
        assert_eq!(db.get_size_with_limit(b"live", 0).unwrap(), Some(3));
        let snap = db.snapshot();
        db.flush().unwrap();
        db.delete(b"point").unwrap();
        db.delete_cf(&cf, b"point").unwrap();
        db.delete_range(b"range", b"s").unwrap();
        db.delete_range_cf(&cf, b"range", b"s").unwrap();
        db.put(b"live", b"new value").unwrap();
        db.put_cf(&cf, b"live", b"new value").unwrap();
        db.put(b"later", b"added").unwrap();
        db.put_cf(&cf, b"later", b"added").unwrap();
        // Exercise both active tombstones and flushed tombstones over older SST values.
        for flushed in [false, true] {
            if flushed {
                db.flush().unwrap();
            }
            for (key, old, current) in [
                (b"point".as_slice(), Some(3), None),
                (b"range", Some(3), None),
                (b"live", Some(3), Some(9)),
                (b"empty", Some(0), Some(0)),
                (b"later", None, Some(5)),
                (b"missing", None, None),
            ] {
                assert_eq!(db.get_size_with_limit(key, 65536).unwrap(), current);
                assert_eq!(snap.get_size_with_limit(key, 65536).unwrap(), old);
                assert_eq!(db.get_size_cf_with_limit(&cf, key, 65536).unwrap(), current);
                assert_eq!(snap.get_size_cf_with_limit(&cf, key, 65536).unwrap(), old);
                assert_eq!(
                    db.get_size_with_limit(key, 65536).unwrap(),
                    db.get_size(key).unwrap()
                );
            }
        }
        // All four APIs must reject prewarmed blocks despite allocating no new backing buffer.
        for result in [
            db.get_size_with_limit(b"live", 1),
            snap.get_size_with_limit(b"live", 1),
            db.get_size_cf_with_limit(&cf, b"live", 1),
            snap.get_size_cf_with_limit(&cf, b"live", 1),
        ] {
            assert!(matches!(
                result,
                Err(Error::DataBlockLimitExceeded {
                    max_data_block_bytes: 1
                })
            ));
        }
    }
}

#[derive(Debug)]
struct NeverMerge;
impl MergeOperator for NeverMerge {
    fn name(&self) -> &'static str {
        "never"
    }
    fn full_merge(&self, _: &[u8], _: Option<&[u8]>, _: &[&[u8]]) -> Option<Vec<u8>> {
        panic!("guarded read invoked merge callback")
    }
}

#[test]
fn guarded_reads_reject_configured_merges_even_for_missing_keys() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default().merge_operator(Some(Arc::new(NeverMerge))),
    )
    .unwrap();
    let cf = db.create_column_family("other").unwrap();
    db.merge(b"merge", b"operand").unwrap();
    db.merge_cf(&cf, b"merge", b"operand").unwrap();
    db.put(b"plain", b"value").unwrap();
    let snap = db.snapshot();
    for key in [b"merge".as_slice(), b"plain", b"missing"] {
        for result in [
            db.get_size_with_limit(key, usize::MAX),
            snap.get_size_with_limit(key, usize::MAX),
            db.get_size_cf_with_limit(&cf, key, usize::MAX),
            snap.get_size_cf_with_limit(&cf, key, usize::MAX),
        ] {
            assert!(matches!(result, Err(Error::InvalidArgument(_))));
        }
    }
}
