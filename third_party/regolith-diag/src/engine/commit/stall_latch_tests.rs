//! Which latch a write is told at a stall, and through which call (E35).
//!
//! Every way of latching the engine goes through one function, and it lands
//! the stall's unit, so a write that waits is told whatever latched it: a
//! log left in an unknown state (`stall_failure_tests.rs`) or, here, a
//! caller's code that panicked inside a commit. A transaction's classifier
//! runs at its commit before the stall admission, so a commit reaches it on
//! a stopped database, and its panic latches the engine under the writes
//! already waiting.
//!
//! A write that arrives after the latch meets it at that same admission. The
//! check each call makes first looks at the handle alone (closed,
//! read-only), so every call that writes is run against a database that is
//! latched and stopped, and each is told the latch: none is handed a wait
//! for a stop that will not clear.

use std::sync::{Arc, mpsc};
use std::time::Instant;

use super::failure_tests::{DEADLINE, fresh_write_error, latch_through_a_flush, stopped_write};
use crate::{
    ColumnFamilyHandle, Db, Error, IsolationLevel, KeyClass, KeyClassifier, MergeOperator,
    OptimisticTransactionDb, Options, TransactionError, TxnOptions, WouldBlock, WriteBatch,
};

/// Panics on a key that starts with `boom`.
struct PanicsOnBoom;

impl KeyClassifier for PanicsOnBoom {
    fn classify(&self, key: &[u8]) -> KeyClass {
        assert!(!key.starts_with(b"boom"), "a test's classifier panics");
        KeyClass::Ordinary
    }
}

/// Concatenates: `merge` is refused before the stall without an operator.
struct Append;

impl MergeOperator for Append {
    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        Some([base.unwrap_or_default(), &operands.concat()].concat())
    }

    fn name(&self) -> &'static str {
        "append"
    }
}

/// A worker, a second column family, one L0 table at a stop of one, and a
/// compaction trigger the worker never reaches: stopped writes wait for a
/// relief that does not come.
fn stopped_database() -> (
    tempfile::TempDir,
    OptimisticTransactionDb,
    ColumnFamilyHandle,
) {
    let dir = tempfile::TempDir::new().unwrap();
    let options = Options::default()
        .level0_slowdown_writes_trigger(1)
        .level0_stop_writes_trigger(1)
        .l0_compaction_trigger(100)
        .merge_operator(Some(Arc::new(Append)));
    let db = OptimisticTransactionDb::open(dir.path(), options)
        .unwrap()
        .with_policy(Arc::new(PanicsOnBoom));
    let other = db.db().create_column_family("other").unwrap();
    db.db().put(b"k", b"v").unwrap();
    db.db().flush().unwrap();
    assert!(matches!(
        db.db().engine().stall_signal().classify(),
        Some((_, true))
    ));
    (dir, db, other)
}

fn is_the_panic(told: &Error) -> bool {
    matches!(
        told,
        Error::CallbackPanicked {
            callback: "KeyClassifier",
            latched: true
        }
    )
}

/// Latch the engine as a commit whose classifier panics does. The commit
/// gets to its classifier on a stopped database: it classifies before it is
/// admitted.
fn latch_through_a_classifier(db: &OptimisticTransactionDb) {
    let tx = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
    // A read is classified at the commit, and only a commit that writes has
    // its reads checked.
    assert_eq!(tx.get(b"boom").unwrap(), None);
    tx.put(b"beside", b"lost").unwrap();
    match tx.commit() {
        Err(TransactionError::Engine(told)) if is_the_panic(&told) => {}
        Err(other) => panic!("expected the classifier's panic, got {other:?}"),
        Ok(_) => panic!("the classifier panicked and its commit went through"),
    }
}

/// With a worker: a write waits for the worker to relieve the stop, a
/// commit's classifier panics and latches the engine, and the write is told
/// at once. Run again, it gets the latch a fresh write gets.
#[test]
fn a_callback_panic_tells_the_write_waiting_for_the_worker() {
    let (_dir, db, _other) = stopped_database();
    let engine = db.db().engine();
    let wait = stopped_write(engine);
    assert!(!wait.is_ready());
    latch_through_a_classifier(&db);
    assert!(
        wait.is_ready(),
        "a callback's panic latched the engine and the stopped write was not told"
    );
    let told = engine
        .admit_write(false)
        .expect_err("a latched engine admits no write");
    assert!(is_the_panic(&told), "{told:?}");
    assert_eq!(told.to_string(), fresh_write_error(engine));
}

/// The blocking form, through the public calls: a thread waits out the
/// stall, a commit's classifier panics, and the thread's write returns that
/// latch instead of waiting until the database closes.
#[test]
fn a_thread_waiting_out_a_stall_returns_the_callback_panic() {
    let (_dir, db, _other) = stopped_database();
    let plain = db.db();
    let (told, when_told) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let outcome = crate::through_stalls(|| plain.put(b"stopped", b"v"));
            told.send(outcome).unwrap();
        });
        // The write is stopped once the stall has its unit.
        let deadline = Instant::now() + DEADLINE;
        while !plain.engine().stall_signal().has_unit() {
            assert!(Instant::now() < deadline, "the write was never stopped");
            std::thread::yield_now();
        }
        latch_through_a_classifier(&db);
        let outcome = when_told.recv_timeout(DEADLINE);
        if outcome.is_err() {
            // Closing completes every wait, so the thread ends and this
            // test fails instead of hanging with it.
            let _ = plain.close();
        }
        let told = outcome
            .expect("the stopped write was still waiting after a callback's panic latched")
            .expect_err("a latched engine takes no write");
        assert!(is_the_panic(&told), "{told:?}");
        assert_eq!(
            told.to_string(),
            plain.put(b"fresh", b"v").unwrap_err().to_string()
        );
    });
}

/// A call that writes, under the name a failure gives it.
type Write<'a> = (&'static str, Box<dyn Fn() -> crate::Result<()> + 'a>);

/// Every call that is admitted at the stall on its own: the default column
/// family's, a named one's, and the two that change the families.
fn every_write<'a>(db: &'a Db, cf: &'a ColumnFamilyHandle) -> Vec<Write<'a>> {
    vec![
        ("put", Box::new(|| db.put(b"a", b"v"))),
        ("delete", Box::new(|| db.delete(b"a"))),
        ("merge", Box::new(|| db.merge(b"a", b"v"))),
        ("delete_range", Box::new(|| db.delete_range(b"a", b"b"))),
        (
            "write",
            Box::new(|| {
                let mut batch = WriteBatch::new();
                batch.put(b"a", b"v");
                db.write(batch)
            }),
        ),
        ("put_cf", Box::new(|| db.put_cf(cf, b"a", b"v"))),
        ("delete_cf", Box::new(|| db.delete_cf(cf, b"a"))),
        ("merge_cf", Box::new(|| db.merge_cf(cf, b"a", b"v"))),
        (
            "delete_range_cf",
            Box::new(|| db.delete_range_cf(cf, b"a", b"b")),
        ),
        (
            "create_column_family",
            Box::new(|| db.create_column_family("new").map(|_| ())),
        ),
        (
            "drop_column_family",
            Box::new(|| db.drop_column_family(cf.clone())),
        ),
    ]
}

/// Setup: the stop holds every call, so each reaches the admission the
/// latch is told at.
fn assert_every_write_is_handed_a_wait(db: &Db, cf: &ColumnFamilyHandle) {
    for (call, write) in every_write(db, cf) {
        let stopped = write();
        assert!(
            matches!(stopped, Err(Error::WouldBlock(WouldBlock::Stall(_)))),
            "setup: the stop hands {call} a wait, got {stopped:?}"
        );
    }
}

/// Every call is told what a fresh write below the admission is told, which
/// is returned.
fn what_every_write_is_told(db: &Db, cf: &ColumnFamilyHandle) -> String {
    let fresh = fresh_write_error(db.engine());
    for (call, write) in every_write(db, cf) {
        let told = write().expect_err("a latched engine takes no write");
        assert_eq!(told.to_string(), fresh, "{call} was not told the latch");
    }
    fresh
}

/// Latched by a log that could not be synced, and stopped: each call that
/// writes is told the latch.
#[test]
fn every_write_that_arrives_stopped_is_told_the_log_that_latched_the_engine() {
    let (dir, db, other) = stopped_database();
    assert_every_write_is_handed_a_wait(db.db(), &other);
    let _fault = latch_through_a_flush(&dir, db.db().engine());
    let told = what_every_write_is_told(db.db(), &other);
    assert!(told.contains("unknown state"), "{told}");
}

/// Latched by a callback that panicked in a commit, and stopped: the same.
#[test]
fn every_write_that_arrives_stopped_is_told_the_callback_panic_that_latched_the_engine() {
    let (_dir, db, other) = stopped_database();
    assert_every_write_is_handed_a_wait(db.db(), &other);
    latch_through_a_classifier(&db);
    let told = what_every_write_is_told(db.db(), &other);
    assert!(told.contains("KeyClassifier"), "{told}");
}
