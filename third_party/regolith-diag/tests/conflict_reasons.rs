//! Why a commit lost: the key, what the transaction did with it, and what the
//! newer write was.
//!
//! Each case begins a transaction, lets a write land around it, and commits.
//! The table of pairs a conflict can name is walked in full: every access with
//! every kind of newer write that can reach it, and the pairs that cannot occur
//! are pinned as commits. The message never prints the key, and the listener
//! hears each conflict once, after the commit released its locks.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::{self, ThreadId};
use std::time::Duration;

use regolith::{
    Access, CommitReceipt, Conflict, EventListener, IsolationLevel, KeyClass, KeyClassifier,
    MergeOperator, OptimisticTransactionDb, Options, Transaction, TransactionDb, TransactionError,
    TxResult, TxnOptions, WriteKind,
};

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

/// Keys under `b/` are blocks named by their content.
struct Blocks;

impl KeyClassifier for Blocks {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"b/") {
            KeyClass::ContentAddressed
        } else {
            KeyClass::Ordinary
        }
    }
}

fn options() -> Options {
    Options::default().merge_operator(Some(Arc::new(CounterMerge)))
}

fn open(dir: &std::path::Path) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir, options())
        .unwrap()
        .with_policy(Arc::new(Blocks))
}

fn zero() -> [u8; 8] {
    0i64.to_be_bytes()
}

fn at(level: IsolationLevel) -> TxnOptions {
    TxnOptions::new().isolation(level)
}

/// A write that lands around a transaction after it began.
#[derive(Clone, Copy, Debug)]
enum Newer {
    Put,
    Delete,
    RangeDelete,
    Merge,
}

const NEWER: [Newer; 4] = [Newer::Put, Newer::Delete, Newer::RangeDelete, Newer::Merge];

impl Newer {
    fn apply(self, db: &OptimisticTransactionDb, key: &[u8]) {
        let end: Vec<u8> = [key, &[0]].concat();
        match self {
            Self::Put => db.db().put(key, b"newer").unwrap(),
            Self::Delete => db.db().delete(key).unwrap(),
            Self::RangeDelete => db.db().delete_range(key, &end).unwrap(),
            Self::Merge => db.db().merge(key, &1i64.to_be_bytes()).unwrap(),
        }
    }

    fn kind(self) -> WriteKind {
        match self {
            Self::Put => WriteKind::Put,
            Self::Delete => WriteKind::Delete,
            Self::RangeDelete => WriteKind::RangeDelete,
            Self::Merge => WriteKind::Merge,
        }
    }
}

/// The reason a commit lost, or a panic naming `what` when it won or failed
/// for another reason.
fn lost<T: std::fmt::Debug>(what: &str, result: TxResult<T>) -> Conflict {
    match result {
        Err(TransactionError::Conflict(conflict)) => conflict,
        other => panic!("{what}: expected a conflict, got {other:?}"),
    }
}

/// A fresh database whose key `k` holds a counter.
fn seeded() -> (tempfile::TempDir, OptimisticTransactionDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", &zero()).unwrap();
    (dir, db)
}

fn assert_reason(what: &str, conflict: &Conflict, mine: Access, theirs: WriteKind) {
    assert_eq!(conflict.key(), b"k", "{what}");
    assert_eq!(
        (conflict.mine(), conflict.theirs()),
        (mine, theirs),
        "{what}: {conflict}"
    );
    assert!(
        conflict.observed_seq() < conflict.latest_seq(),
        "{what}: the newer write is newer than what the transaction observed"
    );
}

#[test]
fn a_read_names_every_kind_of_write_that_overtakes_it() {
    for newer in NEWER {
        let (_dir, db) = seeded();
        let tx = db.begin(&at(IsolationLevel::RepeatableRead));
        tx.get(b"k").unwrap();
        newer.apply(&db, b"k");
        tx.put(b"elsewhere", b"x").unwrap();
        let what = format!("read then {newer:?}");
        let conflict = lost(&what, tx.commit());
        assert_reason(&what, &conflict, Access::Read, newer.kind());
    }
}

#[test]
fn a_read_for_update_names_every_kind_of_write_that_overtakes_it() {
    for newer in NEWER {
        let (_dir, db) = seeded();
        let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
        tx.get_for_update(b"k").unwrap();
        newer.apply(&db, b"k");
        tx.put(b"elsewhere", b"x").unwrap();
        let what = format!("read for update then {newer:?}");
        let conflict = lost(&what, tx.commit());
        assert_reason(&what, &conflict, Access::ReadForUpdate, newer.kind());
    }
}

#[test]
fn a_blind_put_names_every_kind_of_write_that_overtakes_it() {
    for newer in NEWER {
        let (_dir, db) = seeded();
        let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
        tx.put(b"k", b"mine").unwrap();
        newer.apply(&db, b"k");
        let what = format!("put then {newer:?}");
        let conflict = lost(&what, tx.commit());
        assert_reason(&what, &conflict, Access::Put, newer.kind());
    }
}

#[test]
fn a_blind_delete_is_overtaken_by_a_put_or_an_operand_and_elides_a_delete() {
    for newer in [Newer::Put, Newer::Merge] {
        let (_dir, db) = seeded();
        let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
        tx.delete(b"k").unwrap();
        newer.apply(&db, b"k");
        let what = format!("delete then {newer:?}");
        let conflict = lost(&what, tx.commit());
        assert_reason(&what, &conflict, Access::Delete, newer.kind());
    }
    // The key is already gone when the delete commits, so it leaves what the
    // key already holds: elided, and no conflict to name.
    for newer in [Newer::Delete, Newer::RangeDelete] {
        let (_dir, db) = seeded();
        let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
        tx.delete(b"k").unwrap();
        newer.apply(&db, b"k");
        tx.commit()
            .unwrap_or_else(|e| panic!("delete then {newer:?}: {e}"));
    }
}

#[test]
fn a_merge_names_every_kind_of_write_that_overtakes_it() {
    for newer in NEWER {
        let (_dir, db) = seeded();
        let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
        tx.merge(b"k", &1i64.to_be_bytes()).unwrap();
        newer.apply(&db, b"k");
        let what = format!("merge then {newer:?}");
        let conflict = lost(&what, tx.commit());
        assert_reason(&what, &conflict, Access::Merge, newer.kind());
    }
}

#[test]
fn a_blind_merge_conflicts_with_a_replacement_and_commutes_with_an_operand() {
    for newer in [Newer::Put, Newer::Delete, Newer::RangeDelete] {
        let (_dir, db) = seeded();
        let tx = db.begin(&at(IsolationLevel::DefraLevel));
        tx.merge(b"k", &1i64.to_be_bytes()).unwrap();
        newer.apply(&db, b"k");
        let what = format!("blind merge then {newer:?}");
        let conflict = lost(&what, tx.commit());
        assert_reason(&what, &conflict, Access::Merge, newer.kind());
    }
    let (_dir, db) = seeded();
    let tx = db.begin(&at(IsolationLevel::DefraLevel));
    tx.merge(b"k", &1i64.to_be_bytes()).unwrap();
    Newer::Merge.apply(&db, b"k");
    tx.commit()
        .expect("a newer operand does not stop a blind merge");
}

#[test]
fn a_blind_merge_names_the_replacement_beneath_the_operands_on_top_of_it() {
    for (replacement, kind) in [
        (Newer::Put, WriteKind::Put),
        (Newer::Delete, WriteKind::Delete),
        (Newer::RangeDelete, WriteKind::RangeDelete),
    ] {
        for flush in [false, true] {
            let (_dir, db) = seeded();
            let tx = db.begin(&at(IsolationLevel::DefraLevel));
            tx.merge(b"k", &1i64.to_be_bytes()).unwrap();
            replacement.apply(&db, b"k");
            let replaced_at = db.db().latest_sequence();
            Newer::Merge.apply(&db, b"k");
            Newer::Merge.apply(&db, b"k");
            if flush {
                db.db().flush().unwrap();
            }
            let what = format!("blind merge then {replacement:?} and two operands, flush={flush}");
            let conflict = lost(&what, tx.commit());
            assert_reason(&what, &conflict, Access::Merge, kind);
            assert_eq!(
                conflict.latest_seq(),
                replaced_at,
                "{what}: the sequence is the replacement's, not an operand's"
            );
        }
    }
}

#[test]
fn a_write_inside_a_scanned_stretch_names_every_kind_of_write_that_overtakes_it() {
    for newer in NEWER {
        for write in ["put", "delete", "merge"] {
            let (_dir, db) = seeded();
            db.db().put(b"a", &zero()).unwrap();
            db.db().put(b"z", &zero()).unwrap();
            let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
            assert_eq!(tx.scan_stream(None, None).count(), 3);
            match write {
                "put" => tx.put(b"k", b"mine").unwrap(),
                "delete" => tx.delete(b"k").unwrap(),
                _ => tx.merge(b"k", &1i64.to_be_bytes()).unwrap(),
            }
            newer.apply(&db, b"k");
            let what = format!("scan, {write}, then {newer:?}");
            let conflict = lost(&what, tx.commit());
            assert_reason(&what, &conflict, Access::ScannedThenWrote, newer.kind());
        }
    }
}

#[test]
fn a_presence_check_conflicts_only_with_a_write_that_removes_the_key() {
    let block = b"b/content";
    for newer in NEWER {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path());
        db.db().put(block, &zero()).unwrap();
        let tx = db.begin(&at(IsolationLevel::DefraLevel));
        assert!(tx.get(block).unwrap().is_some());
        newer.apply(&db, block);
        tx.put(b"elsewhere", b"x").unwrap();
        let what = format!("presence check then {newer:?}");
        match newer {
            Newer::Delete | Newer::RangeDelete => {
                let conflict = lost(&what, tx.commit());
                assert_eq!(conflict.key(), block, "{what}");
                assert_eq!(
                    (conflict.mine(), conflict.theirs()),
                    (Access::ReadPresence, newer.kind()),
                    "{what}"
                );
            }
            Newer::Put | Newer::Merge => {
                tx.commit().unwrap_or_else(|e| panic!("{what}: {e}"));
            }
        }
    }
}

#[test]
fn a_pessimistic_commit_names_the_write_that_bypassed_the_lock_manager() {
    let dir = tempfile::tempdir().unwrap();
    let db = TransactionDb::open(dir.path(), options()).unwrap();
    db.db().put(b"k", &zero()).unwrap();

    let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
    tx.get_for_update(b"k").unwrap();
    db.db().put(b"k", b"racer").unwrap();
    tx.put(b"k", b"mine").unwrap();
    let conflict = lost("pessimistic get_for_update", tx.commit());
    assert_reason(
        "pessimistic get_for_update",
        &conflict,
        Access::ReadForUpdate,
        WriteKind::Put,
    );

    let tx = db.begin(&at(IsolationLevel::RepeatableRead));
    tx.get(b"k").unwrap();
    db.db().delete(b"k").unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    let conflict = lost("pessimistic get", tx.commit());
    assert_reason(
        "pessimistic get",
        &conflict,
        Access::Read,
        WriteKind::Delete,
    );
}

#[test]
fn neither_the_error_nor_the_conflict_prints_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let key = b"SECRET-user-key-4711";
    db.db().put(key, b"v").unwrap();
    let tx = db.begin(&at(IsolationLevel::RepeatableRead));
    tx.get(key).unwrap();
    db.db().put(key, b"w").unwrap();
    tx.put(b"elsewhere", b"x").unwrap();
    let err = tx.commit().unwrap_err();
    let TransactionError::Conflict(ref conflict) = err else {
        panic!("expected a conflict, got {err:?}");
    };

    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let bytes = format!("{:?}", key.to_vec());
    let messages = [err.to_string(), conflict.to_string()];
    let debugs = [
        format!("{err:?}"),
        format!("{err:#?}"),
        format!("{conflict:?}"),
    ];
    for text in messages.iter().chain(&debugs) {
        for leaked in ["SECRET", "secret", "user-key", hex.as_str(), bytes.as_str()] {
            assert!(!text.contains(leaked), "{leaked:?} leaked into: {text}");
        }
    }
    for text in &messages {
        assert!(text.contains("20-byte key"), "{text}");
    }
    for text in &debugs {
        assert!(text.contains("key_len: 20"), "{text}");
    }
    assert_eq!(conflict.key(), key, "the bytes stay reachable on purpose");
}

#[test]
fn a_busy_lock_names_its_key_by_length_and_hash_and_never_by_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let db = TransactionDb::open(dir.path(), options())
        .unwrap()
        .with_lock_timeout(Duration::from_millis(20));
    let key = b"SECRET-user-key-bytes";
    let holder = db.begin(&TxnOptions::new());
    holder.put(key, b"v").unwrap();
    let waiter = db.begin(&TxnOptions::new());
    let err = waiter.put(key, b"w").unwrap_err();
    let TransactionError::Busy(ref reported) = err else {
        panic!("expected a busy lock, got {err:?}");
    };
    assert_eq!(
        reported.as_slice(),
        key,
        "the bytes stay reachable on purpose"
    );

    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let bytes = format!("{:?}", key.to_vec());
    let texts = [err.to_string(), format!("{err:?}"), format!("{err:#?}")];
    for text in &texts {
        for leaked in ["SECRET", "user-key", hex.as_str(), bytes.as_str()] {
            assert!(!text.contains(leaked), "{leaked:?} leaked into: {text}");
        }
        assert!(text.contains("21-byte key (hash "), "{text}");
    }
    // The same key shows the same hash, a different one does not.
    let other = db.begin(&TxnOptions::new());
    other.put(b"another", b"v").unwrap();
    let again = db
        .begin(&TxnOptions::new())
        .put(key, b"x")
        .unwrap_err()
        .to_string();
    assert_eq!(again, texts[0]);
    let elsewhere = db
        .begin(&TxnOptions::new())
        .put(b"another", b"v")
        .unwrap_err()
        .to_string();
    assert_ne!(elsewhere, texts[0]);
    holder.rollback();
}

/// A merge the operator declines is reported with its key as data. Neither
/// the message nor the `Debug` output prints the key's bytes, from a plain
/// read or from a transaction's.
#[test]
fn a_failed_merge_names_its_key_by_length_and_hash_and_never_by_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    let key = b"SECRET-user-key-merge";
    let declined = b"not eight bytes";
    db.db().put(key, &zero()).unwrap();
    db.db().merge(key, declined).unwrap();

    let plain = db.db().get(key).unwrap_err();
    let regolith::Error::MergeFailed(ref reported) = plain else {
        panic!("expected a failed merge, got {plain:?}");
    };
    assert_eq!(
        reported.as_slice(),
        key,
        "the bytes stay reachable on purpose"
    );
    let in_txn = db.begin(&TxnOptions::new()).get(key).unwrap_err();
    assert!(matches!(
        &in_txn,
        TransactionError::Engine(regolith::Error::MergeFailed(reported)) if reported == key
    ));

    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
    let bytes = format!("{:?}", key.to_vec());
    // The first bytes as a list prints them, on one line or one per line.
    let listed = "83,69,67,82,69,84";
    let texts = [
        plain.to_string(),
        format!("{plain:?}"),
        format!("{plain:#?}"),
        in_txn.to_string(),
        format!("{in_txn:?}"),
        format!("{in_txn:#?}"),
    ];
    for text in &texts {
        for leaked in ["SECRET", "user-key", hex.as_str(), bytes.as_str()] {
            assert!(!text.contains(leaked), "{leaked:?} leaked into: {text}");
        }
        let squeezed: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(!squeezed.contains(listed), "the bytes leaked into: {text}");
        assert!(text.contains("21-byte key (hash "), "{text}");
    }
    // The same key shows the same hash, a different one does not.
    assert_eq!(db.db().get(key).unwrap_err().to_string(), texts[0]);
    db.db().merge(b"another", declined).unwrap();
    assert_ne!(db.db().get(b"another").unwrap_err().to_string(), texts[0]);
}

/// Runs `work` on another thread, and says whether it finished in time. Work
/// that waits for something the calling thread still holds never does.
fn finishes(work: impl FnOnce() + Send + 'static) -> bool {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        work();
        let _ = done.send(());
    });
    finished.recv_timeout(Duration::from_secs(10)).is_ok()
}

/// Records every conflict it hears, and, from inside the callback, whether
/// another thread could still do what the committing thread might hold up.
struct Recorder {
    probe: OnceLock<Box<dyn Fn() -> bool + Send + Sync>>,
    heard: Mutex<Vec<(ThreadId, Conflict)>>,
    probed: Mutex<Vec<bool>>,
}

impl Recorder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            probe: OnceLock::new(),
            heard: Mutex::new(Vec::new()),
            probed: Mutex::new(Vec::new()),
        })
    }

    /// An optimistic database whose probe is a write from another thread,
    /// which needs the commit pipeline the committing thread must have left.
    fn open(self: &Arc<Self>, dir: &std::path::Path) -> Arc<OptimisticTransactionDb> {
        let listener: Arc<dyn EventListener> = self.clone();
        let db = Arc::new(
            OptimisticTransactionDb::open(dir, options().listeners(vec![listener])).unwrap(),
        );
        let weak: Weak<OptimisticTransactionDb> = Arc::downgrade(&db);
        let probe = move || {
            let db = weak.upgrade().unwrap();
            finishes(move || db.db().put(b"probe", b"x").unwrap())
        };
        assert!(self.probe.set(Box::new(probe)).is_ok());
        db
    }

    /// A pessimistic database whose probe is a lock on `k` taken by another
    /// thread, which needs the key lock the committing transaction must have
    /// released.
    fn open_pessimistic(self: &Arc<Self>, dir: &std::path::Path) -> Arc<TransactionDb> {
        let listener: Arc<dyn EventListener> = self.clone();
        let db = Arc::new(TransactionDb::open(dir, options().listeners(vec![listener])).unwrap());
        let weak: Weak<TransactionDb> = Arc::downgrade(&db);
        let probe = move || {
            let db = weak.upgrade().unwrap();
            finishes(move || {
                db.begin(&TxnOptions::new()).get_for_update(b"k").unwrap();
            })
        };
        assert!(self.probe.set(Box::new(probe)).is_ok());
        db
    }

    fn count(&self) -> usize {
        self.heard.lock().unwrap().len()
    }
}

impl EventListener for Recorder {
    fn on_conflict(&self, conflict: &Conflict) {
        self.heard
            .lock()
            .unwrap()
            .push((thread::current().id(), conflict.clone()));
        let probe = self.probe.get().expect("the probe is set after open");
        self.probed.lock().unwrap().push(probe());
    }
}

#[test]
fn a_listener_hears_each_conflict_once_outside_the_commit_mutex() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Recorder::new();
    let db = recorder.open(dir.path());
    db.db().put(b"k", &zero()).unwrap();

    let clean = db.begin(&at(IsolationLevel::SnapshotIsolation));
    clean.put(b"elsewhere", b"x").unwrap();
    clean.commit().unwrap();
    assert_eq!(recorder.count(), 0, "a commit that wins is not a conflict");

    let tx = db.begin(&at(IsolationLevel::RepeatableRead));
    tx.get(b"k").unwrap();
    db.db().delete(b"k").unwrap();
    tx.put(b"elsewhere", b"y").unwrap();
    let returned = lost("the first conflict", tx.commit());
    assert_eq!(recorder.count(), 1);
    {
        let heard = recorder.heard.lock().unwrap();
        let (thread, conflict) = &heard[0];
        assert_eq!(*thread, thread::current().id(), "the committing thread");
        assert_eq!(
            conflict, &returned,
            "the listener hears what the caller gets"
        );
        assert_eq!(
            (conflict.mine(), conflict.theirs()),
            (Access::Read, WriteKind::Delete)
        );
    }

    let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
    tx.put(b"k", b"mine").unwrap();
    db.db().put(b"k", b"theirs").unwrap();
    lost("the second conflict", tx.commit());
    assert_eq!(recorder.count(), 2, "one call per conflict");
    assert_eq!(
        *recorder.probed.lock().unwrap(),
        [true, true],
        "another thread committed from inside the callback"
    );
}

#[test]
fn a_pessimistic_listener_runs_after_the_key_locks_are_released() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Recorder::new();
    let db = recorder.open_pessimistic(dir.path());
    db.db().put(b"k", &zero()).unwrap();

    let tx = db.begin(&at(IsolationLevel::SnapshotIsolation));
    tx.get_for_update(b"k").unwrap();
    db.db().put(b"k", b"racer").unwrap();
    tx.put(b"k", b"mine").unwrap();
    lost("pessimistic conflict", tx.commit());
    assert_eq!(recorder.count(), 1);
    assert_eq!(
        *recorder.probed.lock().unwrap(),
        [true],
        "another thread locked the key from inside the callback"
    );
}

/// A commit that returned a receipt for `tx`, which wrote `keys` keys.
fn commit_writing(tx: Transaction, keys: usize) -> CommitReceipt {
    for i in 0..keys {
        tx.put(format!("w{i}").as_bytes(), b"v").unwrap();
    }
    tx.commit().unwrap()
}

#[test]
fn the_receipt_is_the_sequence_the_writes_became_visible_at_and_orders_commits() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());

    let first = commit_writing(db.begin(&TxnOptions::new()), 3);
    assert_eq!(
        first.seq(),
        db.db().latest_sequence(),
        "a snapshot taken at the receipt's sequence sees all three writes"
    );
    let second = commit_writing(db.begin(&TxnOptions::new()), 1);
    assert_eq!(second.seq(), db.db().latest_sequence());
    assert!(first.seq() < second.seq());

    let dir = tempfile::tempdir().unwrap();
    let pessimistic = TransactionDb::open(dir.path(), options()).unwrap();
    let first = commit_writing(pessimistic.begin(&TxnOptions::new()), 2);
    assert_eq!(first.seq(), pessimistic.db().latest_sequence());
    let second = commit_writing(pessimistic.begin(&TxnOptions::new()), 2);
    assert!(first.seq() < second.seq());
    assert_eq!(second.seq(), pessimistic.db().latest_sequence());
}

#[test]
fn a_commit_with_no_writes_returns_its_snapshot_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let db = open(dir.path());
    db.db().put(b"k", b"1").unwrap();
    let snapshot = db.db().latest_sequence();

    let tx = db.begin(&TxnOptions::new());
    tx.get(b"k").unwrap();
    db.db().put(b"k", b"2").unwrap();
    db.db().put(b"other", b"3").unwrap();
    let receipt = tx.commit().unwrap();
    assert_eq!(receipt.seq(), snapshot);
    assert!(receipt.seq() < db.db().latest_sequence());

    let dir = tempfile::tempdir().unwrap();
    let pessimistic = TransactionDb::open(dir.path(), options()).unwrap();
    pessimistic.db().put(b"k", b"1").unwrap();
    let snapshot = pessimistic.db().latest_sequence();
    let tx = pessimistic.begin(&TxnOptions::new());
    pessimistic.db().put(b"k", b"2").unwrap();
    assert_eq!(tx.commit().unwrap().seq(), snapshot);
}
