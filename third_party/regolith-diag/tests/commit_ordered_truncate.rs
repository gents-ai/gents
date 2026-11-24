//! `Transaction::truncate_log`: drops exactly the entries of a range, never a
//! conflict, shielded from older snapshots, refused past the head, undone by a
//! rollback to a savepoint, and the log's keys stay closed to everything else.
//!
//! The tests are the configurations of `LogTruncate.tla` run against the code:
//! two overlapping truncates, a truncate beside an append, a reader whose
//! snapshot precedes the truncate, a range past the head, and a savepoint.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::ops::RangeInclusive;
use std::path::Path;
use std::sync::Arc;

use regolith::prelude::*;
use regolith::{DurabilityMode, Error, Snapshot, TransactionDb};
use tempfile::TempDir;

/// Entries at `<name>/<20 decimal digits>`, head at `<name>-head`.
struct Journal {
    name: &'static str,
    head: String,
}

impl Journal {
    fn named(name: &'static str) -> Arc<dyn LogLayout> {
        Arc::new(Self {
            name,
            head: format!("{name}-head"),
        })
    }
}

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        self.head.as_bytes()
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("{}/{position:020}", self.name).as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        self.name.len() + 1 + 20
    }
}

fn journal() -> Arc<dyn LogLayout> {
    Journal::named("journal")
}

/// Everything under `journal`, `other` and `once/` is a log key.
struct LogKeys;

impl KeyClassifier for LogKeys {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"journal") || key.starts_with(b"other") || key.starts_with(b"once/") {
            KeyClass::Log
        } else {
            KeyClass::Ordinary
        }
    }
}

const LEVELS: [IsolationLevel; 5] = [
    IsolationLevel::ReadCommitted,
    IsolationLevel::SnapshotIsolation,
    IsolationLevel::RepeatableRead,
    IsolationLevel::Serializable,
    IsolationLevel::DefraLevel,
];

fn at(level: IsolationLevel) -> TxnOptions {
    TxnOptions::new().isolation(level)
}

fn open(dir: &Path) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir, Options::default())
        .unwrap()
        .with_policy(Arc::new(LogKeys))
}

fn be(bytes: Vec<u8>) -> u64 {
    u64::from_be_bytes(bytes.try_into().expect("a position is eight bytes"))
}

fn head_of(db: &Db, name: &str) -> u64 {
    db.get(format!("{name}-head").as_bytes())
        .unwrap()
        .map_or(0, be)
}

fn entry_key(name: &str, position: u64) -> Vec<u8> {
    format!("{name}/{position:020}").into_bytes()
}

/// The positions in `1..=head` of `name`'s log that still hold an entry.
fn live(db: &Db, name: &str) -> Vec<u64> {
    (1..=head_of(db, name))
        .filter(|p| db.get(&entry_key(name, *p)).unwrap().is_some())
        .collect()
}

/// The same, as `snapshot` sees them.
fn live_in(snapshot: &Snapshot, name: &str, head: u64) -> Vec<u64> {
    (1..=head)
        .filter(|p| snapshot.get(&entry_key(name, *p)).unwrap().is_some())
        .collect()
}

/// Appends `count` entries to the journal, each in a commit of its own, the
/// entry at position `p` holding `entry-p`.
fn fill(db: &OptimisticTransactionDb, count: u64) {
    let log = journal();
    let start = head_of(db.db(), "journal");
    for p in start + 1..=start + count {
        let tx = db.begin(&TxnOptions::new());
        tx.append(&log, format!("entry-{p}").as_bytes(), None)
            .unwrap();
        tx.commit().unwrap();
    }
}

/// The range `start..=end`, which is empty when `start` is past `end`.
fn range(start: u64, end: u64) -> RangeInclusive<u64> {
    RangeInclusive::new(start, end)
}

fn truncate(db: &OptimisticTransactionDb, range: RangeInclusive<u64>) {
    let tx = db.begin(&TxnOptions::new());
    tx.truncate_log(&journal(), range).unwrap();
    tx.commit().unwrap();
}

fn is_invalid_argument<T: std::fmt::Debug>(result: TxResult<T>) -> bool {
    matches!(
        result,
        Err(TransactionError::Engine(Error::InvalidArgument(_)))
    )
}

#[test]
fn a_truncate_drops_exactly_the_entries_of_its_range() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    fill(&db, 8);
    truncate(&db, 3..=5);
    assert_eq!(live(db.db(), "journal"), [1, 2, 6, 7, 8]);
    assert_eq!(
        db.db().get(&entry_key("journal", 6)).unwrap().as_deref(),
        Some(&b"entry-6"[..])
    );
}

#[test]
fn a_truncate_leaves_the_head_and_the_once_keys_alone_so_positions_keep_counting() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let tx = db.begin(&TxnOptions::new());
    tx.append(&log, b"one", Some(b"once/a")).unwrap();
    tx.append(&log, b"two", Some(b"once/b")).unwrap();
    tx.commit().unwrap();

    truncate(&db, 1..=2);
    assert_eq!(head_of(db.db(), "journal"), 2);
    assert_eq!(be(db.db().get(b"once/a").unwrap().unwrap()), 1);
    assert_eq!(be(db.db().get(b"once/b").unwrap().unwrap()), 2);

    // A once key that holds a position still appends nothing, and a new
    // entry is numbered from the head.
    let tx = db.begin(&TxnOptions::new());
    tx.append(&log, b"again", Some(b"once/a")).unwrap();
    tx.append(&log, b"three", Some(b"once/c")).unwrap();
    tx.commit().unwrap();
    assert_eq!(head_of(db.db(), "journal"), 3);
    assert_eq!(live(db.db(), "journal"), [3]);
    assert_eq!(be(db.db().get(b"once/c").unwrap().unwrap()), 3);
}

#[test]
fn a_truncate_commits_atomically_with_the_transactions_other_writes() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    fill(&db, 4);
    let tx = db.begin(&TxnOptions::new());
    tx.truncate_log(&journal(), 1..=4).unwrap();
    tx.put(b"merged-through", b"4").unwrap();
    // Nothing is applied before the commit.
    assert_eq!(live(db.db(), "journal"), [1, 2, 3, 4]);
    assert_eq!(db.db().get(b"merged-through").unwrap(), None);
    tx.commit().unwrap();
    assert_eq!(live(db.db(), "journal"), Vec::<u64>::new());
    assert_eq!(
        db.db().get(b"merged-through").unwrap().as_deref(),
        Some(&b"4"[..])
    );

    // A transaction that loses a conflict on another key drops nothing.
    fill(&db, 2);
    let (one, two) = (db.begin(&TxnOptions::new()), db.begin(&TxnOptions::new()));
    one.put(b"owner", b"one").unwrap();
    two.put(b"owner", b"two").unwrap();
    two.truncate_log(&journal(), 5..=5).unwrap();
    one.commit().unwrap();
    assert!(matches!(two.commit(), Err(TransactionError::Conflict(_))));
    assert_eq!(live(db.db(), "journal"), [5, 6]);
}

#[test]
fn an_empty_range_is_a_no_op() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    // Even on an empty log, where any other range is past the head.
    for range in [range(1, 0), range(5, 3), range(0, 0)] {
        let tx = db.begin(&TxnOptions::new());
        tx.truncate_log(&log, range).unwrap();
        tx.commit().unwrap();
    }
    fill(&db, 3);
    for range in [range(4, 1), range(0, 0), range(3, 2)] {
        let tx = db.begin(&TxnOptions::new());
        tx.truncate_log(&log, range).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(live(db.db(), "journal"), [1, 2, 3]);

    // Position 0 never holds an entry: a range that starts there drops the rest.
    truncate(&db, 0..=2);
    assert_eq!(live(db.db(), "journal"), [3]);
}

#[test]
fn a_truncate_is_never_a_conflict_at_any_level_in_either_flavor() {
    let optimistic = |level: IsolationLevel| {
        let dir = TempDir::new().unwrap();
        let db = open(dir.path());
        fill(&db, 6);
        (dir, db, level)
    };
    for level in LEVELS {
        // Overlapping ranges, in both commit orders.
        for first_wins in [true, false] {
            let (_dir, db, level) = optimistic(level);
            let log = journal();
            let one = db.begin(&at(level));
            let two = db.begin(&at(level));
            one.truncate_log(&log, 1..=4).unwrap();
            two.truncate_log(&log, 3..=6).unwrap();
            if first_wins {
                one.commit().unwrap();
                two.commit().unwrap();
            } else {
                two.commit().unwrap();
                one.commit().unwrap();
            }
            assert_eq!(live(db.db(), "journal"), Vec::<u64>::new(), "{level:?}");
        }
        // The same range twice, and a range that an earlier commit already dropped.
        {
            let (_dir, db, level) = optimistic(level);
            let log = journal();
            let one = db.begin(&at(level));
            let two = db.begin(&at(level));
            one.truncate_log(&log, 1..=3).unwrap();
            two.truncate_log(&log, 1..=3).unwrap();
            one.commit().unwrap();
            two.commit().unwrap();
            let late = db.begin(&at(level));
            late.truncate_log(&log, 1..=3).unwrap();
            late.commit().unwrap();
            assert_eq!(live(db.db(), "journal"), [4, 5, 6], "{level:?}");
        }
        // A truncate beside an append, in both commit orders and both ways
        // round: the append moves the head the truncate read, and never
        // writes a position the truncate drops.
        for append_first in [true, false] {
            let (_dir, db, level) = optimistic(level);
            let log = journal();
            let truncating = db.begin(&at(level));
            let appending = db.begin(&at(level));
            truncating.truncate_log(&log, 1..=6).unwrap();
            appending.append(&log, b"seven", None).unwrap();
            if append_first {
                appending.commit().unwrap();
                truncating.commit().unwrap();
            } else {
                truncating.commit().unwrap();
                appending.commit().unwrap();
            }
            assert_eq!(head_of(db.db(), "journal"), 7, "{level:?}");
            assert_eq!(live(db.db(), "journal"), [7], "{level:?}");
        }
    }

    for level in LEVELS {
        let dir = TempDir::new().unwrap();
        let db = TransactionDb::open(dir.path(), Options::default()).unwrap();
        let log = journal();
        let tx = db.begin(&TxnOptions::new());
        for p in 1..=6 {
            tx.append(&log, format!("entry-{p}").as_bytes(), None)
                .unwrap();
        }
        tx.commit().unwrap();
        let one = db.begin(&at(level));
        let two = db.begin(&at(level));
        let appending = db.begin(&at(level));
        one.truncate_log(&log, 1..=4).unwrap();
        two.truncate_log(&log, 3..=6).unwrap();
        appending.append(&log, b"seven", None).unwrap();
        appending.commit().unwrap();
        one.commit().unwrap();
        two.commit().unwrap();
        assert_eq!(head_of(db.db(), "journal"), 7, "pessimistic {level:?}");
        assert_eq!(live(db.db(), "journal"), [7], "pessimistic {level:?}");
    }
}

#[test]
fn a_snapshot_or_transaction_begun_before_the_truncate_still_reads_every_entry() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    fill(&db, 5);
    let snapshot = db.db().snapshot();
    let reading = db.begin(&TxnOptions::new());

    truncate(&db, 1..=4);
    db.db().flush().unwrap();
    db.db().compact_range(None, None).wait().unwrap();

    assert_eq!(live(db.db(), "journal"), [5]);
    assert_eq!(live_in(&snapshot, "journal", 5), [1, 2, 3, 4, 5]);
    for p in 1..=5 {
        assert_eq!(
            reading.get(&entry_key("journal", p)).unwrap().as_deref(),
            Some(format!("entry-{p}").as_bytes()),
            "position {p}"
        );
    }
    reading.rollback();
    drop(snapshot);
}

#[test]
fn a_range_past_the_head_the_transaction_reads_is_refused_and_buffers_nothing() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();

    // An empty log has no position to drop.
    let tx = db.begin(&TxnOptions::new());
    assert!(is_invalid_argument(tx.truncate_log(&log, 1..=1)));
    tx.rollback();

    fill(&db, 3);
    let tx = db.begin(&TxnOptions::new());
    tx.truncate_log(&log, 1..=1).unwrap();
    for range in [1..=4, 4..=4, 2..=u64::MAX, 3..=100] {
        let shown = format!("{range:?}");
        assert!(is_invalid_argument(tx.truncate_log(&log, range)), "{shown}");
    }
    // The transaction is unchanged: only the first truncate commits, with the
    // transaction's other writes.
    tx.put(b"ordinary", b"v").unwrap();
    tx.commit().unwrap();
    assert_eq!(live(db.db(), "journal"), [2, 3]);
    assert_eq!(
        db.db().get(b"ordinary").unwrap().as_deref(),
        Some(&b"v"[..])
    );

    // The head is the one this transaction reads: an entry appended after
    // its snapshot is past it.
    let early = db.begin(&TxnOptions::new());
    fill(&db, 1);
    assert_eq!(head_of(db.db(), "journal"), 4);
    assert!(is_invalid_argument(early.truncate_log(&log, 4..=4)));
    early.truncate_log(&log, 3..=3).unwrap();
    early.commit().unwrap();
    assert_eq!(live(db.db(), "journal"), [2, 4]);
    let late = db.begin(&TxnOptions::new());
    late.truncate_log(&log, 4..=4).unwrap();
    late.commit().unwrap();
    assert_eq!(live(db.db(), "journal"), [2]);
}

/// A layout whose entry key outgrows its declared maximum at a large position.
struct Understated;

impl LogLayout for Understated {
    fn head_key(&self) -> &[u8] {
        b"journal-understated-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(b"journal-understated/");
        out.extend_from_slice(&position.to_be_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        b"journal-understated/".len()
    }
}

/// A layout whose keys the classifier does not call Log.
struct Unclassified;

impl LogLayout for Unclassified {
    fn head_key(&self) -> &[u8] {
        b"plain-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("plain/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        64
    }
}

#[test]
fn a_truncate_makes_the_layout_checks_an_append_makes() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    fill(&db, 2);

    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    let understated: Arc<dyn LogLayout> = Arc::new(Understated);
    assert!(is_invalid_argument(tx.truncate_log(&understated, 1..=1)));
    let unclassified: Arc<dyn LogLayout> = Arc::new(Unclassified);
    assert!(is_invalid_argument(tx.truncate_log(&unclassified, 1..=1)));
    // Even an empty range is refused for a layout that is wrong.
    assert!(is_invalid_argument(
        tx.truncate_log(&unclassified, range(2, 1))
    ));
    // The transaction is unchanged and still commits.
    tx.put(b"k", b"v").unwrap();
    tx.commit().unwrap();
    assert_eq!(db.db().get(b"k").unwrap().as_deref(), Some(&b"v"[..]));

    // Where the classifier is ignored the call is not refused for it.
    db.db().put(b"plain-head", &1u64.to_be_bytes()).unwrap();
    db.db().put(b"plain/00000000000000000001", b"x").unwrap();
    let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
    tx.truncate_log(&unclassified, 1..=1).unwrap();
    tx.commit().unwrap();
    assert_eq!(db.db().get(b"plain/00000000000000000001").unwrap(), None);
}

#[test]
fn a_head_that_is_not_a_position_is_refused() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    db.db().put(b"journal-head", b"not-a-number").unwrap();
    let tx = db.begin(&TxnOptions::new());
    let err = tx.truncate_log(&journal(), 1..=1).unwrap_err();
    assert!(
        matches!(err, TransactionError::Engine(Error::InvalidArgument(_))),
        "{err:?}"
    );
    assert!(
        !err.to_string().contains("not-a-number"),
        "no key or value bytes in the message: {err}"
    );
}

#[test]
fn rolling_back_to_a_savepoint_drops_the_truncates_made_after_it() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    fill(&db, 6);
    let log = journal();
    let mut tx = db.begin(&TxnOptions::new());
    tx.truncate_log(&log, 1..=1).unwrap();
    tx.set_savepoint();
    tx.truncate_log(&log, 2..=3).unwrap();
    tx.set_savepoint();
    tx.truncate_log(&log, 4..=4).unwrap();
    tx.rollback_to_savepoint().unwrap();
    tx.truncate_log(&log, 5..=5).unwrap();
    tx.rollback_to_savepoint().unwrap();
    tx.truncate_log(&log, 6..=6).unwrap();
    tx.commit().unwrap();
    assert_eq!(live(db.db(), "journal"), [2, 3, 4, 5]);
}

#[test]
fn a_savepoint_released_keeps_its_truncates() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    fill(&db, 3);
    let log = journal();
    let mut tx = db.begin(&TxnOptions::new());
    tx.set_savepoint();
    tx.truncate_log(&log, 1..=2).unwrap();
    tx.release_savepoint().unwrap();
    tx.commit().unwrap();
    assert_eq!(live(db.db(), "journal"), [3]);
}

#[test]
fn the_log_keys_stay_closed_to_a_plain_write_after_the_truncate_exists() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    fill(&db, 2);
    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    tx.truncate_log(&journal(), 1..=2).unwrap();
    for key in [
        &b"journal-head"[..],
        b"journal/00000000000000000001",
        b"once/k",
    ] {
        assert!(matches!(
            tx.put(key, b"v"),
            Err(TransactionError::Engine(Error::LogKeyWrite))
        ));
        assert!(matches!(
            tx.delete(key),
            Err(TransactionError::Engine(Error::LogKeyWrite))
        ));
        assert!(matches!(
            tx.merge(key, b"op"),
            Err(TransactionError::Engine(Error::LogKeyWrite))
        ));
    }
    tx.commit().unwrap();
    assert_eq!(live(db.db(), "journal"), Vec::<u64>::new());
}

#[test]
fn truncates_survive_a_reopen_and_the_log_goes_on_from_its_head() {
    for durability in [DurabilityMode::Immediate, DurabilityMode::Eventual] {
        let dir = TempDir::new().unwrap();
        let options = || Options::default().durability(durability);
        {
            let db = OptimisticTransactionDb::open(dir.path(), options())
                .unwrap()
                .with_policy(Arc::new(LogKeys));
            fill(&db, 5);
            truncate(&db, 1..=3);
            db.db().close().unwrap();
        }
        let db = OptimisticTransactionDb::open(dir.path(), options())
            .unwrap()
            .with_policy(Arc::new(LogKeys));
        assert_eq!(live(db.db(), "journal"), [4, 5], "{durability:?}");
        fill(&db, 1);
        assert_eq!(live(db.db(), "journal"), [4, 5, 6], "{durability:?}");
    }
}
