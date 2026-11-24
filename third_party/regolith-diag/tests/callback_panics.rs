//! A panic in code the caller supplied, inside a commit's ordered step.
//!
//! The panic is caught, the commit that ran the code applies nothing and fails
//! with `Error::CallbackPanicked` naming the trait, and the database is
//! read-only until it is reopened. The same code run outside a commit, by an
//! explicit flush, unwinds into that call and leaves the database writable.
//!
//! A flush is not part of a commit unless the commit has to run it: a write
//! that fills the memtable seals it and the background writes it out (E9).
//! Only a rotation that finds the flush before it still pending writes the
//! oldest frozen memtable out itself, in its commit, so a callback that fails
//! every flush latches the database at the second rotation. A panic in the
//! background flush fails that flush alone, and a listener told of a flush
//! that already completed fails nothing.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

use regolith::prelude::*;
use regolith::{
    Db, Error, EventListener, FixedLengthPrefix, FlushJobInfo, PrefixExtractor, Priority,
    RateLimiter,
};

/// Panics on a key that starts with `0`.
struct PanicsOn(&'static [u8]);

impl KeyClassifier for PanicsOn {
    fn classify(&self, key: &[u8]) -> KeyClass {
        assert!(!key.starts_with(self.0), "injected classifier panic");
        KeyClass::Ordinary
    }
}

fn classified(dir: &Path) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir, Options::default())
        .unwrap()
        .with_policy(Arc::new(PanicsOn(b"boom")))
}

fn begin(db: &OptimisticTransactionDb) -> Transaction {
    db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel))
}

fn panicked<T>(callback: &str, result: &TxResult<T>) -> bool {
    matches!(
        result,
        Err(TransactionError::Engine(Error::CallbackPanicked { callback: named, latched: true })) if *named == callback
    )
}

#[test]
fn a_panicking_classifier_fails_the_commit_and_latches_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = classified(dir.path());
    regolith::through_stalls(|| db.db().put(b"before", b"kept")).unwrap();

    // A write is classified when it is made, to refuse a log key, so the
    // classifier meets `boom` at the commit through a read of it.
    let tx = begin(&db);
    assert_eq!(tx.get(b"boom").unwrap(), None);
    tx.put(b"beside", b"lost").unwrap();
    assert!(panicked("KeyClassifier", &tx.commit()));

    let after = db.db().put(b"after", b"v");
    assert!(
        matches!(
            after,
            Err(Error::CallbackPanicked {
                callback: "KeyClassifier",
                latched: true
            })
        ),
        "{after:?}"
    );
    assert!(
        after
            .unwrap_err()
            .to_string()
            .contains("read-only until it is reopened"),
        "a latched panic says the database is read-only"
    );
    let other = begin(&db);
    other.put(b"elsewhere", b"v").unwrap();
    assert!(panicked("KeyClassifier", &other.commit()));
    assert_eq!(db.db().get(b"before").unwrap(), Some(b"kept".to_vec()));
    assert_eq!(
        db.db().get(b"beside").unwrap(),
        None,
        "the commit applied nothing"
    );

    drop(db);
    let reopened = Db::open(dir.path(), Options::default()).unwrap();
    assert_eq!(reopened.get(b"before").unwrap(), Some(b"kept".to_vec()));
    assert_eq!(reopened.get(b"boom").unwrap(), None);
    regolith::through_stalls(|| reopened.put(b"after", b"v")).unwrap();
}

/// A write is classified when it is made, so a panic there is outside any
/// commit: it unwinds into the call that ran the classifier and fails only
/// that call.
#[test]
fn a_classifier_that_panics_in_a_write_unwinds_into_it_and_leaves_the_database_writable() {
    let dir = tempfile::tempdir().unwrap();
    let db = classified(dir.path());
    let tx = begin(&db);
    assert!(catch_unwind(AssertUnwindSafe(|| tx.put(b"boom", b"v"))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| tx.delete(b"boom"))).is_err());
    tx.put(b"fine", b"v").unwrap();
    tx.commit().unwrap();
    regolith::through_stalls(|| db.db().put(b"after", b"v")).unwrap();
    assert_eq!(db.db().get(b"fine").unwrap(), Some(b"v".to_vec()));
    assert_eq!(db.db().get(b"boom").unwrap(), None);
}

#[test]
fn a_classifier_that_panics_on_a_scanned_stretch_fails_the_commit_too() {
    let dir = tempfile::tempdir().unwrap();
    let db = classified(dir.path());
    regolith::through_stalls(|| db.db().put(b"boom/1", b"v")).unwrap();
    regolith::through_stalls(|| db.db().put(b"boom/2", b"v")).unwrap();

    let tx = begin(&db);
    assert_eq!(tx.scan_stream(Some(b"boom/"), Some(b"boom0")).count(), 2);
    // A transaction that writes nothing validates nothing, so it never asks the
    // classifier about its stretches.
    tx.put(b"elsewhere", b"v").unwrap();
    assert!(panicked("KeyClassifier", &tx.commit()));
    assert!(matches!(
        db.db().put(b"after", b"v"),
        Err(Error::CallbackPanicked { .. })
    ));
}

#[test]
fn a_classifier_that_does_not_panic_leaves_commits_alone() {
    let dir = tempfile::tempdir().unwrap();
    let db = classified(dir.path());
    let tx = begin(&db);
    tx.put(b"fine", b"v").unwrap();
    tx.commit().unwrap();
    regolith::through_stalls(|| db.db().put(b"after", b"v")).unwrap();
}

/// A buffer small enough that a few writes rotate it, and so flush.
fn tiny() -> Options {
    Options::default().write_buffer_size(4 * 1024)
}

fn key(i: usize) -> Vec<u8> {
    format!("key{i:04}").into_bytes()
}

/// Write until a write fails, returning the index it failed at and why. The
/// failed write did not land.
fn write_until_it_fails(db: &Db) -> (usize, Error) {
    for i in 0..400 {
        if let Err(err) = regolith::through_stalls(|| db.put(&key(i), &[7u8; 512])) {
            return (i, err);
        }
    }
    panic!("400 writes of 512 bytes never rotated a 4 KiB buffer into a failure");
}

/// A write that rotates the buffer fails with `callback`, the database is
/// read-only after it, and a reopen finds every write before it. The first
/// rotation hands its flush to the background, where the panic fails the
/// flush and leaves the memtable frozen; the next rotation finds it still
/// frozen and writes it out in its own commit, which latches.
fn assert_the_rotating_write_latches(dir: &Path, options: Options, callback: &str) {
    let db = Db::open(dir, options).unwrap();
    let (failed_at, err) = write_until_it_fails(&db);
    assert!(
        matches!(&err, Error::CallbackPanicked { callback: named, latched: true } if *named == callback),
        "{err:?}"
    );
    assert!(failed_at > 0);
    let later = db.put(b"later", b"v");
    assert!(
        matches!(&later, Err(Error::CallbackPanicked { callback: named, latched: true }) if *named == callback),
        "{later:?}"
    );
    assert_eq!(db.get(&key(failed_at)).unwrap(), None);
    assert!(db.get(&key(failed_at - 1)).unwrap().is_some());

    drop(db);
    let reopened = Db::open(dir, tiny()).unwrap();
    for i in 0..failed_at {
        assert!(
            reopened.get(&key(i)).unwrap().is_some(),
            "write {i} was lost"
        );
    }
    assert_eq!(reopened.get(&key(failed_at)).unwrap(), None);
    regolith::through_stalls(|| reopened.put(b"later", b"v")).unwrap();
}

struct PanicsOnFlush;

impl EventListener for PanicsOnFlush {
    fn on_flush_completed(&self, _: &FlushJobInfo) {
        panic!("injected listener panic");
    }
}

/// Counts the flushes it is told of.
#[derive(Default)]
struct CountsFlushes(std::sync::atomic::AtomicUsize);

impl EventListener for CountsFlushes {
    fn on_flush_completed(&self, _: &FlushJobInfo) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A listener told of a flush that already completed cannot undo it: in a
/// flush the background runs, or one a rotation runs in its commit, its panic
/// is caught and logged, the flush stands, the database stays writable and
/// every write survives a reopen.
#[test]
fn a_panicking_listener_told_of_a_completed_flush_fails_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let counted = Arc::new(CountsFlushes::default());
    let options = tiny().listeners(vec![counted.clone(), Arc::new(PanicsOnFlush)]);
    let db = Db::open(dir.path(), options).unwrap();
    for i in 0..200 {
        regolith::through_stalls(|| db.put(&key(i), &[7u8; 512]))
            .unwrap_or_else(|e| panic!("write {i} failed: {e:?}"));
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut pause = std::time::Duration::from_millis(1);
    while counted.0.load(std::sync::atomic::Ordering::SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "200 writes of 512 bytes into a 4 KiB buffer never completed a flush"
        );
        std::thread::sleep(pause);
        pause = (pause * 2).min(std::time::Duration::from_millis(50));
    }
    regolith::through_stalls(|| db.put(b"later", b"v")).expect("the database is still writable");

    drop(db);
    let reopened = Db::open(dir.path(), tiny()).unwrap();
    for i in 0..200 {
        assert!(
            reopened.get(&key(i)).unwrap().is_some(),
            "write {i} was lost"
        );
    }
    assert_eq!(reopened.get(b"later").unwrap(), Some(b"v".to_vec()));
}

struct PanicsOnRequest;

impl RateLimiter for PanicsOnRequest {
    fn request(&self, _: u64, _: Priority) {
        panic!("injected limiter panic");
    }
}

/// The rate limiter throttles the background alone (plan 4.10): a limiter
/// that panics never reaches a caller's call, so the rotations writes run
/// flush unthrottled and every write lands. On the worker its panic fails the
/// flush it ran in, which is reported and retried as any failing flush is,
/// and latches nothing.
#[test]
fn a_panicking_rate_limiter_fails_only_the_background_flush() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(
        dir.path(),
        tiny().rate_limiter(Some(Arc::new(PanicsOnRequest))),
    )
    .unwrap();
    // Fewer writes than it takes the rotations' own flushes to bring L0 to
    // its stop trigger: with every background flush and compaction failing,
    // nothing else would bring it back down.
    for i in 0..200 {
        regolith::through_stalls(|| db.put(&key(i), &[7u8; 512]))
            .expect("a caller's write never meets the limiter");
    }
    for i in 0..200 {
        assert!(db.get(&key(i)).unwrap().is_some(), "write {i} was lost");
    }
    // The last sealed memtable is the worker's to flush, and its flush meets
    // the limiter's panic.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut pause = std::time::Duration::from_millis(1);
    while db
        .get_int_property("regolith.background-errors")
        .unwrap_or(0)
        == 0
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the worker's flush never met the limiter"
        );
        std::thread::sleep(pause);
        pause = (pause * 2).min(std::time::Duration::from_millis(50));
    }
    regolith::through_stalls(|| db.put(b"later", b"v"))
        .expect("a panic on the worker latches nothing");
}

struct PanicsOnExtract;

impl PrefixExtractor for PanicsOnExtract {
    fn extract<'a>(&self, _: &'a [u8]) -> Option<&'a [u8]> {
        panic!("injected extractor panic");
    }

    fn name(&self) -> &'static str {
        "panics"
    }
}

#[test]
fn a_panicking_prefix_extractor_in_the_rotation_a_write_runs_latches_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let options = tiny().prefix_extractor(Some(Arc::new(PanicsOnExtract)));
    assert_the_rotating_write_latches(dir.path(), options, "PrefixExtractor");
}

#[test]
fn the_same_listener_panic_outside_a_commit_fails_only_that_call() {
    let dir = tempfile::tempdir().unwrap();
    let options = tiny().listeners(vec![Arc::new(PanicsOnFlush)]);
    let db = Db::open(dir.path(), options).unwrap();
    regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();

    let unwound = catch_unwind(AssertUnwindSafe(|| db.flush()));
    assert!(
        unwound.is_err(),
        "an explicit flush runs the listener uncaught"
    );

    regolith::through_stalls(|| db.put(b"after", b"v")).expect("the database is still writable");
    assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
    assert_eq!(db.get(b"after").unwrap(), Some(b"v".to_vec()));
}

#[test]
fn a_prefix_extractor_that_does_not_panic_is_unaffected() {
    let dir = tempfile::tempdir().unwrap();
    let options = tiny().prefix_extractor(Some(Arc::new(FixedLengthPrefix(3))));
    let db = Db::open(dir.path(), options).unwrap();
    for i in 0..60 {
        regolith::through_stalls(|| db.put(&key(i), &[7u8; 512])).unwrap();
    }
    assert!(db.get(&key(0)).unwrap().is_some());
}
