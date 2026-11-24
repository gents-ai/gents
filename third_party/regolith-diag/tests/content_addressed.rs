//! `KeyClass::ContentAddressed`: at `DefraLevel` with a classifier installed,
//! a key that determines its bytes is validated only where it matters.
//!
//! A block store keys each block by its content hash and reads every block it
//! is about to write, so two transactions that create documents sharing a
//! block would conflict on it. Declared content-addressed, a put or a merge of
//! the key is not validated, nor is any read of a key the transaction puts or
//! merges, and both commit. A read that found the block is validated for
//! presence only: it holds as long as the block is not gone. A delete of the
//! block, and a read that found nothing, are validated as for any other key.
//! Each case sits beside the conflict that stays: on an ordinary key, at
//! another level, or with no classifier installed.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::sync::Arc;

use regolith::Error;
use regolith::prelude::*;

mod common;

/// Sums big-endian i64 deltas.
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

/// Keys under `b/` are blocks named by their content; keys under `h/` are
/// heads added under fresh names and never contended.
struct Keys;

impl KeyClassifier for Keys {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"b/") {
            KeyClass::ContentAddressed
        } else if key.starts_with(b"h/") {
            KeyClass::CommutativePrefix { len: 2 }
        } else {
            KeyClass::Ordinary
        }
    }
}

fn options() -> Options {
    Options::default().merge_operator(Some(Arc::new(CounterMerge)))
}

fn open(dir: &Path) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir, options())
        .unwrap()
        .with_policy(Arc::new(Keys))
}

fn conflicted<T>(result: TxResult<T>) -> bool {
    matches!(result, Err(TransactionError::Conflict { .. }))
}

/// The key a commit lost on, `None` when it committed. Any other failure
/// fails the test with the error.
fn conflict_key<T>(result: TxResult<T>) -> Option<Vec<u8>> {
    match result {
        Ok(_) => None,
        Err(TransactionError::Conflict(conflict)) => Some(conflict.key().to_vec()),
        Err(other) => panic!("a commit failed for a reason other than a conflict: {other}"),
    }
}

/// A transaction at `DefraLevel`.
fn begin(db: &OptimisticTransactionDb) -> Transaction {
    db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel))
}

/// The value of the counter under `key`.
fn counter(db: &OptimisticTransactionDb, key: &[u8]) -> i64 {
    i64::from_be_bytes(db.db().get(key).unwrap().unwrap()[..].try_into().unwrap())
}

/// The point reads a transaction has.
#[derive(Clone, Copy, Debug)]
enum Read {
    Get,
    GetSlice,
    GetForUpdate,
}

impl Read {
    const ALL: [Self; 3] = [Self::Get, Self::GetSlice, Self::GetForUpdate];

    /// Whether the read finds a value under `key`.
    fn finds(self, tx: &Transaction, key: &[u8]) -> bool {
        match self {
            Self::Get => tx.get(key).unwrap().is_some(),
            Self::GetSlice => tx.get_slice(key).unwrap().is_some(),
            Self::GetForUpdate => tx.get_for_update(key).unwrap().is_some(),
        }
    }
}

/// A way to take `b/block` away from under a reader.
#[derive(Clone, Copy, Debug)]
enum Removal {
    Delete,
    DeleteRange,
    InTransaction,
}

impl Removal {
    const ALL: [Self; 3] = [Self::Delete, Self::DeleteRange, Self::InTransaction];

    fn apply(self, db: &OptimisticTransactionDb) {
        match self {
            Self::Delete => db.db().delete(b"b/block").unwrap(),
            Self::DeleteRange => db.db().delete_range(b"b/", b"b0").unwrap(),
            Self::InTransaction => {
                let collector = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
                collector.delete(b"b/block").unwrap();
                collector.commit().unwrap();
            }
        }
    }
}

/// Two transactions at `level` that each read `key` and put the same bytes
/// under it, as a block store does for a block it is about to write. Returns
/// what the second commit made of it, the first having committed.
fn both_create(db: &OptimisticTransactionDb, level: IsolationLevel, key: &[u8]) -> TxResult<()> {
    let first = db.begin(&TxnOptions::new().isolation(level));
    let second = db.begin(&TxnOptions::new().isolation(level));
    for tx in [&first, &second] {
        assert_eq!(tx.get_for_update(key).unwrap(), None);
        tx.put(key, b"bytes").unwrap();
    }
    first.commit().unwrap();
    second.commit().map(|_| ())
}

#[test]
fn two_transactions_that_create_the_same_block_both_commit() {
    for flush in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        // Each read the key before putting it, and a read that found nothing
        // is validated in full on an ordinary key, so only the exemption lets
        // the second commit through.
        for tx in [&first, &second] {
            assert_eq!(tx.get_for_update(b"b/shared").unwrap(), None);
            tx.put(b"b/shared", b"bytes").unwrap();
        }
        first.commit().unwrap();
        if flush {
            db.db().flush().unwrap();
        }
        second
            .commit()
            .unwrap_or_else(|e| panic!("flush={flush}: {e:?}"));
        assert_eq!(
            db.db().get(b"b/shared").unwrap().as_deref(),
            Some(&b"bytes"[..])
        );
    }
}

#[test]
fn an_ordinary_key_beside_it_keeps_conflicting() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    assert_eq!(
        conflict_key(both_create(&db, IsolationLevel::DefraLevel, b"ordinary")),
        Some(b"ordinary".to_vec())
    );
}

#[test]
fn without_a_classifier_the_same_block_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    assert!(conflicted(both_create(
        &db,
        IsolationLevel::DefraLevel,
        b"b/shared"
    )));
}

#[test]
fn other_levels_with_the_same_classifier_conflict_as_before() {
    for level in [
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        assert!(
            conflicted(both_create(&db, level, b"b/shared")),
            "{level:?}"
        );
    }
}

/// Whether a commit failed because a content-addressed key would have held
/// different bytes.
fn mismatched<T>(result: TxResult<T>) -> bool {
    matches!(
        result,
        Err(TransactionError::Engine(Error::ContentMismatch))
    )
}

#[test]
fn different_bytes_under_one_key_fail_loud_and_the_first_commit_stands() {
    // The caller's contract is that a content-addressed key never holds
    // different bytes. A commit that puts other bytes beside a newer commit
    // of the key breaks it, applies nothing, and says so.
    for flush in [false, true] {
        for one_commits_last in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let db = open(dir.path());
            let one = begin(&db);
            let two = begin(&db);
            for (tx, bytes) in [(&one, b"one"), (&two, b"two")] {
                tx.get_for_update(b"b/shared").unwrap();
                tx.put(b"b/shared", bytes).unwrap();
                tx.put(b"h/side", bytes).unwrap();
            }
            let (earlier, later) = if one_commits_last {
                (two, one)
            } else {
                (one, two)
            };
            earlier.commit().unwrap();
            if flush {
                db.db().flush().unwrap();
            }
            assert!(
                mismatched(later.commit()),
                "flush={flush}, one_commits_last={one_commits_last}"
            );
            let want: &[u8] = if one_commits_last { b"two" } else { b"one" };
            assert_eq!(db.db().get(b"b/shared").unwrap().as_deref(), Some(want));
            assert_eq!(
                db.db().get(b"h/side").unwrap().as_deref(),
                Some(want),
                "the refused commit applied nothing"
            );
        }
    }
}

#[test]
fn a_put_beside_a_plain_write_of_other_bytes_fails_loud() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let tx = begin(&db);
    tx.put(b"b/shared", b"mine").unwrap();
    db.db().put(b"b/shared", b"theirs").unwrap();
    assert!(mismatched(tx.commit()));
    assert_eq!(
        db.db().get(b"b/shared").unwrap().as_deref(),
        Some(&b"theirs"[..])
    );
}

#[test]
fn a_put_beside_a_plain_write_of_the_same_bytes_commits() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let tx = begin(&db);
    tx.put(b"b/shared", b"bytes").unwrap();
    db.db().put(b"b/shared", b"bytes").unwrap();
    tx.commit().unwrap();
}

#[test]
fn a_put_over_a_newer_delete_commits_whatever_the_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"b/shared", b"old").unwrap();
    let tx = begin(&db);
    tx.put(b"b/shared", b"new").unwrap();
    db.db().delete(b"b/shared").unwrap();
    tx.commit().unwrap();
    assert_eq!(
        db.db().get(b"b/shared").unwrap().as_deref(),
        Some(&b"new"[..])
    );
}

#[test]
fn an_ordinary_key_with_different_bytes_still_conflicts_instead() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let tx = begin(&db);
    tx.put(b"ordinary", b"mine").unwrap();
    db.db().put(b"ordinary", b"theirs").unwrap();
    assert!(conflicted(tx.commit()));
}

#[test]
fn a_put_of_a_block_commits_after_a_delete_of_it_but_not_the_other_way_round() {
    for delete_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        db.db().put(b"b/shared", b"bytes").unwrap();
        let put = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        let delete = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        put.get_for_update(b"b/shared").unwrap();
        put.put(b"b/shared", b"bytes").unwrap();
        delete.get_for_update(b"b/shared").unwrap();
        delete.delete(b"b/shared").unwrap();
        if delete_first {
            delete.commit().unwrap();
            // The put brings the block back, as it would for a block that
            // was never there.
            put.commit().unwrap();
        } else {
            put.commit().unwrap();
            // The collector saw the block before the put and would delete it
            // from under whatever the put's transaction now refers to.
            assert_eq!(
                conflict_key(delete.commit()),
                Some(b"b/shared".to_vec()),
                "the delete is stale"
            );
        }
        assert_eq!(
            db.db().get(b"b/shared").unwrap().as_deref(),
            Some(&b"bytes"[..])
        );
    }
}

#[test]
fn a_delete_of_a_block_is_validated_like_a_delete_of_any_other_key() {
    // Each schedule runs on a block and on an ordinary key, and ends the same.
    for key in [&b"b/block"[..], &b"ordinary"[..]] {
        let name = String::from_utf8_lossy(key);
        let fresh = || {
            let dir = tempfile::tempdir().unwrap();
            let db = open(dir.path());
            db.db().put(key, b"bytes").unwrap();
            (dir, db)
        };

        // A blind delete loses to a put that committed first.
        let (_dir, db) = fresh();
        let (delete, put) = (begin(&db), begin(&db));
        delete.delete(key).unwrap();
        put.put(key, b"bytes").unwrap();
        put.commit().unwrap();
        assert_eq!(conflict_key(delete.commit()), Some(key.to_vec()), "{name}");

        // A delete of an ordinary key the transaction read first holds while
        // the key holds the bytes the read returned: a put that left them as
        // they were is not a change, so it commits and the delete follows it.
        // A block's read is for its presence only and does not stand for the
        // delete, which is checked as the write it is: the put beat it.
        let (_dir, db) = fresh();
        let (delete, put) = (begin(&db), begin(&db));
        assert!(delete.get_for_update(key).unwrap().is_some());
        delete.delete(key).unwrap();
        put.put(key, b"bytes").unwrap();
        put.commit().unwrap();
        if key == b"ordinary" {
            delete.commit().unwrap();
            assert_eq!(db.db().get(key).unwrap(), None, "{name}");
        } else {
            assert_eq!(conflict_key(delete.commit()), Some(key.to_vec()), "{name}");
        }

        // A put that changed the bytes beats the delete on either.
        let (_dir, db) = fresh();
        let (delete, put) = (begin(&db), begin(&db));
        assert!(delete.get_for_update(key).unwrap().is_some());
        delete.delete(key).unwrap();
        put.put(key, b"changed").unwrap();
        put.commit().unwrap();
        assert_eq!(conflict_key(delete.commit()), Some(key.to_vec()), "{name}");

        // Two deleters that each read the key first: the second finds it gone.
        let (_dir, db) = fresh();
        let (first, second) = (begin(&db), begin(&db));
        for tx in [&first, &second] {
            assert!(tx.get_for_update(key).unwrap().is_some());
            tx.delete(key).unwrap();
        }
        first.commit().unwrap();
        assert_eq!(conflict_key(second.commit()), Some(key.to_vec()), "{name}");

        // Two blind deleters: the second stores what the first left.
        let (_dir, db) = fresh();
        let (first, second) = (begin(&db), begin(&db));
        first.delete(key).unwrap();
        second.delete(key).unwrap();
        first.commit().unwrap();
        second.commit().unwrap();
        assert_eq!(db.db().get(key).unwrap(), None, "{name}");
    }
}

#[test]
fn a_block_read_as_present_conflicts_once_it_is_gone() {
    for read in Read::ALL {
        for removal in Removal::ALL {
            for flush in [false, true] {
                for write_elsewhere in [false, true] {
                    let dir = tempfile::tempdir().unwrap();
                    let db = open(dir.path());
                    db.db().put(b"b/block", b"bytes").unwrap();
                    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
                    assert!(read.finds(&tx, b"b/block"));
                    removal.apply(&db);
                    if flush {
                        db.db().flush().unwrap();
                    }
                    if write_elsewhere {
                        tx.put(b"elsewhere", b"x").unwrap();
                    }
                    // A transaction that writes nothing validates the reads it
                    // asked to have checked, and no other.
                    let checked = write_elsewhere || matches!(read, Read::GetForUpdate);
                    assert_eq!(
                        conflict_key(tx.commit()),
                        checked.then(|| b"b/block".to_vec()),
                        "{read:?} {removal:?} flush={flush} write_elsewhere={write_elsewhere}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_block_read_as_absent_conflicts_once_it_is_put() {
    for read in Read::ALL {
        for by_transaction in [false, true] {
            for flush in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let db = open(dir.path());
                let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
                assert!(!read.finds(&tx, b"b/block"));
                if by_transaction {
                    let writer = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
                    writer.put(b"b/block", b"bytes").unwrap();
                    writer.commit().unwrap();
                } else {
                    db.db().put(b"b/block", b"bytes").unwrap();
                }
                if flush {
                    db.db().flush().unwrap();
                }
                tx.put(b"elsewhere", b"x").unwrap();
                assert_eq!(
                    conflict_key(tx.commit()),
                    Some(b"b/block".to_vec()),
                    "{read:?} by_transaction={by_transaction} flush={flush}"
                );
            }
        }
    }
}

/// A transaction at `level` reads three blocks that held a value, then a plain
/// write rewrites one with the same bytes, adds an operand to another, and
/// deletes the third and puts it back. Returns what its commit made of it.
fn reads_blocks_that_stay_present(level: IsolationLevel, read: Read, flush: bool) -> TxResult<()> {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"b/back", b"bytes").unwrap();
    db.db().put(b"b/block", b"bytes").unwrap();
    db.db().put(b"b/count", &0i64.to_be_bytes()).unwrap();
    let tx = db.begin(&TxnOptions::new().isolation(level));
    for key in [&b"b/back"[..], &b"b/block"[..], &b"b/count"[..]] {
        assert!(read.finds(&tx, key));
    }
    db.db().delete(b"b/back").unwrap();
    db.db().put(b"b/back", b"bytes").unwrap();
    db.db().put(b"b/block", b"bytes").unwrap();
    db.db().merge(b"b/count", &1i64.to_be_bytes()).unwrap();
    if flush {
        db.db().flush().unwrap();
    }
    tx.put(b"elsewhere", b"x").unwrap();
    tx.commit().map(|_| ())
}

#[test]
fn a_block_read_as_present_survives_whatever_leaves_it_present() {
    for read in Read::ALL {
        for flush in [false, true] {
            let outcome = reads_blocks_that_stay_present(IsolationLevel::DefraLevel, read, flush);
            assert!(outcome.is_ok(), "{read:?} flush={flush}: {outcome:?}");
            assert!(
                conflicted(reads_blocks_that_stay_present(
                    IsolationLevel::RepeatableRead,
                    read,
                    flush
                )),
                "{read:?} flush={flush}: below DefraLevel the same schedule conflicts"
            );
        }
    }
}

#[test]
fn a_transaction_mixing_both_kinds_conflicts_only_on_the_ordinary_one() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    for tx in [&first, &second] {
        tx.get_for_update(b"b/shared").unwrap();
        tx.put(b"b/shared", b"bytes").unwrap();
        tx.get_for_update(b"ordinary").unwrap();
        tx.put(b"ordinary", b"mine").unwrap();
    }
    first.commit().unwrap();
    // "b/shared" sorts before "ordinary", so a conflict on the block would
    // be the one reported.
    assert_eq!(conflict_key(second.commit()), Some(b"ordinary".to_vec()));

    let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    for (tx, own) in [(&first, &b"first"[..]), (&second, &b"second"[..])] {
        tx.get_for_update(b"b/other").unwrap();
        tx.put(b"b/other", b"bytes").unwrap();
        tx.put(own, b"mine").unwrap();
    }
    first.commit().unwrap();
    second
        .commit()
        .expect("sharing only a block is no conflict");
}

/// A transaction at `level` reads `b/block`, which a plain write rewrites
/// with the same bytes before it commits. Returns whether the commit
/// succeeded, with or without a write elsewhere (a read-only commit still
/// validates its reads).
fn reads_a_block_that_changes(
    level: IsolationLevel,
    for_update: bool,
    write_elsewhere: bool,
) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"b/block", b"bytes").unwrap();
    let tx = db.begin(&TxnOptions::new().isolation(level));
    if for_update {
        tx.get_for_update(b"b/block").unwrap();
    } else {
        tx.get(b"b/block").unwrap();
    }
    db.db().put(b"b/block", b"bytes").unwrap();
    if write_elsewhere {
        tx.put(b"elsewhere", b"x").unwrap();
    }
    common::committed(&tx.commit())
}

#[test]
fn a_read_of_a_block_survives_a_rewrite_of_it() {
    for for_update in [false, true] {
        for write_elsewhere in [false, true] {
            assert!(
                reads_a_block_that_changes(IsolationLevel::DefraLevel, for_update, write_elsewhere),
                "for_update={for_update} write_elsewhere={write_elsewhere}"
            );
        }
    }
    assert!(!reads_a_block_that_changes(
        IsolationLevel::RepeatableRead,
        false,
        true
    ));
    assert!(!reads_a_block_that_changes(
        IsolationLevel::RepeatableRead,
        true,
        false
    ));
    assert!(!reads_a_block_that_changes(
        IsolationLevel::SnapshotIsolation,
        true,
        true
    ));
}

/// A transaction at `level` scans `[prefix, end)`, then puts `key` inside the
/// stretch, or deletes it when `delete` is set, which a plain write rewrites
/// before it commits. Returns whether the commit succeeded.
fn scans_then_writes(
    level: IsolationLevel,
    prefix: &[u8],
    end: &[u8],
    key: &[u8],
    delete: bool,
) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    for name in [b"1", b"2", b"3"] {
        let mut scanned = prefix.to_vec();
        scanned.extend_from_slice(name);
        db.db().put(&scanned, b"bytes").unwrap();
    }
    let tx = db.begin(&TxnOptions::new().isolation(level));
    assert_eq!(tx.scan_stream(Some(prefix), Some(end)).count(), 3);
    if delete {
        tx.delete(key).unwrap();
    } else {
        tx.put(key, b"mine").unwrap();
    }
    // A block is put with the bytes it names, so the write that lands first
    // carries the same ones.
    let newer: &[u8] = if delete { b"changed" } else { b"mine" };
    db.db().put(key, newer).unwrap();
    common::committed(&tx.commit())
}

#[test]
fn a_scan_that_returns_a_block_records_no_read_for_a_put_of_it() {
    assert!(scans_then_writes(
        IsolationLevel::DefraLevel,
        b"b/",
        b"b0",
        b"b/2",
        false
    ));
    assert!(
        !scans_then_writes(IsolationLevel::RepeatableRead, b"b/", b"b0", b"b/2", false),
        "at RepeatableRead the scanned key the transaction writes is validated as a read"
    );
    assert!(
        !scans_then_writes(IsolationLevel::DefraLevel, b"o/", b"o0", b"o/2", false),
        "an ordinary key in a scanned stretch is still validated as a read"
    );
}

#[test]
fn a_scanned_block_the_transaction_deletes_is_validated_as_a_read() {
    assert!(
        !scans_then_writes(IsolationLevel::DefraLevel, b"b/", b"b0", b"b/2", true),
        "a delete of a block is not exempt, so the scan's read of it stands"
    );
}

#[test]
fn a_commutative_prefix_and_a_block_in_one_commit_each_keep_their_rule() {
    for (level, both_commit) in [
        (IsolationLevel::DefraLevel, true),
        (IsolationLevel::RepeatableRead, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        db.db().put(b"h/a", b"cid").unwrap();
        db.db().put(b"h/z", b"cid").unwrap();
        let first = db.begin(&TxnOptions::new().isolation(level));
        let second = db.begin(&TxnOptions::new().isolation(level));
        for tx in [&first, &second] {
            assert_eq!(tx.scan_stream(Some(b"h/"), Some(b"h0")).count(), 2);
            tx.put(b"h/m", b"superseded").unwrap();
            tx.get_for_update(b"b/shared").unwrap();
            tx.put(b"b/shared", b"bytes").unwrap();
        }
        first.commit().unwrap();
        assert_eq!(
            common::committed(&second.commit()),
            both_commit,
            "{level:?}"
        );
    }
}

#[test]
fn a_merge_into_a_block_is_not_validated() {
    for (level, commits) in [
        (IsolationLevel::DefraLevel, true),
        (IsolationLevel::RepeatableRead, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        db.db().put(b"b/count", &0i64.to_be_bytes()).unwrap();
        let tx = db.begin(&TxnOptions::new().isolation(level));
        tx.get(b"b/count").unwrap();
        tx.merge(b"b/count", &1i64.to_be_bytes()).unwrap();
        // A replacement, not another operand: nothing but the exemption lets a
        // merge commit past a newer put.
        db.db().put(b"b/count", &5i64.to_be_bytes()).unwrap();
        assert_eq!(common::committed(&tx.commit()), commits, "{level:?}");
        if commits {
            assert_eq!(counter(&db, b"b/count"), 6, "the operand lands on the put");
        }
    }
}

#[test]
fn a_commit_of_many_blocks_validates_none_of_them() {
    const BLOCKS: usize = 500;
    for flush in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        let block = |i: usize| format!("b/{i:04}").into_bytes();
        let first = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        let sides = [
            (&first, 0..BLOCKS, "first"),
            (&second, BLOCKS / 2..BLOCKS * 3 / 2, "second"),
        ];
        // Each reads its blocks before putting them, and a read that found
        // nothing is validated in full on an ordinary key, so only the
        // exemption lets the blocks both write through.
        for (tx, range, writer) in sides {
            for i in range {
                tx.get_for_update(&block(i)).unwrap();
                tx.put(&block(i), format!("block {i}").as_bytes()).unwrap();
            }
            tx.put(b"head", writer.as_bytes()).unwrap();
        }
        first.commit().unwrap();
        if flush {
            db.db().flush().unwrap();
        }
        // Half the blocks are in both commits and all of them sort before
        // `head`, which is ordinary and written with different bytes: it
        // is the one conflict the second commit reports.
        assert_eq!(conflict_key(second.commit()), Some(b"head".to_vec()));

        let second = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
        for i in BLOCKS / 2..BLOCKS * 3 / 2 {
            second
                .put(&block(i), format!("block {i}").as_bytes())
                .unwrap();
        }
        second.commit().unwrap();
        for i in 0..BLOCKS * 3 / 2 {
            assert_eq!(
                db.db().get(&block(i)).unwrap().as_deref(),
                Some(format!("block {i}").as_bytes()),
                "flush={flush} block {i}"
            );
        }
    }
}
