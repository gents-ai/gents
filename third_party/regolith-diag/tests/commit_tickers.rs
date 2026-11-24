//! Commit and policy tickers: what a caller sees when it polls
//! `Statistics` around transaction commits.
//!
//! Each scenario drives the decision the ticker records and asserts the
//! exact count, so the tickers stay anchored to the decisions that bump
//! them: one per commit for `regolith.commit.count` and
//! `regolith.commit.conflicts`, one per key for the per-key subsets, and
//! one per dropped stretch or commuted blind merge for the policy
//! tickers. The elision and policy tickers count only a commit that
//! succeeds: one that aborts leaves them where they were.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::{
    IsolationLevel, KeyClass, KeyClassifier, MergeOperator, OptimisticTransactionDb, Options,
    Statistics, Ticker, TransactionError, TxResult, TxnOptions,
};

/// Sums big-endian i64 deltas, so two `+1` operands make `+2` and the
/// operation is plainly not idempotent.
struct CounterMerge;

impl MergeOperator for CounterMerge {
    fn name(&self) -> &'static str {
        "counter"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut total: i64 = match base {
            Some(bytes) if bytes.len() == 8 => i64::from_be_bytes(bytes.try_into().unwrap()),
            Some(_) => return None,
            None => 0,
        };
        for operand in operands {
            if operand.len() != 8 {
                return None;
            }
            total = total.wrapping_add(i64::from_be_bytes((*operand).try_into().unwrap()));
        }
        Some(total.to_be_bytes().to_vec())
    }
}

/// Keys under `h/` are added under fresh names and never contended.
struct HeadPrefix;

impl KeyClassifier for HeadPrefix {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"h/") {
            KeyClass::CommutativePrefix { len: 2 }
        } else {
            KeyClass::Ordinary
        }
    }
}

fn open(dir: &std::path::Path, stats: Arc<Statistics>) -> OptimisticTransactionDb {
    let options = Options::default()
        .merge_operator(Some(Arc::new(CounterMerge)))
        .statistics(Some(stats));
    OptimisticTransactionDb::open(dir, options)
        .unwrap()
        .with_policy(Arc::new(HeadPrefix))
}

fn conflicted<T>(result: TxResult<T>) -> bool {
    matches!(result, Err(TransactionError::Conflict { .. }))
}

#[test]
fn a_clean_commit_counts_once_and_a_conflict_counts_once() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), Arc::clone(&stats));
    db.db().put(b"k", b"old").unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    tx.put(b"elsewhere", b"x").unwrap();
    tx.commit().unwrap();
    assert_eq!(stats.get_ticker(Ticker::CommitCount), 1);
    assert_eq!(stats.get_ticker(Ticker::CommitConflicts), 0);

    // A read overtaken by an external write conflicts once, whatever the
    // number of keys the transaction touched.
    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    tx.get(b"k").unwrap();
    db.db().put(b"k", b"new").unwrap();
    tx.put(b"elsewhere", b"y").unwrap();
    assert!(conflicted(tx.commit()));
    assert_eq!(stats.get_ticker(Ticker::CommitCount), 1);
    assert_eq!(stats.get_ticker(Ticker::CommitConflicts), 1);
    assert_eq!(stats.get_ticker(Ticker::CommitConflictsOnRead), 1);
    assert_eq!(stats.get_ticker(Ticker::CommitConflictsOnWrite), 0);
}

#[test]
fn a_conflict_on_a_written_key_is_counted_on_write() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), Arc::clone(&stats));
    db.db().put(b"k", b"old").unwrap();

    // A blind put of a different value than the key now holds: the
    // conflict is on the written key, not on any read.
    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    tx.put(b"k", b"mine").unwrap();
    db.db().put(b"k", b"theirs").unwrap();
    assert!(conflicted(tx.commit()));
    assert_eq!(stats.get_ticker(Ticker::CommitConflicts), 1);
    assert_eq!(stats.get_ticker(Ticker::CommitConflictsOnRead), 0);
    assert_eq!(stats.get_ticker(Ticker::CommitConflictsOnWrite), 1);
}

#[test]
fn an_identical_blind_write_is_elided_not_counted_as_a_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), Arc::clone(&stats));
    db.db().put(b"k", b"same").unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    tx.put(b"k", b"same").unwrap();
    db.db().put(b"k", b"same").unwrap();
    tx.commit().unwrap();

    assert_eq!(stats.get_ticker(Ticker::CommitCount), 1);
    assert_eq!(stats.get_ticker(Ticker::CommitConflicts), 0);
    assert_eq!(stats.get_ticker(Ticker::CommitWritesElided), 1);
}

#[test]
fn a_blind_merge_past_a_newer_operand_commutes_once_per_key() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), Arc::clone(&stats));
    db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
    db.db().merge(b"counter", &1i64.to_be_bytes()).unwrap();
    tx.commit().unwrap();

    assert_eq!(stats.get_ticker(Ticker::CommitCount), 1);
    assert_eq!(stats.get_ticker(Ticker::CommitConflicts), 0);
    assert_eq!(stats.get_ticker(Ticker::PolicyBlindMergesCommuted), 1);
}

#[test]
fn a_scan_inside_a_commutative_prefix_drops_one_stretch() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), Arc::clone(&stats));
    db.db().put(b"h/a", b"cid").unwrap();
    db.db().put(b"h/z", b"cid").unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    let walked: Vec<_> = tx.scan_stream(Some(b"h/"), Some(b"h0")).collect();
    assert!(!walked.is_empty());
    tx.put(b"h/m", b"superseded").unwrap();
    tx.commit().unwrap();

    assert_eq!(stats.get_ticker(Ticker::CommitCount), 1);
    assert_eq!(stats.get_ticker(Ticker::PolicyScanRunsDropped), 1);
}

#[test]
fn a_scan_leaving_the_prefix_keeps_its_stretch() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), Arc::clone(&stats));
    db.db().put(b"h/a", b"cid").unwrap();
    db.db().put(b"i/other", b"x").unwrap();

    // Two transactions scan a stretch that leaves the commutative
    // prefix: the stretch is recorded, so the second commit conflicts
    // where a scan fully inside the prefix would not.
    let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    for tx in [&first, &second] {
        let walked: Vec<_> = tx.scan_stream(Some(b"h/"), Some(b"j")).collect();
        assert!(!walked.is_empty());
        tx.put(b"h/m", b"superseded").unwrap();
    }
    first.commit().unwrap();
    assert!(conflicted(second.commit()));

    assert_eq!(stats.get_ticker(Ticker::PolicyScanRunsDropped), 0);
    assert_eq!(stats.get_ticker(Ticker::CommitConflicts), 1);
}

#[test]
fn without_statistics_nothing_panics_and_the_commit_still_runs() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options::default().merge_operator(Some(Arc::new(CounterMerge)));
    let db = OptimisticTransactionDb::open(dir.path(), options)
        .unwrap()
        .with_policy(Arc::new(HeadPrefix));
    db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
    db.db().merge(b"counter", &1i64.to_be_bytes()).unwrap();
    tx.commit().unwrap();
    assert_eq!(
        db.db().get(b"counter").unwrap().as_deref(),
        Some(2i64.to_be_bytes().as_slice())
    );
}

fn count(stats: &Statistics, ticker: Ticker) -> u64 {
    stats.get_ticker(ticker)
}

/// Commuted blind merges count once per key, not per operand in the
/// transaction or per newer operand under it, and a key with nothing newer
/// does not count at all.
#[test]
fn commuted_blind_merges_count_once_per_key() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), stats.clone());
    for key in [b"a", b"b", b"c", b"d"] {
        db.db().put(key, &0i64.to_be_bytes()).unwrap();
    }

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    for key in [b"a", b"a", b"b", b"c", b"d"] {
        tx.merge(key, &1i64.to_be_bytes()).unwrap();
    }
    // Newer operands on three keys, two of them on `b`; none on `d`.
    for key in [b"a", b"b", b"b", b"c"] {
        db.db().merge(key, &1i64.to_be_bytes()).unwrap();
    }
    stats.reset();
    tx.commit().unwrap();
    assert_eq!(count(&stats, Ticker::PolicyBlindMergesCommuted), 3);
    assert_eq!(count(&stats, Ticker::CommitCount), 1);
    assert_eq!(count(&stats, Ticker::CommitConflicts), 0);
}

/// Elided writes count once per key that a newer identical write made
/// elidable, and never for keys nothing overtook.
#[test]
fn elided_writes_count_once_per_overtaken_key() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), stats.clone());

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    for key in [b"a", b"b", b"c", b"d", b"e"] {
        tx.put(key, b"same").unwrap();
    }
    for key in [b"a", b"b", b"c"] {
        db.db().put(key, b"same").unwrap();
    }
    stats.reset();
    tx.commit().unwrap();
    assert_eq!(count(&stats, Ticker::CommitWritesElided), 3);
    assert_eq!(count(&stats, Ticker::CommitCount), 1);
}

/// A commit with merges and writes that nothing overtook moves neither
/// policy nor elision counters.
#[test]
fn an_uncontended_commit_moves_only_the_commit_count() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), stats.clone());
    db.db().put(b"counter", &0i64.to_be_bytes()).unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    tx.merge(b"counter", &1i64.to_be_bytes()).unwrap();
    tx.put(b"k", b"v").unwrap();
    let _: Vec<_> = tx.scan_stream(Some(b"a"), Some(b"z")).collect();
    stats.reset();
    tx.commit().unwrap();
    assert_eq!(count(&stats, Ticker::CommitCount), 1);
    for ticker in [
        Ticker::CommitConflicts,
        Ticker::CommitConflictsOnRead,
        Ticker::CommitConflictsOnWrite,
        Ticker::CommitWritesElided,
        Ticker::PolicyBlindMergesCommuted,
        Ticker::PolicyScanRunsDropped,
    ] {
        assert_eq!(count(&stats, ticker), 0, "{ticker:?}");
    }
}

/// `a` is overtaken by an identical write and would be elided, but `b`,
/// validated after it, is overtaken by a different one and aborts the
/// commit: nothing was elided.
#[test]
fn an_aborted_commit_counts_no_elided_writes() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), stats.clone());

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    tx.put(b"a", b"same").unwrap();
    tx.put(b"b", b"mine").unwrap();
    db.db().put(b"a", b"same").unwrap();
    db.db().put(b"b", b"theirs").unwrap();
    stats.reset();
    assert!(conflicted(tx.commit()));
    assert_eq!(count(&stats, Ticker::CommitConflicts), 1);
    assert_eq!(count(&stats, Ticker::CommitConflictsOnWrite), 1);
    assert_eq!(count(&stats, Ticker::CommitWritesElided), 0);
    assert_eq!(count(&stats, Ticker::CommitCount), 0);
}

/// A newer operand on `a` commutes, but a newer put on `b`, merged after it,
/// does not and aborts the commit: no blind merge was accepted.
#[test]
fn an_aborted_commit_counts_no_commuted_blind_merges() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), stats.clone());
    db.db().put(b"a", &0i64.to_be_bytes()).unwrap();
    db.db().put(b"b", &0i64.to_be_bytes()).unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    tx.merge(b"a", &1i64.to_be_bytes()).unwrap();
    tx.merge(b"b", &1i64.to_be_bytes()).unwrap();
    db.db().merge(b"a", &1i64.to_be_bytes()).unwrap();
    db.db().put(b"b", &5i64.to_be_bytes()).unwrap();
    stats.reset();
    assert!(conflicted(tx.commit()));
    assert_eq!(count(&stats, Ticker::CommitConflicts), 1);
    assert_eq!(count(&stats, Ticker::CommitConflictsOnWrite), 1);
    assert_eq!(count(&stats, Ticker::PolicyBlindMergesCommuted), 0);
    assert_eq!(count(&stats, Ticker::CommitCount), 0);
}

/// The scan stays inside the commutative prefix and is dropped from
/// validation, but the write to `k` is overtaken and aborts the commit: no
/// stretch was dropped.
#[test]
fn an_aborted_commit_counts_no_dropped_scan_stretches() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), stats.clone());
    db.db().put(b"h/a", b"cid").unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    let walked: Vec<_> = tx.scan_stream(Some(b"h/"), Some(b"h0")).collect();
    assert!(!walked.is_empty());
    tx.put(b"k", b"mine").unwrap();
    db.db().put(b"k", b"theirs").unwrap();
    stats.reset();
    assert!(conflicted(tx.commit()));
    assert_eq!(count(&stats, Ticker::CommitConflicts), 1);
    assert_eq!(count(&stats, Ticker::PolicyScanRunsDropped), 0);
    assert_eq!(count(&stats, Ticker::CommitCount), 0);
}

fn pessimistic(dir: &std::path::Path, stats: Arc<Statistics>) -> regolith::TransactionDb {
    let options = Options::default().statistics(Some(stats));
    regolith::TransactionDb::open(dir, options)
        .unwrap()
        .with_lock_timeout(std::time::Duration::from_millis(50))
}

#[test]
fn pessimistic_commits_and_conflicts_are_counted() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = pessimistic(dir.path(), stats.clone());
    db.db().put(b"k", b"old").unwrap();
    stats.reset();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    tx.put(b"a", b"1").unwrap();
    tx.commit().unwrap();
    assert_eq!(count(&stats, Ticker::CommitCount), 1);

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    tx.get(b"k").unwrap();
    db.db().put(b"k", b"new").unwrap();
    tx.put(b"b", b"1").unwrap();
    assert!(conflicted(tx.commit()));
    assert_eq!(count(&stats, Ticker::CommitConflicts), 1);
    assert_eq!(count(&stats, Ticker::CommitConflictsOnRead), 1);
    assert_eq!(count(&stats, Ticker::CommitCount), 1);
}

/// A lock timeout is neither a commit nor a conflict.
#[test]
fn a_busy_lock_counts_as_neither_commit_nor_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = pessimistic(dir.path(), stats.clone());

    let holder = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    holder.put(b"k", b"held").unwrap();
    let waiter = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    assert!(matches!(
        waiter.put(b"k", b"blocked"),
        Err(TransactionError::Busy(_))
    ));
    drop(waiter);
    assert_eq!(count(&stats, Ticker::CommitCount), 0);
    assert_eq!(count(&stats, Ticker::CommitConflicts), 0);
    holder.commit().unwrap();
    assert_eq!(count(&stats, Ticker::CommitCount), 1);
}

/// What the commit of a transaction that did `mine` with `k` counts after
/// another writer did `theirs` to it: whether it conflicted, the conflicts,
/// and the conflicts counted as following a delete.
fn after_delete_count(
    level: IsolationLevel,
    mine: impl FnOnce(&regolith::Transaction),
    theirs: impl FnOnce(&regolith::Db),
) -> (bool, u64, u64) {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), Arc::clone(&stats));
    db.db().put(b"k", &7i64.to_be_bytes()).unwrap();
    let tx = db.begin(&TxnOptions::new().isolation(level));
    mine(&tx);
    theirs(db.db());
    stats.reset();
    let lost = conflicted(tx.commit());
    (
        lost,
        count(&stats, Ticker::CommitConflicts),
        count(&stats, Ticker::CommitConflictsAfterDelete),
    )
}

/// A commit refused because the key it read or wrote was deleted after its
/// snapshot counts once in `regolith.commit.conflicts_after_delete`, whatever
/// it did with the key and whichever delete removed it.
#[test]
fn a_conflict_with_a_delete_is_counted_after_delete() {
    let delete = |db: &regolith::Db| db.delete(b"k").unwrap();
    let range_delete = |db: &regolith::Db| db.delete_range(b"a", b"z").unwrap();
    let read = |tx: &regolith::Transaction| {
        tx.get(b"k").unwrap();
        tx.put(b"elsewhere", b"x").unwrap();
    };
    let read_for_update = |tx: &regolith::Transaction| {
        tx.get_for_update(b"k").unwrap();
    };
    let put = |tx: &regolith::Transaction| tx.put(b"k", b"mine").unwrap();
    let read_then_delete = |tx: &regolith::Transaction| {
        tx.get(b"k").unwrap();
        tx.delete(b"k").unwrap();
    };

    let level = IsolationLevel::RepeatableRead;
    assert_eq!(after_delete_count(level, read, delete), (true, 1, 1));
    assert_eq!(after_delete_count(level, read, range_delete), (true, 1, 1));
    assert_eq!(
        after_delete_count(level, read_for_update, delete),
        (true, 1, 1)
    );
    assert_eq!(after_delete_count(level, put, delete), (true, 1, 1));
    assert_eq!(after_delete_count(level, put, range_delete), (true, 1, 1));
    assert_eq!(
        after_delete_count(level, read_then_delete, delete),
        (true, 1, 1),
        "a delete after a delete conflicts when the deleter read the key first"
    );
    assert_eq!(
        after_delete_count(IsolationLevel::DefraLevel, read, delete),
        (true, 1, 1)
    );
}

/// A conflict with a put or a merge operand is a conflict, and is not counted
/// as following a delete. Neither is a commit that did not conflict at all.
#[test]
fn a_plain_conflict_is_not_counted_after_delete() {
    let overwrite = |db: &regolith::Db| db.put(b"k", &8i64.to_be_bytes()).unwrap();
    let operand = |db: &regolith::Db| db.merge(b"k", &1i64.to_be_bytes()).unwrap();
    let delete = |db: &regolith::Db| db.delete(b"k").unwrap();
    let read = |tx: &regolith::Transaction| {
        tx.get(b"k").unwrap();
        tx.put(b"elsewhere", b"x").unwrap();
    };
    let put = |tx: &regolith::Transaction| tx.put(b"k", b"mine").unwrap();
    let blind_delete = |tx: &regolith::Transaction| tx.delete(b"k").unwrap();
    let elsewhere = |tx: &regolith::Transaction| tx.put(b"elsewhere", b"x").unwrap();

    let level = IsolationLevel::RepeatableRead;
    assert_eq!(after_delete_count(level, read, overwrite), (true, 1, 0));
    assert_eq!(after_delete_count(level, read, operand), (true, 1, 0));
    assert_eq!(after_delete_count(level, put, overwrite), (true, 1, 0));
    assert_eq!(after_delete_count(level, put, operand), (true, 1, 0));
    // A put that came back after the delete decides the race, not the delete.
    let delete_then_put = |db: &regolith::Db| {
        db.delete(b"k").unwrap();
        db.put(b"k", &9i64.to_be_bytes()).unwrap();
    };
    assert_eq!(
        after_delete_count(level, read, delete_then_put),
        (true, 1, 0)
    );
    // A blind delete of a key already deleted is elided: no conflict at all.
    assert_eq!(
        after_delete_count(level, blind_delete, delete),
        (false, 0, 0)
    );
    // A key the transaction never touched was deleted: no conflict at all.
    assert_eq!(after_delete_count(level, elsewhere, delete), (false, 0, 0));
}

/// The count belongs to a refused commit. A write that early validation
/// refuses before any commit, for the same deleted key, counts nothing.
#[test]
fn only_a_commit_is_counted_after_delete() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), Arc::clone(&stats));
    db.db().put(b"k", b"old").unwrap();

    let tx = db.begin(
        &TxnOptions::new()
            .isolation(IsolationLevel::RepeatableRead)
            .early_validation(true),
    );
    db.db().delete(b"k").unwrap();
    stats.reset();
    assert!(conflicted(tx.put(b"k", b"mine")));
    assert_eq!(count(&stats, Ticker::CommitConflicts), 0);
    assert_eq!(count(&stats, Ticker::CommitConflictsAfterDelete), 0);
}

/// A commit whose outcome comes through a ticket, and a pessimistic commit,
/// are counted where a blocking optimistic commit is.
#[test]
fn a_ticket_and_a_pessimistic_commit_are_counted_after_delete() {
    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = open(dir.path(), Arc::clone(&stats));
    db.db().put(b"k", b"old").unwrap();
    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    tx.get(b"k").unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    db.db().delete(b"k").unwrap();
    stats.reset();
    let ticket = tx.commit_nowait();
    let mut queue = db.db().io_queue();
    assert!(conflicted(queue.block_on(ticket)));
    assert_eq!(count(&stats, Ticker::CommitConflicts), 1);
    assert_eq!(count(&stats, Ticker::CommitConflictsAfterDelete), 1);

    let dir = tempfile::tempdir().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = pessimistic(dir.path(), Arc::clone(&stats));
    db.db().put(b"k", b"old").unwrap();
    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    tx.get(b"k").unwrap();
    db.db().delete(b"k").unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    stats.reset();
    assert!(conflicted(tx.commit()));
    assert_eq!(count(&stats, Ticker::CommitConflicts), 1);
    assert_eq!(count(&stats, Ticker::CommitConflictsAfterDelete), 1);
}

#[test]
fn the_after_delete_ticker_is_exported_by_name() {
    assert!(Ticker::ALL.contains(&Ticker::CommitConflictsAfterDelete));
    assert_eq!(
        Ticker::CommitConflictsAfterDelete.name(),
        "regolith.commit.conflicts_after_delete"
    );
}
