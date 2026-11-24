//! A commit that writes nothing (plan 3.8, E50): at every level and in both
//! flavours it takes no slot of the commit pipeline, checks what its level
//! checks at the published horizon and at the decided frontier
//! (`write_free.rs`), commits on a database latched read-only, and reports
//! its snapshot as its receipt.
//!
//! Two tests stop the committer between taking its points and loading its
//! view with the `WriteFree` hook: a commit made there comes after the check
//! and does not refuse it, and a version at or below the points that
//! compaction folds there under a newer one still fails the read. The
//! windows of the pipeline (a commit decided and not applied, applied and not
//! published, discarded by a rollback) are in `write_free_window_tests.rs`.

use std::sync::Arc;

use tempfile::TempDir;

use super::hook::{self, Stage};
use crate::{
    Access, IsolationLevel, OptimisticTransactionDb, Options, ReadMode, ScanCheck, ScanDirection,
    Transaction, TransactionDb, TransactionError, TxnOptions,
};

const LEVELS: [IsolationLevel; 5] = [
    IsolationLevel::ReadCommitted,
    IsolationLevel::SnapshotIsolation,
    IsolationLevel::RepeatableRead,
    IsolationLevel::Serializable,
    IsolationLevel::DefraLevel,
];

/// Either flavour behind one type.
enum AnyDb {
    Optimistic(OptimisticTransactionDb),
    Pessimistic(TransactionDb),
}

impl AnyDb {
    fn open(optimistic: bool, dir: &TempDir) -> Self {
        if optimistic {
            Self::Optimistic(OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap())
        } else {
            Self::Pessimistic(TransactionDb::open(dir.path(), Options::default()).unwrap())
        }
    }

    fn db(&self) -> &crate::Db {
        match self {
            Self::Optimistic(db) => db.db(),
            Self::Pessimistic(db) => db.db(),
        }
    }

    fn begin(&self, level: IsolationLevel) -> Transaction {
        let options = TxnOptions::new().isolation(level);
        match self {
            Self::Optimistic(db) => db.begin(&options),
            Self::Pessimistic(db) => db.begin(&options),
        }
    }

    fn is_optimistic(&self) -> bool {
        matches!(self, Self::Optimistic(_))
    }
}

/// How a transaction reads the key it checks.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Read {
    Plain,
    ForUpdate,
}

impl Read {
    fn of(self, tx: &Transaction, key: &[u8]) -> Option<Vec<u8>> {
        match self {
            Self::Plain => tx.get(key).unwrap(),
            Self::ForUpdate => tx.get_for_update(key).unwrap(),
        }
    }

    /// Whether a commit that writes nothing checks a read made this way.
    fn checked(self, level: IsolationLevel, optimistic: bool) -> bool {
        match (self, level) {
            // DefraLevel's write-free relaxation, optimistic only.
            (Self::Plain, IsolationLevel::DefraLevel) => !optimistic,
            (Self::Plain, level) => {
                matches!(
                    level,
                    IsolationLevel::RepeatableRead | IsolationLevel::Serializable
                )
            }
            (Self::ForUpdate, level) => level != IsolationLevel::ReadCommitted,
        }
    }
}

/// Every flavour, level and read, each on a database of its own holding
/// `k = v0`.
fn each_case(case: impl Fn(&AnyDb, IsolationLevel, Read)) {
    for optimistic in [true, false] {
        for level in LEVELS {
            for read in [Read::Plain, Read::ForUpdate] {
                let dir = TempDir::new().unwrap();
                let db = AnyDb::open(optimistic, &dir);
                db.db().put(b"k", b"v0").unwrap();
                case(&db, level, read);
            }
        }
    }
}

/// The commit takes no slot of the pipeline, at any level, in either
/// flavour, whatever it checks and whether its check passes or fails.
#[test]
fn a_commit_that_writes_nothing_takes_no_slot_at_any_level() {
    each_case(|db, level, read| {
        for overtaken in [false, true] {
            let tx = db.begin(level);
            read.of(&tx, b"k");
            if overtaken {
                db.db().put(b"k", b"v1").unwrap();
            }
            let slots = db.db().engine().pipeline_slots_taken();
            let _ = tx.commit();
            assert_eq!(
                db.db().engine().pipeline_slots_taken(),
                slots,
                "{level:?} {read:?}, optimistic {}: a commit with nothing to write took a slot",
                db.is_optimistic()
            );
        }
    });
}

/// The check is the level's: a read overtaken by a commit fails exactly
/// where the level checks it, names the key and the read, and a read nobody
/// overtook never fails.
#[test]
fn a_commit_that_writes_nothing_checks_what_its_level_checks() {
    each_case(|db, level, read| {
        let case = format!("{level:?} {read:?}, optimistic {}", db.is_optimistic());
        let quiet = db.begin(level);
        read.of(&quiet, b"k");
        // A write elsewhere moves the horizon past the snapshot: the read is
        // looked up, and holds.
        db.db().put(b"elsewhere", b"x").unwrap();
        assert!(quiet.commit().is_ok(), "{case}: nothing overtook the read");

        let tx = db.begin(level);
        read.of(&tx, b"k");
        db.db().put(b"k", b"v1").unwrap();
        match tx.commit() {
            Err(TransactionError::Conflict(lost)) => {
                assert!(read.checked(level, db.is_optimistic()), "{case}: refused");
                assert_eq!(lost.key(), b"k", "{case}");
                let mine = match read {
                    Read::Plain => Access::Read,
                    Read::ForUpdate => Access::ReadForUpdate,
                };
                assert_eq!(lost.mine(), mine, "{case}");
                assert!(lost.observed_seq() < lost.latest_seq(), "{case}");
            }
            Ok(_) => assert!(
                !read.checked(level, db.is_optimistic()),
                "{case}: not refused"
            ),
            Err(other) => panic!("{case}: {other}"),
        }
    });
}

/// A validated scan ([`ScanCheck::Range`]) in a transaction that writes
/// nothing is checked at every level and flavour but under DefraLevel's
/// write-free relaxation: a key that appears in its range fails it.
#[test]
fn a_validated_scan_that_writes_nothing_is_checked() {
    for optimistic in [true, false] {
        for level in LEVELS {
            let dir = TempDir::new().unwrap();
            let db = AnyDb::open(optimistic, &dir);
            db.db().put(b"seat/1", b"taken").unwrap();
            let tx = db.begin(level);
            let mut cursor = tx.cursor(
                Some(&b"seat/"[..]),
                Some(&b"seat0"[..]),
                ScanDirection::Forward,
                ScanCheck::Range,
            );
            while !cursor.next_page(&tx, 64 * 1024).unwrap().done {}
            db.db().put(b"seat/2", b"taken").unwrap();
            let relaxed = optimistic && level == IsolationLevel::DefraLevel;
            match tx.commit() {
                Err(TransactionError::Conflict(lost)) if !relaxed => {
                    assert_eq!(lost.mine(), Access::ScannedRange, "{level:?}");
                    assert_eq!(lost.key(), b"seat/2", "{level:?}");
                }
                Ok(_) if relaxed => {}
                other => panic!("{level:?}, optimistic {optimistic}: {other:?}"),
            }
        }
    }
}

/// A database latched read-only refuses every write and commits a
/// transaction that writes nothing, its checks run as on any database.
#[test]
fn a_latched_database_commits_a_transaction_that_writes_nothing() {
    each_case(|db, level, read| {
        let case = format!("{level:?} {read:?}, optimistic {}", db.is_optimistic());
        let tx = db.begin(level);
        read.of(&tx, b"k");
        let writer = db.begin(level);
        writer.put(b"other", b"x").unwrap();
        db.db()
            .engine()
            .latch_wal_failure(&std::io::Error::other("a test's failure"));
        assert!(
            matches!(writer.commit(), Err(TransactionError::Engine(_))),
            "{case}: a latched database refuses a commit that writes"
        );
        let receipt = tx
            .commit()
            .unwrap_or_else(|e| panic!("{case}: refused on a latched database: {e}"));
        assert_eq!(receipt.seq(), 1, "{case}: the receipt names the snapshot");
    });
}

/// The receipt names the snapshot the transaction read at, not the horizon
/// its check ran at, at every level.
#[test]
fn a_commit_that_writes_nothing_reports_its_snapshot() {
    each_case(|db, level, read| {
        let tx = db.begin(level);
        let snapshot = db.db().latest_sequence();
        read.of(&tx, b"k");
        db.db().put(b"elsewhere", b"x").unwrap();
        db.db().put(b"elsewhere", b"y").unwrap();
        let receipt = tx.commit().unwrap();
        assert_eq!(receipt.seq(), snapshot, "{level:?} {read:?}");
        assert!(
            receipt.seq() < db.db().latest_sequence(),
            "{level:?} {read:?}"
        );
    });
}

/// Run `commit` with `between` run on the committing thread once the check
/// took its points and before it loads its view.
fn between_horizon_and_view<T>(between: impl FnOnce() + 'static, commit: impl FnOnce() -> T) -> T {
    let mut between = Some(between);
    hook::set(move |stage| {
        if stage == Stage::WriteFree
            && let Some(between) = between.take()
        {
            between();
        }
    });
    let out = commit();
    hook::clear();
    out
}

/// A memtable sealed and a commit of the key published after the check took
/// its points: that commit comes after the write-free one, which is placed
/// at its points, and does not refuse it, although the view the check loads
/// holds the new memtable and the write in it. Loading that view after the
/// points is what makes every commit at or below them reachable through it
/// (the loom model `horizon_then_view`); a commit after them is left out.
#[test]
fn a_commit_made_after_the_points_does_not_refuse_the_check() {
    for level in [IsolationLevel::RepeatableRead, IsolationLevel::Serializable] {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap());
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new().isolation(level));
        tx.get(b"k").unwrap();
        // The read is below the horizon, so the check looks it up.
        db.db().put(b"elsewhere", b"x").unwrap();
        let raw = Arc::clone(&db);
        let outcome = between_horizon_and_view(
            move || {
                raw.db().engine().seal_active().unwrap();
                raw.db().put(b"k", b"v1").unwrap();
            },
            || tx.commit(),
        );
        assert!(
            outcome.is_ok(),
            "{level:?}: a commit after the check's points refused it: {outcome:?}"
        );
    }
}

/// Run `commit` with `before` run on the committing thread just before the
/// check takes its points.
fn before_the_points<T>(before: impl FnOnce() + 'static, commit: impl FnOnce() -> T) -> T {
    let mut before = Some(before);
    hook::set(move |stage| {
        if stage == Stage::WriteFreePoints
            && let Some(before) = before.take()
        {
            before();
        }
    });
    let out = commit();
    hook::clear();
    out
}

/// A memtable sealed and a commit of the key published just before the check
/// takes its points: the points cover that commit, and the view the check
/// loads after them holds the new memtable and the write in it, so the read
/// is refused. Loading the view before the points would check a view without
/// that memtable under points that cover its commit, and miss the write (the
/// loom model `horizon_then_view` explores the same order).
#[test]
fn a_commit_published_just_before_the_points_is_not_missed() {
    for level in [IsolationLevel::RepeatableRead, IsolationLevel::Serializable] {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap());
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new().isolation(level));
        tx.get(b"k").unwrap();
        let raw = Arc::clone(&db);
        let outcome = before_the_points(
            move || {
                raw.db().engine().seal_active().unwrap();
                raw.db().put(b"k", b"v1").unwrap();
            },
            || tx.commit(),
        );
        let Err(TransactionError::Conflict(lost)) = outcome else {
            panic!("{level:?}: the write of k was missed: {outcome:?}");
        };
        assert_eq!(lost.key(), b"k", "{level:?}");
    }
}

/// A version of the key at or below the points, folded by compaction under a
/// newer one above them before the check loads its view: a table then holds
/// a sequence above the points, so the newer version fails the read, since
/// the one it may stand for lies below. A lookup bounded at the points would
/// find only the version the snapshot holds and miss the change. Rewriting
/// the bytes the transaction read shows the same for a read checked by
/// value: the value at the points was not the one read.
#[test]
fn a_version_folded_under_a_newer_one_still_fails_the_check() {
    for (level, read) in [
        (IsolationLevel::RepeatableRead, Read::Plain),
        (IsolationLevel::Serializable, Read::Plain),
        (IsolationLevel::SnapshotIsolation, Read::ForUpdate),
        (IsolationLevel::DefraLevel, Read::ForUpdate),
    ] {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap());
        db.db().put(b"k", b"v0").unwrap();
        let tx = db.begin(&TxnOptions::new().isolation(level));
        assert_eq!(read.of(&tx, b"k").as_deref(), Some(&b"v0"[..]));
        // Published before the commit: at or below its horizon.
        db.db().put(b"k", b"v1").unwrap();
        let below = db.db().latest_sequence();
        let raw = Arc::clone(&db);
        let outcome = between_horizon_and_view(
            move || {
                raw.db().put(b"k", b"v0").unwrap();
                raw.db().flush().unwrap();
                raw.db().compact_range(None, None).wait().unwrap();
            },
            || tx.commit(),
        );
        let Err(TransactionError::Conflict(lost)) = outcome else {
            panic!("{level:?} {read:?}: the write at {below} was missed: {outcome:?}");
        };
        assert_eq!(lost.key(), b"k", "{level:?} {read:?}");
        assert!(lost.latest_seq() > below, "{level:?} {read:?}");
    }
}

/// A key in a table no block of which is in the cache.
const COLD: &[u8] = b"k01000";

/// A database reopened over one table of small blocks, so its cache holds
/// none of them.
fn cold(dir: &TempDir) -> OptimisticTransactionDb {
    let options = || Options::default().block_size(256);
    {
        let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        for i in 0..2_000u32 {
            db.db().put(format!("k{i:05}").as_bytes(), b"x").unwrap();
        }
        db.db().flush().unwrap();
        db.db().close().unwrap();
    }
    OptimisticTransactionDb::open(dir.path(), options()).unwrap()
}

/// A `commit_nowait` through a `CacheOnly` handle never reads the device in
/// its check, written or not: a miss leaves the commit on its queue, which
/// reads the block, and the check run again there commits or conflicts as
/// it says. The earlier contract refused the commit here with the wait in
/// its ticket, and the transaction it consumed was lost (`NowaitMiss.tla`).
#[test]
fn a_cache_only_commit_that_writes_nothing_never_reads_the_device() {
    for level in [IsolationLevel::DefraLevel, IsolationLevel::RepeatableRead] {
        let dir = TempDir::new().unwrap();
        let db = cold(&dir);
        let mut queue = db.db().io_queue();
        let options = TxnOptions::new()
            .isolation(level)
            .read_mode(ReadMode::CacheOnly(queue.id()));
        // Run until the reads are cached; each miss is read by the queue.
        let mut attempt = || {
            let tx = db.begin(&options);
            match tx.get_for_update(COLD) {
                Ok(_) => Some(tx),
                Err(TransactionError::WouldBlock(_)) => {
                    queue.poll(crate::IoBudget::ALL);
                    None
                }
                Err(e) => panic!("{level:?}: {e}"),
            }
        };
        let tx = (0..8)
            .find_map(|_| attempt())
            .expect("the read never cached");
        // A newer version of the key in a table the cache never saw: the
        // check has to read it.
        db.db().put(COLD, b"y").unwrap();
        db.db().flush().unwrap();
        let ticket = tx.commit_nowait();
        assert!(
            !ticket.is_ready(),
            "{level:?}: the check read the device, or its miss ended the commit"
        );
        assert!(
            matches!(queue.block_on(ticket), Err(TransactionError::Conflict(_))),
            "{level:?}: the check run again missed the newer version"
        );
    }
}
