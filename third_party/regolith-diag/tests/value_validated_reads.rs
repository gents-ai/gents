//! Value-validated reads and write-free transactions at DefraLevel (plan 3.5
//! and 3.8).
//!
//! A read is current at commit while the key holds the bytes it returned, so a
//! rewrite of the same bytes is not a change. A transaction that writes
//! nothing validates no plain read, takes no part in the commit pipeline, and
//! never conflicts, except on a read it asked to have checked.

// Native-only: these use the filesystem and threads.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use common::parted::{Parted, add, value};
use regolith::{
    Access, CommitReceipt, Db, IsolationLevel, MergeOperator, OptimisticTransactionDb, Options,
    Transaction, TransactionDb, TransactionError, TxResult, TxnOptions, WriteKind,
};

const KEY: &[u8] = b"version";

fn options() -> Options {
    Options::default().merge_operator(Some(Arc::new(Parted)))
}

fn at(level: IsolationLevel) -> TxnOptions {
    TxnOptions::new().isolation(level)
}

/// How the transaction reads the key.
#[derive(Clone, Copy, Debug)]
enum Read {
    Get,
    GetSlice,
    GetForUpdate,
}

impl Read {
    const ALL: [Self; 3] = [Self::Get, Self::GetSlice, Self::GetForUpdate];

    fn apply(self, tx: &Transaction) -> Option<Vec<u8>> {
        match self {
            Self::Get => tx.get(KEY).unwrap(),
            Self::GetSlice => tx.get_slice(KEY).unwrap().map(|v| v.to_vec()),
            Self::GetForUpdate => tx.get_for_update(KEY).unwrap(),
        }
    }

    fn access(self) -> Access {
        match self {
            Self::Get | Self::GetSlice => Access::Read,
            Self::GetForUpdate => Access::ReadForUpdate,
        }
    }
}

fn flushed(db: &Db, flush: bool) {
    if flush {
        db.flush().unwrap();
    }
}

fn conflict(result: TxResult<CommitReceipt>) -> Option<(Access, WriteKind)> {
    match result {
        Err(TransactionError::Conflict(c)) => Some((c.mine(), c.theirs())),
        Ok(_) => None,
        Err(other) => panic!("expected a commit or a conflict, got {other:?}"),
    }
}

/// Seed `KEY`, begin at `level`, read it, let `concurrent` write, then write
/// elsewhere (and `mine`) and commit.
fn run(
    level: IsolationLevel,
    read: Read,
    seed: Option<&[u8]>,
    flush: bool,
    concurrent: impl FnOnce(&Db),
    mine: impl FnOnce(&Transaction),
) -> Option<(Access, WriteKind)> {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    if let Some(seed) = seed {
        db.db().put(KEY, seed).unwrap();
    }
    let tx = db.begin(&at(level));
    assert_eq!(read.apply(&tx).as_deref(), seed);
    concurrent(db.db());
    flushed(db.db(), flush);
    tx.put(b"elsewhere", b"x").unwrap();
    mine(&tx);
    conflict(tx.commit())
}

fn leave(_: &Transaction) {}

const V1: &[u8] = &[1; 24];
const V2: &[u8] = &[2; 24];

#[test]
fn a_rewrite_of_the_same_bytes_is_not_a_change() {
    for read in Read::ALL {
        for flush in [false, true] {
            let rewrite = |db: &Db| db.put(KEY, V1).unwrap();
            let outcome = run(
                IsolationLevel::DefraLevel,
                read,
                Some(V1),
                flush,
                rewrite,
                leave,
            );
            assert_eq!(outcome, None, "{read:?} flush={flush}");
        }
    }
}

#[test]
fn a_read_modify_write_survives_a_rewrite_it_did_not_notice() {
    // Serial order: the rewrite, then this transaction, which reads the same
    // bytes and writes what it derived from them.
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(KEY, V1).unwrap();
    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    assert_eq!(tx.get(KEY).unwrap().as_deref(), Some(V1));
    db.db().put(KEY, V1).unwrap();
    tx.put(KEY, V2).unwrap();
    tx.commit().unwrap();
    assert_eq!(db.db().get(KEY).unwrap().as_deref(), Some(V2));
}

#[test]
fn a_changed_value_is_a_change_and_the_reason_names_the_read() {
    for read in Read::ALL {
        for flush in [false, true] {
            let change = |db: &Db| db.put(KEY, V2).unwrap();
            let outcome = run(
                IsolationLevel::DefraLevel,
                read,
                Some(V1),
                flush,
                change,
                leave,
            );
            assert_eq!(
                outcome,
                Some((read.access(), WriteKind::Put)),
                "{read:?} flush={flush}"
            );

            let delete = |db: &Db| db.delete(KEY).unwrap();
            let outcome = run(
                IsolationLevel::DefraLevel,
                read,
                Some(V1),
                flush,
                delete,
                leave,
            );
            assert_eq!(
                outcome,
                Some((read.access(), WriteKind::Delete)),
                "{read:?}"
            );
        }
    }
}

#[test]
fn a_value_that_changed_and_came_back_is_the_value_read() {
    for read in Read::ALL {
        for flush in [false, true] {
            let there_and_back = |db: &Db| {
                db.put(KEY, V2).unwrap();
                db.put(KEY, V1).unwrap();
            };
            let outcome = run(
                IsolationLevel::DefraLevel,
                read,
                Some(V1),
                flush,
                there_and_back,
                leave,
            );
            assert_eq!(outcome, None, "{read:?} flush={flush}");
        }
    }
}

#[test]
fn an_operand_on_top_is_a_change_however_little_it_adds() {
    for read in Read::ALL {
        for flush in [false, true] {
            // An operand that adds nothing leaves the value as it was read,
            // but the commit does not fold it to find out.
            let nothing = |db: &Db| db.merge(KEY, &add(0, 0)).unwrap();
            let outcome = run(
                IsolationLevel::DefraLevel,
                read,
                Some(&value([0; 3])),
                flush,
                nothing,
                leave,
            );
            assert_eq!(
                outcome,
                Some((read.access(), WriteKind::Merge)),
                "{read:?} flush={flush}"
            );

            let identical_then_operand = |db: &Db| {
                db.put(KEY, &value([0; 3])).unwrap();
                db.merge(KEY, &add(1, 1)).unwrap();
            };
            let outcome = run(
                IsolationLevel::DefraLevel,
                read,
                Some(&value([0; 3])),
                flush,
                identical_then_operand,
                leave,
            );
            assert!(outcome.is_some(), "{read:?} flush={flush}");
        }
    }
}

#[test]
fn a_read_that_found_nothing_is_current_while_nothing_is_there() {
    for read in Read::ALL {
        for flush in [false, true] {
            let put = |db: &Db| db.put(KEY, V1).unwrap();
            let outcome = run(IsolationLevel::DefraLevel, read, None, flush, put, leave);
            assert_eq!(
                outcome,
                Some((read.access(), WriteKind::Put)),
                "{read:?} flush={flush}"
            );

            let delete = |db: &Db| db.delete(KEY).unwrap();
            assert_eq!(
                run(IsolationLevel::DefraLevel, read, None, flush, delete, leave),
                None,
                "{read:?}: a delete of nothing leaves nothing"
            );

            let created_and_removed = |db: &Db| {
                db.put(KEY, V1).unwrap();
                db.delete(KEY).unwrap();
            };
            assert_eq!(
                run(
                    IsolationLevel::DefraLevel,
                    read,
                    None,
                    flush,
                    created_and_removed,
                    leave
                ),
                None,
                "{read:?} flush={flush}: created and removed again"
            );
        }
    }
}

#[test]
fn a_read_of_a_merged_value_is_current_while_the_folded_bytes_are_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(KEY, &value([1, 1, 1])).unwrap();
    db.db().merge(KEY, &add(0, 4)).unwrap();
    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    assert_eq!(tx.get(KEY).unwrap().unwrap(), value([5, 1, 1]));
    db.db().put(KEY, &value([5, 1, 1])).unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    assert_eq!(conflict(tx.commit()), None, "a rewrite of the folded value");

    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    assert_eq!(tx.get(KEY).unwrap().unwrap(), value([5, 1, 1]));
    db.db().put(KEY, &value([6, 1, 1])).unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    assert_eq!(conflict(tx.commit()), Some((Access::Read, WriteKind::Put)));
}

#[test]
fn a_key_scanned_then_merged_is_validated_by_value_too() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(KEY, &value([1, 1, 1])).unwrap();
    let outcome = |rewrite: Vec<u8>| {
        let tx = db.begin(&at(IsolationLevel::DefraLevel));
        assert_eq!(tx.scan_stream(None, None).filter_map(Result::ok).count(), 1);
        tx.merge(KEY, &add(0, 1)).unwrap();
        db.db().put(KEY, &rewrite).unwrap();
        conflict(tx.commit())
    };
    assert_eq!(outcome(value([1, 1, 1])), None);
    assert_eq!(
        outcome(value([9, 9, 9])),
        Some((Access::ScannedThenWrote, WriteKind::Put))
    );
}

#[test]
fn other_levels_and_pessimistic_transactions_still_compare_sequences() {
    let rewrite = |db: &Db| db.put(KEY, V1).unwrap();
    for level in [IsolationLevel::RepeatableRead, IsolationLevel::Serializable] {
        let outcome = run(level, Read::Get, Some(V1), false, rewrite, leave);
        assert_eq!(outcome, Some((Access::Read, WriteKind::Put)), "{level:?}");
    }

    let dir = tempfile::tempdir().unwrap();
    let db = TransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(KEY, V1).unwrap();
    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    tx.get(KEY).unwrap();
    db.db().put(KEY, V1).unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    assert_eq!(
        conflict(tx.commit()),
        Some((Access::Read, WriteKind::Put)),
        "a pessimistic transaction validates as at RepeatableRead"
    );
}

// ---- write-free transactions ----

#[test]
fn a_write_free_transaction_never_conflicts_on_a_plain_read() {
    for flush in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        db.db().put(KEY, V1).unwrap();
        db.db().put(b"other", V1).unwrap();
        let tx = db.begin(&at(IsolationLevel::DefraLevel));
        let snapshot = db.db().latest_sequence();
        assert_eq!(tx.get(KEY).unwrap().as_deref(), Some(V1));
        assert_eq!(tx.get_slice(b"other").unwrap().as_deref(), Some(V1));
        assert_eq!(tx.scan_stream(None, None).count(), 2);
        db.db().put(KEY, V2).unwrap();
        db.db().delete(b"other").unwrap();
        flushed(db.db(), flush);
        // Both reads changed under it, and it still reads one point in time.
        assert_eq!(tx.get(KEY).unwrap().as_deref(), Some(V1));
        let receipt = tx
            .commit()
            .expect("a write-free transaction never conflicts");
        assert_eq!(
            receipt.seq(),
            snapshot,
            "it returns the receipt of its snapshot"
        );
    }
}

#[test]
fn a_write_free_transaction_still_checks_a_read_it_asked_to_have_checked() {
    for flush in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
        db.db().put(KEY, V1).unwrap();
        let tx = db.begin(&at(IsolationLevel::DefraLevel));
        tx.get_for_update(KEY).unwrap();
        db.db().put(KEY, V2).unwrap();
        flushed(db.db(), flush);
        assert_eq!(
            conflict(tx.commit()),
            Some((Access::ReadForUpdate, WriteKind::Put)),
            "flush={flush}"
        );

        // By value, like any DefraLevel read.
        let tx = db.begin(&at(IsolationLevel::DefraLevel));
        tx.get_for_update(KEY).unwrap();
        db.db().put(KEY, V2).unwrap();
        tx.commit().expect("an identical rewrite is not a change");
    }
}

#[test]
fn a_transaction_with_a_write_is_not_write_free() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(KEY, V1).unwrap();
    for write in [
        (|tx: &Transaction| tx.put(b"x", b"1").unwrap()) as fn(&Transaction),
        |tx| tx.delete(b"x").unwrap(),
        |tx| tx.merge(b"x", &add(0, 1)).unwrap(),
    ] {
        let tx = db.begin(&at(IsolationLevel::DefraLevel));
        tx.get(KEY).unwrap();
        write(&tx);
        db.db().put(KEY, V2).unwrap();
        db.db().put(KEY, V1).unwrap();
        db.db().put(KEY, V2).unwrap();
        assert!(conflict(tx.commit()).is_some());
        db.db().put(KEY, V1).unwrap();
    }
}

#[test]
fn a_write_free_transaction_is_optimistic_only() {
    // T reads a at its snapshot; W writes a and b; T reads b through
    // get_for_update at a later horizon and writes nothing. T would see two
    // points in time, so it must abort.
    let dir = tempfile::tempdir().unwrap();
    let db = TransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(b"a", b"0").unwrap();
    db.db().put(b"b", b"0").unwrap();
    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    assert_eq!(tx.get(b"a").unwrap().as_deref(), Some(&b"0"[..]));
    db.db().put(b"a", b"1").unwrap();
    db.db().put(b"b", b"1").unwrap();
    assert_eq!(tx.get_for_update(b"b").unwrap().as_deref(), Some(&b"1"[..]));
    match tx.commit() {
        Err(TransactionError::Conflict(c)) => assert_eq!(c.key(), b"a"),
        other => panic!("the pessimistic transaction read two points in time: {other:?}"),
    }
}

/// An operator that parks inside `full_merge` once armed, which the commit
/// reaches while it holds the pipeline.
struct Parks {
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl MergeOperator for Parks {
    fn name(&self) -> &'static str {
        "parks"
    }

    fn full_merge(&self, key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let entered = self.entered.lock().unwrap().take();
        if let Some(entered) = entered {
            entered.send(()).unwrap();
            let release = self.release.lock().unwrap().take().unwrap();
            release.recv().unwrap();
        }
        Parted.full_merge(key, base, operands)
    }
}

#[test]
fn a_write_free_commit_takes_no_part_in_the_commit_pipeline() {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let parks = Arc::new(Parks {
        entered: Mutex::new(None),
        release: Mutex::new(None),
    });
    struct Shared(Arc<Parks>);
    impl MergeOperator for Shared {
        fn name(&self) -> &'static str {
            "shared"
        }
        fn full_merge(&self, k: &[u8], b: Option<&[u8]>, o: &[&[u8]]) -> Option<Vec<u8>> {
            self.0.full_merge(k, b, o)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        OptimisticTransactionDb::open(
            dir.path(),
            Options::default().merge_operator(Some(Arc::new(Shared(parks.clone())))),
        )
        .unwrap(),
    );
    // A key that needs the operator to read, and a transaction that read it.
    db.db().put(KEY, &value([1, 1, 1])).unwrap();
    db.db().merge(KEY, &add(0, 1)).unwrap();
    let writer = db.begin(&at(IsolationLevel::DefraLevel));
    assert_eq!(writer.get(KEY).unwrap().unwrap(), value([2, 1, 1]));
    writer.put(b"elsewhere", b"x").unwrap();
    // A newer version sends its commit down the value path, which folds the
    // operands again under the pipeline, where the operator parks.
    db.db().put(KEY, &value([2, 1, 1])).unwrap();
    *parks.entered.lock().unwrap() = Some(entered_tx);
    *parks.release.lock().unwrap() = Some(release_rx);
    let committing = thread::spawn(move || writer.commit());
    entered
        .recv_timeout(Duration::from_secs(10))
        .expect("the commit reached the operator");

    // The pipeline is held. A write-free transaction commits anyway.
    let reader = db.begin(&at(IsolationLevel::DefraLevel));
    reader.get(KEY).unwrap();
    reader.get(b"elsewhere").unwrap();
    let (done_tx, done) = mpsc::channel();
    thread::spawn(move || done_tx.send(reader.commit().map(|r| r.seq())).unwrap());
    let seq = done
        .recv_timeout(Duration::from_secs(10))
        .expect("a write-free commit waited for the pipeline")
        .unwrap();
    assert!(seq > 0);

    release.send(()).unwrap();
    committing
        .join()
        .unwrap()
        .expect("the parked commit finishes");
}
