//! `RepeatableRead` against the client protocol a Merkle-DAG CRDT store runs
//! over a collection's head set: a DAG whose tips are derived from keys and
//! reclaimed by a separate sweep.
//!
//! A head key `h/{block}` names a DAG tip candidate; a marker key
//! `m/{parent}/{child}` records that `child` supersedes `parent`. Live
//! heads are the head keys with no marker naming them as a parent.
//! Appending a block scans for the live heads, writes the new head and a
//! marker against each one, and commits. Reclamation scans the same two
//! prefixes and deletes every head (and its markers) that a marker
//! already names as a parent. `RepeatableRead` is the level this protocol
//! wants: the live-heads scan is allowed to be stale by construction, and
//! only a point read or a write should ever cost a conflict.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Instant;

use regolith::{
    IsolationLevel, OptimisticTransactionDb, Options, Transaction, TransactionError, TxnOptions,
};
use tempfile::TempDir;

fn opt_db(dir: &TempDir) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap()
}

fn block_id(n: u64) -> String {
    format!("{n:06}")
}

fn head_key(n: u64) -> Vec<u8> {
    format!("h/{}", block_id(n)).into_bytes()
}

fn marker_key(parent: u64, child: u64) -> Vec<u8> {
    format!("m/{}/{}", block_id(parent), block_id(child)).into_bytes()
}

fn parse_head(key: &[u8]) -> u64 {
    std::str::from_utf8(&key[b"h/".len()..])
        .unwrap_or_else(|e| panic!("head key {key:?} is not utf8: {e}"))
        .parse()
        .unwrap_or_else(|e| panic!("head key {key:?} does not end in a block id: {e}"))
}

fn parse_marker(key: &[u8]) -> (u64, u64) {
    let rest = std::str::from_utf8(&key[b"m/".len()..])
        .unwrap_or_else(|e| panic!("marker key {key:?} is not utf8: {e}"));
    let (parent, child) = rest
        .split_once('/')
        .unwrap_or_else(|| panic!("marker key {key:?} has no child segment"));
    (
        parent
            .parse()
            .unwrap_or_else(|e| panic!("marker parent {parent:?} in {key:?}: {e}")),
        child
            .parse()
            .unwrap_or_else(|e| panic!("marker child {child:?} in {key:?}: {e}")),
    )
}

/// Live heads of the snapshot `txn` reads from: head keys with no marker
/// naming them as a parent, from two scans over the `h/` and `m/`
/// prefixes.
fn live_heads(txn: &Transaction) -> Vec<u64> {
    let heads: Vec<u64> = txn
        .scan_stream(Some(b"h/"), Some(b"h0"))
        .map(|item| parse_head(&item.unwrap().0))
        .collect();
    let named_as_parent: HashSet<u64> = txn
        .scan_stream(Some(b"m/"), Some(b"m0"))
        .map(|item| parse_marker(&item.unwrap().0).0)
        .collect();
    heads
        .into_iter()
        .filter(|h| !named_as_parent.contains(h))
        .collect()
}

/// Bounds the retry loop on [`TransactionError::Busy`] so a stuck
/// pessimistic-style lock (never expected here, since both flavors this
/// file drives are optimistic) surfaces as a panic instead of a hang.
const MAX_BUSY_ATTEMPTS: usize = 1000;

/// Append `block`, deriving its parents from the current live heads.
/// Retries `Busy`; a `Conflict` is counted and the attempt is abandoned
/// rather than retried, since the protocol never expects a live-heads
/// scan to cost one at `RepeatableRead`.
fn try_append(
    db: &OptimisticTransactionDb,
    level: IsolationLevel,
    block: u64,
    log: &Mutex<Vec<(u64, Vec<u64>)>>,
    conflicts: &AtomicU64,
) {
    for _ in 0..MAX_BUSY_ATTEMPTS {
        let txn = db.begin(&TxnOptions::new().isolation(level));
        let heads = live_heads(&txn);
        txn.put(&head_key(block), b"0")
            .unwrap_or_else(|e| panic!("put head {block}: {e}"));
        for &parent in &heads {
            txn.put(&marker_key(parent, block), b"0")
                .unwrap_or_else(|e| panic!("put marker {parent}/{block}: {e}"));
        }
        match txn.commit() {
            Ok(_) => {
                log.lock()
                    .unwrap_or_else(|e| panic!("log lock: {e}"))
                    .push((block, heads));
                return;
            }
            Err(TransactionError::Busy(_)) => continue,
            Err(TransactionError::Conflict { .. }) => {
                conflicts.fetch_add(1, Ordering::Relaxed);
                return;
            }
            Err(e) => panic!("append block {block}: {e}"),
        }
    }
    panic!("append block {block} exhausted {MAX_BUSY_ATTEMPTS} busy retries");
}

/// Reclaim every head the current snapshot shows as superseded: a head
/// key that is both currently live (present in the `h/` scan) and named
/// as a parent by some marker. A marker whose parent's head key was
/// already deleted in an earlier pass is left alone: it is an orphan by
/// construction, and the protocol keeps those on purpose.
fn try_prune(db: &OptimisticTransactionDb, level: IsolationLevel, conflicts: &AtomicU64) {
    for _ in 0..MAX_BUSY_ATTEMPTS {
        let txn = db.begin(&TxnOptions::new().isolation(level));
        let heads: Vec<u64> = txn
            .scan_stream(Some(b"h/"), Some(b"h0"))
            .map(|item| parse_head(&item.unwrap().0))
            .collect();
        let markers: Vec<(u64, u64)> = txn
            .scan_stream(Some(b"m/"), Some(b"m0"))
            .map(|item| parse_marker(&item.unwrap().0))
            .collect();
        let named_as_parent: HashSet<u64> = markers.iter().map(|&(parent, _)| parent).collect();
        let superseded: HashSet<u64> = heads
            .into_iter()
            .filter(|h| named_as_parent.contains(h))
            .collect();
        for &parent in &superseded {
            txn.delete(&head_key(parent))
                .unwrap_or_else(|e| panic!("delete head {parent}: {e}"));
        }
        for &(parent, child) in &markers {
            if superseded.contains(&parent) {
                txn.delete(&marker_key(parent, child))
                    .unwrap_or_else(|e| panic!("delete marker {parent}/{child}: {e}"));
            }
        }
        match txn.commit() {
            Ok(_) => return,
            Err(TransactionError::Busy(_)) => continue,
            Err(TransactionError::Conflict { .. }) => {
                conflicts.fetch_add(1, Ordering::Relaxed);
                return;
            }
            Err(e) => panic!("prune: {e}"),
        }
    }
    panic!("prune exhausted {MAX_BUSY_ATTEMPTS} busy retries");
}

/// Reproduces the client protocol a Merkle-DAG CRDT store runs over a
/// collection's head set: appenders extend the DAG by scanning for
/// live heads and marking each one as a parent, while a separate sweep
/// reclaims heads a marker has since superseded. Both run concurrently at
/// `RepeatableRead`, the level for a scan of a set concurrent writers only add
/// to or reclaim.
///
/// Fails under: `IsolationLevel::validates_scanned_keys` returning `true`
/// for `RepeatableRead` (an appender then aborts whenever a reclamation lands
/// between its live-heads scan and its commit, and the conflict counts
/// below stop being zero).
#[test]
fn repeatable_read_appends_never_abort_under_reclamation() {
    const APPENDER_THREADS: u64 = 4;
    const APPENDS_PER_THREAD: u64 = 200;

    let dir = TempDir::new().unwrap();
    let db = opt_db(&dir);
    db.db().put(&head_key(0), b"0").unwrap();

    let next_block = AtomicU64::new(1);
    let log: Mutex<Vec<(u64, Vec<u64>)>> = Mutex::new(vec![(0, Vec::new())]);
    let appender_conflicts = AtomicU64::new(0);
    let pruner_conflicts = AtomicU64::new(0);
    let appenders_done = AtomicBool::new(false);

    let start = Instant::now();
    thread::scope(|scope| {
        let appender_handles: Vec<_> = (0..APPENDER_THREADS)
            .map(|_| {
                let db = &db;
                let next_block = &next_block;
                let log = &log;
                let appender_conflicts = &appender_conflicts;
                scope.spawn(move || {
                    for _ in 0..APPENDS_PER_THREAD {
                        let block = next_block.fetch_add(1, Ordering::Relaxed);
                        try_append(
                            db,
                            IsolationLevel::RepeatableRead,
                            block,
                            log,
                            appender_conflicts,
                        );
                    }
                })
            })
            .collect();

        scope.spawn(|| {
            loop {
                try_prune(&db, IsolationLevel::RepeatableRead, &pruner_conflicts);
                if appenders_done.load(Ordering::Acquire) {
                    break;
                }
                thread::yield_now();
            }
            // The final pass: appenders are joined and their commits are
            // fully visible, so this is the one snapshot in which "every
            // superseded head that still exists" and "every superseded
            // head" coincide.
            try_prune(&db, IsolationLevel::RepeatableRead, &pruner_conflicts);
        });

        for handle in appender_handles {
            handle
                .join()
                .unwrap_or_else(|e| std::panic::resume_unwind(e));
        }
        appenders_done.store(true, Ordering::Release);
    });
    let wall = start.elapsed();
    println!("repeatable_read_appends_never_abort_under_reclamation wall time: {wall:?}");

    assert_eq!(
        appender_conflicts.load(Ordering::Relaxed),
        0,
        "an appender aborted on a scanned-only key at RepeatableRead"
    );
    assert_eq!(
        pruner_conflicts.load(Ordering::Relaxed),
        0,
        "the sole pruner aborted, which no writer here should ever cause"
    );

    let log = log.into_inner().unwrap_or_else(|e| panic!("log lock: {e}"));
    let all_blocks: HashSet<u64> = log.iter().map(|&(block, _)| block).collect();
    let named_as_parent: HashSet<u64> = log
        .iter()
        .flat_map(|(_, parents)| parents.iter().copied())
        .collect();
    let mut expected_tips: Vec<u64> = all_blocks.difference(&named_as_parent).copied().collect();
    expected_tips.sort_unstable();

    let mut remaining_heads: Vec<u64> = db
        .db()
        .scan_stream(Some(b"h/"), Some(b"h0"))
        .unwrap_or_else(|e| panic!("final head scan: {e}"))
        .map(|entry| parse_head(&entry.unwrap().0))
        .collect();
    remaining_heads.sort_unstable();
    assert_eq!(
        remaining_heads, expected_tips,
        "the heads left after the final prune must be exactly the DAG's tips"
    );

    let remaining_marker_parents: HashSet<u64> = db
        .db()
        .scan_stream(Some(b"m/"), Some(b"m0"))
        .unwrap_or_else(|e| panic!("final marker scan: {e}"))
        .map(|entry| parse_marker(&entry.unwrap().0).0)
        .collect();
    for &tip in &remaining_heads {
        assert!(
            !remaining_marker_parents.contains(&tip),
            "tip {tip} still has a marker naming it as a parent"
        );
    }
}

/// The calibration for the test above: the same scan-then-reclaim-then-
/// write schedule aborts at `Serializable`, where a scanned key is
/// validated, and survives at `RepeatableRead`, where it is not. Pinned to a
/// fixed schedule on one thread, so the difference between the two
/// levels is exactly the one this test names.
///
/// Fails under: `validates_scanned_keys` returning `false` for
/// `Serializable` (the first half would then commit) or `true` for
/// `RepeatableRead` (the second half would then conflict).
#[test]
fn serializable_aborts_an_append_when_reclamation_commits_underneath() {
    for (level, expect_conflict) in [
        (IsolationLevel::Serializable, true),
        (IsolationLevel::RepeatableRead, false),
    ] {
        let dir = TempDir::new().unwrap();
        let db = opt_db(&dir);
        db.db().put(&head_key(0), b"0").unwrap();

        // One committed append of block 1 supersedes h/000000: it is
        // still a head key in the store, but a marker now names it as a
        // parent.
        {
            let txn = db.begin(&TxnOptions::new().isolation(level));
            let heads = live_heads(&txn);
            assert_eq!(heads, [0], "{level:?}: only the seeded head is live");
            txn.put(&head_key(1), b"0").unwrap();
            for &parent in &heads {
                txn.put(&marker_key(parent, 1), b"0").unwrap();
            }
            txn.commit()
                .unwrap_or_else(|e| panic!("{level:?}: seed append: {e}"));
        }

        // The probe: this scan yields h/000000 (superseded, but still
        // present), then a concurrent prune reclaims it before this
        // transaction writes and commits.
        let appender = db.begin(&TxnOptions::new().isolation(level));
        let heads = live_heads(&appender);
        assert_eq!(heads, [1], "{level:?}: block 1 is the only live head");

        let ignored_prune_conflicts = AtomicU64::new(0);
        try_prune(&db, level, &ignored_prune_conflicts);
        assert_eq!(
            ignored_prune_conflicts.load(Ordering::Relaxed),
            0,
            "{level:?}: the prune has no concurrent writer to conflict with"
        );

        appender.put(&head_key(2), b"0").unwrap();
        for &parent in &heads {
            appender.put(&marker_key(parent, 2), b"0").unwrap();
        }
        let result = appender.commit();
        assert_eq!(
            matches!(result, Err(TransactionError::Conflict { .. })),
            expect_conflict,
            "{level:?}: append after a concurrent reclamation of a scanned-but-superseded head: {result:?}"
        );
    }
}

/// `RepeatableRead` keeps the point-read half of `Serializable`: a plain `get`
/// is validated at commit even though it is never promoted through
/// `get_for_update`. Eight threads racing a read-modify-write of one
/// counter through `get` must still land every increment.
///
/// Fails under: `IsolationLevel::validates_every_read` returning `false`
/// for `RepeatableRead` (increments are then lost to the lost-update anomaly
/// and the final counter lands below the expected total).
#[test]
fn repeatable_read_read_modify_write_of_a_register_never_loses_an_update() {
    const THREADS: u64 = 8;
    const INCREMENTS_PER_THREAD: u64 = 100;
    const MAX_ATTEMPTS: u32 = 10_000;

    fn increment_once(db: &OptimisticTransactionDb, attempts: &AtomicU64) {
        for _ in 0..MAX_ATTEMPTS {
            attempts.fetch_add(1, Ordering::Relaxed);
            let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
            let current = match txn.get(b"counter") {
                Ok(None) => 0,
                Ok(Some(bytes)) => u64::from_le_bytes(
                    bytes
                        .as_slice()
                        .try_into()
                        .unwrap_or_else(|_| panic!("counter is {} bytes, expected 8", bytes.len())),
                ),
                Err(e) => panic!("read counter: {e}"),
            };
            txn.put(b"counter", &(current + 1).to_le_bytes())
                .unwrap_or_else(|e| panic!("write counter: {e}"));
            match txn.commit() {
                Ok(_) => return,
                Err(TransactionError::Conflict { .. } | TransactionError::Busy(_)) => {
                    thread::yield_now();
                }
                Err(e) => panic!("commit counter: {e}"),
            }
        }
        panic!("increment exhausted {MAX_ATTEMPTS} attempts without committing");
    }

    let dir = TempDir::new().unwrap();
    let db = opt_db(&dir);
    let attempts = AtomicU64::new(0);

    thread::scope(|scope| {
        for _ in 0..THREADS {
            let db = &db;
            let attempts = &attempts;
            scope.spawn(move || {
                for _ in 0..INCREMENTS_PER_THREAD {
                    increment_once(db, attempts);
                }
            });
        }
    });

    let total_attempts = attempts.load(Ordering::Relaxed);
    println!(
        "repeatable_read_read_modify_write_of_a_register_never_loses_an_update total attempts: {total_attempts}"
    );

    let final_value = match db.db().get(b"counter").unwrap() {
        None => 0,
        Some(bytes) => u64::from_le_bytes(
            bytes
                .as_slice()
                .try_into()
                .unwrap_or_else(|_| panic!("counter is {} bytes, expected 8", bytes.len())),
        ),
    };
    assert_eq!(
        final_value,
        THREADS * INCREMENTS_PER_THREAD,
        "lost an update after {total_attempts} total attempts across {THREADS} threads"
    );
}
