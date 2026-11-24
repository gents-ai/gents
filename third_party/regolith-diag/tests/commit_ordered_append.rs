//! `Transaction::append`: entries numbered in commit order, dense from 1, at
//! most once per once key, never a conflict.
//!
//! The tests are the configurations of `CommitOrderedAppend.tla` run against
//! the code: several writers, a writer appending twice, two writers sharing a
//! once key, a writer appending one once key twice, a commit that does not
//! append, aborts, and a reopen. The property test is in
//! `commit_ordered_append_props.rs` and the power-loss test in
//! `append_power_loss.rs`. The `KeyClass::Log` rules sit beside them.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use regolith::prelude::*;
use regolith::{DurabilityMode, Error, MemEnv, Snapshot, TransactionDb};
use tempfile::TempDir;

mod common;

/// Entries at `journal/<20 decimal digits>`, head at `journal-head`.
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

fn open_with(dir: &Path, opts: Options) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir, opts)
        .unwrap()
        .with_policy(Arc::new(LogKeys))
}

fn mem_options(env: &MemEnv, durability: DurabilityMode) -> Options {
    Options::default()
        .env(Arc::new(env.clone()))
        .max_background_compactions(0)
        .durability(durability)
}

fn be(bytes: Vec<u8>) -> u64 {
    u64::from_be_bytes(bytes.try_into().expect("a position is eight bytes"))
}

/// The head of `name`'s log, 0 while empty.
fn head_of(db: &Db, name: &str) -> u64 {
    db.get(format!("{name}-head").as_bytes())
        .unwrap()
        .map_or(0, be)
}

/// Every entry of `name`'s log, positions 1 to head, failing if one is missing.
fn log_of(db: &Db, name: &str) -> Vec<Vec<u8>> {
    (1..=head_of(db, name))
        .map(|p| {
            db.get(format!("{name}/{p:020}").as_bytes())
                .unwrap()
                .unwrap_or_else(|| panic!("{name} position {p} is missing below the head"))
        })
        .collect()
}

fn once_of(db: &Db, key: &[u8]) -> Option<u64> {
    db.get(key).unwrap().map(be)
}

fn texts(entries: &[Vec<u8>]) -> Vec<String> {
    entries
        .iter()
        .map(|e| String::from_utf8(e.clone()).unwrap())
        .collect()
}

#[test]
fn positions_are_dense_from_one_in_commit_order() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    for i in 0..10 {
        let tx = db.begin(&TxnOptions::new());
        tx.append(&log, format!("e{i}").as_bytes(), None).unwrap();
        tx.commit().unwrap();
        assert_eq!(head_of(db.db(), "journal"), i + 1);
    }
    assert_eq!(
        texts(&log_of(db.db(), "journal")),
        (0..10).map(|i| format!("e{i}")).collect::<Vec<_>>()
    );
}

#[test]
fn positions_follow_commit_order_not_begin_order() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let first = db.begin(&TxnOptions::new());
    let second = db.begin(&TxnOptions::new());
    first.append(&log, b"began-first", None).unwrap();
    second.append(&log, b"began-second", None).unwrap();
    second.commit().unwrap();
    first.commit().unwrap();
    assert_eq!(
        texts(&log_of(db.db(), "journal")),
        ["began-second", "began-first"]
    );
}

#[test]
fn a_writer_appending_twice_takes_consecutive_positions_in_append_order() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let other = db.begin(&TxnOptions::new());
    let tx = db.begin(&TxnOptions::new());
    tx.append(&log, b"a", None).unwrap();
    other.append(&log, b"x", None).unwrap();
    tx.append(&log, b"b", None).unwrap();
    tx.commit().unwrap();
    other.commit().unwrap();
    assert_eq!(texts(&log_of(db.db(), "journal")), ["a", "b", "x"]);
}

#[test]
fn logs_are_numbered_apart() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let (one, two) = (Journal::named("journal"), Journal::named("other"));
    let tx = db.begin(&TxnOptions::new());
    tx.append(&one, b"1a", None).unwrap();
    tx.append(&two, b"2a", None).unwrap();
    tx.append(&one, b"1b", None).unwrap();
    tx.commit().unwrap();
    assert_eq!(texts(&log_of(db.db(), "journal")), ["1a", "1b"]);
    assert_eq!(texts(&log_of(db.db(), "other")), ["2a"]);
}

#[test]
fn two_writers_sharing_a_once_key_produce_one_entry_whichever_commits_first() {
    for first_commits in [0, 1] {
        let dir = TempDir::new().unwrap();
        let db = open(dir.path());
        let log = journal();
        let writers = [db.begin(&TxnOptions::new()), db.begin(&TxnOptions::new())];
        writers[0].append(&log, b"from-0", Some(b"once/k")).unwrap();
        writers[1].append(&log, b"from-1", Some(b"once/k")).unwrap();
        let [w0, w1] = writers;
        if first_commits == 0 {
            w0.commit().unwrap();
            w1.commit().unwrap();
        } else {
            w1.commit().unwrap();
            w0.commit().unwrap();
        }
        let expected = format!("from-{first_commits}");
        assert_eq!(texts(&log_of(db.db(), "journal")), [expected]);
        assert_eq!(once_of(db.db(), b"once/k"), Some(1));
    }
}

#[test]
fn a_writer_appending_one_once_key_twice_yields_one_entry() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let tx = db.begin(&TxnOptions::new());
    tx.append(&log, b"first", Some(b"once/k")).unwrap();
    tx.append(&log, b"second", Some(b"once/k")).unwrap();
    tx.append(&log, b"third", None).unwrap();
    tx.commit().unwrap();
    assert_eq!(texts(&log_of(db.db(), "journal")), ["first", "third"]);
    assert_eq!(once_of(db.db(), b"once/k"), Some(1));
}

#[test]
fn a_once_key_that_already_holds_a_position_appends_nothing_later() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    for entry in [&b"first"[..], b"again"] {
        let tx = db.begin(&TxnOptions::new());
        tx.append(&log, entry, Some(b"once/k")).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(texts(&log_of(db.db(), "journal")), ["first"]);
    assert_eq!(head_of(db.db(), "journal"), 1);
}

#[test]
fn distinct_once_keys_each_record_their_own_position() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let tx = db.begin(&TxnOptions::new());
    for key in [&b"once/a"[..], b"once/b", b"once/c"] {
        tx.append(&log, key, Some(key)).unwrap();
    }
    tx.commit().unwrap();
    assert_eq!(once_of(db.db(), b"once/a"), Some(1));
    assert_eq!(once_of(db.db(), b"once/b"), Some(2));
    assert_eq!(once_of(db.db(), b"once/c"), Some(3));
}

#[test]
fn a_commit_that_does_not_append_leaves_no_hole_and_the_head_alone() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    for round in 0..3 {
        let writes = db.begin(&TxnOptions::new());
        writes
            .put(format!("plain{round}").as_bytes(), b"v")
            .unwrap();
        writes.commit().unwrap();
        let appends = db.begin(&TxnOptions::new());
        appends.append(&log, b"e", None).unwrap();
        appends.commit().unwrap();
        assert_eq!(head_of(db.db(), "journal"), round + 1, "round {round}");
    }
    db.db().put(b"direct", b"v").unwrap();
    assert_eq!(log_of(db.db(), "journal").len(), 3);
}

#[test]
fn a_transaction_that_rolls_back_or_conflicts_takes_no_position() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();

    let rolled = db.begin(&TxnOptions::new());
    rolled.append(&log, b"rolled", None).unwrap();
    rolled.rollback();

    let loser = db.begin(&TxnOptions::new());
    loser.put(b"contended", b"loser").unwrap();
    loser.append(&log, b"loser", None).unwrap();
    db.db().put(b"contended", b"winner").unwrap();
    assert!(matches!(
        loser.commit(),
        Err(TransactionError::Conflict { .. })
    ));

    let dropped = db.begin(&TxnOptions::new());
    dropped.append(&log, b"dropped", None).unwrap();
    drop(dropped);

    let kept = db.begin(&TxnOptions::new());
    kept.append(&log, b"kept", None).unwrap();
    kept.commit().unwrap();

    assert_eq!(texts(&log_of(db.db(), "journal")), ["kept"]);
    assert_eq!(head_of(db.db(), "journal"), 1);
}

#[test]
fn a_transaction_does_not_see_its_own_appends() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let seed = db.begin(&TxnOptions::new());
    seed.append(&log, b"seed", None).unwrap();
    seed.commit().unwrap();

    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    tx.append(&log, b"mine", Some(b"once/k")).unwrap();
    assert_eq!(
        tx.get(b"journal-head").unwrap(),
        Some(1u64.to_be_bytes().to_vec())
    );
    assert_eq!(tx.get(b"journal/00000000000000000002").unwrap(), None);
    assert_eq!(tx.get(b"once/k").unwrap(), None);
    let scanned = tx.scan_stream(Some(b"journal/"), Some(b"journal0")).count();
    assert_eq!(scanned, 1, "the scan finds the committed entry only");
    tx.commit().unwrap();
    assert_eq!(texts(&log_of(db.db(), "journal")), ["seed", "mine"]);
}

#[test]
fn an_append_never_conflicts_at_any_level_in_either_flavor() {
    for level in LEVELS {
        let dir = TempDir::new().unwrap();
        let db = open(dir.path());
        let log = journal();
        let writers: Vec<Transaction> = (0..4).map(|_| db.begin(&at(level))).collect();
        for (i, tx) in writers.iter().enumerate() {
            tx.append(&log, format!("w{i}").as_bytes(), None).unwrap();
        }
        for tx in writers {
            tx.commit()
                .unwrap_or_else(|e| panic!("optimistic {level:?}: {e}"));
        }
        assert_eq!(head_of(db.db(), "journal"), 4, "{level:?}");

        let dir = TempDir::new().unwrap();
        let db = TransactionDb::open(dir.path(), Options::default()).unwrap();
        let writers: Vec<Transaction> = (0..4).map(|_| db.begin(&at(level))).collect();
        for (i, tx) in writers.iter().enumerate() {
            tx.append(&log, format!("w{i}").as_bytes(), None).unwrap();
        }
        for tx in writers {
            tx.commit()
                .unwrap_or_else(|e| panic!("pessimistic {level:?}: {e}"));
        }
        assert_eq!(head_of(db.db(), "journal"), 4, "{level:?}");
    }
}

#[test]
fn an_append_beside_other_writes_still_validates_those_writes() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let loser = db.begin(&TxnOptions::new());
    loser.put(b"contended", b"loser").unwrap();
    loser.append(&log, b"loser", None).unwrap();
    db.db().put(b"contended", b"winner").unwrap();
    assert!(matches!(loser.commit(), Err(TransactionError::Conflict(_))));
    assert_eq!(head_of(db.db(), "journal"), 0);
    assert_eq!(db.db().get(b"contended").unwrap(), Some(b"winner".to_vec()));
}

#[test]
fn a_commit_writes_its_entry_and_its_other_writes_atomically() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let tx = db.begin(&TxnOptions::new());
    tx.put(b"doc", b"body").unwrap();
    tx.append(&log, b"doc-created", Some(b"once/doc")).unwrap();
    let receipt = tx.commit().unwrap();
    let snap = db.db().snapshot();
    assert!(snap.sequence() >= receipt.seq());
    assert_eq!(snap.get(b"doc").unwrap(), Some(b"body".to_vec()));
    assert_eq!(snap.get(b"journal-head").unwrap().map(be), Some(1));
    assert_eq!(
        snap.get(b"journal/00000000000000000001").unwrap(),
        Some(b"doc-created".to_vec())
    );
    assert_eq!(snap.get(b"once/doc").unwrap().map(be), Some(1));
}

/// Eight writers at each level: positions are dense and unique, follow the
/// order the commits took their sequences, and a reader's snapshot never
/// finds a row missing below the head it reads.
#[test]
fn eight_writers_at_every_level_number_in_commit_order_and_readers_never_skip() {
    const WRITERS: usize = 8;
    const COMMITS: usize = 25;
    for level in LEVELS {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(open(dir.path()));
        let log = journal();
        let done = Arc::new(AtomicBool::new(false));

        let reader = {
            let (db, done) = (Arc::clone(&db), Arc::clone(&done));
            thread::spawn(move || {
                let mut reads = 0u64;
                while !done.load(Ordering::Acquire) {
                    let snap: Snapshot = db.db().snapshot();
                    let head = snap.get(b"journal-head").unwrap().map_or(0, be);
                    for p in 1..=head {
                        let key = format!("journal/{p:020}");
                        assert!(
                            snap.get(key.as_bytes()).unwrap().is_some(),
                            "position {p} missing below head {head}"
                        );
                    }
                    reads += 1;
                }
                reads
            })
        };

        let handles: Vec<_> = (0..WRITERS)
            .map(|w| {
                let (db, log) = (Arc::clone(&db), Arc::clone(&log));
                thread::spawn(move || {
                    (0..COMMITS)
                        .map(|i| {
                            let tx = db.begin(&at(level));
                            let entry = format!("w{w}-{i}");
                            tx.append(&log, entry.as_bytes(), None).unwrap();
                            let receipt = tx.commit().unwrap();
                            (receipt.seq(), entry)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut committed: Vec<(u64, String)> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        done.store(true, Ordering::Release);
        reader.join().unwrap();

        committed.sort();
        let sequences: BTreeSet<u64> = committed.iter().map(|c| c.0).collect();
        assert_eq!(
            sequences.len(),
            WRITERS * COMMITS,
            "{level:?}: sequences are distinct"
        );
        let log_entries = texts(&log_of(db.db(), "journal"));
        let in_commit_order: Vec<String> = committed.into_iter().map(|c| c.1).collect();
        assert_eq!(
            log_entries, in_commit_order,
            "{level:?}: positions follow the order of the commit sequences"
        );
    }
}

#[test]
fn a_pessimistic_transaction_appends_in_the_same_step() {
    let dir = TempDir::new().unwrap();
    let db = TransactionDb::open(dir.path(), Options::default()).unwrap();
    let log = journal();
    let a = db.begin(&TxnOptions::new());
    let b = db.begin(&TxnOptions::new());
    a.put(b"a-key", b"v").unwrap();
    b.put(b"b-key", b"v").unwrap();
    b.append(&log, b"b", Some(b"once/b")).unwrap();
    a.append(&log, b"a", None).unwrap();
    b.commit().unwrap();
    a.commit().unwrap();
    assert_eq!(texts(&log_of(db.db(), "journal")), ["b", "a"]);
    assert_eq!(once_of(db.db(), b"once/b"), Some(1));
}

// ── KeyClass::Log ───────────────────────────────────────────────────────

#[test]
fn a_put_delete_or_merge_of_a_log_key_is_refused_at_defra_level() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let tx = db.begin(&at(IsolationLevel::DefraLevel));
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
    tx.put(b"ordinary", b"v").unwrap();
    tx.commit().unwrap();
    assert_eq!(db.db().get(b"journal-head").unwrap(), None);
}

#[test]
fn a_refused_write_buffers_nothing() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    let _ = tx.put(b"journal-head", b"v");
    assert_eq!(tx.get(b"journal-head").unwrap(), None);
    tx.commit().unwrap();
    assert_eq!(db.db().get(b"journal-head").unwrap(), None);
}

/// Where the classifier does not apply, a write to a log key is not refused:
/// the other levels, a pessimistic transaction, and a database with no
/// classifier. That is the caller's contract.
#[test]
fn the_refusal_applies_only_where_the_classifier_does() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    for level in [
        IsolationLevel::ReadCommitted,
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
    ] {
        let tx = db.begin(&at(level));
        tx.put(b"journal-head", b"v")
            .unwrap_or_else(|e| panic!("{level:?}: {e}"));
    }

    let dir = TempDir::new().unwrap();
    let plain = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    plain
        .begin(&at(IsolationLevel::DefraLevel))
        .put(b"journal-head", b"v")
        .unwrap();

    let dir = TempDir::new().unwrap();
    let pessimistic = TransactionDb::open(dir.path(), Options::default()).unwrap();
    pessimistic
        .begin(&at(IsolationLevel::DefraLevel))
        .put(b"journal-head", b"v")
        .unwrap();
}

/// The ways a transaction reads a key.
#[derive(Clone, Copy, Debug)]
enum Read {
    Get,
    GetSlice,
    GetForUpdate,
}

impl Read {
    fn run(self, tx: &Transaction, key: &[u8]) {
        match self {
            Read::Get => drop(tx.get(key).unwrap()),
            Read::GetSlice => drop(tx.get_slice(key).unwrap()),
            Read::GetForUpdate => drop(tx.get_for_update(key).unwrap()),
        }
    }

    /// Whether `level` validates this read of a key the transaction does not
    /// write, which is what it did before the Log class existed.
    fn validated_at(self, level: IsolationLevel) -> bool {
        match (self, level) {
            (_, IsolationLevel::ReadCommitted) => false,
            (Read::GetForUpdate, _) => true,
            (_, IsolationLevel::SnapshotIsolation) => false,
            _ => true,
        }
    }
}

/// A read of a log key is never validated at DefraLevel with a classifier,
/// and is validated as the level always did at every other level, so the
/// head key moved by a later append conflicts there.
#[test]
fn a_read_of_a_log_key_is_never_validated_at_defra_level() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    for level in LEVELS {
        for read in [Read::Get, Read::GetSlice, Read::GetForUpdate] {
            let reader = db.begin(&at(level));
            read.run(&reader, b"journal-head");
            reader.put(b"derived", b"v").unwrap();
            let appender = db.begin(&TxnOptions::new());
            appender.append(&log, b"later", None).unwrap();
            appender.commit().unwrap();
            let conflicted = !common::committed(&reader.commit());
            let expected = level != IsolationLevel::DefraLevel && read.validated_at(level);
            assert_eq!(conflicted, expected, "{level:?} {read:?}");
        }
    }
}

/// An entry key and a once key are log keys too.
#[test]
fn a_read_of_an_entry_or_a_once_key_is_never_validated_at_defra_level() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let seed = db.begin(&TxnOptions::new());
    seed.append(&log, b"seed", Some(b"once/seed")).unwrap();
    seed.commit().unwrap();
    let reader = db.begin(&at(IsolationLevel::DefraLevel));
    assert!(
        reader
            .get(b"journal/00000000000000000001")
            .unwrap()
            .is_some()
    );
    assert!(reader.get_for_update(b"once/seed").unwrap().is_some());
    reader.put(b"derived", b"v").unwrap();
    // A writer replaces both keys, as a repair tool outside the contract might.
    db.db()
        .put(b"journal/00000000000000000001", b"changed")
        .unwrap();
    db.db().put(b"once/seed", &9u64.to_be_bytes()).unwrap();
    reader.commit().unwrap();
}

// ── refusal at the call ─────────────────────────────────────────────────

/// Declares a maximum its keys exceed.
struct Understated;

impl LogLayout for Understated {
    fn head_key(&self) -> &[u8] {
        b"journal-understated"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("journal/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        8
    }
}

#[test]
fn an_append_refuses_a_layout_that_overruns_its_declared_key_length() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log: Arc<dyn LogLayout> = Arc::new(Understated);
    let tx = db.begin(&TxnOptions::new());
    let err = tx.append(&log, b"x", None).unwrap_err();
    assert!(
        matches!(err, TransactionError::Engine(Error::InvalidArgument(_))),
        "{err:?}"
    );
    // The transaction is unchanged and still commits.
    tx.put(b"k", b"v").unwrap();
    tx.commit().unwrap();
    assert_eq!(db.db().get(b"journal-understated").unwrap(), None);
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
fn with_a_classifier_an_append_refuses_keys_that_are_not_log_keys() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());

    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    let unclassified: Arc<dyn LogLayout> = Arc::new(Unclassified);
    assert!(matches!(
        tx.append(&unclassified, b"x", None),
        Err(TransactionError::Engine(Error::InvalidArgument(_)))
    ));
    // A once key outside the Log class is refused too.
    let log = journal();
    assert!(matches!(
        tx.append(&log, b"x", Some(b"ordinary-once")),
        Err(TransactionError::Engine(Error::InvalidArgument(_)))
    ));
    tx.append(&log, b"fine", Some(b"once/ok")).unwrap();
    tx.commit().unwrap();
    assert_eq!(log_of(db.db(), "journal").len(), 1);

    // Where the classifier is ignored the call is not refused for it.
    let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
    tx.append(&unclassified, b"x", None).unwrap();
    tx.commit().unwrap();
    assert_eq!(log_of(db.db(), "plain").len(), 1);
}

#[test]
fn a_commit_refuses_a_key_past_the_database_limit_and_applies_nothing() {
    let dir = TempDir::new().unwrap();
    let db = open_with(dir.path(), Options::default().max_key_size(16));
    let log = journal();
    let tx = db.begin(&TxnOptions::new());
    tx.put(b"ok", b"v").unwrap();
    tx.append(&log, b"x", None).unwrap();
    let err = tx.commit().unwrap_err();
    assert!(
        matches!(err, TransactionError::Engine(Error::InvalidArgument(_))),
        "{err:?}"
    );
    assert_eq!(db.db().get(b"ok").unwrap(), None);
    assert_eq!(head_of(db.db(), "journal"), 0);
}

#[test]
fn a_commit_refuses_an_entry_past_the_value_limit_and_applies_nothing() {
    let dir = TempDir::new().unwrap();
    let db = open_with(dir.path(), Options::default().max_value_size(8));
    let log = journal();
    let tx = db.begin(&TxnOptions::new());
    tx.append(&log, &[0u8; 9], None).unwrap();
    assert!(matches!(
        tx.commit(),
        Err(TransactionError::Engine(Error::InvalidArgument(_)))
    ));
    assert_eq!(head_of(db.db(), "journal"), 0);
}

#[test]
fn a_head_that_is_not_a_position_fails_the_commit_and_nothing_is_applied() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    db.db().put(b"journal-head", b"not-a-number").unwrap();
    let log = journal();
    let tx = db.begin(&TxnOptions::new());
    tx.put(b"ok", b"v").unwrap();
    tx.append(&log, b"x", None).unwrap();
    let err = tx.commit().unwrap_err();
    assert!(
        matches!(err, TransactionError::Engine(Error::InvalidArgument(_))),
        "{err:?}"
    );
    assert!(
        !err.to_string().contains("not-a-number"),
        "no key or value bytes in the message: {err}"
    );
    assert_eq!(db.db().get(b"ok").unwrap(), None);
}

// ── savepoints ──────────────────────────────────────────────────────────

#[test]
fn a_savepoint_keeps_the_appends_before_it_and_drops_those_after() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let mut tx = db.begin(&TxnOptions::new());
    tx.append(&log, b"before", None).unwrap();
    tx.set_savepoint();
    tx.append(&log, b"after-1", Some(b"once/x")).unwrap();
    tx.set_savepoint();
    tx.append(&log, b"after-2", None).unwrap();
    tx.rollback_to_savepoint().unwrap();
    tx.append(&log, b"replacement", None).unwrap();
    tx.rollback_to_savepoint().unwrap();
    tx.append(&log, b"final", None).unwrap();
    tx.commit().unwrap();
    assert_eq!(texts(&log_of(db.db(), "journal")), ["before", "final"]);
    assert_eq!(once_of(db.db(), b"once/x"), None);
}

#[test]
fn rolling_back_to_a_savepoint_with_no_appends_clears_later_ones() {
    let dir = TempDir::new().unwrap();
    let db = open(dir.path());
    let log = journal();
    let mut tx = db.begin(&TxnOptions::new());
    tx.set_savepoint();
    tx.append(&log, b"gone", None).unwrap();
    tx.rollback_to_savepoint().unwrap();
    tx.put(b"k", b"v").unwrap();
    tx.commit().unwrap();
    assert_eq!(head_of(db.db(), "journal"), 0);
}

// ── reopen ──────────────────────────────────────────────────────────────

fn fill(db: &OptimisticTransactionDb, from: u64, to: u64) {
    let log = journal();
    for i in from..to {
        let tx = db.begin(&TxnOptions::new());
        tx.append(
            &log,
            format!("e{i}").as_bytes(),
            Some(format!("once/{i}").as_bytes()),
        )
        .unwrap();
        tx.commit().unwrap();
    }
}

fn check_log(db: &Db, entries: u64) {
    assert_eq!(head_of(db, "journal"), entries);
    assert_eq!(
        texts(&log_of(db, "journal")),
        (0..entries).map(|i| format!("e{i}")).collect::<Vec<_>>()
    );
    for i in 0..entries {
        assert_eq!(once_of(db, format!("once/{i}").as_bytes()), Some(i + 1));
    }
}

/// The log continues across a reopen, by WAL replay and from a flushed table,
/// on a filesystem at both durability modes and in memory (which cannot sync,
/// so only at Eventual).
#[test]
fn the_log_continues_after_a_reopen_at_both_durabilities() {
    let env = MemEnv::new();
    let path = Path::new("/log-db");
    let options = || mem_options(&env, DurabilityMode::Eventual);
    {
        let db = open_with(path, options());
        fill(&db, 0, 5);
    }
    {
        let db = open_with(path, options());
        check_log(db.db(), 5);
        fill(&db, 5, 8);
        db.db().flush().unwrap();
        fill(&db, 8, 10);
    }
    let db = open_with(path, options());
    check_log(db.db(), 10);
    fill(&db, 10, 11);
    check_log(db.db(), 11);

    for durability in [DurabilityMode::Immediate, DurabilityMode::Eventual] {
        let dir = TempDir::new().unwrap();
        let options = || Options::default().durability(durability);
        {
            let db = open_with(dir.path(), options());
            fill(&db, 0, 6);
            db.db().close().unwrap();
        }
        {
            let db = open_with(dir.path(), options());
            check_log(db.db(), 6);
            fill(&db, 6, 8);
            db.db().flush().unwrap();
            fill(&db, 8, 9);
        }
        let db = open_with(dir.path(), options());
        check_log(db.db(), 9);
    }
}

#[test]
fn a_once_key_survives_a_reopen_and_still_appends_nothing() {
    let env = MemEnv::new();
    let path = Path::new("/once-db");
    let log = journal();
    for entry in [&b"first"[..], b"again"] {
        let db = open_with(path, mem_options(&env, DurabilityMode::Eventual));
        let tx = db.begin(&TxnOptions::new());
        tx.append(&log, entry, Some(b"once/k")).unwrap();
        tx.commit().unwrap();
    }
    let db = open_with(path, mem_options(&env, DurabilityMode::Eventual));
    assert_eq!(texts(&log_of(db.db(), "journal")), ["first"]);
    assert_eq!(once_of(db.db(), b"once/k"), Some(1));
}

// ── a panic in the layout ───────────────────────────────────────────────

/// Builds its one probe key at the largest position and panics for any other.
struct Panicky;

impl LogLayout for Panicky {
    fn head_key(&self) -> &[u8] {
        b"journal-panicky"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        assert_eq!(
            position,
            u64::MAX,
            "the layout panics when a commit numbers"
        );
        out.extend_from_slice(b"journal/probe");
    }

    fn max_entry_key_len(&self) -> usize {
        64
    }
}

/// The layout is the caller's code, run in the ordered step: a panic there is
/// caught, fails the commit with the trait named, and latches the database
/// read-only until it is reopened. Nothing of the commit is applied.
#[test]
fn a_layout_that_panics_in_a_commit_latches_the_database_read_only() {
    let dir = TempDir::new().unwrap();
    {
        let db = open(dir.path());
        let log: Arc<dyn LogLayout> = Arc::new(Panicky);
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"ok", b"v").unwrap();
        tx.append(&log, b"x", None).unwrap();
        let err = tx.commit().unwrap_err();
        assert!(
            matches!(
                err,
                TransactionError::Engine(Error::CallbackPanicked {
                    callback: "LogLayout",
                    latched: true
                })
            ),
            "{err:?}"
        );
        assert_eq!(db.db().get(b"ok").unwrap(), None);
        assert!(matches!(
            db.db().put(b"later", b"v"),
            Err(Error::CallbackPanicked { .. })
        ));
    }
    let db = open(dir.path());
    db.db().put(b"later", b"v").unwrap();
    assert_eq!(db.db().get(b"ok").unwrap(), None);
}
