//! What the unit table holds: a unit lives only while some queue owes its
//! read, so a finished read, a close and a dropped queue each leave the table
//! empty. A unit whose reopen found no open-file slot stays in it, parked,
//! until a freed slot sends it back to run (D60).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Wake, Waker};

use crate::engine::block_cache::BlockCache;
use crate::engine::io::scope;
use crate::engine::io::unit::{Ran, UnitKey, Work};
use crate::env::open_file_limit::slots::SlotWaiter;
use crate::{Db, Error, IoBudget, IoWait, Options, ReadMode, WouldBlock};

/// A database whose keys all live in tables, reopened with a cold cache.
fn cold() -> (tempfile::TempDir, Db) {
    let dir = tempfile::TempDir::new().unwrap();
    let opts = || Options::default().block_size(256);
    {
        let db = Db::open(dir.path(), opts()).unwrap();
        for i in 0..2_000u32 {
            db.put(&key(i), format!("value-{i}").as_bytes()).unwrap();
        }
        db.flush().unwrap();
        db.close().unwrap();
    }
    let db = Db::open(dir.path(), opts()).unwrap();
    (dir, db)
}

fn key(i: u32) -> Vec<u8> {
    format!("k{i:05}").into_bytes()
}

fn missed(read: crate::Result<Option<Vec<u8>>>) {
    assert!(
        matches!(read, Err(Error::WouldBlock(WouldBlock::Io(_)))),
        "{read:?}"
    );
}

#[test]
fn a_finished_read_leaves_no_unit_behind() {
    let (_dir, db) = cold();
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    missed(snapshot.get(&key(700)));
    missed(snapshot.get(&key(1_300)));
    assert_eq!(queue.runtime().units_in_flight(), 2);
    queue.poll(IoBudget::ALL);
    assert_eq!(queue.runtime().units_in_flight(), 0);
    assert_eq!(queue.pending_bytes(), 0);
}

#[test]
fn close_leaves_no_unit_behind() {
    let (_dir, db) = cold();
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    missed(snapshot.get(&key(700)));
    queue.poll(IoBudget::units(0));
    missed(snapshot.get(&key(1_300)));
    db.close().unwrap();
    assert_eq!(queue.runtime().units_in_flight(), 0);
    queue.poll(IoBudget::ALL);
    assert_eq!(queue.pending_bytes(), 0);
}

#[test]
fn a_dropped_queue_leaves_no_unit_behind() {
    let (_dir, db) = cold();
    let watcher = db.io_queue();
    let queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    missed(snapshot.get(&key(700)));
    let mut queue = queue;
    queue.poll(IoBudget::units(0));
    missed(snapshot.get(&key(1_300)));
    drop(queue);
    assert_eq!(watcher.runtime().units_in_flight(), 0);
}

/// A job a queue owns is listed until it lands, run or settled: the step a
/// write leaves owing is on the table until the queue's owner polls.
#[test]
fn a_job_a_queue_owns_is_listed_until_it_lands() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .max_background_compactions(0)
            .write_buffer_size(4 * 1024),
    )
    .unwrap();
    let mut queue = db.io_queue();
    let runtime = queue.runtime();
    let mut seen = false;
    for i in 0..64u32 {
        crate::through_stalls(|| db.put(&key(i), &[7u8; 512])).unwrap();
        seen |= runtime.jobs_in_flight() == 1;
    }
    assert!(
        seen,
        "a write that sealed a memtable left its flush on the queue"
    );
    assert!(
        runtime.jobs_in_flight() <= 1,
        "one owed job waits at a time"
    );
    while queue.poll(IoBudget::ALL).more_pending {}
    assert_eq!(queue.runtime().jobs_in_flight(), 0);
}

/// Close settles every job a queue owns before that queue polls again: the
/// job leaves the table at close, unrun, and the poll after it only delivers
/// the `Closed` the close decided.
#[test]
fn close_settles_a_job_a_queue_owns_before_its_poll() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default().max_background_compactions(0)).unwrap();
    db.put(b"a", b"1").unwrap();
    db.flush().unwrap();
    db.put(b"b", b"2").unwrap();
    db.flush().unwrap();
    let mut queue = db.io_queue();
    let runtime = queue.runtime();
    let job = db.compact_range(None, None);
    assert_eq!(runtime.jobs_in_flight(), 1, "the job waits for the poll");
    db.close().unwrap();
    assert_eq!(runtime.jobs_in_flight(), 0, "close settled it");
    assert!(!job.is_ready(), "the poll delivers it");
    queue.poll(IoBudget::ALL);
    assert!(matches!(job.wait(), Err(Error::Closed)));
}

/// A read that checked the database open before close and misses after it,
/// the window the close gate shuts, answers `Closed` and leaves no unit; a
/// close that failed reopens the table to misses.
#[test]
fn a_miss_after_close_answers_closed_and_leaves_no_unit() {
    use crate::engine::io::unit::{UnitKey, Work};

    let (_dir, db) = cold();
    let queue = db.io_queue();
    let runtime = queue.runtime();
    let key = UnitKey {
        file_id: 1,
        offset: 0,
        guard: None,
    };
    let work =
        || -> std::io::Result<Work> { Ok(Box::new(|_| Err(std::io::Error::other("never read")))) };
    runtime.close();
    let Err(err) = runtime.miss(queue.id(), key, 1, work) else {
        panic!("a closed table answered a miss");
    };
    assert!(matches!(Error::from(err), Error::Closed));
    assert_eq!(runtime.units_in_flight(), 0);
    runtime.reopen();
    let Err(err) = runtime.miss(queue.id(), key, 1, work) else {
        panic!("a miss answered without a read");
    };
    assert!(matches!(Error::from(err), Error::WouldBlock(_)));
    assert_eq!(runtime.units_in_flight(), 1);
}

const PARKING_KEY: UnitKey = UnitKey {
    file_id: 1,
    offset: 0,
    guard: None,
};

/// A unit whose first run finds no open-file slot and parks, and whose
/// later runs read (and fail, which completes the read all the same),
/// counting its runs.
fn parking_work(runs: &Arc<AtomicUsize>) -> impl Fn() -> std::io::Result<Work> + '_ {
    move || {
        let runs = Arc::clone(runs);
        Ok(Box::new(move |_| {
            if runs.fetch_add(1, Ordering::SeqCst) == 0 {
                scope::note_parked();
                Err(std::io::Error::new(
                    std::io::ErrorKind::ResourceBusy,
                    "no open-file slot",
                ))
            } else {
                Err(std::io::Error::other("the device read"))
            }
        }))
    }
}

fn would_block(err: std::io::Error) -> IoWait {
    match Error::from(err) {
        Error::WouldBlock(WouldBlock::Io(wait)) => wait,
        other => panic!("expected WouldBlock, got {other:?}"),
    }
}

struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A parked unit stays in the table and is not run again, however often
/// its owner polls, and an idle owner is not woken for it, until a slot
/// frees: that wakes the idle owner once, and its next poll runs the unit
/// and completes the read.
#[test]
fn a_parked_unit_runs_again_only_once_a_slot_frees() {
    let (_dir, db) = cold();
    let mut queue = db.io_queue();
    let runs = Arc::new(AtomicUsize::new(0));
    let Err(err) = queue
        .runtime()
        .miss(queue.id(), PARKING_KEY, 1, parking_work(&runs))
    else {
        panic!("a miss answered without a read");
    };
    let wait = would_block(err);
    let progress = queue.poll(IoBudget::ALL);
    assert_eq!(progress.completed, 0);
    assert!(progress.more_pending);
    assert!(!wait.is_ready());
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let unit = queue
        .runtime()
        .unit(&PARKING_KEY)
        .expect("a parked unit stays in the table");
    assert!(unit.is_parked());
    queue.poll(IoBudget::ALL);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "a parked unit waits for a slot"
    );
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    queue.idle_waker(&Waker::from(Arc::clone(&wakes)));
    assert_eq!(
        wakes.0.load(Ordering::SeqCst),
        0,
        "nothing to run: the owner idles"
    );

    queue.shared().slot_freed();
    assert_eq!(
        wakes.0.load(Ordering::SeqCst),
        1,
        "the freed slot woke the idle owner"
    );
    let progress = queue.poll(IoBudget::ALL);
    assert_eq!(progress.completed, 1);
    assert!(!progress.more_pending);
    assert!(wait.is_ready());
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(queue.runtime().units_in_flight(), 0);
    assert_eq!(queue.pending_bytes(), 0);
}

/// Close sweeps the table while a poll holds a unit claimed; the poll's run
/// then parks it. The run sees close's mark and lets the unit go itself, so
/// no parked unit outlives close.
#[test]
fn a_park_that_lands_after_close_swept_is_released_by_its_run() {
    let (_dir, db) = cold();
    let mut queue = db.io_queue();
    let runs = Arc::new(AtomicUsize::new(0));
    let Err(err) = queue
        .runtime()
        .miss(queue.id(), PARKING_KEY, 1, parking_work(&runs))
    else {
        panic!("a miss answered without a read");
    };
    let wait = would_block(err);
    {
        let runtime = queue.runtime();
        let unit = runtime.unit(&PARKING_KEY).unwrap();
        assert!(unit.claim(), "a poll claimed it before close swept");
        runtime.close();
        assert_eq!(
            runtime.units_in_flight(),
            1,
            "the sweep leaves a claimed unit to its runner"
        );
        let ran = runtime.run(&unit, queue.shared(), &BlockCache::new(0));
        assert_eq!(
            ran,
            Ran::Finished,
            "the run parked after close, and let the unit go"
        );
        assert!(unit.is_done());
        assert_eq!(runtime.units_in_flight(), 0);
    }
    assert_eq!(queue.poll(IoBudget::ALL).completed, 1);
    assert!(wait.is_ready());
    queue.runtime().reopen();
}

/// A job that parks while `open` is unset, keeping the waker its step was
/// given, and counts its runs.
struct GatedJob {
    open: Arc<std::sync::atomic::AtomicBool>,
    runs: Arc<AtomicUsize>,
    waker: Arc<std::sync::Mutex<Option<Waker>>>,
}

impl crate::engine::io::job::JobBody for GatedJob {
    fn run(self: Box<Self>) {
        self.runs.fetch_add(1, Ordering::SeqCst);
    }

    fn step(self: Box<Self>, waker: &Waker) -> crate::engine::io::job::Step {
        if self.open.load(Ordering::SeqCst) {
            self.run();
            return crate::engine::io::job::Step::Done;
        }
        *self.waker.lock().unwrap() = Some(waker.clone());
        crate::engine::io::job::Step::Parked(self)
    }

    fn release(self: Box<Self>) {}
}

/// One queue holds a read parked for an open-file slot (closed to claims
/// until a slot frees, D60) and a job parked for something else (free to
/// claim again once its waker says so). Each goes on for its own cause
/// only, in either order: a freed slot runs the read and leaves the job
/// parked, and the job's waker runs the job and leaves the read parked.
#[test]
fn a_parked_read_and_a_parked_job_on_one_queue_each_wake_for_their_own_cause() {
    for slot_first in [true, false] {
        let (_dir, db) = cold();
        let mut queue = db.io_queue();
        let reads = Arc::new(AtomicUsize::new(0));
        let Err(err) = queue
            .runtime()
            .miss(queue.id(), PARKING_KEY, 1, parking_work(&reads))
        else {
            panic!("a miss answered without a read");
        };
        let wait = would_block(err);
        let open = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let job_runs = Arc::new(AtomicUsize::new(0));
        let waker = Arc::new(std::sync::Mutex::new(None));
        let job = crate::engine::io::job::Job::new(Box::new(GatedJob {
            open: Arc::clone(&open),
            runs: Arc::clone(&job_runs),
            waker: Arc::clone(&waker),
        }));
        assert!(
            queue
                .runtime()
                .submit(queue.shared(), Arc::clone(&job), None)
                .is_ok()
        );

        let progress = queue.poll(IoBudget::ALL);
        assert_eq!(progress.completed, 0);
        assert!(progress.more_pending, "both are still owed");
        assert_eq!(reads.load(Ordering::SeqCst), 1, "the read ran and parked");
        assert_eq!(job_runs.load(Ordering::SeqCst), 0, "the job parked");
        queue.poll(IoBudget::ALL);
        assert_eq!(reads.load(Ordering::SeqCst), 1, "a parked read is not run");

        let free_the_slot = |queue: &mut crate::IoQueue| {
            queue.shared().slot_freed();
            queue.poll(IoBudget::ALL);
        };
        let wake_the_job = |queue: &mut crate::IoQueue| {
            open.store(true, Ordering::SeqCst);
            waker
                .lock()
                .unwrap()
                .take()
                .expect("the parked job kept its waker")
                .wake();
            queue.poll(IoBudget::ALL);
        };
        if slot_first {
            free_the_slot(&mut queue);
            assert!(wait.is_ready(), "the freed slot ran the read");
            assert_eq!(job_runs.load(Ordering::SeqCst), 0, "and not the job");
            assert!(!job.is_done());
            wake_the_job(&mut queue);
        } else {
            wake_the_job(&mut queue);
            assert_eq!(job_runs.load(Ordering::SeqCst), 1, "its waker ran the job");
            assert!(!wait.is_ready(), "and not the read");
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            free_the_slot(&mut queue);
        }
        assert!(wait.is_ready());
        assert_eq!(reads.load(Ordering::SeqCst), 2, "the read ran once more");
        assert_eq!(job_runs.load(Ordering::SeqCst), 1, "the job ran once");
        assert!(job.is_done());
        assert_eq!(queue.runtime().units_in_flight(), 0);
        assert_eq!(queue.runtime().jobs_in_flight(), 0);
    }
}
