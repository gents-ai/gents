//! A write under a stall never waits (D23): a stop returns
//! `WouldBlock::Stall` with a `StallWait` and applies nothing; the wait
//! completes when the stall clears, on the writer's own queue when it has
//! one; with no worker, inline compaction runs the stall's step as a job on
//! the writer's queue, or inline for a writer with no queue.
//!
//! A transaction's commit is the exception, since it cannot be run again:
//! it waits the stall out (`tests/commit_through_stall.rs`).
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::{Db, Error, IoBudget, Options, StallWait, WouldBlock};
use tempfile::TempDir;

/// Writes stop once L0 holds two tables, and nothing compacts on its own.
fn stop_options() -> Options {
    Options::default()
        .write_buffer_size(4 * 1024)
        .l0_compaction_trigger(1000)
        .level0_slowdown_writes_trigger(0)
        .level0_stop_writes_trigger(2)
        .max_write_buffer_number(0)
}

fn l0(db: &Db) -> u64 {
    db.get_int_property("regolith.num-files-at-level0").unwrap()
}

/// Bring L0 to the stop with explicit flushes.
fn stop(db: &Db) {
    for round in 0..2 {
        regolith::through_stalls(|| db.put(format!("fill{round}").as_bytes(), b"v")).unwrap();
        db.flush().unwrap();
    }
    assert_eq!(l0(db), 2);
}

fn stalled(outcome: regolith::Result<()>) -> StallWait {
    match outcome {
        Err(Error::WouldBlock(WouldBlock::Stall(wait))) => wait,
        other => panic!("a stopped write returns its wait, got {other:?}"),
    }
}

#[test]
fn a_stopped_write_returns_its_wait_and_applies_nothing() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), stop_options()).unwrap();
    stop(&db);
    let stopped = db.put(b"k", b"v");
    assert_eq!(
        stopped.as_ref().unwrap_err().to_string(),
        "writes are stalled behind background work (stop: too many L0 files); wait for the \
         stall to clear and run the write again"
    );
    let wait = stalled(stopped);
    assert_eq!(wait.reason(), "stop: too many L0 files");
    assert_eq!(wait.queue(), None);
    assert!(!wait.is_ready());
    assert_eq!(db.get(b"k").unwrap(), None, "the write applied nothing");
    // Another stopped write waits on the same stall.
    let again = stalled(db.put(b"k2", b"v"));
    db.compact_range(None, None).wait().unwrap();
    assert!(
        wait.is_ready() && again.is_ready(),
        "the clearing compaction told both"
    );
    regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
}

/// A writer with a queue gets a wait recorded on it: the stall clearing on
/// another thread lands the stall's unit, but the wait becomes ready only at
/// the writer's own poll.
#[test]
fn a_wait_on_a_queue_is_ready_only_at_that_queues_poll() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), stop_options()).unwrap());
    stop(&db);
    let mut queue = db.io_queue();
    let wait = stalled(db.put(b"k", b"v"));
    assert_eq!(wait.queue(), Some(queue.id()));
    let clearer = Arc::clone(&db);
    std::thread::spawn(move || clearer.compact_range(None, None).wait().unwrap())
        .join()
        .unwrap();
    assert_eq!(l0(&db), 0, "the stall cleared");
    assert!(!wait.is_ready(), "cleared is not delivered");
    assert_eq!(queue.poll(IoBudget::ALL).completed, 1);
    assert!(wait.is_ready());
    regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
}

/// A blocking caller waits for the stall itself, on its own thread, through
/// `through_stalls`, whether or not its thread has a queue.
#[test]
fn through_stalls_waits_for_the_stall_to_clear() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path(), stop_options()).unwrap());
    stop(&db);
    let writer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let _queue = db.io_queue();
            regolith::through_stalls(|| db.put(b"k", b"v"))
        })
    };
    // The writer may run before or after this compaction: either it finds no
    // stall, or this compaction is what clears the one it waits on.
    db.compact_range(None, None).wait().unwrap();
    writer.join().unwrap().unwrap();
    assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
}

/// With no worker and inline compaction on, a stopped writer with a queue
/// leaves the stall's step on that queue: nothing runs until it polls, and
/// its polls clear the stall.
#[test]
fn with_no_worker_the_stall_step_runs_at_the_writers_poll() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        stop_options()
            .max_background_compactions(0)
            .inline_compaction(true)
            .l0_compaction_trigger(2),
    )
    .unwrap();
    let mut queue = db.io_queue();
    // The steps these writes owe wait on the queue, which nothing polls yet.
    stop(&db);
    let before = l0(&db);
    let wait = stalled(db.put(b"k", b"v"));
    assert_eq!(wait.queue(), Some(queue.id()));
    assert_eq!(l0(&db), before, "the step waits for the poll");
    while !wait.is_ready() {
        queue.poll(IoBudget::ALL);
    }
    assert!(l0(&db) < before, "the polls ran the step");
    regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
}

/// With no worker and inline compaction on, a stopped writer with no queue
/// runs the step inline, bounded, and its write goes on.
#[test]
fn with_no_worker_and_no_queue_the_writer_relieves_the_stall_inline() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        stop_options()
            .max_background_compactions(0)
            .inline_compaction(true)
            .l0_compaction_trigger(2),
    )
    .unwrap();
    for i in 0..200u32 {
        regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), &[1u8; 256])).unwrap();
    }
    assert!(l0(&db) < 2);
}

/// With no worker and inline compaction turned off, nothing clears a stall
/// until a caller compacts: the stopped write's wait completes then.
#[test]
fn with_no_worker_and_inline_off_a_caller_clears_the_stall() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        stop_options()
            .max_background_compactions(0)
            .inline_compaction(false)
            .l0_compaction_trigger(2),
    )
    .unwrap();
    // A step a write owes runs only after the write committed: the stopped
    // write below owes nothing, so nothing relieves the stop on its own.
    stop(&db);
    let wait = stalled(db.put(b"k", b"v"));
    assert!(!wait.is_ready());
    db.compact_step().unwrap();
    assert!(wait.is_ready());
    regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
}

/// `no_slowdown` still answers any stall with `Busy` at once.
#[test]
fn no_slowdown_answers_a_stop_with_busy() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), stop_options()).unwrap();
    stop(&db);
    let opts = regolith::WriteOptions {
        no_slowdown: true,
        ..regolith::WriteOptions::default()
    };
    assert!(matches!(db.put_opt(&opts, b"k", b"v"), Err(Error::Busy(_))));
}

/// Close tells every stopped writer: its wait completes and the write run
/// again answers `Closed`.
#[test]
fn close_tells_every_stopped_writer() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), stop_options()).unwrap();
    stop(&db);
    let wait = stalled(db.put(b"k", b"v"));
    db.close().unwrap();
    assert!(wait.is_ready());
    assert!(matches!(db.put(b"k", b"v"), Err(Error::Closed)));
}
