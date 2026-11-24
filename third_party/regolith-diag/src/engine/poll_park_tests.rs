//! A job a queue's poll runs parks instead of waiting for another thread,
//! whichever job it is: the step a write left owing, the step that relieves
//! a stall, a `compact_range`, an ingest, a checkpoint (`commit::park`,
//! `foreground.rs`, `background_step.rs`, `drain.rs`).
//!
//! Each test has another thread hold the one thing the job needs next, as a
//! thread in the middle of its work holds it, polls the queue, and checks
//! the same four things: the poll returns without the thing (a poll that
//! waited for it would never return, since it is released only afterwards),
//! the job did none of the work the thing guards, the owner, idle, is woken
//! once the thing is free, and the next polls finish the job. Nothing is
//! timed.

use std::path::Path;
use std::sync::Arc;
use std::task::Waker;

use tempfile::TempDir;

use super::park_support::{Wakes, counting, hold, while_another_thread_holds};
use crate::{
    Db, Error, IngestOptions, IoBudget, IoQueue, JobTicket, Options, SstFileWriter, WouldBlock,
};

/// No worker: a foreground job, and the step a write leaves owing, run at
/// the poll of the queue of the thread that asked.
fn no_worker(dir: &Path) -> Db {
    Db::open(dir, Options::default().max_background_compactions(0)).unwrap()
}

/// How many tables the database's table directory holds, staged ones
/// included.
fn table_files(dir: &Path) -> usize {
    std::fs::read_dir(dir.join("sst")).unwrap().count()
}

/// One external table holding `key`.
fn external_table(dir: &Path, name: &str, key: &[u8]) -> std::path::PathBuf {
    let source = dir.join(name);
    let mut writer = SstFileWriter::create(&source, &Options::default()).unwrap();
    writer.put(key, b"ingested").unwrap();
    writer.finish().unwrap();
    source
}

/// Poll `queue` while what `ticket`'s job needs next is held by another
/// thread: the poll returns, the job is still owed, and the owner, idle, is
/// not woken. Returns the idle owner's wakes.
pub(super) fn poll_parks(queue: &mut IoQueue, ticket: &JobTicket) -> Arc<Wakes> {
    let progress = queue.poll(IoBudget::ALL);
    assert_eq!(progress.completed, 0, "the job ended without what it needs");
    assert!(progress.more_pending, "the job is still owed");
    assert!(!ticket.is_ready());
    idle(queue)
}

/// The owner of `queue` has nothing to poll for and idles.
fn idle(queue: &IoQueue) -> Arc<Wakes> {
    let (idle, waker) = counting();
    queue.idle_waker(&waker);
    assert_eq!(idle.count(), 0, "a parked job is no work to poll for");
    idle
}

/// What the job parked for is free: the idle owner was woken, once, and its
/// polls finish the job.
fn poll_finishes(queue: &mut IoQueue, ticket: JobTicket, idle: &Wakes) -> Result<(), Error> {
    assert_eq!(
        idle.count(),
        1,
        "the idle owner is woken once what the job parked for is free"
    );
    poll_until_ready(queue, &ticket);
    ticket.wait()
}

/// A job may park once for each thing it needs: poll until it is over.
fn poll_until_ready(queue: &mut IoQueue, ticket: &JobTicket) {
    for _ in 0..16 {
        queue.poll(IoBudget::ALL);
        if ticket.is_ready() {
            return;
        }
    }
    panic!("the job did not finish with everything it needs free");
}

/// The version store's step, kept as a thread in the middle of a section
/// keeps it.
pub(super) fn the_step<'a>(db: &'a Db) -> impl FnOnce() -> super::step::Hold<'a> + Send + 'a {
    || {
        db.engine()
            .versions
            .hold_or_park(Waker::noop())
            .expect("the step is free")
    }
}

/// The flush exclusion, kept as a thread in the middle of a flush keeps it.
fn the_flush_exclusion<'a>(db: &'a Db) -> impl FnOnce() -> super::flush::Flushing<'a> + Send + 'a {
    || db.engine().flusher.flushing().unwrap()
}

// ---- compact_range ---------------------------------------------------------

/// The drain's unit seals the active memtable, which takes a file id: with
/// the step held the unit parks, and the job with it, under the gate.
#[test]
fn a_compact_range_on_a_poll_parks_at_its_seal_while_another_thread_holds_the_step() {
    let dir = TempDir::new().unwrap();
    let db = no_worker(dir.path());
    db.put(b"k", b"v").unwrap();
    let mut queue = db.io_queue();
    let ticket = db.compact_range(None, None);
    assert!(!ticket.is_ready(), "the job waits for its queue's poll");

    let idle = while_another_thread_holds(
        the_step(&db),
        || {
            let idle = poll_parks(&mut queue, &ticket);
            assert_eq!(db.engine().frozen_memtables(), 0, "nothing was sealed");
            idle
        },
        |idle| idle,
    );
    poll_finishes(&mut queue, ticket, &idle).unwrap();
    assert!(db.engine().memtables_hold_no_data());
    assert_eq!(db.engine().level_file_count(0), 0, "pushed down from L0");
    assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
}

/// The seal went through; the flush that follows needs the flush exclusion,
/// and with a flush in flight on another thread the job parks before it.
#[test]
fn a_compact_range_on_a_poll_parks_at_its_flush_while_another_thread_flushes() {
    let dir = TempDir::new().unwrap();
    let db = no_worker(dir.path());
    db.put(b"k", b"v").unwrap();
    let mut queue = db.io_queue();
    let ticket = db.compact_range(None, None);

    let idle = while_another_thread_holds(
        the_flush_exclusion(&db),
        || {
            let idle = poll_parks(&mut queue, &ticket);
            assert_eq!(db.engine().frozen_memtables(), 1, "sealed, not flushed");
            assert_eq!(db.engine().total_file_count(), 0);
            idle
        },
        |idle| idle,
    );
    poll_finishes(&mut queue, ticket, &idle).unwrap();
    assert_eq!(db.engine().frozen_memtables(), 0);
    assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
}

/// Nothing in memory, so no drain: the passes are next, and they hand
/// sections to the step. With the step held the job parks before the first.
#[test]
fn a_compact_range_on_a_poll_parks_before_its_passes_while_another_thread_holds_the_step() {
    let dir = TempDir::new().unwrap();
    let db = no_worker(dir.path());
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    assert_eq!(db.engine().level_file_count(0), 1, "setup: one table in L0");
    let mut queue = db.io_queue();
    let ticket = db.compact_range(None, None);

    let idle = while_another_thread_holds(
        the_step(&db),
        || {
            let idle = poll_parks(&mut queue, &ticket);
            assert_eq!(db.engine().level_file_count(0), 1, "no pass ran");
            idle
        },
        |idle| idle,
    );
    poll_finishes(&mut queue, ticket, &idle).unwrap();
    assert_eq!(db.engine().level_file_count(0), 0, "pushed down from L0");
    assert_eq!(db.get(b"k").unwrap(), Some(b"v".to_vec()));
}

/// A job parked under the gate keeps the gate. Its queue dropped, the job
/// is settled and the gate given back: the next job gets it.
#[test]
fn a_job_parked_under_the_gate_gives_the_gate_back_when_its_queue_is_dropped() {
    let dir = TempDir::new().unwrap();
    let db = no_worker(dir.path());
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    let mut queue = db.io_queue();
    let ticket = db.compact_range(None, None);

    while_another_thread_holds(
        the_step(&db),
        || {
            poll_parks(&mut queue, &ticket);
            assert!(
                db.engine().compaction_gate().try_shared().is_none(),
                "the parked job holds the gate"
            );
        },
        |()| (),
    );
    drop(queue);
    let told = ticket.wait().expect_err("the job was never run to its end");
    assert!(told.to_string().contains("dropped before it ran"), "{told}");
    assert!(
        db.engine().compaction_gate().try_shared().is_some(),
        "the settled job gave the gate back"
    );
    // This thread has no queue now: the call runs the job itself.
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(db.engine().level_file_count(0), 0);
}

// ---- ingest ------------------------------------------------------------------

/// Staging takes its file ids in one section of the step, before it copies
/// anything: with the step held the job parks with nothing staged.
#[test]
fn an_ingest_on_a_poll_parks_before_staging_while_another_thread_holds_the_step() {
    let dir = TempDir::new().unwrap();
    let db = no_worker(&dir.path().join("db"));
    let source = external_table(dir.path(), "external.sst", b"z");
    let mut queue = db.io_queue();
    let before = table_files(&dir.path().join("db"));
    let ticket = db.ingest_external_files(std::slice::from_ref(&source), IngestOptions::default());

    let idle = while_another_thread_holds(
        the_step(&db),
        || {
            let idle = poll_parks(&mut queue, &ticket);
            assert_eq!(
                table_files(&dir.path().join("db")),
                before,
                "nothing was staged"
            );
            idle
        },
        |idle| idle,
    );
    poll_finishes(&mut queue, ticket, &idle).unwrap();
    assert_eq!(db.get(b"z").unwrap(), Some(b"ingested".to_vec()));
}

/// The table is staged and the gate held; the install is an ordered unit
/// that may flush, so it takes the flush exclusion before its body. With a
/// flush in flight the unit parks, in the ring, and the job with it.
#[test]
fn an_ingest_on_a_poll_parks_at_its_install_while_another_thread_flushes() {
    let dir = TempDir::new().unwrap();
    let db = no_worker(&dir.path().join("db"));
    let source = external_table(dir.path(), "external.sst", b"z");
    let mut queue = db.io_queue();
    let before = table_files(&dir.path().join("db"));
    let ticket = db.ingest_external_files(std::slice::from_ref(&source), IngestOptions::default());

    let idle = while_another_thread_holds(
        the_flush_exclusion(&db),
        || {
            let idle = poll_parks(&mut queue, &ticket);
            assert_eq!(
                table_files(&dir.path().join("db")),
                before + 1,
                "staged, not installed"
            );
            assert_eq!(db.get(b"z").unwrap(), None);
            idle
        },
        |idle| idle,
    );
    poll_finishes(&mut queue, ticket, &idle).unwrap();
    assert_eq!(db.get(b"z").unwrap(), Some(b"ingested".to_vec()));
}

/// A job settled with a table's install already in the ring sees that
/// install through: whichever thread reaches a unit runs it, so the table
/// would otherwise go in after its caller was told the ingest failed. It
/// was the only table, so the ingest is complete and the ticket says so.
#[test]
fn an_ingest_settled_with_its_last_install_in_the_ring_reports_the_table_in() {
    let dir = TempDir::new().unwrap();
    let db = no_worker(&dir.path().join("db"));
    let source = external_table(dir.path(), "external.sst", b"z");
    let mut queue = db.io_queue();
    let ticket = db.ingest_external_files(std::slice::from_ref(&source), IngestOptions::default());

    while_another_thread_holds(
        the_flush_exclusion(&db),
        || {
            poll_parks(&mut queue, &ticket);
        },
        |()| (),
    );
    drop(queue);
    ticket.wait().unwrap();
    assert_eq!(db.get(b"z").unwrap(), Some(b"ingested".to_vec()));
}

/// The same with a second table still to install: the first goes in, the
/// second does not, and the ticket reports the ingest as not done, as an
/// error after the first table does.
#[test]
fn an_ingest_settled_between_two_installs_reports_it_and_leaves_the_first_in() {
    let dir = TempDir::new().unwrap();
    let db = no_worker(&dir.path().join("db"));
    let sources = [
        external_table(dir.path(), "first.sst", b"a"),
        external_table(dir.path(), "second.sst", b"z"),
    ];
    let mut queue = db.io_queue();
    let ticket = db.ingest_external_files(&sources, IngestOptions::default());

    while_another_thread_holds(
        the_flush_exclusion(&db),
        || {
            poll_parks(&mut queue, &ticket);
        },
        |()| (),
    );
    drop(queue);
    let told = ticket
        .wait()
        .expect_err("the second table was never installed");
    assert!(told.to_string().contains("dropped before it ran"), "{told}");
    assert_eq!(db.get(b"a").unwrap(), Some(b"ingested".to_vec()));
    assert_eq!(db.get(b"z").unwrap(), None);
}

// ---- checkpoint --------------------------------------------------------------

/// Before the gate a checkpoint drains the memtables, and the drain's unit
/// needs the step: with the step held the job parks before it sealed
/// anything, and holds no gate.
#[test]
fn a_checkpoint_on_a_poll_parks_in_its_drain_while_another_thread_holds_the_step() {
    let dir = TempDir::new().unwrap();
    let db = no_worker(&dir.path().join("db"));
    db.put(b"k", b"v").unwrap();
    let mut queue = db.io_queue();
    let target = dir.path().join("checkpoint");
    let ticket = db.checkpoint(&target);

    let idle = while_another_thread_holds(
        the_step(&db),
        || {
            let idle = poll_parks(&mut queue, &ticket);
            assert_eq!(db.engine().frozen_memtables(), 0, "nothing was sealed");
            assert!(
                db.engine().compaction_gate().try_shared().is_some(),
                "a job parked before the gate does not hold it"
            );
            idle
        },
        |idle| idle,
    );
    poll_finishes(&mut queue, ticket, &idle).unwrap();
    let copy = Db::open(&target, Options::default()).unwrap();
    assert_eq!(copy.get(b"k").unwrap(), Some(b"v".to_vec()));
}

/// Under the gate the capture is a section of the step. The job is first
/// held at the gate by another thread; when it is given the gate the step
/// is held, and it parks there, before it captured anything.
#[test]
fn a_checkpoint_on_a_poll_parks_at_its_capture_while_another_thread_holds_the_step() {
    let dir = TempDir::new().unwrap();
    let db = no_worker(&dir.path().join("db"));
    db.put(b"k", b"v").unwrap();
    let mut queue = db.io_queue();
    let target = dir.path().join("checkpoint");
    let ticket = db.checkpoint(&target);

    let idle = std::thread::scope(|scope| {
        // The drain goes through, and the job parks at the gate.
        let gate = hold(scope, || db.engine().compaction_gate().exclusive().unwrap());
        let at_the_gate = poll_parks(&mut queue, &ticket);
        assert!(db.engine().memtables_hold_no_data(), "the drain is done");
        // Handed the gate with the step held: it parks at its capture.
        let step = hold(scope, the_step(&db));
        gate.release();
        assert_eq!(at_the_gate.count(), 1, "woken when the gate is free");
        let at_the_capture = poll_parks(&mut queue, &ticket);
        assert!(
            !target.join("MANIFEST").exists(),
            "nothing was captured without the step"
        );
        step.release();
        at_the_capture
    });
    poll_finishes(&mut queue, ticket, &idle).unwrap();
    let copy = Db::open(&target, Options::default()).unwrap();
    assert_eq!(copy.get(b"k").unwrap(), Some(b"v".to_vec()));
}

// ---- the steps writes leave owing -----------------------------------------------

/// A write that seals a memtable on a database with no worker leaves one
/// flush owing, on its thread's queue. The flush takes a file id and
/// installs its table through the step: with the step held the owed step
/// parks before it flushed anything, and runs once the step is free.
#[test]
fn an_owed_flush_on_a_poll_parks_while_another_thread_holds_the_step() {
    let dir = TempDir::new().unwrap();
    let options = Options::default()
        .max_background_compactions(0)
        .write_buffer_size(4 * 1024);
    let db = Db::open(dir.path(), options).unwrap();
    let mut queue = db.io_queue();
    let engine = db.engine();
    for i in 0..64u32 {
        if engine.frozen_memtables() > 0 {
            break;
        }
        db.put(format!("k{i:04}").as_bytes(), &[7u8; 1024]).unwrap();
    }
    assert_eq!(engine.frozen_memtables(), 1, "setup: one memtable sealed");
    assert_eq!(engine.total_file_count(), 0, "setup: the flush is owed");

    let idle = while_another_thread_holds(
        the_step(&db),
        || {
            let progress = queue.poll(IoBudget::ALL);
            assert!(progress.more_pending, "the step is still owed");
            assert_eq!(engine.frozen_memtables(), 1, "nothing was flushed");
            assert_eq!(engine.total_file_count(), 0);
            idle(&queue)
        },
        |idle| idle,
    );
    assert_eq!(idle.count(), 1, "woken once the step is free");
    queue.poll(IoBudget::ALL);
    assert_eq!(engine.frozen_memtables(), 0, "the owed flush ran");
    assert_eq!(engine.total_file_count(), 1);
}

/// A write a stall stopped leaves the step that relieves the stop on its
/// queue. That step is a compaction pass here, which installs through the
/// step: with the step held it parks, and the stopped write is still
/// waiting; once the step is free the pass runs and the write is told.
#[test]
fn a_stall_step_on_a_poll_parks_while_another_thread_holds_the_step() {
    let dir = TempDir::new().unwrap();
    let options = Options::default()
        .max_background_compactions(0)
        .inline_compaction(true)
        .l0_compaction_trigger(1)
        .level0_slowdown_writes_trigger(1)
        .level0_stop_writes_trigger(1);
    let db = Db::open(dir.path(), options).unwrap();
    db.put(b"m", b"in L0").unwrap();
    db.flush().unwrap();
    let engine = db.engine();
    assert_eq!(engine.level_file_count(0), 1, "setup: one table in L0");
    let mut queue = db.io_queue();
    let wait = match db.put(b"stopped", b"v") {
        Err(Error::WouldBlock(WouldBlock::Stall(wait))) => wait,
        other => panic!("setup: the write was not stopped: {other:?}"),
    };

    let idle = while_another_thread_holds(
        the_step(&db),
        || {
            let progress = queue.poll(IoBudget::ALL);
            assert!(progress.more_pending, "the stop still holds");
            assert!(!wait.is_ready());
            assert_eq!(engine.level_file_count(0), 1, "no pass ran");
            idle(&queue)
        },
        |idle| idle,
    );
    assert_eq!(idle.count(), 1, "woken once the step is free");
    for _ in 0..16 {
        queue.poll(IoBudget::ALL);
        if wait.is_ready() {
            break;
        }
    }
    assert!(
        wait.is_ready(),
        "the pass relieved the stop and told the write"
    );
    assert_eq!(engine.level_file_count(0), 0);
    db.put(b"stopped", b"v").unwrap();
}
