//! Coverage for the record-length limit at transaction commit: a commit
//! whose framed WAL record would exceed the limit is refused before it
//! waits, applies nothing and consumes no sequence number.

use tempfile::TempDir;

use super::*;
use crate::WriteBatch;
use crate::WriteBatchOp;
use crate::engine::wal::{MAX_RECORD_LEN, ops_record_len};

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
        let take = remaining.min(crate::DEFAULT_MAX_VALUE_SIZE);
        *value = vec![0u8; take];
        remaining -= take;
    }
    assert_eq!(ops_record_len(&ops), framed);
    ops
}

/// Buffer 17 zeroed values straight into `tx.writes`: `Transaction::put`
/// would copy each one again into a fresh buffer of its own, and the
/// values here are already the buffers the commit will frame.
fn refuses_an_oversized_commit(db: &Db, tx: Transaction) {
    let seq0 = db.latest_sequence();

    for op in puts_framing_to(MAX_RECORD_LEN as usize + 1, 17) {
        let WriteBatchOp::Put { key, value } = op else {
            unreachable!("puts_framing_to only builds Put ops")
        };
        tx.writes.insert(key, Write::Put(value));
    }

    let err = tx
        .commit()
        .expect_err("an oversized commit must be refused");
    let TransactionError::Engine(Error::InvalidArgument(message)) = err else {
        panic!("expected TransactionError::Engine(InvalidArgument), got {err:?}");
    };
    assert_eq!(
        message,
        format!(
            "write is too large: it would log {} bytes and one write can log at most {}; \
             split it into smaller writes",
            MAX_RECORD_LEN as usize + 1,
            MAX_RECORD_LEN
        )
    );

    assert_eq!(db.latest_sequence(), seq0);
    assert_eq!(db.get(b"k00").unwrap(), None);

    let mut canary = WriteBatch::new();
    canary.put(b"canary", b"v");
    assert_eq!(db.write_sequenced(canary).unwrap(), seq0 + 1);
}

#[test]
fn an_oversized_commit_is_refused_and_leaves_no_trace() {
    let dir = TempDir::new().expect("tempdir");
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).expect("open");
    refuses_an_oversized_commit(db.db(), db.begin(&TxnOptions::new()));

    let dir = TempDir::new().expect("tempdir");
    let db = TransactionDb::open(dir.path(), Options::default()).expect("open");
    refuses_an_oversized_commit(db.db(), db.begin(&TxnOptions::new()));
}
