//! Commits and reads that start after an ingest has taken its sequence and
//! before it installs its table. A commit in that window takes a sequence
//! above the ingest's and must not publish the visible sequence past the
//! ingest's until the table is installed: a snapshot sampled there would
//! read the old value until the table lands and the ingested one after,
//! and a transaction that read the old value would commit over the ingest
//! unnoticed. Each test puts a commit in the window by hand: the hook runs
//! on the ingesting thread, starts a write on another thread, and waits
//! until it has committed or is queued behind the pipeline.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::engine::ingest::after_next_ingest_seq;
use crate::{
    Db, IngestOptions, IsolationLevel, OptimisticTransactionDb, Options, SstFileWriter,
    TransactionError, TxnOptions, WriteBatch,
};

fn options() -> Options {
    Options::default().max_background_compactions(0)
}

/// An external table holding `k = new`.
fn source(dir: &Path, opts: &Options) -> PathBuf {
    let path = dir.join("source.sst");
    let mut writer = SstFileWriter::create(&path, opts).unwrap();
    writer.put(b"k", b"new").unwrap();
    writer.finish().unwrap();
    path
}

/// A write of `other = x` that reports the sequence it committed at.
fn put_other(db: &Db) -> crate::Result<u64> {
    let mut batch = WriteBatch::new();
    batch.put(b"other", b"x");
    db.write_sequenced(batch)
}

/// A commit started while an ingest was between taking its sequence and
/// installing its table, and the visible sequence published by then.
struct Window {
    ingest_seq: u64,
    horizon: u64,
    writer: JoinHandle<crate::Result<u64>>,
}

impl Window {
    /// Called from the hook: run `write` on another thread and return once
    /// it has committed or is queued in the commit ring, whichever the
    /// engine allows.
    fn open(db: &Db, write: impl FnOnce() -> crate::Result<u64> + Send + 'static) -> Self {
        // The hook runs right after the ingest's allocation, inside its
        // ordered step, and nothing else writes: the ingest took the
        // sequence after the last one decided before its step.
        let ingest_seq = db.engine.last_decided_seq() + 1;
        let taken = db.engine.pipeline_slots_taken();
        let writer = thread::spawn(write);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut backoff = Duration::from_micros(100);
        while !writer.is_finished() && db.engine.pipeline_slots_taken() == taken {
            assert!(
                Instant::now() < deadline,
                "the write neither committed nor queued"
            );
            thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_millis(20));
        }
        Self {
            ingest_seq,
            horizon: db.engine.snapshot_seq(),
            writer,
        }
    }

    /// Once the ingest has returned: the commit took a sequence above the
    /// ingest's, and the visible sequence had not passed the ingest's while
    /// the ingest was pending.
    fn close(self) {
        let committed = self.writer.join().unwrap().unwrap();
        assert!(
            committed > self.ingest_seq,
            "the commit took sequence {committed}, not one above the ingest's {}",
            self.ingest_seq
        );
        assert!(
            self.horizon < self.ingest_seq,
            "a commit published the visible sequence {} past the pending ingest's {}",
            self.horizon,
            self.ingest_seq
        );
    }
}

#[test]
fn a_commit_above_a_pending_ingest_is_not_published_before_it() {
    let dir = TempDir::new().unwrap();
    let opts = options();
    let db = Arc::new(Db::open(dir.path().join("db"), opts.clone()).unwrap());
    db.put(b"k", b"old").unwrap();
    let path = source(dir.path(), &opts);

    let seen = Rc::new(RefCell::new(None));
    let (held, slot) = (Arc::clone(&db), Rc::clone(&seen));
    after_next_ingest_seq(move || {
        let writer_db = Arc::clone(&held);
        *slot.borrow_mut() = Some(Window::open(&held, move || put_other(&writer_db)));
    });
    db.ingest_external_files(&[path], IngestOptions::default())
        .wait()
        .unwrap();

    seen.borrow_mut()
        .take()
        .expect("the ingest took its sequence, which fired the hook")
        .close();
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(db.get(b"other").unwrap().as_deref(), Some(&b"x"[..]));
}

#[test]
fn a_snapshot_taken_before_an_ingest_installs_reads_one_value_throughout() {
    let dir = TempDir::new().unwrap();
    let opts = options();
    let db = Arc::new(Db::open(dir.path().join("db"), opts.clone()).unwrap());
    db.put(b"k", b"old").unwrap();
    let path = source(dir.path(), &opts);

    let seen = Rc::new(RefCell::new(None));
    let (held, slot) = (Arc::clone(&db), Rc::clone(&seen));
    after_next_ingest_seq(move || {
        let writer_db = Arc::clone(&held);
        let window = Window::open(&held, move || put_other(&writer_db));
        let snapshot = held.snapshot();
        let before = snapshot.get(b"k").unwrap();
        *slot.borrow_mut() = Some((window, snapshot, before));
    });
    db.ingest_external_files(&[path], IngestOptions::default())
        .wait()
        .unwrap();

    let (window, snapshot, before) = seen
        .borrow_mut()
        .take()
        .expect("the ingest took its sequence, which fired the hook");
    assert_eq!(before.as_deref(), Some(&b"old"[..]));
    assert_eq!(
        snapshot.get(b"k").unwrap(),
        before,
        "the snapshot read a different value once the ingested table was installed"
    );
    window.close();
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(db.get(b"other").unwrap().as_deref(), Some(&b"x"[..]));
}

#[test]
fn a_transaction_that_read_the_value_an_ingest_replaces_conflicts() {
    let dir = TempDir::new().unwrap();
    let opts = options();
    let db = Arc::new(OptimisticTransactionDb::open(dir.path().join("db"), opts.clone()).unwrap());
    db.db().put(b"k", b"old").unwrap();
    let path = source(dir.path(), &opts);

    let seen = Rc::new(RefCell::new(None));
    let (held, slot) = (Arc::clone(&db), Rc::clone(&seen));
    after_next_ingest_seq(move || {
        let writer_db = Arc::clone(&held);
        let window = Window::open(held.db(), move || put_other(writer_db.db()));
        let txn = held.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
        let read = txn.get(b"k").unwrap();
        *slot.borrow_mut() = Some((window, txn, read));
    });
    db.db()
        .ingest_external_files(&[path], IngestOptions::default())
        .wait()
        .unwrap();

    let (window, txn, read) = seen
        .borrow_mut()
        .take()
        .expect("the ingest took its sequence, which fired the hook");
    assert_eq!(read.as_deref(), Some(&b"old"[..]));
    txn.put(b"k", b"old, updated").unwrap();
    match txn.commit() {
        Err(TransactionError::Conflict { .. }) => {}
        other => panic!(
            "a transaction that read the value the ingest replaced committed over it: {other:?}"
        ),
    }
    window.close();
    assert_eq!(db.db().get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
}
