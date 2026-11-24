//! The key a `MergeOperator` is handed: the key as the caller wrote it, in
//! every call the database makes of the operator.
//!
//! A database has one operator, so a caller with several value shapes
//! dispatches on the key. The operator here does that on the first byte, and
//! declines a key it does not know, so a call that hands it anything but the
//! caller's key gives a failed merge or a wrong answer. It also records every
//! key it is handed. Each test drives one path that calls the operator (a
//! point read, a scan, compaction, a transaction's read of its own merges, a
//! commit's checks) and checks both what the dispatch produced and that no
//! call saw a key the test did not write.

// Native-only. wasm-pack builds every test target for wasm32, and the browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::{Arc, Mutex};

use common::parted::{Parted, add, parts_of, value};
use regolith::{
    ColumnFamilyHandle, CompactionOutcome, Db, IsolationLevel, MergeOperator,
    OptimisticTransactionDb, Options, ScanCheck, ScanDirection, Snapshot, TransactionError,
    TxnOptions,
};

/// Which method of the operator a call was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Full,
    Partial,
    Touches,
}

/// Dispatches on the first byte of the key. A key under `c` holds a
/// big-endian `u64` counter and its operands add to it. A key under `p` holds
/// the three-part value of `common::parted`. Any other key is not this
/// operator's: it declines the merge, and says an operand touches everything.
#[derive(Default)]
struct ByKey {
    seen: Mutex<Vec<(Call, Vec<u8>)>>,
}

fn counter(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_be_bytes(bytes.try_into().ok()?))
}

fn count(n: u64) -> Vec<u8> {
    n.to_be_bytes().to_vec()
}

impl ByKey {
    fn saw(&self, call: Call, key: &[u8]) {
        self.seen.lock().unwrap().push((call, key.to_vec()));
    }

    /// The calls made since the last time this was asked.
    fn take(&self) -> Vec<(Call, Vec<u8>)> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }

    /// Take the calls made since the last check: every one must have been
    /// handed one of `keys`, and at least one must have been a `call`.
    #[track_caller]
    fn expect(&self, call: Call, keys: &[&[u8]]) {
        let seen = self.take();
        for (made, key) in &seen {
            assert!(
                keys.contains(&key.as_slice()),
                "{made:?} was handed {key:?}, not one of the caller's keys {keys:?}"
            );
        }
        assert!(
            seen.iter().any(|(made, _)| *made == call),
            "no {call:?} call was made; the calls were {seen:?}"
        );
    }
}

impl MergeOperator for ByKey {
    fn name(&self) -> &'static str {
        "by-key"
    }

    fn full_merge(&self, key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        self.saw(Call::Full, key);
        match key.first()? {
            b'c' => {
                let mut total = base.map_or(Some(0), counter)?;
                for operand in operands {
                    total = total.wrapping_add(counter(operand)?);
                }
                Some(count(total))
            }
            b'p' => Parted.full_merge(key, base, operands),
            _ => None,
        }
    }

    fn partial_merge(&self, key: &[u8], left: &[u8], right: &[u8]) -> Option<Vec<u8>> {
        self.saw(Call::Partial, key);
        match key.first()? {
            b'c' => Some(count(counter(left)?.wrapping_add(counter(right)?))),
            b'p' => Parted.partial_merge(key, left, right),
            _ => None,
        }
    }

    fn touches(&self, key: &[u8], operand: &[u8], parts: &[u32]) -> bool {
        self.saw(Call::Touches, key);
        match key.first() {
            Some(b'p') => Parted.touches(key, operand, parts),
            _ => true,
        }
    }
}

/// An in-memory database with `operator`: no compaction worker, and a
/// compaction due once level 0 holds two tables.
fn options(operator: &Arc<ByKey>) -> Options {
    let operator: Arc<dyn MergeOperator> = operator.clone();
    Options::memory()
        .l0_compaction_trigger(2)
        .merge_operator(Some(operator))
}

fn open(name: &str) -> (Db, Arc<ByKey>) {
    let operator = Arc::new(ByKey::default());
    let db = Db::open(name, options(&operator)).unwrap();
    (db, operator)
}

/// The default column family through the plain calls, or a named one through
/// the `_cf` calls: the two routes by which a caller's key reaches the engine.
enum Family {
    Default,
    Named(ColumnFamilyHandle),
}

impl Family {
    fn both(db: &Db) -> [Self; 2] {
        let named = db.create_column_family("named").unwrap();
        [Self::Default, Self::Named(named)]
    }

    fn put(&self, db: &Db, key: &[u8], value: &[u8]) {
        match self {
            Self::Default => db.put(key, value).unwrap(),
            Self::Named(cf) => db.put_cf(cf, key, value).unwrap(),
        }
    }

    fn merge(&self, db: &Db, key: &[u8], operand: &[u8]) {
        match self {
            Self::Default => db.merge(key, operand).unwrap(),
            Self::Named(cf) => db.merge_cf(cf, key, operand).unwrap(),
        }
    }

    fn get(&self, db: &Db, key: &[u8]) -> Option<Vec<u8>> {
        match self {
            Self::Default => db.get(key).unwrap(),
            Self::Named(cf) => db.get_cf(cf, key).unwrap(),
        }
    }

    fn multi_get(&self, db: &Db, keys: &[&[u8]]) -> Vec<Option<Vec<u8>>> {
        match self {
            Self::Default => db.multi_get(keys).unwrap(),
            Self::Named(cf) => db.multi_get_cf(cf, keys).unwrap(),
        }
    }

    fn snapshot_get(&self, snapshot: &Snapshot, key: &[u8]) -> Option<Vec<u8>> {
        match self {
            Self::Default => snapshot.get(key).unwrap(),
            Self::Named(cf) => snapshot.get_cf(cf, key).unwrap(),
        }
    }

    fn scan(&self, db: &Db) -> Vec<(Vec<u8>, Vec<u8>)> {
        match self {
            Self::Default => db.scan(None, None).unwrap(),
            Self::Named(cf) => db.scan_cf(cf, None, None).unwrap(),
        }
    }

    /// The family's entries through its iterator, forward or backward.
    fn walk(&self, db: &Db, reverse: bool) -> Vec<(Vec<u8>, Vec<u8>)> {
        let iter = match self {
            Self::Default => db.iter(),
            Self::Named(cf) => db.iter_cf(cf),
        };
        let entries = if reverse {
            iter.entries_rev()
        } else {
            iter.entries()
        };
        entries
            .map(|entry| entry.map(|(key, value)| (key, value.into_vec())))
            .collect::<regolith::Result<_>>()
            .unwrap()
    }
}

const HITS: &[u8] = b"c/hits";
const MISSES: &[u8] = b"c/misses";
const DOC: &[u8] = b"p/doc";

/// A counter and a three-part value, each a base with two operands on it.
fn seed(family: &Family, db: &Db) {
    family.put(db, HITS, &count(5));
    family.merge(db, HITS, &count(2));
    family.merge(db, HITS, &count(3));
    family.put(db, DOC, &value([10, 20, 30]));
    family.merge(db, DOC, &add(0, 1));
    family.merge(db, DOC, &add(2, 4));
}

fn seeded() -> Vec<(Vec<u8>, Vec<u8>)> {
    vec![
        (HITS.to_vec(), count(10)),
        (DOC.to_vec(), value([11, 20, 34])),
    ]
}

/// A point read folds the operands by the key's own shape, from the memtable
/// and from a table, through `get`, `multi_get` and a snapshot.
#[test]
fn a_point_read_hands_the_operator_the_callers_key() {
    let (db, operator) = open("merge-key-point");
    for family in Family::both(&db) {
        seed(&family, &db);
        operator.take();
        for flushed in [false, true] {
            if flushed {
                db.flush().unwrap();
            }
            assert_eq!(family.get(&db, HITS), Some(count(10)), "{flushed}");
            operator.expect(Call::Full, &[HITS]);
            assert_eq!(family.get(&db, DOC), Some(value([11, 20, 34])));
            operator.expect(Call::Full, &[DOC]);
            assert_eq!(
                family.multi_get(&db, &[HITS, DOC, b"c/absent"]),
                vec![Some(count(10)), Some(value([11, 20, 34])), None]
            );
            operator.expect(Call::Full, &[HITS, DOC]);
            let snapshot = db.snapshot();
            assert_eq!(family.snapshot_get(&snapshot, HITS), Some(count(10)));
            operator.expect(Call::Full, &[HITS]);
        }
    }
}

/// A scan folds each key it yields, in both directions, from the memtable and
/// from a table.
#[test]
fn a_scan_hands_the_operator_the_callers_key() {
    let (db, operator) = open("merge-key-scan");
    for family in Family::both(&db) {
        seed(&family, &db);
        operator.take();
        for flushed in [false, true] {
            if flushed {
                db.flush().unwrap();
            }
            assert_eq!(family.scan(&db), seeded(), "{flushed}");
            operator.expect(Call::Full, &[HITS, DOC]);
            assert_eq!(family.walk(&db, false), seeded());
            operator.expect(Call::Full, &[HITS, DOC]);
            let mut backward = seeded();
            backward.reverse();
            assert_eq!(family.walk(&db, true), backward);
            operator.expect(Call::Full, &[HITS, DOC]);
        }
    }
}

/// Run the one compaction pass that two tables in level 0 are due. Nothing
/// else compacts them first: the database has no worker, and the step a flush
/// leaves owing runs only at the next write.
fn compact(db: &Db) {
    assert_eq!(db.compact_step().unwrap(), CompactionOutcome::DidWork);
}

/// Compaction folds a key's operands onto the value under them with
/// `full_merge`, and operands with no value in their stripe (a snapshot holds
/// the value apart) with `partial_merge`. Both are asked by the caller's key,
/// so the fold happens and reads the same afterwards.
#[test]
fn compaction_hands_the_operator_the_callers_key() {
    let (db, operator) = open("merge-key-compaction");
    for family in Family::both(&db) {
        // Onto a value: one stripe holds the put and both operands. The put
        // and the operands are flushed apart, so the pass has two tables
        // holding one key to merge, and rewrites them.
        family.put(&db, HITS, &count(5));
        db.flush().unwrap();
        family.merge(&db, HITS, &count(2));
        family.merge(&db, HITS, &count(3));
        db.flush().unwrap();
        operator.take();
        compact(&db);
        operator.expect(Call::Full, &[HITS]);
        assert_eq!(family.get(&db, HITS), Some(count(10)));
        assert_eq!(
            operator.take(),
            [],
            "a key folded to one value is read without the operator"
        );

        // Operands alone: the snapshot keeps the put in a stripe of its own.
        family.put(&db, MISSES, &count(100));
        let snapshot = db.snapshot();
        family.merge(&db, MISSES, &count(1));
        db.flush().unwrap();
        family.merge(&db, MISSES, &count(1));
        family.merge(&db, MISSES, &count(1));
        db.flush().unwrap();
        operator.take();
        compact(&db);
        let folding = operator.take();
        assert_eq!(
            folding,
            [
                (Call::Partial, MISSES.to_vec()),
                (Call::Partial, MISSES.to_vec())
            ],
            "three operands fold pairwise into one, each fold asked by the caller's key"
        );
        assert_eq!(family.snapshot_get(&snapshot, MISSES), Some(count(100)));
        assert_eq!(family.get(&db, MISSES), Some(count(103)));
        operator.expect(Call::Full, &[MISSES]);
        drop(snapshot);
    }
}

fn open_txn(name: &str) -> (OptimisticTransactionDb, Arc<ByKey>) {
    let operator = Arc::new(ByKey::default());
    let db = OptimisticTransactionDb::open(name, options(&operator)).unwrap();
    (db, operator)
}

fn defra() -> TxnOptions {
    TxnOptions::new().isolation(IsolationLevel::DefraLevel)
}

/// A transaction that reads a key it merged into folds its own operands, by
/// `get`, `get_slice`, `get_for_update`, and a scan in either direction; the
/// commit stores what it read.
#[test]
fn a_transaction_reads_its_own_merges_through_the_callers_key() {
    let (db, operator) = open_txn("merge-key-own-writes");
    db.db().put(HITS, &count(5)).unwrap();
    db.db().put(DOC, &value([10, 20, 30])).unwrap();

    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::RepeatableRead));
    tx.merge(HITS, &count(2)).unwrap();
    tx.merge(HITS, &count(3)).unwrap();
    tx.merge(DOC, &add(0, 1)).unwrap();
    tx.merge(DOC, &add(2, 4)).unwrap();
    // Operands on a key the database does not hold, and on one the
    // transaction put: neither reads the database for a value to lie on.
    tx.merge(MISSES, &count(7)).unwrap();
    tx.put(b"c/own", &count(1)).unwrap();
    tx.merge(b"c/own", &count(1)).unwrap();
    operator.take();

    assert_eq!(tx.get(HITS).unwrap(), Some(count(10)));
    operator.expect(Call::Full, &[HITS]);
    assert_eq!(
        tx.get_slice(DOC).unwrap().map(|slice| slice.into_vec()),
        Some(value([11, 20, 34]))
    );
    operator.expect(Call::Full, &[DOC]);
    assert_eq!(tx.get_for_update(MISSES).unwrap(), Some(count(7)));
    operator.expect(Call::Full, &[MISSES]);
    assert_eq!(tx.get(b"c/own").unwrap(), Some(count(2)));
    operator.expect(Call::Full, &[b"c/own"]);

    let expected = vec![
        (HITS.to_vec(), count(10)),
        (MISSES.to_vec(), count(7)),
        (b"c/own".to_vec(), count(2)),
        (DOC.to_vec(), value([11, 20, 34])),
    ];
    let scanned: Vec<(Vec<u8>, Vec<u8>)> = tx
        .scan_stream(None, None)
        .map(|entry| entry.map(|(key, value)| (key, value.into_vec())))
        .collect::<Result<_, TransactionError>>()
        .unwrap();
    assert_eq!(scanned, expected);
    operator.expect(Call::Full, &[HITS, MISSES, b"c/own", DOC]);

    let mut cursor = tx.cursor(None, None, ScanDirection::Reverse, ScanCheck::Stretch);
    let page = cursor.next_page(&tx, usize::MAX).unwrap();
    let backward: Vec<(Vec<u8>, Vec<u8>)> = page
        .entries
        .into_iter()
        .map(|(key, value)| (key, value.into_vec()))
        .collect();
    assert_eq!(backward, expected.into_iter().rev().collect::<Vec<_>>());
    operator.expect(Call::Full, &[HITS, MISSES, b"c/own", DOC]);

    tx.commit().unwrap();
    assert_eq!(db.db().get(HITS).unwrap(), Some(count(10)));
    assert_eq!(db.db().get(DOC).unwrap(), Some(value([11, 20, 34])));
    assert_eq!(db.db().get(MISSES).unwrap(), Some(count(7)));
    assert_eq!(db.db().get(b"c/own").unwrap(), Some(count(2)));
}

/// A projected read is refused only by an operand that touches a part it
/// named, and `touches` knows the parts of a value only by the key's shape.
/// Handed anything but the caller's key it would answer that every operand
/// touches everything, and refuse the first commit below.
#[test]
fn a_projected_read_asks_touches_by_the_callers_key() {
    for flushed in [false, true] {
        let (db, operator) = open_txn("merge-key-touches");
        db.db().put(DOC, &value([10, 20, 30])).unwrap();

        // An operand on part 2 leaves a read of part 1 standing.
        let tx = db.begin(&defra());
        let read = tx.get_parts(DOC, &[1]).unwrap().unwrap();
        assert_eq!(parts_of(&read), Some([10, 20, 30]));
        db.db().merge(DOC, &add(2, 5)).unwrap();
        if flushed {
            db.db().flush().unwrap();
        }
        tx.put(b"elsewhere", b"x").unwrap();
        operator.take();
        tx.commit().unwrap();
        operator.expect(Call::Touches, &[DOC]);

        // An operand on part 1 refuses it.
        let tx = db.begin(&defra());
        tx.get_parts(DOC, &[1]).unwrap();
        db.db().merge(DOC, &add(1, 5)).unwrap();
        if flushed {
            db.db().flush().unwrap();
        }
        tx.put(b"elsewhere", b"y").unwrap();
        operator.take();
        assert!(matches!(tx.commit(), Err(TransactionError::Conflict(_))));
        operator.expect(Call::Touches, &[DOC]);
    }
}

/// At `DefraLevel` a read is current when the key holds the bytes the read
/// returned. The commit reads the value the transaction saw again, which
/// folds the operands under it, by the caller's key: a put of the same bytes
/// is then no change, and the commit goes through.
#[test]
fn a_commit_checks_a_read_value_through_the_callers_key() {
    let (db, operator) = open_txn("merge-key-value-rule");
    db.db().put(HITS, &count(5)).unwrap();
    db.db().merge(HITS, &count(1)).unwrap();

    let tx = db.begin(&defra());
    assert_eq!(tx.get(HITS).unwrap(), Some(count(6)));
    db.db().put(HITS, &count(6)).unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    operator.take();
    tx.commit().unwrap();
    operator.expect(Call::Full, &[HITS]);
}

/// The two families hold the same caller's key apart, and the operator is
/// handed the same key for both: it cannot tell them apart, and does not need
/// to.
#[test]
fn one_key_in_two_families_is_one_key_to_the_operator() {
    let (db, operator) = open("merge-key-families");
    let [default, named] = Family::both(&db);
    default.put(&db, HITS, &count(1));
    default.merge(&db, HITS, &count(1));
    named.put(&db, HITS, &count(40));
    named.merge(&db, HITS, &count(2));
    operator.take();
    assert_eq!(default.get(&db, HITS), Some(count(2)));
    assert_eq!(named.get(&db, HITS), Some(count(42)));
    assert_eq!(
        operator.take(),
        [(Call::Full, HITS.to_vec()), (Call::Full, HITS.to_vec())]
    );
}
