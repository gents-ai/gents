//! The workload stripes exist for: one hot counter merged into by
//! overlapping transactions, so a snapshot is live at every instant.
//!
//! Each transaction holds the snapshot it began at until it commits, a
//! window of them stay open at once, and the counter is flushed and
//! compacted throughout. The count it checks is exact: the key's entries
//! across every SSTable, read back through the engine.

use std::collections::VecDeque;
use std::sync::Arc;

use tempfile::TempDir;

use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::{IsolationLevel, OptimisticTransactionDb, Options, TxnOptions};

use super::fixtures::Sum;

/// Transactions open at once, each holding its begin snapshot.
const WINDOW: usize = 8;
const COMMITS: usize = 3_000;
const COMPACT_EVERY: usize = 500;

#[test]
fn a_hot_counter_stays_folded_under_rolling_transactions() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default()
            .merge_operator(Some(Arc::new(Sum)))
            // Compaction runs only where this test asks for it, so the
            // counts below do not depend on a worker's timing.
            .max_background_compactions(0),
    )
    .unwrap();
    let counter = prefix_key(DEFAULT_CF_ID, b"counter");
    let entries = || {
        db.db()
            .engine()
            .all_persisted_versions_of(&counter)
            .unwrap()
            .len()
    };
    let pins = || db.db().get_int_property("regolith.num-snapshots").unwrap();

    db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();
    // Held for the whole run, so the key's oldest stripe is always live.
    let first_reader = db.db().snapshot();

    let mut open = VecDeque::new();
    let mut worst = 0;
    for commit in 1..=COMMITS {
        let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
        open.push_back(tx);
        if open.len() > WINDOW {
            open.pop_front().unwrap().commit().unwrap();
        }
        if commit % 50 == 0 {
            db.db().flush().unwrap();
        }
        if commit % COMPACT_EVERY == 0 {
            db.db().compact_range(None, None).wait().unwrap();
            // One stripe per live snapshot, and the top one above them all.
            let bound = pins() as usize + 1;
            let kept = entries();
            worst = worst.max(kept);
            assert!(
                kept <= bound,
                "{kept} entries on disk after {commit} merges with {} snapshots live",
                pins()
            );
        }
    }

    let live = pins();
    println!(
        "hot counter: {} entries on disk after {COMMITS} merges, {live} snapshots live, \
         at most {worst} after any compaction",
        entries()
    );

    for tx in open {
        tx.commit().unwrap();
    }
    let sum = |bytes: Vec<u8>| i64::from_be_bytes(bytes.try_into().unwrap());
    assert_eq!(
        sum(db.db().get(b"counter").unwrap().unwrap()),
        COMMITS as i64
    );
    assert_eq!(
        sum(first_reader.get(b"counter").unwrap().unwrap()),
        0,
        "the oldest snapshot still reads the value it began at"
    );
}
