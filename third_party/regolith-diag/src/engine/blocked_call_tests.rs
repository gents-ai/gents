//! A call that blocks on a thread that owns a queue steps that queue's own
//! jobs before it sleeps, so it never waits for work only its own poll
//! would run (`IoRuntime::step_own`).
//!
//! The case: no worker, and this thread's queue holds a foreground job that
//! parked under the compaction gate. A transaction's commit on the same
//! thread meets a stop; the step that would relieve it needs the gate. The
//! thread is inside the commit, so it does not poll: unless the commit
//! steps the job itself, the two wait for each other.
//!
//! Each scenario runs on one thread, the queue's owner, while the test's
//! thread watches it with a deadline; a scenario that hangs is ended by
//! closing the database, so the test fails instead of hanging with it.
//! Nothing is timed.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use tempfile::TempDir;

use super::park_support::while_another_thread_holds;
use super::poll_park_tests::{poll_parks, the_step};
use crate::{IoBudget, OptimisticTransactionDb, Options, TxnOptions};

/// How long the test's thread waits for the scenario's before it calls the
/// wait a hang.
const DEADLINE: Duration = Duration::from_secs(60);

/// No worker, and one table in level 0 is both where writes stop and where
/// a compaction is due: one put and one flush bring the stop on, and one
/// pass relieves it.
fn stopped_at_one_table(dir: &Path) -> OptimisticTransactionDb {
    let db = OptimisticTransactionDb::open(
        dir,
        Options::default()
            .max_background_compactions(0)
            .l0_compaction_trigger(1)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(1),
    )
    .unwrap();
    db.db().put(b"k", b"v").unwrap();
    db.db().flush().unwrap();
    assert_eq!(db.db().engine().level_file_count(0), 1, "setup: the stop");
    db
}

/// Run `scenario` on a thread of its own and return what it returns. If it
/// has not returned by the deadline the database is closed, which ends
/// every wait in it, and the test fails.
fn on_the_owners_thread<T: Send>(
    db: &OptimisticTransactionDb,
    scenario: impl FnOnce() -> T + Send,
) -> T {
    let (done, is_done) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = done.send(scenario());
        });
        let outcome = is_done.recv_timeout(DEADLINE);
        if outcome.is_err() {
            let _ = db.db().close();
        }
        outcome.expect("the owner's thread was still blocked at the deadline")
    })
}

/// The scenario of the module docs. The commit steps the parked job, which
/// runs to its end and gives the gate back; the commit then goes through.
/// The job's ticket is delivered where it always is, at its queue's poll.
#[test]
fn a_commit_behind_a_stop_steps_its_own_queues_job_parked_under_the_gate() {
    let dir = TempDir::new().unwrap();
    let db = stopped_at_one_table(dir.path());
    let committed = on_the_owners_thread(&db, || {
        let mut queue = db.db().io_queue();
        let ticket = db.db().compact_range(None, None);
        while_another_thread_holds(
            the_step(db.db()),
            || {
                poll_parks(&mut queue, &ticket);
                assert!(
                    db.db().engine().compaction_gate().try_shared().is_none(),
                    "setup: the parked job holds the gate"
                );
            },
            |()| (),
        );
        // What the job parked for is free, and this thread does not poll.
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"committed", b"v").unwrap();
        let committed = tx.commit().map(|_| ()).map_err(|err| err.to_string());
        assert!(
            !ticket.is_ready(),
            "the job's ticket is delivered at its queue's poll, not by the commit"
        );
        queue.poll(IoBudget::ALL);
        assert!(ticket.is_ready(), "the job ran to its end");
        ticket.wait().unwrap();
        committed
    });
    committed.expect("the commit goes through");
    assert_eq!(
        db.db().get(b"committed").unwrap(),
        Some(b"v".to_vec()),
        "the commit's write is in"
    );
    assert_eq!(db.db().engine().level_file_count(0), 0, "the stop is gone");
}

/// Two jobs of one queue, parked: the first under the gate, the second
/// waiting for the gate behind it. A call that ran the second to its end in
/// one go would wait in it for the gate, which only the first gives back,
/// and the first is this thread's own to run. Stepped, the second parks
/// again, the first runs to its end, and the commit goes through.
#[test]
fn two_parked_jobs_of_one_queue_do_not_deadlock_a_blocked_commit() {
    let dir = TempDir::new().unwrap();
    let db = stopped_at_one_table(dir.path());
    let committed = on_the_owners_thread(&db, || {
        let mut queue = db.db().io_queue();
        let first = db.db().compact_range(None, None);
        let second = while_another_thread_holds(
            the_step(db.db()),
            || {
                poll_parks(&mut queue, &first);
                // Left on the queue behind the first and polled once: it
                // keeps its place among the gate's waiters.
                let second = db.db().compact_range(None, None);
                poll_parks(&mut queue, &second);
                second
            },
            |second| second,
        );
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"committed", b"v").unwrap();
        let committed = tx.commit().map(|_| ()).map_err(|err| err.to_string());
        for _ in 0..16 {
            queue.poll(IoBudget::ALL);
        }
        assert!(first.is_ready() && second.is_ready(), "both jobs end");
        first.wait().unwrap();
        second.wait().unwrap();
        committed
    });
    committed.expect("the commit goes through");
    assert_eq!(db.db().get(b"committed").unwrap(), Some(b"v".to_vec()));
}

/// A job on another thread's queue is that thread's to run. A commit
/// blocked behind a stop on this thread waits for it and gives it no step:
/// when the other queue is dropped its job is told it never ran, the gate
/// comes back, and the commit relieves the stop itself.
#[test]
fn a_blocked_commit_gives_no_step_to_a_job_on_another_threads_queue() {
    let dir = TempDir::new().unwrap();
    let db = stopped_at_one_table(dir.path());
    let (parked, is_parked) = mpsc::channel();
    let (drop_it, dropped) = mpsc::channel::<()>();
    let (done, is_done) = mpsc::channel();
    let db = &db;
    let (committed, never_ran) = std::thread::scope(|scope| {
        // The other thread: its queue holds a job parked under the gate,
        // and it does not poll.
        let other = scope.spawn(move || {
            let mut queue = db.db().io_queue();
            let ticket = db.db().compact_range(None, None);
            while_another_thread_holds(
                the_step(db.db()),
                || {
                    poll_parks(&mut queue, &ticket);
                },
                |()| (),
            );
            parked.send(()).unwrap();
            let _ = dropped.recv();
            drop(queue);
            ticket.wait()
        });
        is_parked
            .recv_timeout(DEADLINE)
            .expect("the other thread never parked its job");
        // The committing thread, with a queue of its own and nothing on it.
        scope.spawn(move || {
            let _queue = db.db().io_queue();
            let tx = db.begin(&TxnOptions::new());
            tx.put(b"committed", b"v").unwrap();
            let _ = done.send(tx.commit().map(|_| ()).map_err(|err| err.to_string()));
        });
        // The other queue is dropped only once the commit has met the stop
        // and installed the stall's unit: its pass over its own jobs
        // follows at once, and a pass that took the other queue's job would
        // run it to its end before that queue goes.
        let deadline = std::time::Instant::now() + DEADLINE;
        while !db.db().engine().stall_signal().has_unit() {
            if std::time::Instant::now() > deadline {
                let _ = db.db().close();
                panic!("the commit never met the stop");
            }
            std::thread::yield_now();
        }
        drop_it.send(()).unwrap();
        let committed = is_done.recv_timeout(DEADLINE);
        if committed.is_err() {
            let _ = db.db().close();
        }
        (
            committed.expect("the commit was still blocked at the deadline"),
            other.join().unwrap(),
        )
    });
    committed.expect("the commit goes through once the gate is back");
    let told = never_ran.expect_err("the other queue's job was run to its end");
    assert!(told.to_string().contains("dropped before it ran"), "{told}");
    assert_eq!(db.db().get(b"committed").unwrap(), Some(b"v".to_vec()));
}

/// A ticket's blocking wait, on a thread whose queue holds an older job
/// parked under the gate: the wait steps both, the older one to its end
/// first since it holds the gate, and returns the newer job's result.
/// Before, the newer job was run to its end in one go and waited in it for
/// a gate only this thread's own poll would have given back.
#[test]
fn a_tickets_wait_steps_an_older_job_of_the_same_queue_that_holds_the_gate() {
    let dir = TempDir::new().unwrap();
    let db = stopped_at_one_table(dir.path());
    let waited = on_the_owners_thread(&db, || {
        let mut queue = db.db().io_queue();
        let older = db.db().compact_range(None, None);
        while_another_thread_holds(
            the_step(db.db()),
            || {
                poll_parks(&mut queue, &older);
            },
            |()| (),
        );
        let newer = db.db().compact_range(None, None);
        let waited = newer.wait().map_err(|err| err.to_string());
        queue.poll(IoBudget::ALL);
        assert!(older.is_ready(), "the older job ran to its end too");
        older.wait().unwrap();
        waited
    });
    waited.expect("the newer job's wait returns its result");
    assert_eq!(db.db().engine().level_file_count(0), 0);
}

/// A plain write that blocks for a stall on a thread with a queue: the
/// step that relieves the stop was left on that queue, and the wait steps
/// it, where it used to sleep for a step only its own poll would run.
#[test]
fn a_plain_write_that_blocks_for_a_stall_runs_the_step_its_own_queue_holds() {
    let dir = TempDir::new().unwrap();
    let db = stopped_at_one_table(dir.path());
    let written = on_the_owners_thread(&db, || {
        let _queue = db.db().io_queue();
        crate::through_stalls(|| db.db().put(b"written", b"v")).map_err(|err| err.to_string())
    });
    written.expect("the write goes in once its own step relieved the stop");
    assert_eq!(db.db().get(b"written").unwrap(), Some(b"v".to_vec()));
}

/// The single-threaded configuration: no worker, an environment that starts
/// no thread, and one thread driving the database. A job left on its queue
/// and a stop: the commit is not handed the wait. It goes through, and the
/// stop is gone. (With one thread nothing parks a job under the gate, so
/// the commit's own step finds nothing in its way here.)
#[test]
fn on_one_thread_a_commit_behind_a_stop_goes_through_with_a_job_on_its_queue() {
    let db = OptimisticTransactionDb::open(
        Path::new("blocked-call-one-thread"),
        Options::memory()
            .max_background_compactions(0)
            .l0_compaction_trigger(1)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(1),
    )
    .unwrap();
    db.db().put(b"k", b"v").unwrap();
    db.db().flush().unwrap();
    assert_eq!(db.db().engine().level_file_count(0), 1, "setup: the stop");
    let committed = on_the_owners_thread(&db, || {
        let mut queue = db.db().io_queue();
        // Left on the queue and never polled.
        let ticket = db.db().compact_range(None, None);
        let tx = db.begin(&TxnOptions::new());
        tx.put(b"committed", b"v").unwrap();
        let committed = tx.commit().map(|_| ()).map_err(|err| err.to_string());
        queue.poll(IoBudget::ALL);
        ticket.wait().unwrap();
        committed
    });
    committed.expect("the commit goes through");
    assert_eq!(db.db().get(b"committed").unwrap(), Some(b"v".to_vec()));
    assert_eq!(db.db().engine().level_file_count(0), 0, "the stop is gone");
}
