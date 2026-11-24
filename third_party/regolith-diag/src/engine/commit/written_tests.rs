//! A write is counted as written once it landed, and not before.
//!
//! The write tickers (`KeysWritten`, `KeysDeleted`, `MergesWritten`,
//! `RangeDeletesWritten`, `BytesWritten`) and the `BytesPerWrite` histogram
//! are counted in one place: where a commit's owner learns what the pipeline
//! decided for it (`RegolithEngine::settled`). A write the engine refuses,
//! one the log discards and a transaction that loses a conflict add nothing
//! there, and every call that writes is counted the same way: a put, a
//! batch, a column family's, a transaction's commit, blocking or not.

use std::sync::Arc;

use tempfile::TempDir;

use super::WriteRequest;
use super::pipeline_tests::install;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::engine::RegolithEngine;
use crate::engine::wal_rotation_tests::SyncFault;
use crate::{
    ColumnFamilyHandle, Db, DurabilityMode, Histogram, IoBudget, MergeOperator,
    OptimisticTransactionDb, Options, Statistics, Ticker, TransactionError, TxnOptions, WriteBatch,
    WriteOptions,
};

/// Concatenates: `merge` is refused at the call without an operator.
struct Append;

impl MergeOperator for Append {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        Some([base.unwrap_or_default(), &operands.concat()].concat())
    }

    fn name(&self) -> &'static str {
        "append"
    }
}

/// Everything the write statistics say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Counted {
    keys_written: u64,
    keys_deleted: u64,
    merges_written: u64,
    range_deletes_written: u64,
    bytes_written: u64,
    /// Samples of `BytesPerWrite`: one a write that landed.
    writes: u64,
    /// Their sum.
    bytes_in_writes: u64,
}

fn counted(stats: &Statistics) -> Counted {
    let per_write = stats.get_histogram_snapshot(Histogram::BytesPerWrite);
    Counted {
        keys_written: stats.get_ticker(Ticker::KeysWritten),
        keys_deleted: stats.get_ticker(Ticker::KeysDeleted),
        merges_written: stats.get_ticker(Ticker::MergesWritten),
        range_deletes_written: stats.get_ticker(Ticker::RangeDeletesWritten),
        bytes_written: stats.get_ticker(Ticker::BytesWritten),
        writes: per_write.count,
        bytes_in_writes: per_write.sum,
    }
}

/// A database with statistics, a merge operator, a second column family and
/// no worker.
fn database() -> (
    TempDir,
    OptimisticTransactionDb,
    ColumnFamilyHandle,
    Arc<Statistics>,
) {
    database_with(Options::default())
}

/// [`database`] over `options`.
fn database_with(
    options: Options,
) -> (
    TempDir,
    OptimisticTransactionDb,
    ColumnFamilyHandle,
    Arc<Statistics>,
) {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let options = options
        .max_background_compactions(0)
        .statistics(Some(Arc::clone(&stats)))
        .merge_operator(Some(Arc::new(Append)));
    let db = OptimisticTransactionDb::open(dir.path(), options).unwrap();
    let other = db.db().create_column_family("other").unwrap();
    (dir, db, other, stats)
}

/// A call that writes, under the name a failure gives it, and what it adds
/// to the statistics when it lands.
type Write<'a> = (
    &'static str,
    Box<dyn Fn() -> crate::Result<()> + 'a>,
    Counted,
);

/// What one write of `bytes` user bytes adds, before its operations.
fn one_write(bytes: u64) -> Counted {
    Counted {
        bytes_written: bytes,
        writes: 1,
        bytes_in_writes: bytes,
        ..Counted::default()
    }
}

/// Every plain call that writes: the default column family's and a named
/// one's. Keys are two bytes and values three, so a put is five user bytes
/// and a delete two, in either family: the family's prefix is not the
/// user's.
fn every_write<'a>(db: &'a Db, cf: &'a ColumnFamilyHandle) -> Vec<Write<'a>> {
    let put = Counted {
        keys_written: 1,
        ..one_write(5)
    };
    let delete = Counted {
        keys_deleted: 1,
        ..one_write(2)
    };
    let merge = Counted {
        merges_written: 1,
        ..one_write(0)
    };
    let delete_range = Counted {
        range_deletes_written: 1,
        ..one_write(0)
    };
    vec![
        ("put", Box::new(|| db.put(b"k1", b"abc")), put),
        ("delete", Box::new(|| db.delete(b"k1")), delete),
        ("merge", Box::new(|| db.merge(b"k2", b"abc")), merge),
        (
            "delete_range",
            Box::new(|| db.delete_range(b"k1", b"k9")),
            delete_range,
        ),
        (
            "write",
            Box::new(|| {
                let mut batch = WriteBatch::new();
                batch.put(b"k3", b"abc");
                batch.put(b"k4", b"abc");
                batch.delete(b"k3");
                batch.merge(b"k5", b"abc");
                batch.delete_range(b"k6", b"k7");
                db.write(batch)
            }),
            Counted {
                keys_written: 2,
                keys_deleted: 1,
                merges_written: 1,
                range_deletes_written: 1,
                ..one_write(12)
            },
        ),
        ("put_cf", Box::new(|| db.put_cf(cf, b"k1", b"abc")), put),
        ("delete_cf", Box::new(|| db.delete_cf(cf, b"k1")), delete),
        (
            "merge_cf",
            Box::new(|| db.merge_cf(cf, b"k2", b"abc")),
            merge,
        ),
        (
            "delete_range_cf",
            Box::new(|| db.delete_range_cf(cf, b"k1", b"k9")),
            delete_range,
        ),
    ]
}

fn plus(a: Counted, b: Counted) -> Counted {
    Counted {
        keys_written: a.keys_written + b.keys_written,
        keys_deleted: a.keys_deleted + b.keys_deleted,
        merges_written: a.merges_written + b.merges_written,
        range_deletes_written: a.range_deletes_written + b.range_deletes_written,
        bytes_written: a.bytes_written + b.bytes_written,
        writes: a.writes + b.writes,
        bytes_in_writes: a.bytes_in_writes + b.bytes_in_writes,
    }
}

/// Each call, landed, adds exactly its own operations and its user bytes,
/// once: the same in a named column family as in the default one.
#[test]
fn every_write_that_lands_is_counted_once() {
    let (_dir, db, other, stats) = database();
    let mut expected = counted(&stats);
    for (call, write, adds) in every_write(db.db(), &other) {
        write().unwrap_or_else(|err| panic!("{call} failed: {err}"));
        expected = plus(expected, adds);
        assert_eq!(counted(&stats), expected, "after {call}");
    }
}

/// A latched engine refuses every write below the call's own checks, so
/// each call is run against one: none of them is counted as written.
#[test]
fn a_write_a_latched_engine_refuses_is_not_counted() {
    let (_dir, db, other, stats) = database();
    db.db().put(b"before", b"v").unwrap();
    db.db()
        .engine()
        .latch_wal_failure(&std::io::Error::other("a test's failure"));
    let before = counted(&stats);
    for (call, write, _) in every_write(db.db(), &other) {
        write().expect_err("a latched engine takes no write");
        assert_eq!(
            counted(&stats),
            before,
            "{call} was refused and counted as written"
        );
    }
    let tx = db.begin(&TxnOptions::new());
    tx.put(b"k1", b"abc").unwrap();
    assert!(tx.commit().is_err(), "a latched engine takes no commit");
    assert_eq!(
        counted(&stats),
        before,
        "a commit was refused and counted as written"
    );
}

/// A write the pipeline took and the log then discarded (its sync failed)
/// is not counted either: it was decided, and it never landed.
#[test]
fn a_write_the_log_discards_is_not_counted() {
    let (dir, db, _other, stats) = database();
    db.db().put(b"before", b"v").unwrap();
    let before = counted(&stats);
    let _fault = SyncFault::arm(&dir);
    let synced = WriteOptions {
        sync: true,
        ..WriteOptions::default()
    };
    db.db()
        .put_opt(&synced, b"k1", b"abc")
        .expect_err("the log cannot be synced");
    let mut batch = WriteBatch::new();
    batch.put(b"k2", b"abc");
    batch.delete(b"k1");
    db.db()
        .write_opt(&synced, batch)
        .expect_err("the log cannot be synced");
    assert_eq!(db.db().get(b"k1").unwrap(), None, "the put was discarded");
    assert_eq!(counted(&stats), before);
}

/// A transaction's commit is a write like any other: counted once when it
/// lands, and not at all when it loses a conflict.
#[test]
fn a_commit_is_counted_when_it_lands_and_not_when_it_conflicts() {
    let (_dir, db, _other, stats) = database();
    let before = counted(&stats);

    let first = db.begin(&TxnOptions::new());
    let second = db.begin(&TxnOptions::new());
    first.put(b"k1", b"abc").unwrap();
    first.delete(b"k2").unwrap();
    // Read before the other commit writes it: what makes the second lose.
    assert_eq!(second.get(b"k1").unwrap(), None);
    second.put(b"k1", b"xyz").unwrap();
    second.put(b"k3", b"xyz").unwrap();
    first.commit().unwrap();
    let landed = plus(
        before,
        Counted {
            keys_written: 1,
            keys_deleted: 1,
            ..one_write(7)
        },
    );
    assert_eq!(counted(&stats), landed, "the commit that landed");

    match second.commit() {
        Err(TransactionError::Conflict(_)) => {}
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        counted(&stats),
        landed,
        "a commit that lost a conflict was counted as written"
    );
}

/// A conflict found when the slot is decided, past the check a commit makes
/// before the pipeline: the commit holds a slot and its operations by then,
/// and is still no write.
#[test]
fn a_commit_that_loses_when_its_slot_is_decided_is_not_counted() {
    let (_dir, db, _other, stats) = database();
    let tx = db.begin(&TxnOptions::new());
    assert_eq!(tx.get(b"k1").unwrap(), None);
    tx.put(b"k1", b"xyz").unwrap();
    tx.put(b"k3", b"xyz").unwrap();
    // A put of the key the transaction read, in the ring and not decided
    // yet: the check before the pipeline cannot see it, the slot's does.
    let put = WriteRequest::Put {
        key: prefix_key(DEFAULT_CF_ID, b"k1"),
        value: b"abc".to_vec(),
        durability: crate::engine::DurabilityMode::Eventual,
        disable_wal: false,
    };
    let (_, ahead) = install(db.db().engine(), put);
    let before = counted(&stats);
    match tx.commit() {
        Err(TransactionError::Conflict(_)) => {}
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        counted(&stats),
        before,
        "a commit that lost at its slot was counted as written"
    );
    // The put it lost to landed, and is counted when its owner asks.
    assert!(RegolithEngine::settled(Some(&stats), &ahead).is_ok());
    assert_eq!(
        counted(&stats),
        plus(
            before,
            Counted {
                keys_written: 1,
                ..one_write(5)
            }
        )
    );
}

/// The same for a commit that does not wait and lands in its call: counted
/// once.
#[test]
fn a_commit_that_does_not_wait_is_counted_once() {
    let (_dir, db, _other, stats) = database();
    let mut queue = db.db().io_queue();
    let before = counted(&stats);
    let tx = db.begin(&TxnOptions::new().io_queue(queue.id()));
    tx.put(b"k1", b"abc").unwrap();
    let ticket = tx.commit_nowait();
    queue.block_on(ticket).unwrap();
    queue.poll(IoBudget::ALL);
    assert_eq!(
        counted(&stats),
        plus(
            before,
            Counted {
                keys_written: 1,
                ..one_write(5)
            }
        )
    );
}

/// And for one that is still owed its sync when the call returns: not
/// counted while it is pending, counted once by the poll that delivers it.
#[test]
fn a_pending_commit_is_counted_when_its_poll_delivers_it_and_not_before() {
    let (_dir, db, _other, stats) =
        database_with(Options::default().durability(DurabilityMode::Immediate));
    let mut queue = db.db().io_queue();
    // What the first write owes the background runs in this call, and is
    // not a job the polls below would complete.
    db.db().put(b"filler", b"v").unwrap();
    let before = counted(&stats);
    let tx = db.begin(&TxnOptions::new().io_queue(queue.id()));
    tx.put(b"k1", b"abc").unwrap();
    let ticket = tx.commit_nowait();
    assert!(!ticket.is_ready(), "setup: the commit waits for its sync");
    assert_eq!(
        counted(&stats),
        before,
        "a commit that has not landed was counted as written"
    );
    queue.block_on(ticket).unwrap();
    queue.poll(IoBudget::ALL);
    assert_eq!(
        counted(&stats),
        plus(
            before,
            Counted {
                keys_written: 1,
                ..one_write(5)
            }
        )
    );
}

/// Any thread may publish another's commit, and each is still counted once:
/// by its owner.
#[test]
fn writers_that_help_each_other_are_each_counted_once() {
    const WRITERS: u64 = 4;
    const PUTS: u64 = 200;
    let (_dir, db, _other, stats) = database();
    let before = counted(&stats);
    std::thread::scope(|scope| {
        for writer in 0..WRITERS {
            let db = db.db();
            scope.spawn(move || {
                for n in 0..PUTS {
                    let key = [b'a' + writer as u8, (n % 251) as u8];
                    db.put(&key, b"abc").unwrap();
                }
            });
        }
    });
    let writes = WRITERS * PUTS;
    assert_eq!(
        counted(&stats),
        plus(
            before,
            Counted {
                keys_written: writes,
                bytes_written: 5 * writes,
                writes,
                bytes_in_writes: 5 * writes,
                ..Counted::default()
            }
        )
    );
}
