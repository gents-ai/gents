//! Validated scans (`ScanCheck::Range`) against the two things that could
//! switch one off without anyone asking: a key classifier, and a savepoint
//! rollback.
//!
//! A classifier relaxes validation for the keys it names: a scan stretch
//! inside a commutative prefix records nothing, a put of a content-addressed
//! key is never validated, and neither is a read of a log key or an append.
//! A validated scan is the one read no class relaxes, because its caller
//! decides on everything the range holds. Each class is tried here by a
//! classifier that names the whole range, with a concurrent write of exactly
//! the kind that class lets through, and beside every refusal sits the plain
//! scan the class does exempt, so the classifier is seen to be consulted.
//!
//! A savepoint rollback takes writes back and no read, so a range a scan
//! covered stays validated whether the savepoint was set between two of its
//! pages or before its first one, and the pages after the rollback show the
//! transaction's writes as they then are.

// Native-only: these use the filesystem.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::sync::Arc;

use regolith::prelude::*;
use regolith::{Access, CommitReceipt, Db, ScanCheck, ScanDirection, TxnCursor, WriteKind};

/// Entries at `log/<20 decimal digits>`, the head at `log-head`.
struct Journal;

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        b"log-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("log/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        4 + 20
    }
}

fn journal() -> Arc<dyn LogLayout> {
    Arc::new(Journal)
}

/// Names every key of the scanned ranges a class that relaxes validation:
/// `c/` a commutative prefix, `b/` content-addressed, the journal a log.
struct Exempting;

impl KeyClassifier for Exempting {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"c/") {
            KeyClass::CommutativePrefix { len: 2 }
        } else if key.starts_with(b"b/") {
            KeyClass::ContentAddressed
        } else if key.starts_with(b"log") {
            KeyClass::Log
        } else {
            KeyClass::Ordinary
        }
    }
}

fn classified(dir: &Path) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir, Options::default())
        .unwrap()
        .with_policy(Arc::new(Exempting))
}

fn begin(db: &OptimisticTransactionDb, level: IsolationLevel) -> Transaction {
    db.begin(&TxnOptions::new().isolation(level))
}

fn defra(db: &OptimisticTransactionDb) -> Transaction {
    begin(db, IsolationLevel::DefraLevel)
}

/// What a commit that lost reports: the key, what the transaction did with it
/// and the write that overtook it. `None` when it committed.
fn outcome(result: TxResult<CommitReceipt>) -> Option<(Vec<u8>, Access, WriteKind)> {
    match result {
        Err(TransactionError::Conflict(c)) => Some((c.key().to_vec(), c.mine(), c.theirs())),
        Ok(_) => None,
        Err(other) => panic!("expected a commit or a conflict, got {other:?}"),
    }
}

/// Read every page that is left and return the keys.
fn keys(cursor: &mut TxnCursor, txn: &Transaction) -> Vec<String> {
    let mut seen = Vec::new();
    loop {
        let page = cursor.next_page(txn, usize::MAX).unwrap();
        seen.extend(
            page.entries
                .into_iter()
                .map(|(key, _)| String::from_utf8(key).unwrap()),
        );
        if page.done {
            return seen;
        }
    }
}

/// Read one page of one entry and return its key.
fn one(cursor: &mut TxnCursor, txn: &Transaction) -> String {
    let page = cursor.next_page(txn, 0).unwrap();
    assert_eq!(page.entries.len(), 1, "a page of one entry");
    String::from_utf8(page.entries[0].0.clone()).unwrap()
}

/// A transaction at `DefraLevel` that scans `[start, end)` whole with `check`,
/// lets `concurrent` commit, writes one ordinary key outside every range (so
/// it is not write-free) and commits.
fn scan_then_commit(
    db: &OptimisticTransactionDb,
    (start, end): (&[u8], &[u8]),
    check: ScanCheck,
    concurrent: impl FnOnce(&OptimisticTransactionDb),
) -> Option<(Vec<u8>, Access, WriteKind)> {
    let txn = defra(db);
    let mut cursor = txn.cursor(Some(start), Some(end), ScanDirection::Forward, check);
    keys(&mut cursor, &txn);
    concurrent(db);
    txn.put(b"decision", b"taken").unwrap();
    outcome(txn.commit())
}

// ---- a classifier never exempts a validated scan (plan 3.15) ----

const COMMUTATIVE: (&[u8], &[u8]) = (b"c/", b"c0");

#[test]
fn a_validated_scan_inside_a_commutative_prefix_is_still_refused() {
    let seeded = |dir: &Path| {
        let db = classified(dir);
        for key in [&b"c/1"[..], b"c/5"] {
            db.db().put(key, b"member").unwrap();
        }
        db
    };
    // A member added under a fresh name, and one taken away: the two writes a
    // commutative prefix is declared for.
    let added = |db: &OptimisticTransactionDb| db.db().put(b"c/3", b"member").unwrap();
    let removed = |db: &OptimisticTransactionDb| db.db().delete(b"c/1").unwrap();

    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        scan_then_commit(&seeded(dir.path()), COMMUTATIVE, ScanCheck::Range, added),
        Some((b"c/3".to_vec(), Access::ScannedRange, WriteKind::Put)),
        "a key that appeared in the prefix"
    );
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        scan_then_commit(&seeded(dir.path()), COMMUTATIVE, ScanCheck::Range, removed),
        Some((b"c/1".to_vec(), Access::ScannedRange, WriteKind::Delete)),
        "a key that left the prefix"
    );

    // The same scans as plain stretches: inside the prefix they record
    // nothing, so the classifier is consulted and does exempt them.
    for concurrent in [&added as &dyn Fn(&OptimisticTransactionDb), &removed] {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            scan_then_commit(
                &seeded(dir.path()),
                COMMUTATIVE,
                ScanCheck::Stretch,
                concurrent
            ),
            None,
            "a plain scan inside a commutative prefix is exempt"
        );
    }
}

#[test]
fn a_validated_scan_stays_refused_when_the_scanner_writes_inside_the_prefix_too() {
    let dir = tempfile::tempdir().unwrap();
    let db = classified(dir.path());
    db.db().put(b"c/1", b"member").unwrap();
    db.db().put(b"c/9", b"member").unwrap();

    // The scanner adds a member of its own, between the two it was returned:
    // inside a stretch of the prefix that write is blind, and it stays a
    // blind write here. The range is what refuses the commit.
    let txn = defra(&db);
    let mut cursor = txn.cursor(
        Some(COMMUTATIVE.0),
        Some(COMMUTATIVE.1),
        ScanDirection::Forward,
        ScanCheck::Range,
    );
    assert_eq!(keys(&mut cursor, &txn), ["c/1", "c/9"]);
    txn.put(b"c/4", b"mine").unwrap();
    db.db().put(b"c/6", b"theirs").unwrap();
    assert_eq!(
        outcome(txn.commit()),
        Some((b"c/6".to_vec(), Access::ScannedRange, WriteKind::Put))
    );
    assert_eq!(db.db().get(b"c/4").unwrap(), None, "nothing was applied");

    // With nothing landing in the range the same transaction commits: its own
    // write inside the range is not a change to it.
    let txn = defra(&db);
    let mut cursor = txn.cursor(
        Some(COMMUTATIVE.0),
        Some(COMMUTATIVE.1),
        ScanDirection::Forward,
        ScanCheck::Range,
    );
    assert_eq!(keys(&mut cursor, &txn), ["c/1", "c/6", "c/9"]);
    txn.put(b"c/4", b"mine").unwrap();
    assert_eq!(outcome(txn.commit()), None);
}

const BLOCKS: (&[u8], &[u8]) = (b"b/", b"b0");

#[test]
fn a_validated_scan_of_content_addressed_keys_is_still_refused() {
    let seeded = |dir: &Path| {
        let db = classified(dir);
        db.db().put(b"b/1", b"one").unwrap();
        db
    };
    // Each is a commit the class never validates on its own account: a
    // transaction that creates a new block, and one that stores a block that
    // is already there, byte for byte.
    let created = |db: &OptimisticTransactionDb| {
        let other = defra(db);
        other.put(b"b/2", b"two").unwrap();
        other.commit().unwrap();
    };
    let stored_again = |db: &OptimisticTransactionDb| {
        let other = defra(db);
        other.put(b"b/1", b"one").unwrap();
        other.commit().unwrap();
    };
    let collected = |db: &OptimisticTransactionDb| db.db().delete(b"b/1").unwrap();

    for (concurrent, key, theirs) in [
        (
            &created as &dyn Fn(&OptimisticTransactionDb),
            &b"b/2"[..],
            WriteKind::Put,
        ),
        (&stored_again, b"b/1", WriteKind::Put),
        (&collected, b"b/1", WriteKind::Delete),
    ] {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            scan_then_commit(&seeded(dir.path()), BLOCKS, ScanCheck::Range, concurrent),
            Some((key.to_vec(), Access::ScannedRange, theirs)),
        );
    }

    // A plain scan of the blocks records nothing a put of one could refuse.
    for concurrent in [&created as &dyn Fn(&OptimisticTransactionDb), &stored_again] {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            scan_then_commit(&seeded(dir.path()), BLOCKS, ScanCheck::Stretch, concurrent),
            None,
            "a plain scan of content-addressed keys is not validated"
        );
    }
}

#[test]
fn a_validated_scan_is_refused_over_a_block_the_scanner_puts_as_well() {
    let dir = tempfile::tempdir().unwrap();
    let db = classified(dir.path());
    db.db().put(b"b/1", b"one").unwrap();

    // Both transactions create block 2 with the same bytes. A put of a
    // content-addressed key is exempt from every check of that key, and the
    // second create would commit, had it not also decided on what the range
    // held.
    let scanner = defra(&db);
    let mut cursor = scanner.cursor(
        Some(BLOCKS.0),
        Some(BLOCKS.1),
        ScanDirection::Forward,
        ScanCheck::Range,
    );
    assert_eq!(keys(&mut cursor, &scanner), ["b/1"]);
    scanner.put(b"b/2", b"two").unwrap();

    let other = defra(&db);
    other.put(b"b/2", b"two").unwrap();
    other.commit().unwrap();

    assert_eq!(
        outcome(scanner.commit()),
        Some((b"b/2".to_vec(), Access::ScannedRange, WriteKind::Put))
    );

    // The same pair without the scan: both creates commit.
    let (first, second) = (defra(&db), defra(&db));
    first.put(b"b/3", b"three").unwrap();
    second.put(b"b/3", b"three").unwrap();
    first.commit().unwrap();
    assert_eq!(outcome(second.commit()), None);
}

const ENTRIES: (&[u8], &[u8]) = (b"log/", b"log0");

fn append(db: &OptimisticTransactionDb, entry: &[u8]) {
    let txn = defra(db);
    txn.append(&journal(), entry, None).unwrap();
    txn.commit().unwrap();
}

#[test]
fn a_validated_scan_of_a_log_is_still_refused_by_an_append() {
    let seeded = |dir: &Path| {
        let db = classified(dir);
        append(&db, b"first");
        append(&db, b"second");
        db
    };
    let appended = |db: &OptimisticTransactionDb| append(db, b"third");

    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        scan_then_commit(&seeded(dir.path()), ENTRIES, ScanCheck::Range, appended),
        Some((
            format!("log/{:020}", 3).into_bytes(),
            Access::ScannedRange,
            WriteKind::Put
        )),
        "the entry that landed in the range"
    );

    // The exempt reads of the same log: a plain scan of its entries, and a
    // point read of its head and of an entry. None is validated.
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        scan_then_commit(&seeded(dir.path()), ENTRIES, ScanCheck::Stretch, appended),
        None,
        "a plain scan of log keys is never validated"
    );
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(dir.path());
    let reader = defra(&db);
    assert!(reader.get(b"log-head").unwrap().is_some());
    assert!(
        reader
            .get(format!("log/{:020}", 2).as_bytes())
            .unwrap()
            .is_some()
    );
    appended(&db);
    reader.put(b"decision", b"taken").unwrap();
    assert_eq!(outcome(reader.commit()), None);
}

#[test]
fn a_validated_scan_of_a_log_is_refused_while_its_own_append_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let db = classified(dir.path());
    append(&db, b"first");

    // The scanner appends too. Its append is never validated, and with
    // nothing else landing it commits, the entry it wrote inside its own
    // range included.
    let scanner = defra(&db);
    let mut cursor = scanner.cursor(
        Some(ENTRIES.0),
        Some(ENTRIES.1),
        ScanDirection::Forward,
        ScanCheck::Range,
    );
    assert_eq!(keys(&mut cursor, &scanner).len(), 1);
    scanner.append(&journal(), b"mine", None).unwrap();
    assert_eq!(outcome(scanner.commit()), None);

    // With another append between its scan and its commit, the range refuses
    // it, and its entry is not in the log.
    let scanner = defra(&db);
    let mut cursor = scanner.cursor(
        Some(ENTRIES.0),
        Some(ENTRIES.1),
        ScanDirection::Forward,
        ScanCheck::Range,
    );
    assert_eq!(keys(&mut cursor, &scanner).len(), 2);
    scanner.append(&journal(), b"late", None).unwrap();
    append(&db, b"theirs");
    assert_eq!(
        outcome(scanner.commit()).map(|(_, mine, theirs)| (mine, theirs)),
        Some((Access::ScannedRange, WriteKind::Put))
    );
    let head = db.db().get(b"log-head").unwrap().unwrap();
    assert_eq!(
        u64::from_be_bytes(head.try_into().unwrap()),
        3,
        "first, mine, theirs"
    );
}

// ---- validated scans with savepoints (plan 7.2) ----

const GRANTS: (&[u8], &[u8]) = (b"g/", b"g0");

fn plain(dir: &Path) -> OptimisticTransactionDb {
    let db = OptimisticTransactionDb::open(dir, Options::default()).unwrap();
    for key in ["g/1", "g/3", "g/5", "g/7"] {
        db.db().put(key.as_bytes(), b"grant").unwrap();
    }
    db
}

fn ranged(txn: &Transaction, direction: ScanDirection) -> TxnCursor {
    txn.cursor(Some(GRANTS.0), Some(GRANTS.1), direction, ScanCheck::Range)
}

/// The savepoint is set between two pages of the scan and rolled back to
/// between two later ones.
#[test]
fn a_rollback_inside_a_validated_range_takes_back_the_writes_and_not_the_range() {
    // What lands between the scan and the commit, with the conflict it must
    // cause; `None` lands nothing.
    type Landed = Option<(&'static [u8], WriteKind)>;
    let cases: [Landed; 4] = [
        None,
        // A phantom in the part read before the savepoint.
        Some((b"g/0", WriteKind::Put)),
        // A key that left the part read after the rollback.
        Some((b"g/7", WriteKind::Delete)),
        // The key the rolled-back write named: the scan was handed it, so
        // the range covers it whoever writes it.
        Some((b"g/2", WriteKind::Put)),
    ];
    for landed in cases {
        let dir = tempfile::tempdir().unwrap();
        let db = plain(dir.path());
        let mut txn = defra(&db);
        txn.put(b"decision", b"taken").unwrap();
        let mut cursor = ranged(&txn, ScanDirection::Forward);
        assert_eq!(one(&mut cursor, &txn), "g/1");

        txn.set_savepoint();
        txn.put(b"g/2", b"first").unwrap();
        // Seen: the cursor's fold of the buffer now holds `g/2`.
        assert_eq!(one(&mut cursor, &txn), "g/2");
        txn.rollback_to_savepoint().unwrap();
        // The buffer is as long again as when the cursor folded it, with
        // another key in it: the pages that follow are the transaction's
        // view now, the rolled-back write gone and the new one there.
        txn.put(b"g/4", b"second").unwrap();
        assert_eq!(keys(&mut cursor, &txn), ["g/3", "g/4", "g/5", "g/7"]);

        match landed {
            Some((key, WriteKind::Delete)) => db.db().delete(key).unwrap(),
            Some((key, _)) => db.db().put(key, b"theirs").unwrap(),
            None => {}
        }
        let expected = landed.map(|(key, theirs)| (key.to_vec(), Access::ScannedRange, theirs));
        assert_eq!(outcome(txn.commit()), expected, "{landed:?}");
        if landed.is_none() {
            assert_eq!(
                db.db().get(b"g/2").unwrap(),
                None,
                "the rolled-back write is not committed"
            );
            assert_eq!(
                db.db().get(b"g/4").unwrap().as_deref(),
                Some(&b"second"[..])
            );
        }
    }
}

/// The savepoint is set before the scan begins and rolled back to after it
/// ended: the whole range lies between the two.
#[test]
fn a_rollback_across_a_validated_range_leaves_it_validated() {
    for direction in [ScanDirection::Forward, ScanDirection::Reverse] {
        for flush in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let db = plain(dir.path());
            let mut txn = defra(&db);
            txn.put(b"decision", b"taken").unwrap();

            txn.set_savepoint();
            let mut cursor = ranged(&txn, direction);
            txn.put(b"g/2", b"mine").unwrap();
            assert_eq!(keys(&mut cursor, &txn).len(), 5);
            txn.rollback_to_savepoint().unwrap();

            // A phantom the transaction would have been returned.
            db.db().put(b"g/6", b"theirs").unwrap();
            if flush {
                db.db().flush().unwrap();
            }
            assert_eq!(
                outcome(txn.commit()),
                Some((b"g/6".to_vec(), Access::ScannedRange, WriteKind::Put)),
                "{direction:?} flush={flush}"
            );
            assert_eq!(db.db().get(b"decision").unwrap(), None);
        }
    }

    // With nothing landing the commit goes through, and applies only what
    // was written before the savepoint.
    let dir = tempfile::tempdir().unwrap();
    let db = plain(dir.path());
    let mut txn = defra(&db);
    txn.put(b"decision", b"taken").unwrap();
    txn.set_savepoint();
    let mut cursor = ranged(&txn, ScanDirection::Forward);
    txn.put(b"g/2", b"mine").unwrap();
    assert_eq!(keys(&mut cursor, &txn).len(), 5);
    txn.rollback_to_savepoint().unwrap();
    assert_eq!(outcome(txn.commit()), None);
    assert_eq!(db.db().get(b"g/2").unwrap(), None);
    assert_eq!(
        db.db().get(b"decision").unwrap().as_deref(),
        Some(&b"taken"[..])
    );
}

/// A scan that stopped early inside a savepoint validates what it read and
/// nothing ahead of it, after the rollback as before it.
#[test]
fn a_rollback_keeps_an_early_stop_where_the_scan_stopped() {
    let stopped_early = |concurrent: &dyn Fn(&Db)| {
        let dir = tempfile::tempdir().unwrap();
        let db = plain(dir.path());
        let mut txn = defra(&db);
        txn.put(b"decision", b"taken").unwrap();
        txn.set_savepoint();
        let mut cursor = ranged(&txn, ScanDirection::Forward);
        assert_eq!(one(&mut cursor, &txn), "g/1");
        assert_eq!(one(&mut cursor, &txn), "g/3");
        txn.put(b"g/9", b"mine").unwrap();
        txn.rollback_to_savepoint().unwrap();
        concurrent(db.db());
        outcome(txn.commit())
    };
    assert!(
        stopped_early(&|db| db.put(b"g/2", b"v").unwrap()).is_some(),
        "a gap it passed"
    );
    assert!(
        stopped_early(&|db| db.delete(b"g/3").unwrap()).is_some(),
        "the last key it took"
    );
    assert!(
        stopped_early(&|db| db.put(b"g/4", b"v").unwrap()).is_none(),
        "ahead of it"
    );
    assert!(
        stopped_early(&|db| db.delete(b"g/7").unwrap()).is_none(),
        "ahead of it"
    );
}

/// A released savepoint keeps its writes, a nested rollback takes back only
/// its own, and neither forgets a range.
#[test]
fn nested_and_released_savepoints_keep_every_range() {
    let nested = |concurrent: &dyn Fn(&Db)| {
        let dir = tempfile::tempdir().unwrap();
        let db = plain(dir.path());
        db.db().put(b"m/1", b"member").unwrap();
        let mut txn = defra(&db);

        txn.set_savepoint();
        let mut grants = ranged(&txn, ScanDirection::Forward);
        assert_eq!(keys(&mut grants, &txn).len(), 4);
        txn.put(b"kept", b"outer").unwrap();

        txn.set_savepoint();
        let mut members = txn.cursor(
            Some(b"m/"),
            Some(b"m0"),
            ScanDirection::Forward,
            ScanCheck::Range,
        );
        assert_eq!(keys(&mut members, &txn), ["m/1"]);
        txn.put(b"dropped", b"inner").unwrap();
        txn.rollback_to_savepoint().unwrap();
        txn.release_savepoint().unwrap();

        concurrent(db.db());
        let result = outcome(txn.commit());
        let state = (
            db.db().get(b"kept").unwrap().is_some(),
            db.db().get(b"dropped").unwrap().is_some(),
        );
        (result.map(|(key, mine, _)| (key, mine)), state)
    };
    assert_eq!(
        nested(&|_| {}),
        (None, (true, false)),
        "the released savepoint's write commits, the rolled-back one does not"
    );
    assert_eq!(
        nested(&|db| db.put(b"g/4", b"v").unwrap()),
        (
            Some((b"g/4".to_vec(), Access::ScannedRange)),
            (false, false)
        ),
        "the range scanned under the released savepoint"
    );
    assert_eq!(
        nested(&|db| db.put(b"m/2", b"v").unwrap()),
        (
            Some((b"m/2".to_vec(), Access::ScannedRange)),
            (false, false)
        ),
        "the range scanned under the rolled-back savepoint"
    );
}

/// A rollback that leaves no write at all makes an optimistic `DefraLevel`
/// transaction write-free, which validates nothing (plan 3.8); at any other
/// level the range is validated, written or not.
#[test]
fn a_rollback_to_no_writes_validates_the_range_only_below_defra_level() {
    for (level, refused) in [
        (IsolationLevel::ReadCommitted, true),
        (IsolationLevel::SnapshotIsolation, true),
        (IsolationLevel::RepeatableRead, true),
        (IsolationLevel::DefraLevel, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let db = plain(dir.path());
        let mut txn = begin(&db, level);
        txn.set_savepoint();
        let mut cursor = ranged(&txn, ScanDirection::Forward);
        assert_eq!(keys(&mut cursor, &txn).len(), 4);
        txn.put(b"decision", b"taken").unwrap();
        txn.rollback_to_savepoint().unwrap();
        db.db().put(b"g/4", b"theirs").unwrap();
        assert_eq!(
            outcome(txn.commit()).map(|(key, mine, _)| (key, mine)),
            refused.then(|| (b"g/4".to_vec(), Access::ScannedRange)),
            "{level:?}"
        );
    }
}
