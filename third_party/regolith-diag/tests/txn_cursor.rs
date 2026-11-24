//! `Transaction::cursor` (plan 3.0 and 3.15): a scan that holds no borrow of its
//! transaction and is read a page at a time, with a check that decides what
//! the commit validates of it.
//!
//! The pages and the writes between them are checked against the keys the
//! cursor must hand out; the checks (`Stretch`, `Parts`, `Range`) against the
//! conflicts they must and must not cause; and an error in the middle of the
//! range against never reading as its end.

// Native-only: these use the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::{Arc, Mutex};

use common::parted::{Parted, add, value};
use regolith::{
    Access, CommitReceipt, Db, Error, IsolationLevel, MergeOperator, OptimisticTransactionDb,
    Options, ScanCheck, ScanDirection, Transaction, TransactionDb, TransactionError, TxResult,
    TxnCursor, TxnOptions, WriteKind,
};

fn open(dir: &std::path::Path) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(
        dir,
        Options::default().merge_operator(Some(Arc::new(Parted))),
    )
    .unwrap()
}

fn begin(db: &OptimisticTransactionDb, level: IsolationLevel) -> Transaction {
    db.begin(&TxnOptions::new().isolation(level))
}

fn keys(cursor: &mut TxnCursor, txn: &Transaction, max_bytes: usize) -> Vec<Vec<u8>> {
    let mut seen = Vec::new();
    loop {
        let page = cursor.next_page(txn, max_bytes).unwrap();
        seen.extend(page.entries.into_iter().map(|(key, _)| key));
        if page.done {
            return seen;
        }
    }
}

fn names(keys: &[Vec<u8>]) -> Vec<String> {
    keys.iter()
        .map(|key| String::from_utf8(key.clone()).unwrap())
        .collect()
}

fn seed(db: &Db, keys: &[&str]) {
    for key in keys {
        db.put(key.as_bytes(), key.as_bytes()).unwrap();
    }
}

const TEN: [&str; 10] = ["k0", "k1", "k2", "k3", "k4", "k5", "k6", "k7", "k8", "k9"];

// ---- pages ----

#[test]
fn pages_cover_the_range_in_order_whatever_their_size() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &TEN);
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);
    for max_bytes in [0, 1, 5, 7, 40, usize::MAX] {
        for (start, end, expected) in [
            (None, None, TEN.to_vec()),
            (
                Some(&b"k2"[..]),
                Some(&b"k6"[..]),
                vec!["k2", "k3", "k4", "k5"],
            ),
            (Some(&b"k8"[..]), None, vec!["k8", "k9"]),
            (None, Some(&b"k2"[..]), vec!["k0", "k1"]),
            (Some(&b"k4x"[..]), Some(&b"k5"[..]), vec![]),
        ] {
            let mut forward = txn.cursor(start, end, ScanDirection::Forward, ScanCheck::Stretch);
            assert_eq!(
                names(&keys(&mut forward, &txn, max_bytes)),
                expected,
                "{max_bytes}"
            );

            let mut reverse = txn.cursor(start, end, ScanDirection::Reverse, ScanCheck::Stretch);
            let mut backwards = expected.clone();
            backwards.reverse();
            assert_eq!(
                names(&keys(&mut reverse, &txn, max_bytes)),
                backwards,
                "{max_bytes}"
            );
        }
    }
}

#[test]
fn a_page_holds_at_least_one_entry_and_stops_once_it_has_max_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &TEN);
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);

    // Zero bytes reads one entry per page.
    let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
    let sizes: Vec<usize> = std::iter::from_fn(|| {
        let page = cursor.next_page(&txn, 0).unwrap();
        (!page.entries.is_empty()).then_some(page.entries.len())
    })
    .collect();
    assert_eq!(sizes, [1; 10]);

    // An entry is key plus value: 4 bytes. Ten bytes take three, and the
    // last page of the ten holds the remainder.
    let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
    let mut sizes = Vec::new();
    loop {
        let page = cursor.next_page(&txn, 10).unwrap();
        sizes.push(page.entries.len());
        if page.done {
            break;
        }
    }
    assert_eq!(sizes, [3, 3, 3, 1], "a page may overshoot by one entry");
}

#[test]
fn done_is_set_once_the_range_has_ended_and_stays() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["k0", "k1"]);
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);
    let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);

    let page = cursor.next_page(&txn, 4).unwrap();
    assert_eq!((page.entries.len(), page.done), (1, false));
    let page = cursor.next_page(&txn, usize::MAX).unwrap();
    assert_eq!(
        (page.entries.len(), page.done),
        (1, true),
        "the last entry and the end together"
    );
    for _ in 0..3 {
        let page = cursor.next_page(&txn, usize::MAX).unwrap();
        assert_eq!((page.entries.len(), page.done), (0, true));
    }

    let mut empty = txn.cursor(Some(b"z"), None, ScanDirection::Forward, ScanCheck::Stretch);
    let page = empty.next_page(&txn, 10).unwrap();
    assert_eq!((page.entries.len(), page.done), (0, true));
}

#[test]
fn the_transaction_may_be_moved_between_pages() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &TEN);
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);
    let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
    let first = cursor.next_page(&txn, 4).unwrap();
    let txn = Box::new(txn);
    let moved = std::thread::spawn(move || {
        let mut cursor = cursor;
        let rest = keys(&mut cursor, &txn, 8);
        (txn, rest)
    })
    .join()
    .unwrap();
    assert_eq!(first.entries.len() + moved.1.len(), 10);
}

#[test]
fn a_cursor_made_for_one_transaction_refuses_another() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["k0"]);
    let (one, other) = (
        begin(&db, IsolationLevel::SnapshotIsolation),
        begin(&db, IsolationLevel::SnapshotIsolation),
    );
    db.db().put(b"k1", b"v").unwrap();
    let third = begin(&db, IsolationLevel::SnapshotIsolation);
    let mut cursor = one.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
    // `one` and `other` share a snapshot, which the cursor cannot tell apart;
    // a transaction at another snapshot it can.
    assert!(matches!(
        cursor.next_page(&third, 10),
        Err(TransactionError::Engine(Error::InvalidArgument(_)))
    ));
    drop(other);
}

// ---- writes between pages ----

#[test]
fn a_write_ahead_of_the_cursor_is_seen_by_the_pages_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["a", "c", "e", "g", "i"]);
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);
    let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
    let first = cursor.next_page(&txn, 0).unwrap();
    assert_eq!(
        names(
            &first
                .entries
                .iter()
                .map(|(k, _)| k.clone())
                .collect::<Vec<_>>()
        ),
        ["a"]
    );

    txn.put(b"b", b"mine").unwrap(); // ahead, a new key
    txn.put(b"0", b"behind").unwrap(); // behind the cursor
    txn.put(b"a", b"behind").unwrap(); // the key it just returned
    txn.delete(b"c").unwrap(); // ahead, hiding a snapshot key
    txn.put(b"g", b"replaced").unwrap(); // ahead, replacing one
    txn.put(b"j", b"tail").unwrap();

    let rest = cursor.next_page(&txn, usize::MAX).unwrap();
    let rest: Vec<(String, String)> = rest
        .entries
        .into_iter()
        .map(|(k, v)| {
            (
                String::from_utf8(k).unwrap(),
                String::from_utf8(v.to_vec()).unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rest,
        [
            ("b".into(), "mine".into()),
            ("e".into(), "e".into()),
            ("g".into(), "replaced".into()),
            ("i".into(), "i".into()),
            ("j".into(), "tail".into()),
        ]
    );
}

#[test]
fn a_reverse_cursor_sees_writes_below_its_position() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["a", "c", "e"]);
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);
    let mut cursor = txn.cursor(None, None, ScanDirection::Reverse, ScanCheck::Stretch);
    assert_eq!(cursor.next_page(&txn, 0).unwrap().entries.len(), 1);
    txn.put(b"d", b"ahead").unwrap();
    txn.put(b"z", b"behind").unwrap();
    txn.delete(b"a").unwrap();
    assert_eq!(names(&keys(&mut cursor, &txn, usize::MAX)), ["d", "c"]);
}

#[test]
fn a_savepoint_rollback_between_pages_does_not_leave_a_stale_fold() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["a", "e"]);
    let mut txn = begin(&db, IsolationLevel::SnapshotIsolation);
    let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
    assert_eq!(cursor.next_page(&txn, 0).unwrap().entries.len(), 1);

    txn.set_savepoint();
    txn.put(b"b", b"first").unwrap();
    // Seen: the fold now holds `b`.
    let page = cursor.next_page(&txn, 0).unwrap();
    assert_eq!(page.entries[0].0, b"b");
    txn.rollback_to_savepoint().unwrap();
    // The buffer is as long as it was before `b`, and is then as long as it
    // was when the cursor folded it: a length alone would call that unchanged.
    txn.put(b"c", b"second").unwrap();
    assert_eq!(names(&keys(&mut cursor, &txn, usize::MAX)), ["c", "e"]);
}

/// A failed `before_commit` takes its writes back without a savepoint
/// rollback, and the buffer then regrows to the length the cursor folded.
#[test]
fn a_failed_before_commit_between_pages_does_not_leave_a_stale_fold() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["a", "e"]);
    let mut txn = begin(&db, IsolationLevel::SnapshotIsolation);
    let cursor = Arc::new(Mutex::new(txn.cursor(
        None,
        None,
        ScanDirection::Forward,
        ScanCheck::Stretch,
    )));
    assert_eq!(
        cursor
            .lock()
            .unwrap()
            .next_page(&txn, 0)
            .unwrap()
            .entries
            .len(),
        1
    );

    let held = Arc::clone(&cursor);
    let mut failed = false;
    txn.before_commit(move |txn| {
        if std::mem::replace(&mut failed, true) {
            return Ok(());
        }
        txn.put(b"b", b"from the callback")?;
        // Seen: the fold now holds `b`.
        let page = held.lock().unwrap().next_page(txn, 0).unwrap();
        assert_eq!(page.entries[0].0, b"b");
        Err(TransactionError::NoSavepoint)
    });
    assert!(txn.prepare().is_err());
    // `b` is gone and the buffer is as long as when the cursor folded it.
    txn.put(b"c", b"mine").unwrap();
    let mut cursor = cursor.lock().unwrap();
    assert_eq!(names(&keys(&mut cursor, &txn, usize::MAX)), ["c", "e"]);
}

// ---- what the check records ----

fn outcome(result: TxResult<CommitReceipt>) -> Option<(Vec<u8>, Access, WriteKind)> {
    match result {
        Err(TransactionError::Conflict(c)) => Some((c.key().to_vec(), c.mine(), c.theirs())),
        Ok(_) => None,
        Err(other) => panic!("expected a commit or a conflict, got {other:?}"),
    }
}

#[test]
fn a_stretch_is_extended_across_pages_and_covers_the_keys_between_them() {
    for level in [
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::DefraLevel,
    ] {
        // `b` is not there: the cursor walked past it between two pages.
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        seed(db.db(), &["a", "c", "e"]);
        let txn = begin(&db, level);
        let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
        for _ in 0..2 {
            assert_eq!(cursor.next_page(&txn, 0).unwrap().entries.len(), 1);
        }
        txn.put(b"b", b"mine").unwrap();
        db.db().put(b"b", b"theirs").unwrap();
        assert_eq!(
            outcome(txn.commit()),
            Some((b"b".to_vec(), Access::ScannedThenWrote, WriteKind::Put)),
            "{level:?}"
        );
    }
}

#[test]
fn a_commit_between_pages_sees_the_stretch_as_far_as_the_last_page_took_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["a", "c", "e", "g"]);
    let case = |key: &[u8]| {
        let txn = begin(&db, IsolationLevel::SnapshotIsolation);
        let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
        for _ in 0..2 {
            assert_eq!(cursor.next_page(&txn, 0).unwrap().entries.len(), 1); // a, c
        }
        // The same bytes: a blind write of them commits, so only a write
        // inside the stretch, validated as a read, can lose.
        txn.put(key, b"same").unwrap();
        db.db().put(key, b"same").unwrap();
        let result = outcome(txn.commit());
        drop(cursor); // the cursor outlives the commit
        result
    };
    assert!(case(b"a").is_some(), "inside what was read");
    assert!(case(b"c").is_some(), "the last key read");
    assert!(
        case(b"g").is_none(),
        "beyond it: a blind write of the same bytes"
    );
}

#[test]
fn a_cursor_that_was_never_paged_records_no_stretch() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["a"]);
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);
    let _cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
    txn.put(b"a", b"mine").unwrap();
    db.db().put(b"a", b"theirs").unwrap();
    assert!(
        outcome(txn.commit()).is_some(),
        "the blind put loses on its own"
    );
    let txn = begin(&db, IsolationLevel::DefraLevel);
    let _cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Range);
    txn.put(b"zz", b"mine").unwrap();
    db.db().put(b"a", b"theirs").unwrap();
    assert!(
        outcome(txn.commit()).is_none(),
        "nothing was consumed, nothing is validated"
    );
}

// ---- validated ranges ----

const RANGE: (&[u8], &[u8]) = (b"g/", b"g0");

/// Seed `g/1 g/3 g/5 g/7`, scan `RANGE` with `check` `Range` in `direction`
/// for `pages` pages of one entry (all of it when `None`), let `concurrent`
/// write, write elsewhere and commit.
fn ranged(
    level: IsolationLevel,
    direction: ScanDirection,
    pages: Option<usize>,
    flush: bool,
    concurrent: impl FnOnce(&Db),
) -> Option<(Vec<u8>, Access, WriteKind)> {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["g/1", "g/3", "g/5", "g/7", "h/1"]);
    let txn = begin(&db, level);
    let mut cursor = txn.cursor(Some(RANGE.0), Some(RANGE.1), direction, ScanCheck::Range);
    match pages {
        None => {
            assert_eq!(keys(&mut cursor, &txn, usize::MAX).len(), 4);
        }
        Some(pages) => {
            for _ in 0..pages {
                assert_eq!(cursor.next_page(&txn, 0).unwrap().entries.len(), 1);
            }
        }
    }
    concurrent(db.db());
    if flush {
        db.db().flush().unwrap();
    }
    txn.put(b"zz", b"x").unwrap();
    outcome(txn.commit())
}

#[test]
fn a_revocation_or_a_phantom_inside_the_range_refuses_the_scan() {
    for direction in [ScanDirection::Forward, ScanDirection::Reverse] {
        for flush in [false, true] {
            let all = |concurrent: &dyn Fn(&Db)| {
                ranged(
                    IsolationLevel::DefraLevel,
                    direction,
                    None,
                    flush,
                    concurrent,
                )
            };
            let revoked = all(&|db| db.delete(b"g/3").unwrap());
            assert_eq!(
                revoked,
                Some((b"g/3".to_vec(), Access::ScannedRange, WriteKind::Delete)),
                "{direction:?} flush={flush}"
            );
            let phantom = all(&|db| db.put(b"g/4", b"new").unwrap());
            assert_eq!(
                phantom,
                Some((b"g/4".to_vec(), Access::ScannedRange, WriteKind::Put)),
                "{direction:?} flush={flush}"
            );
            let operand = all(&|db| db.merge(b"g/9", &add(0, 1)).unwrap());
            assert_eq!(
                operand.map(|(_, access, theirs)| (access, theirs)),
                Some((Access::ScannedRange, WriteKind::Merge))
            );
            // Any write counts, a rewrite of the same bytes too.
            assert!(all(&|db| db.put(b"g/5", b"g/5").unwrap()).is_some());
            let range_delete = all(&|db| db.delete_range(b"g/2", b"g/4").unwrap());
            assert_eq!(
                range_delete.map(|(_, access, theirs)| (access, theirs)),
                Some((Access::ScannedRange, WriteKind::RangeDelete)),
                "{direction:?} flush={flush}"
            );
        }
    }
}

#[test]
fn a_write_outside_the_range_does_not_refuse_it() {
    for direction in [ScanDirection::Forward, ScanDirection::Reverse] {
        for flush in [false, true] {
            let outside = |concurrent: &dyn Fn(&Db)| {
                ranged(
                    IsolationLevel::DefraLevel,
                    direction,
                    None,
                    flush,
                    concurrent,
                )
            };
            assert!(outside(&|db| db.put(b"f/9", b"v").unwrap()).is_none());
            assert!(outside(&|db| db.put(b"h/1", b"v").unwrap()).is_none());
            // The end is exclusive, the start inclusive.
            assert!(
                outside(&|db| db.put(b"g0", b"v").unwrap()).is_none(),
                "the end"
            );
            assert!(
                outside(&|db| db.put(b"g/", b"v").unwrap()).is_some(),
                "the start"
            );
            assert!(outside(&|db| db.delete_range(b"a", b"g/").unwrap()).is_none());
            assert!(outside(&|db| db.delete_range(b"g0", b"z").unwrap()).is_none());
        }
    }
}

#[test]
fn a_scan_that_stops_early_validates_only_what_it_read() {
    // Forward: g/1 then g/3 were read.
    for flush in [false, true] {
        let early = |concurrent: &dyn Fn(&Db)| {
            ranged(
                IsolationLevel::DefraLevel,
                ScanDirection::Forward,
                Some(2),
                flush,
                concurrent,
            )
        };
        assert!(early(&|db| db.delete(b"g/1").unwrap()).is_some());
        assert!(
            early(&|db| db.put(b"g/2", b"v").unwrap()).is_some(),
            "a gap it passed"
        );
        assert!(
            early(&|db| db.delete(b"g/3").unwrap()).is_some(),
            "the last key it took"
        );
        assert!(
            early(&|db| db.put(b"g/4", b"v").unwrap()).is_none(),
            "ahead of it"
        );
        assert!(early(&|db| db.delete(b"g/7").unwrap()).is_none());

        // Reverse: g/7 then g/5 were read.
        let early = |concurrent: &dyn Fn(&Db)| {
            ranged(
                IsolationLevel::DefraLevel,
                ScanDirection::Reverse,
                Some(2),
                flush,
                concurrent,
            )
        };
        assert!(early(&|db| db.delete(b"g/7").unwrap()).is_some());
        assert!(early(&|db| db.put(b"g/6", b"v").unwrap()).is_some());
        assert!(early(&|db| db.delete(b"g/5").unwrap()).is_some());
        assert!(early(&|db| db.put(b"g/4", b"v").unwrap()).is_none());
        assert!(early(&|db| db.delete(b"g/1").unwrap()).is_none());
    }

    // Nothing read, nothing validated.
    let none = |concurrent: &dyn Fn(&Db)| {
        ranged(
            IsolationLevel::DefraLevel,
            ScanDirection::Forward,
            Some(0),
            false,
            concurrent,
        )
    };
    assert!(none(&|db| db.delete(b"g/1").unwrap()).is_none());
}

#[test]
fn a_range_works_at_every_level_and_a_write_free_defralevel_transaction_validates_nothing() {
    let revoke = |db: &Db| db.delete(b"g/3").unwrap();
    for level in [
        IsolationLevel::ReadCommitted,
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
        IsolationLevel::DefraLevel,
    ] {
        let outcome = ranged(level, ScanDirection::Forward, None, false, revoke);
        // At Serializable the scan also reads each key it yields, and that
        // read is checked first.
        let access = if level == IsolationLevel::Serializable {
            Access::Read
        } else {
            Access::ScannedRange
        };
        assert_eq!(
            outcome.map(|(_, access, theirs)| (access, theirs)),
            Some((access, WriteKind::Delete)),
            "{level:?}"
        );
    }

    // The same scan, in a transaction that writes nothing at all.
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["g/1", "g/3"]);
    let txn = begin(&db, IsolationLevel::DefraLevel);
    let mut cursor = txn.cursor(
        Some(RANGE.0),
        Some(RANGE.1),
        ScanDirection::Forward,
        ScanCheck::Range,
    );
    assert_eq!(keys(&mut cursor, &txn, usize::MAX).len(), 2);
    db.db().delete(b"g/3").unwrap();
    assert!(outcome(txn.commit()).is_none());
    // Below DefraLevel it validates, written or not.
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);
    let mut cursor = txn.cursor(
        Some(RANGE.0),
        Some(RANGE.1),
        ScanDirection::Forward,
        ScanCheck::Range,
    );
    assert_eq!(keys(&mut cursor, &txn, usize::MAX).len(), 1);
    db.db().put(b"g/4", b"v").unwrap();
    assert!(outcome(txn.commit()).is_some());
}

#[test]
fn a_range_of_a_pessimistic_transaction_is_validated() {
    let dir = tempfile::tempdir().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default()).unwrap();
    db.db().put(b"g/1", b"v").unwrap();
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    let mut cursor = txn.cursor(
        Some(RANGE.0),
        Some(RANGE.1),
        ScanDirection::Forward,
        ScanCheck::Range,
    );
    assert_eq!(keys(&mut cursor, &txn, usize::MAX).len(), 1);
    db.db().put(b"g/2", b"v").unwrap();
    txn.put(b"elsewhere", b"x").unwrap();
    assert_eq!(
        outcome(txn.commit()).map(|(_, access, _)| access),
        Some(Access::ScannedRange)
    );
}

#[test]
fn ranges_of_several_cursors_all_apply() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &["a/1", "g/1", "m/1"]);
    let txn = begin(&db, IsolationLevel::DefraLevel);
    for (start, end) in [(&b"a/"[..], &b"a0"[..]), (b"g/", b"g0"), (b"a/", b"b")] {
        let mut cursor = txn.cursor(
            Some(start),
            Some(end),
            ScanDirection::Forward,
            ScanCheck::Range,
        );
        keys(&mut cursor, &txn, usize::MAX);
    }
    db.db().put(b"m/2", b"outside every range").unwrap();
    db.db().put(b"g/2", b"inside one").unwrap();
    txn.put(b"elsewhere", b"x").unwrap();
    assert_eq!(
        outcome(txn.commit()).map(|(key, _, _)| key),
        Some(b"g/2".to_vec())
    );
}

// ---- scan_parts ----

#[test]
fn a_key_merged_inside_a_parts_scan_is_validated_over_the_parts_only() {
    let case = |scan: ScanCheck, concurrent: &dyn Fn(&Db)| {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        for key in ["d/1", "d/2", "d/3"] {
            db.db().put(key.as_bytes(), &value([1, 1, 1])).unwrap();
        }
        let txn = begin(&db, IsolationLevel::DefraLevel);
        let mut cursor = txn.cursor(Some(b"d/"), Some(b"d0"), ScanDirection::Forward, scan);
        assert_eq!(keys(&mut cursor, &txn, usize::MAX).len(), 3);
        txn.merge(b"d/2", &add(1, 10)).unwrap();
        concurrent(db.db());
        outcome(txn.commit())
    };
    let parts = || ScanCheck::Parts(Box::from([0]));
    let other_part = |db: &Db| db.merge(b"d/2", &add(2, 1)).unwrap();
    let same_part = |db: &Db| db.merge(b"d/2", &add(0, 1)).unwrap();

    assert_eq!(case(parts(), &other_part), None, "part 2 was not read");
    assert_eq!(
        case(parts(), &same_part),
        Some((b"d/2".to_vec(), Access::ScannedThenWrote, WriteKind::Merge))
    );
    // A plain stretch reads the whole value.
    assert!(case(ScanCheck::Stretch, &other_part).is_some());
    // A put replaces every part, and the scan used part 0 of it: validated
    // in whole, but by value, so a rewrite of the same bytes is not a change.
    assert!(
        case(parts(), &|db: &Db| db
            .put(b"d/2", &value([1, 1, 2]))
            .unwrap())
        .is_some()
    );
    assert_eq!(
        case(parts(), &|db: &Db| db
            .put(b"d/2", &value([1, 1, 1]))
            .unwrap()),
        None
    );
    // No parts: the scan decided on nothing in the values.
    assert_eq!(case(ScanCheck::Parts(Box::from([])), &same_part), None);
    // A key the scan did not walk is not covered.
    assert_eq!(
        case(parts(), &|db: &Db| db.merge(b"d/1", &add(0, 1)).unwrap()),
        None
    );
}

#[test]
fn a_parts_scan_is_a_plain_scan_where_the_level_cannot_project() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"d/1", &value([1, 1, 1])).unwrap();
    let txn = begin(&db, IsolationLevel::RepeatableRead);
    let mut cursor = txn.cursor(
        Some(b"d/"),
        Some(b"d0"),
        ScanDirection::Forward,
        ScanCheck::Parts(Box::from([0])),
    );
    assert_eq!(keys(&mut cursor, &txn, usize::MAX).len(), 1);
    txn.merge(b"d/1", &add(1, 1)).unwrap();
    db.db().merge(b"d/1", &add(2, 1)).unwrap();
    assert!(outcome(txn.commit()).is_some());
}

// ---- errors ----

/// Declines any operand `bad`.
struct Declines;

impl MergeOperator for Declines {
    fn name(&self) -> &'static str {
        "declines"
    }

    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        if operands.contains(&&b"bad"[..]) {
            return None;
        }
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        operands
            .iter()
            .for_each(|operand| out.extend_from_slice(operand));
        Some(out)
    }
}

fn declining(dir: &std::path::Path) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(
        dir,
        Options::default().merge_operator(Some(Arc::new(Declines))),
    )
    .unwrap()
}

#[test]
fn an_error_in_the_middle_of_a_range_is_an_error_and_never_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let db = declining(dir.path());
    seed(db.db(), &["k0", "k1", "k2", "k3"]);
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);
    txn.merge(b"k2", b"bad").unwrap();

    // With entries before it, the page that holds them comes back, and the
    // next call reports the error, again and again.
    let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
    let page = cursor.next_page(&txn, usize::MAX).unwrap();
    assert_eq!(
        names(
            &page
                .entries
                .iter()
                .map(|(k, _)| k.clone())
                .collect::<Vec<_>>()
        ),
        ["k0", "k1"]
    );
    assert!(!page.done, "the range did not end");
    for _ in 0..3 {
        match cursor.next_page(&txn, usize::MAX) {
            Err(TransactionError::Engine(Error::MergeFailed(key))) => assert_eq!(key, b"k2"),
            other => panic!("expected the merge failure, got {other:?}"),
        }
    }

    // With nothing before it, the first page is the error.
    let mut cursor = txn.cursor(
        Some(b"k2"),
        None,
        ScanDirection::Forward,
        ScanCheck::Stretch,
    );
    assert!(matches!(
        cursor.next_page(&txn, 10),
        Err(TransactionError::Engine(Error::MergeFailed(_)))
    ));
}

#[test]
fn an_error_ends_a_scan_stream_with_an_err_item() {
    let dir = tempfile::tempdir().unwrap();
    let db = declining(dir.path());
    seed(db.db(), &["k0", "k1", "k2", "k3"]);
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);
    txn.merge(b"k2", b"bad").unwrap();

    let items: Vec<_> = txn.scan_stream(None, None).collect();
    assert_eq!(items.len(), 3, "two entries, the error, then nothing");
    assert!(items[0].is_ok() && items[1].is_ok());
    assert!(matches!(
        items[2],
        Err(TransactionError::Engine(Error::MergeFailed(_)))
    ));
    assert!(
        txn.scan_stream(None, None)
            .collect::<Result<Vec<_>, _>>()
            .is_err()
    );

    // Walking down, it is the second item.
    let items: Vec<_> = txn
        .scan_stream_in(None, None, ScanDirection::Reverse)
        .collect();
    assert_eq!(items.len(), 2);
    assert!(items[0].is_ok() && items[1].is_err());
}

#[test]
fn a_database_that_died_under_a_scan_ends_it_with_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    seed(db.db(), &TEN);
    let txn = begin(&db, IsolationLevel::SnapshotIsolation);
    txn.put(b"zzz", b"buffered").unwrap();
    db.db().close().unwrap();
    // A cursor made now carries a terminal error. It must not finish on the
    // buffered write alone and read as a complete range.
    let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
    for _ in 0..3 {
        assert!(cursor.next_page(&txn, 10).is_err());
    }
}

// ---- the pages against a model ----

mod model {
    use std::collections::BTreeMap;

    use super::*;
    use proptest::prelude::*;

    const KEYS: u8 = 9;

    #[derive(Clone, Debug)]
    enum Step {
        Put(u8, u8),
        Delete(u8),
        Page(usize),
        Savepoint,
        Rollback,
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            3 => (0..KEYS, 0u8..50).prop_map(|(k, v)| Step::Put(k, v)),
            2 => (0..KEYS).prop_map(Step::Delete),
            5 => (0usize..9).prop_map(Step::Page),
            1 => Just(Step::Savepoint),
            1 => Just(Step::Rollback),
        ]
    }

    fn key(k: u8) -> Vec<u8> {
        vec![b'a' + k]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

        /// Whatever the page sizes and the writes between pages, the pages are
        /// the entries beyond the cursor's position, as the transaction sees
        /// them when the page is read.
        #[test]
        fn pages_are_the_transactions_view_beyond_the_cursors_position(
            seeded in 0u16..512,
            reverse in any::<bool>(),
            start in proptest::option::of(0..KEYS),
            end in proptest::option::of(0..KEYS),
            steps in proptest::collection::vec(step(), 1..40),
        ) {
            let dir = tempfile::tempdir().unwrap();
            let db = open(dir.path());
            let mut snapshot: BTreeMap<u8, u8> = BTreeMap::new();
            for k in (0..KEYS).filter(|k| seeded & (1 << k) != 0) {
                db.db().put(&key(k), &[100 + k]).unwrap();
                snapshot.insert(k, 100 + k);
            }
            let mut txn = begin(&db, IsolationLevel::SnapshotIsolation);
            let (start_key, end_key) = (start.map(key), end.map(key));
            let direction = if reverse { ScanDirection::Reverse } else { ScanDirection::Forward };
            let mut cursor = txn.cursor(
                start_key.as_deref(),
                end_key.as_deref(),
                direction,
                ScanCheck::Stretch,
            );

            let mut log: Vec<(u8, Option<u8>)> = Vec::new();
            let mut savepoints: Vec<usize> = Vec::new();
            let mut position: Option<u8> = None;
            let mut finished = false;
            for step in steps {
                match step {
                    Step::Put(k, v) => {
                        txn.put(&key(k), &[v]).unwrap();
                        log.push((k, Some(v)));
                    }
                    Step::Delete(k) => {
                        txn.delete(&key(k)).unwrap();
                        log.push((k, None));
                    }
                    Step::Savepoint => {
                        txn.set_savepoint();
                        savepoints.push(log.len());
                    }
                    Step::Rollback => {
                        if let Some(len) = savepoints.pop() {
                            txn.rollback_to_savepoint().unwrap();
                            log.truncate(len);
                        }
                    }
                    Step::Page(max_bytes) => {
                        let view = |k: u8| {
                            log.iter()
                                .rev()
                                .find(|(written, _)| *written == k)
                                .map_or_else(|| snapshot.get(&k).copied(), |(_, v)| *v)
                        };
                        let mut remaining: Vec<u8> = (0..KEYS)
                            .filter(|k| start.is_none_or(|s| *k >= s) && end.is_none_or(|e| *k < e))
                            .filter(|k| match position {
                                None => true,
                                Some(at) if reverse => *k < at,
                                Some(at) => *k > at,
                            })
                            .filter(|k| view(*k).is_some())
                            .collect();
                        if reverse {
                            remaining.reverse();
                        }
                        let (mut want, mut taken, mut walked_off_the_end) = (Vec::new(), 0usize, true);
                        if !finished {
                            for k in remaining {
                                want.push((key(k), vec![view(k).unwrap()]));
                                position = Some(k);
                                taken += 2;
                                if taken >= max_bytes {
                                    walked_off_the_end = false;
                                    break;
                                }
                            }
                        }
                        let page = cursor.next_page(&txn, max_bytes).unwrap();
                        let got: Vec<(Vec<u8>, Vec<u8>)> = page
                            .entries
                            .iter()
                            .map(|(k, v)| (k.clone(), v.to_vec()))
                            .collect();
                        prop_assert_eq!(got, want);
                        prop_assert_eq!(page.done, walked_off_the_end || finished);
                        finished |= page.done;
                    }
                }
            }
        }
    }
}
