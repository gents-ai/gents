//! A job of the commit pipeline that a queue's poll steps parks instead of
//! waiting for another thread (`park.rs`): for a round, a unit, the version
//! store's step, the flush exclusion and a failed log's writes in flight.
//!
//! Each test puts the thing in the job's way by hand, as a thread in the
//! middle of its work holds it, steps the job as a poll does, and checks
//! the four rules: the step returns without the thing (it did not wait),
//! the job holds nothing (K1), the end of the wait wakes it (K2), and the
//! next step finishes the work. Nothing is timed: a step that waited would
//! never return, since what it would wait for is released only after it.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::Waker;
use std::thread;

use tempfile::TempDir;

use super::super::io::job::Job;
use super::super::park_support::{counting, while_another_thread_holds};
use super::super::{EngineOptions, RegolithEngine};
use super::descriptor::Work;
use super::ordered::Ordered;
use super::park::{Help, Needs};
use super::pipeline_tests::{durable_put, eventual_put, install, key, seq_of};
use crate::{IoBudget, OptimisticTransactionDb, Options, TxnOptions};

/// Step `job` as a queue's poll does once its claim won. `false` when the
/// job parked.
/// No worker. A test here has another thread hold the one thing a unit
/// needs, at a moment the test chooses; a worker takes the flush exclusion
/// and the version store's step on its own, at moments it chooses.
fn open_engine(dir: &TempDir) -> Arc<RegolithEngine> {
    RegolithEngine::open(
        dir.path(),
        EngineOptions {
            max_background_compactions: 0,
            ..EngineOptions::default()
        },
    )
    .expect("engine open")
}

fn poll_step(job: &Arc<Job>, waker: &Waker) -> bool {
    assert!(job.claim(), "nobody else has the job");
    job.step(waker)
}

/// Put a unit in the ring whose body may wait for `needs` and runs `body`,
/// and bring it no further: the thread that asked for it stopped. Returns
/// its slot and its job.
fn unit(
    engine: &RegolithEngine,
    needs: Needs,
    body: impl FnOnce(&RegolithEngine, &mut Ordered<'_>) + Send + 'static,
) -> (u64, Arc<Job>) {
    let desc = engine.unit(needs, Box::new(body));
    let index = engine.enter(engine.pipe().unwrap(), &desc);
    let Work::Unit { job, .. } = &desc.work else {
        unreachable!("a unit's descriptor holds a unit");
    };
    (index, Arc::clone(job))
}

/// A unit whose body counts its runs in `ran`.
fn counted_unit(engine: &RegolithEngine, needs: Needs, ran: &Arc<AtomicUsize>) -> (u64, Arc<Job>) {
    let ran = Arc::clone(ran);
    unit(engine, needs, move |_, _| {
        ran.fetch_add(1, Ordering::SeqCst);
    })
}

/// K1 to K4 for a round another thread claimed: the unit drains as far as
/// it goes, parks when the commit before it needs a sync it may not wait
/// for, and goes on when the round ends.
#[test]
fn a_unit_on_a_poll_parks_for_a_round_another_thread_has_and_goes_on_when_it_ends() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    let (first_index, first) = install(&engine, durable_put(b"first", b"v"));
    let ran = Arc::new(AtomicUsize::new(0));
    let (unit_index, job) = counted_unit(&engine, Needs::NONE, &ran);
    let (wakes, waker) = counting();

    // As a thread in the middle of the log's device step holds it.
    let held = engine.hold_round(pipe, Help::Wait);
    assert!(
        !poll_step(&job, &waker),
        "the unit ran under a round it did not hold"
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert!(job.is_runnable(), "K1: the claim is given back");
    // It helped as far as helping goes: the commit is decided and written,
    // and waits for the round like the unit.
    assert_eq!(pipe.ring.decided(), first_index);
    assert_eq!(pipe.ring.published(), first_index - 1);
    assert_eq!(wakes.count(), 0, "woken before the round ended");

    drop(held);
    assert_eq!(wakes.count(), 1, "K2: the round's end wakes the unit");

    let (_, again) = counting();
    assert!(poll_step(&job, &again));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(pipe.ring.decided(), unit_index);
    assert_eq!(seq_of(&first).unwrap(), engine.snapshot_seq());
}

/// The rotation check of a write's turn is a job of the same kind: under a
/// held round it parks before it seals anything, and rotates when woken.
#[test]
fn a_rotation_check_on_a_poll_parks_for_a_held_round_and_rotates_when_woken() {
    let dir = TempDir::new().unwrap();
    let engine = RegolithEngine::open(
        dir.path(),
        EngineOptions {
            write_buffer_size: 1024,
            max_background_compactions: 0,
            ..EngineOptions::default()
        },
    )
    .unwrap();
    let pipe = engine.pipe().unwrap();
    // One write past the buffer size: the next write's turn begins with
    // the check.
    engine
        .submit(eventual_put(b"filler", &[7u8; 2048]))
        .unwrap();
    let (index, write) = install(&engine, eventual_put(b"next", b"v"));
    // What a `commit_nowait` call is handed and leaves on its queue.
    let super::pipeline::Turn::Blocked(check) = engine.help_decide(pipe, index, Help::Leave, None)
    else {
        panic!("the rotation check was not due");
    };
    let (wakes, waker) = counting();

    let held = engine.hold_round(pipe, Help::Wait);
    assert!(
        !poll_step(&check, &waker),
        "the check ran under a held round"
    );
    assert_eq!(engine.frozen_memtables(), 0, "K1: nothing was sealed");
    assert!(check.is_runnable());
    drop(held);
    assert_eq!(wakes.count(), 1);

    let (_, again) = counting();
    assert!(poll_step(&check, &again));
    assert_eq!(engine.frozen_memtables(), 1, "the memtable was sealed");
    engine.drive(pipe, index, &write);
    seq_of(&write).unwrap();
    assert_eq!(
        engine.get(&key(b"next"), u64::MAX).unwrap(),
        Some(b"v".to_vec())
    );
}

/// The decision of a slot left to a thread that reads the device parks on a
/// unit in the slot's way that another thread is running, and decides the
/// slot once that unit landed.
#[test]
fn a_slot_decision_on_a_poll_parks_for_a_unit_another_thread_runs() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    let ran = Arc::new(AtomicUsize::new(0));
    let (_, unit_job) = counted_unit(&engine, Needs::NONE, &ran);
    let (index, write) = install(&engine, eventual_put(b"k", b"v"));
    let decision = engine.decide_unit(index);
    let (wakes, waker) = counting();

    // Another thread claimed the unit and is running it.
    assert!(unit_job.claim());
    assert!(!poll_step(&decision, &waker), "the decision passed a unit");
    assert!(desc_undecided(&write), "the slot was decided past the unit");
    assert_eq!(wakes.count(), 0);
    engine.io().run_job(&unit_job);
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(
        wakes.count(),
        1,
        "K2: the unit's landing wakes the decision"
    );

    let (_, again) = counting();
    assert!(poll_step(&decision, &again));
    assert!(!desc_undecided(&write));
    engine.drive(pipe, index, &write);
    seq_of(&write).unwrap();
}

fn desc_undecided(desc: &super::descriptor::Descriptor) -> bool {
    desc.decision().is_none()
}

/// K3 for the version store's step: a unit that said its body changes the
/// version set does not begin while another thread holds the step, and its
/// body then finds the step held by its own thread.
#[test]
fn a_unit_that_changes_the_version_set_parks_while_another_thread_holds_the_step() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    let took = Arc::new(AtomicUsize::new(0));
    let body_took = Arc::clone(&took);
    let (index, job) = unit(&engine, Needs::VERSIONS, move |engine, _| {
        let id = engine.versions.take_file_id().expect("a file id");
        body_took.store(id as usize, Ordering::SeqCst);
    });
    let (wakes, waker) = counting();

    while_another_thread_holds(
        || {
            engine
                .versions
                .hold_or_park(Waker::noop())
                .expect("the step is free")
        },
        || {
            assert!(!poll_step(&job, &waker), "the body began without the step");
            assert_eq!(took.load(Ordering::SeqCst), 0);
            assert!(job.is_runnable(), "K1: the claim is given back");
            assert!(
                engine.current_round(pipe).job.is_runnable(),
                "K1: the round the unit held is let go"
            );
            assert_eq!(wakes.count(), 0);
        },
        |()| assert_eq!(wakes.count(), 1, "K2: giving the step up wakes the unit"),
    );

    let (_, again) = counting();
    assert!(poll_step(&job, &again));
    assert_ne!(took.load(Ordering::SeqCst), 0, "the body took a file id");
    assert_eq!(pipe.ring.decided(), index);
    assert!(!engine.versions.has_waiting());
}

/// K3 for the flush exclusion: a unit that said its body flushes does not
/// begin while a flush is in flight, and its body then takes the exclusion
/// again without waiting.
#[test]
fn a_unit_that_flushes_parks_while_a_flush_is_in_flight() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    let ran = Arc::new(AtomicUsize::new(0));
    let body_ran = Arc::clone(&ran);
    let (index, job) = unit(&engine, Needs::FLUSH, move |engine, _| {
        // As a rotation at the memtable cap takes it, inside the body.
        let flushing = engine.flusher.flushing().expect("the exclusion");
        engine
            .flusher
            .flush_oldest(&flushing)
            .expect("nothing to flush");
        engine.versions.take_file_id().expect("a file id");
        body_ran.fetch_add(1, Ordering::SeqCst);
    });
    let (wakes, waker) = counting();

    while_another_thread_holds(
        || engine.flusher.flushing().expect("no flush is in flight"),
        || {
            assert!(!poll_step(&job, &waker), "the body began beside a flush");
            assert_eq!(ran.load(Ordering::SeqCst), 0);
            assert!(job.is_runnable());
            assert_eq!(wakes.count(), 0);
        },
        |()| assert_eq!(wakes.count(), 1, "K2: the flush's end wakes the unit"),
    );

    let (_, again) = counting();
    assert!(poll_step(&job, &again));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(pipe.ring.decided(), index);
    assert!(
        engine.flusher.try_flushing().is_some(),
        "the exclusion is free again"
    );
}

/// A round a poll claimed finds the log failed, with a write of a commit to
/// discard still on its way to the device. A blocking caller waits that
/// write out; the poll's round parks, having only turned later writes away,
/// and rolls back when the write has left.
#[test]
fn a_round_on_a_poll_parks_for_a_write_in_flight_on_a_failed_log() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    engine.submit(durable_put(b"kept", b"v")).unwrap();
    let tip = pipe.tip.load();
    let log = Arc::clone(&tip.after.log);
    let in_flight = log.write_in_flight();
    log.poison(io::Error::other("the device said no"));
    let round = engine.current_round(pipe);
    let (wakes, waker) = counting();

    assert!(
        !poll_step(&round.job, &waker),
        "the round cut the log under a write in flight"
    );
    assert!(log.is_poisoned(), "K1: nothing is rolled back yet");
    assert!(round.job.is_runnable());
    assert!(!pipe.ring.is_deciding_frozen(), "K1: nothing is frozen");
    assert_eq!(
        log.epoch(),
        tip.after.epoch + 1,
        "later writes of the commits to discard are turned away"
    );
    assert_eq!(wakes.count(), 0);

    drop(in_flight);
    assert_eq!(
        wakes.count(),
        1,
        "K2: the last write to leave wakes the round"
    );

    let (_, again) = counting();
    assert!(poll_step(&round.job, &again));
    assert!(!log.is_poisoned(), "the round rolled the log back");
    engine
        .submit(durable_put(b"after", b"v"))
        .expect("the pipeline goes on");
    assert_eq!(
        engine.get(&key(b"kept"), u64::MAX).unwrap(),
        Some(b"v".to_vec())
    );
}

/// The same wait met two jobs deep: a unit's drain claims the round, which
/// nobody has, and steps it inside its own step. The round parks for the
/// write in flight, so the unit parks with it, and the one wake serves
/// both: the unit's next step claims the round again.
#[test]
fn a_round_stepped_inside_a_units_step_parks_both_and_one_wake_serves_both() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    engine.submit(durable_put(b"kept", b"v")).unwrap();
    let log = Arc::clone(&pipe.tip.load().after.log);
    let in_flight = log.write_in_flight();
    let (_, doomed) = install(&engine, eventual_put(b"doomed", b"v"));
    log.poison(io::Error::other("the device said no"));
    let ran = Arc::new(AtomicUsize::new(0));
    let (index, job) = counted_unit(&engine, Needs::NONE, &ran);
    let round = engine.current_round(pipe);
    let (wakes, waker) = counting();

    assert!(!poll_step(&job, &waker), "the unit ran over a failed log");
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert!(job.is_runnable(), "K1: the unit's claim is given back");
    assert!(round.job.is_runnable(), "K1: and the round's");
    assert!(log.is_poisoned());
    assert_eq!(wakes.count(), 0);

    drop(in_flight);
    assert_eq!(wakes.count(), 1, "K2: one wake, for the unit that parked");

    let (_, again) = counting();
    assert!(poll_step(&job, &again));
    assert!(!log.is_poisoned(), "the round, claimed again, rolled back");
    seq_of(&doomed).expect_err("the commit decided over the failed log was discarded");
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(pipe.ring.decided(), index);
}

/// K4: a thread blocked on a unit is not left waiting for a poll that
/// parked it. Whichever came first, the park or the block, the thread
/// claims the unit and runs it as a blocking caller does.
#[test]
fn a_thread_blocked_on_a_unit_runs_it_once_the_poll_that_had_it_parks() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    let (_, first) = install(&engine, durable_put(b"first", b"v"));
    let ran_on = Arc::new(std::sync::Mutex::new(None));
    let body_ran_on = Arc::clone(&ran_on);
    let (_, job) = unit(&engine, Needs::NONE, move |_, _| {
        *body_ran_on.lock().unwrap() = Some(thread::current().id());
    });
    let held = engine.hold_round(pipe, Help::Wait);
    let (_, waker) = counting();

    // The poll has the unit when the other thread arrives.
    assert!(job.claim());
    let blocked = thread::scope(|scope| {
        let blocked = scope.spawn(|| {
            engine.run_or_wait(&job);
            thread::current().id()
        });
        assert!(
            !job.step(&waker),
            "the poll's unit parks under a held round"
        );
        // The blocked thread claims the unit and, a blocking caller, waits
        // for the round this thread still holds.
        drop(held);
        blocked.join().unwrap()
    });
    assert!(job.is_done());
    assert_eq!(*ran_on.lock().unwrap(), Some(blocked));
    seq_of(&first).unwrap();
}

/// K3's check: a body that takes something its unit did not say fails the
/// poll that ran it, in a build with debug assertions, naming the path.
/// Nobody holds the step here, so nothing waits this time: the check is on
/// the path, and does not need a race to be lost while the test runs.
#[cfg(debug_assertions)]
#[test]
fn a_body_that_takes_what_its_unit_did_not_say_fails_the_poll() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    // The body takes a file id, which is a section of the version store's
    // step, and the unit says it needs nothing.
    let (_, job) = unit(&engine, Needs::NONE, |engine, _| {
        engine.versions.take_file_id().expect("a file id");
    });
    let (_, waker) = counting();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| poll_step(&job, &waker)));
    let panic = failed.expect_err("a poll that took a path that waits for another thread passed");
    let message = panic
        .downcast_ref::<String>()
        .expect("the check panics with a message");
    assert!(
        message.contains("a queue's poll took a path that waits for another thread"),
        "{message}"
    );
    assert!(message.contains("the version store's step"), "{message}");
    assert!(job.is_done(), "the unit's job landed all the same");
}

/// The same body in a unit that says what it takes passes: the poll held the
/// step for it.
#[test]
fn a_body_that_takes_what_its_unit_said_passes_the_poll() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let (_, job) = unit(&engine, Needs::VERSIONS, |engine, _| {
        engine.versions.take_file_id().expect("a file id");
    });
    let (_, waker) = counting();
    assert!(poll_step(&job, &waker), "the unit ran to its end");
}

/// The whole path through the public API. A `commit_nowait` commit whose
/// turn begins with the rotation check leaves the check on its queue. The
/// poll that runs it finds the version store's step held by another thread
/// and returns: the owner is free to do other work, is woken when the step
/// is given up, and its next poll rotates and lands the commit.
#[test]
fn a_poll_returns_while_another_thread_holds_what_its_rotation_needs() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default()
            .write_buffer_size(1024)
            .max_background_compactions(0),
    )
    .unwrap();
    let engine = Arc::clone(&db.db().engine);
    // Before this thread has a queue, so that what the first write owes the
    // background (a database with no worker checks its disk once) runs in
    // the call and is not a job the poll below would complete.
    db.db().put(b"filler", &[7u8; 2048]).unwrap();
    let mut queue = db.db().io_queue();
    let (idle, idle_waker) = counting();

    let ticket = while_another_thread_holds(
        || {
            engine
                .versions
                .hold_or_park(Waker::noop())
                .expect("the step is free")
        },
        || {
            let txn = db.begin(&TxnOptions::new().io_queue(queue.id()));
            txn.put(b"k", b"v").unwrap();
            let ticket = txn.commit_nowait();
            assert!(!ticket.is_ready(), "the commit waits for the rotation");
            // Had the poll waited for the step, it would never return: the
            // other thread lets go only after it.
            let progress = queue.poll(IoBudget::ALL);
            assert_eq!(progress.completed, 0);
            assert!(progress.more_pending, "the commit is still owed");
            assert!(!ticket.is_ready());
            assert_eq!(engine.frozen_memtables(), 0, "nothing was sealed");
            queue.idle_waker(&idle_waker);
            assert_eq!(idle.count(), 0, "a parked check is no work to poll for");
            ticket
        },
        |ticket| ticket,
    );
    assert_eq!(
        idle.count(),
        1,
        "the idle owner is woken once the step is free"
    );

    queue.poll(IoBudget::ALL);
    assert!(
        ticket.is_ready(),
        "the woken poll rotated and landed the commit"
    );
    assert_eq!(engine.frozen_memtables(), 1);
    assert_eq!(db.db().get(b"k").unwrap(), Some(b"v".to_vec()));
}
