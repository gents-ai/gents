//! What a transactional scan costs at commit, per isolation level.
//!
//! A transaction scans `n` keys, writes one key outside the scanned range
//! and commits. `Serializable` holds one read-set entry per scanned key and
//! checks each at commit under the write pipeline; `RepeatableRead` and
//! `SnapshotIsolation` hold one entry per stretch. The commit is timed on
//! its own, so the number is the per-key check and not the scan, and the
//! whole transaction is timed beside it so the scan's own cost per level is
//! visible too.

mod common;

use std::time::Instant;

use regolith::{IsolationLevel, OptimisticTransactionDb, TxnOptions};

const LEVELS: [(IsolationLevel, &str); 4] = [
    (IsolationLevel::SnapshotIsolation, "snapshot"),
    (IsolationLevel::RepeatableRead, "repeatable-read"),
    (IsolationLevel::Serializable, "serializable"),
    (IsolationLevel::DefraLevel, "defralevel"),
];

/// The scanned range. The transaction's own write sits outside it, so every
/// rep scans exactly the seeded keys.
const RANGE: (&[u8], &[u8]) = (b"scan/", b"scan0");
const OWN_KEY: &[u8] = b"zzz/own";

fn seed(db: &OptimisticTransactionDb, n: usize) {
    for i in 0..n {
        db.db()
            .put(format!("scan/{i:08}").as_bytes(), b"v")
            .unwrap_or_else(|e| panic!("seed: {e}"));
    }
}

/// Median microseconds of (commit alone, scan and commit) over `reps`
/// transactions that each scan all `n` keys, write one key and commit.
fn micros(
    db: &OptimisticTransactionDb,
    level: IsolationLevel,
    n: usize,
    reps: usize,
) -> (f64, f64) {
    let mut commit = Vec::with_capacity(reps);
    let mut total = Vec::with_capacity(reps);
    for rep in 0..reps {
        let started = Instant::now();
        let txn = db.begin(&TxnOptions::new().isolation(level));
        let yielded = txn.scan_stream(Some(RANGE.0), Some(RANGE.1)).count();
        assert_eq!(
            yielded, n,
            "{level:?}: the scan must yield every seeded key"
        );
        txn.put(OWN_KEY, &rep.to_le_bytes())
            .unwrap_or_else(|e| panic!("put: {e}"));
        let committing = Instant::now();
        txn.commit()
            .unwrap_or_else(|e| panic!("{level:?}: commit: {e}"));
        commit.push(committing.elapsed().as_secs_f64() * 1e6);
        total.push(started.elapsed().as_secs_f64() * 1e6);
    }
    (common::median(&mut commit), common::median(&mut total))
}

fn main() {
    let quick = common::args()
        .iter()
        .any(|a| a == "--quick" || a == "--test");
    let (sizes, reps): (&[usize], usize) = if quick {
        (&[32, 1024], 20)
    } else {
        (&[32, 1024, 16384], 200)
    };

    println!("scan commit: a transaction scans n keys, writes one of its own, commits");
    let mut rows = Vec::with_capacity(sizes.len() * LEVELS.len());
    for &n in sizes {
        let tmp = common::TempDb::new(&format!("scan-commit-{n}"));
        let db = OptimisticTransactionDb::open(tmp.path(), common::default_opts())
            .unwrap_or_else(|e| panic!("open: {e}"));
        seed(&db, n);
        for (level, name) in LEVELS {
            let (commit_us, total_us) = micros(&db, level, n, reps);
            println!(
                "{n:>6} keys  {name:<13} commit {commit_us:>9.1} us   scan+commit {total_us:>9.1} us"
            );
            rows.push(format!(
                "{{\"keys\":{n},\"isolation\":\"{name}\",\
                 \"commit_us\":{commit_us:.1},\"scan_and_commit_us\":{total_us:.1}}}"
            ));
        }
    }
    common::write_family(
        "scan_commit",
        &format!(
            "{{\"quick\":{quick},\"reps\":{reps},\"rows\":[{}]}}",
            rows.join(",")
        ),
    );
}
