//! Coverage for the record-length limit on [`Db::write`]: a batch whose
//! framed WAL record would exceed the limit is refused before it is
//! applied, and `disable_wal` is exempt.

use tempfile::TempDir;

use super::*;
use crate::engine::wal::{MAX_RECORD_LEN, ops_record_len};

fn open_tmp() -> (Db, TempDir) {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    (db, dir)
}

/// Puts under `count` keys whose values are zeroed and sized so the
/// write frames to exactly `framed` bytes. Uses the fact that a
/// record's length is its length with empty values plus the sum of the
/// value lengths.
fn puts_framing_to(framed: usize, count: usize) -> Vec<WriteBatchOp> {
    let mut ops: Vec<WriteBatchOp> = (0..count)
        .map(|i| WriteBatchOp::Put {
            key: prefix_key(DEFAULT_CF_ID, format!("k{i:02}").as_bytes()),
            value: Vec::new(),
        })
        .collect();
    let base = ops_record_len(&ops);
    let mut remaining = framed - base;
    for op in &mut ops {
        let WriteBatchOp::Put { value, .. } = op else {
            unreachable!("puts_framing_to only builds Put ops")
        };
        let take = remaining.min(DEFAULT_MAX_VALUE_SIZE);
        *value = vec![0u8; take];
        remaining -= take;
    }
    assert_eq!(ops_record_len(&ops), framed);
    ops
}

fn wal_dir_bytes(dir: &TempDir) -> u64 {
    std::fs::read_dir(dir.path().join("wal"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.metadata().unwrap().len())
        .sum()
}

fn exact_message(framed_len: usize, limit: usize) -> String {
    format!(
        "write is too large: it would log {framed_len} bytes and one write can log at most \
         {limit}; split it into smaller writes"
    )
}

fn assert_invalid_argument(err: Error, framed_len: usize) {
    match err {
        Error::InvalidArgument(msg) => {
            assert_eq!(msg, exact_message(framed_len, MAX_RECORD_LEN as usize));
        }
        other => panic!("expected Error::InvalidArgument, got {other:?}"),
    }
}

#[test]
fn db_write_is_refused_one_byte_past_the_record_limit() {
    let (db, _dir) = open_tmp();

    let batch = WriteBatch {
        ops: puts_framing_to(MAX_RECORD_LEN as usize, 17),
    };
    assert!(db.validate_batch_sizes(&batch, false).is_ok());

    let batch = WriteBatch {
        ops: puts_framing_to(MAX_RECORD_LEN as usize + 1, 17),
    };
    let err = db
        .validate_batch_sizes(&batch, false)
        .expect_err("a batch one byte past the record limit must be refused");
    assert_invalid_argument(err, MAX_RECORD_LEN as usize + 1);

    assert!(db.validate_batch_sizes(&batch, true).is_ok());
}

#[test]
fn a_refused_db_write_leaves_nothing_visible_and_consumes_no_sequence() {
    let (db, dir) = open_tmp();
    db.put(b"before", b"kept").unwrap();
    let seq0 = db.latest_sequence();
    let wal_bytes_before = wal_dir_bytes(&dir);

    let batch = WriteBatch {
        ops: puts_framing_to(MAX_RECORD_LEN as usize + 1, 17),
    };
    let err = db
        .write(batch)
        .expect_err("an oversized write must be refused");
    assert_invalid_argument(err, MAX_RECORD_LEN as usize + 1);

    assert_eq!(db.latest_sequence(), seq0);
    assert_eq!(db.get(b"k00").unwrap(), None);
    assert_eq!(wal_dir_bytes(&dir), wal_bytes_before);

    let mut canary = WriteBatch::new();
    canary.put(b"canary", b"v");
    assert_eq!(db.write_sequenced(canary).unwrap(), seq0 + 1);

    drop(db);
    let db = Db::open(dir.path(), Options::default()).unwrap();
    assert_eq!(db.get(b"before").unwrap(), Some(b"kept".to_vec()));
    assert_eq!(db.get(b"k00").unwrap(), None);
}
