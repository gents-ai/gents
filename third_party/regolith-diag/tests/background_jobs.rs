//! `compact_range`, ingest and checkpoint return a `JobTicket` at once and
//! never wait on the compaction gate (plan 4.10): they run on the compaction
//! worker, or, with no worker, as a job on the caller's queue, or inline for
//! a caller with neither. The ticket completes on the caller's queue, or,
//! with none, wherever the job finished.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::ThreadId;

use common::device_env::cold_db;
use regolith::{Db, Error, IngestOptions, IoBudget, Options, SstFileWriter};
use tempfile::TempDir;

fn l0(db: &Db) -> u64 {
    db.get_int_property("regolith.num-files-at-level0").unwrap()
}

/// A database with two L0 tables and nothing compacting them on its own.
fn two_tables(options: Options) -> (TempDir, Db) {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options.l0_compaction_trigger(1000)).unwrap();
    db.put(b"a", b"1").unwrap();
    db.flush().unwrap();
    db.put(b"b", b"2").unwrap();
    db.flush().unwrap();
    assert_eq!(l0(&db), 2);
    (dir, db)
}

fn sst(dir: &std::path::Path, key: &[u8], value: &[u8]) -> std::path::PathBuf {
    let path = dir.join("ingest.sst");
    let mut writer = SstFileWriter::create(&path, &Options::default()).unwrap();
    writer.put(key, value).unwrap();
    writer.finish().unwrap();
    path
}

/// Every foreground job returns a ticket, on a database with a worker and
/// from a thread with no queue: the job runs on the worker and the ticket is
/// completed there, so `wait` returns its result.
#[test]
fn every_background_job_returns_a_ticket() {
    let (_dir, db) = two_tables(Options::default());
    let staging = TempDir::new().unwrap();
    let checkpoint = TempDir::new().unwrap();

    let compacted = db.compact_range(None, None);
    let ingested =
        db.ingest_external_files(&[sst(staging.path(), b"c", b"3")], IngestOptions::default());
    let checkpointed = db.checkpoint(checkpoint.path().join("cp"));
    compacted.wait().unwrap();
    ingested.wait().unwrap();
    checkpointed.wait().unwrap();

    assert_eq!(l0(&db), 0, "the compaction ran");
    assert_eq!(db.get(b"c").unwrap(), Some(b"3".to_vec()), "the ingest ran");
    let copy = Db::open(checkpoint.path().join("cp"), Options::default()).unwrap();
    assert_eq!(
        copy.get(b"a").unwrap(),
        Some(b"1".to_vec()),
        "the checkpoint ran"
    );
}

/// The call never waits on the compaction gate: while the worker's
/// compaction holds the gate, held at the device mid-read, another
/// foreground job's call returns its ticket at once, not ready.
#[test]
fn no_caller_waits_on_the_compaction_gate() {
    let (_dir, db, device) = cold_db(
        |env| Options::default().env(env).l0_compaction_trigger(1000),
        |db| {
            for i in 0..200u32 {
                db.put(format!("k{i:05}").as_bytes(), &[7u8; 256]).unwrap();
                if i % 50 == 49 {
                    db.flush().unwrap();
                }
            }
        },
    );
    device.hold();
    let first = db.compact_range(None, None);
    device.wait_held(1);
    let checkpoint = TempDir::new().unwrap();
    let second = db.checkpoint(checkpoint.path().join("cp"));
    let third = db.compact_range(None, None);
    assert!(!first.is_ready() && !second.is_ready() && !third.is_ready());
    device.release();
    first.wait().unwrap();
    second.wait().unwrap();
    third.wait().unwrap();
    assert_eq!(l0(&db), 0);
}

/// With a worker, the ticket of a caller with a queue is completed on that
/// queue: its `on_complete` runs on the queue's thread, at the poll that
/// delivers it.
#[test]
fn a_workers_job_completes_at_the_callers_poll() {
    let (_dir, db) = two_tables(Options::default());
    let mut queue = db.io_queue();
    let ticket = db.compact_range(None, None);
    let ran: Arc<std::sync::Mutex<Vec<ThreadId>>> = Arc::default();
    let seen = Arc::clone(&ran);
    ticket.on_complete(move |result| {
        assert!(result.is_ok());
        seen.lock().unwrap().push(std::thread::current().id());
    });
    queue.block_on(ticket).unwrap();
    assert_eq!(*ran.lock().unwrap(), vec![std::thread::current().id()]);
    assert_eq!(l0(&db), 0);
}

/// With no worker, the job is a unit on the caller's queue: nothing runs
/// until that queue's owner polls, and the poll runs it.
#[test]
fn with_no_worker_the_job_runs_at_the_callers_poll() {
    let (_dir, db) = two_tables(Options::default().max_background_compactions(0));
    let mut queue = db.io_queue();
    let ticket = db.compact_range(None, None);
    assert!(!ticket.is_ready());
    assert_eq!(l0(&db), 2, "nothing ran before the poll");
    let progress = queue.poll(IoBudget::ALL);
    assert_eq!(progress.completed, 1);
    assert!(ticket.is_ready());
    assert_eq!(l0(&db), 0);
    queue.block_on(ticket).unwrap();
}

/// With no worker and no queue, the caller runs the job inline and the
/// ticket is ready when the call returns.
#[test]
fn with_neither_the_job_runs_inline() {
    let (_dir, db) = two_tables(Options::default().max_background_compactions(0));
    let ticket = db.compact_range(None, None);
    assert!(ticket.is_ready());
    assert_eq!(l0(&db), 0);
    ticket.wait().unwrap();
}

/// `wait` on a job left on this thread's queue runs it here, as a blocking
/// call does its own I/O; the ticket is still delivered at the queue's poll.
#[test]
fn wait_runs_a_queued_job_itself() {
    let (_dir, db) = two_tables(Options::default().max_background_compactions(0));
    let mut queue = db.io_queue();
    let delivered = Arc::new(AtomicUsize::new(0));
    let ticket = db.compact_range(None, None);
    let count = Arc::clone(&delivered);
    ticket.on_complete(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
    });
    ticket.wait().unwrap();
    assert_eq!(l0(&db), 0, "wait ran the job");
    assert_eq!(delivered.load(Ordering::SeqCst), 0, "not delivered yet");
    queue.poll(IoBudget::ALL);
    assert_eq!(delivered.load(Ordering::SeqCst), 1);
}

/// A queue dropped before it ran its job fails the job's ticket, delivered
/// on the dropping thread, and leaves the database as it was.
#[test]
fn a_dropped_queue_fails_the_job_it_never_ran() {
    let (_dir, db) = two_tables(Options::default().max_background_compactions(0));
    let queue = db.io_queue();
    let ticket = db.compact_range(None, None);
    drop(queue);
    assert!(ticket.is_ready());
    assert!(matches!(ticket.wait(), Err(Error::InvalidArgument(_))));
    assert_eq!(l0(&db), 2);
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(l0(&db), 0, "the job can be asked for again");
}

/// An invalid argument fails the ticket at once.
#[test]
fn an_invalid_bound_fails_the_ticket_at_once() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default().max_key_size(8)).unwrap();
    let ticket = db.compact_range(Some(b"a key longer than eight"), None);
    assert!(ticket.is_ready());
    assert!(matches!(ticket.wait(), Err(Error::InvalidArgument(_))));
}

/// The ticket is a future: awaited through the queue it completes on.
#[test]
fn a_ticket_is_a_future() {
    let (_dir, db) = two_tables(Options::default().max_background_compactions(0));
    let mut queue = db.io_queue();
    let result = queue.block_on(async { db.compact_range(None, None).await });
    result.unwrap();
    assert_eq!(l0(&db), 0);
}

#[test]
fn a_ticket_is_shared_and_sent_freely() {
    fn shared<T: Send + Sync>() {}
    shared::<regolith::JobTicket>();
    let (_dir, db) = two_tables(Options::default());
    let ticket = db.compact_range(None, None);
    std::thread::spawn(move || ticket.wait().unwrap())
        .join()
        .unwrap();
}
