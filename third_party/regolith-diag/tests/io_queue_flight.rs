//! The per-thread I/O queues themselves (plan 4.10, D53): single-flight
//! units, completions that reach only the queue that asked, idle wakeups
//! that reach an idle owner once and a busy owner never, `close`, the byte
//! bound, and one thread finishing everything on its own.
//!
//! What touches the device is counted at the `Env`, and a device read can be
//! held there, so the interleavings below are forced rather than hoped for.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::task::{Context, Poll, Wake, Waker};

use common::device_env::{DeviceEnv, cold_db};
use regolith::{Db, Error, IoBudget, IoQueue, IoWait, Options, ReadMode, Snapshot, WouldBlock};

/// Keys written, `k00000`..`k01999`, about eight to a 256-byte block.
const KEYS: usize = 2_000;

fn key(i: usize) -> Vec<u8> {
    format!("k{i:05}").into_bytes()
}

fn value(i: usize) -> Vec<u8> {
    format!("value-{i:05}").into_bytes()
}

fn options(env: Arc<DeviceEnv>) -> Options {
    Options::default().env(env).block_size(256)
}

fn fill(db: &Db) {
    for i in 0..KEYS {
        db.put(&key(i), &value(i)).unwrap();
    }
}

/// A cold database with every key in tables.
fn cold() -> (tempfile::TempDir, Db, Arc<common::device_env::Device>) {
    cold_db(options, fill)
}

fn cache_only(db: &Db, queue: &IoQueue) -> Snapshot {
    db.snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()))
}

/// The wait a cold read returns.
fn wait_of(read: Result<Option<Vec<u8>>, Error>) -> IoWait {
    match read {
        Err(Error::WouldBlock(WouldBlock::Io(wait))) => wait,
        other => panic!("expected a wait, got {other:?}"),
    }
}

/// A waker that counts its wakes.
#[derive(Default)]
struct Counter(AtomicUsize);

impl Wake for Counter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Counter {
    fn waker() -> (Arc<Self>, Waker) {
        let counter = Arc::new(Self::default());
        (Arc::clone(&counter), Waker::from(counter))
    }

    fn wakes(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

#[test]
fn many_queues_that_miss_one_block_read_it_once() {
    const THREADS: usize = 8;
    let (_dir, db, device) = cold();
    let db = Arc::new(db);
    let missed = Arc::new(Barrier::new(THREADS + 1));
    let go = Arc::new(Barrier::new(THREADS + 1));
    let target = 1_234;
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let (db, missed, go) = (Arc::clone(&db), Arc::clone(&missed), Arc::clone(&go));
            std::thread::spawn(move || {
                let mut queue = db.io_queue();
                let snapshot = cache_only(&db, &queue);
                let wait = wait_of(snapshot.get(&key(target)));
                missed.wait();
                go.wait();
                while !wait.is_ready() {
                    queue.poll(IoBudget::ALL);
                    std::thread::yield_now();
                }
                let found = snapshot.get(&key(target)).unwrap();
                (wait.unit(), found)
            })
        })
        .collect();
    missed.wait();
    let before = device.reads();
    go.wait();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        device.reads() - before,
        1,
        "{THREADS} queues missed one block: it is read once"
    );
    let unit = results[0].0;
    assert!(unit.is_some());
    for (named, found) in results {
        assert_eq!(named, unit, "every miss of the block names one unit");
        assert_eq!(found, Some(value(target)));
    }
}

#[test]
fn a_completion_waits_in_its_owners_queue_until_the_owner_polls() {
    let (_dir, db, device) = cold();
    let mut a = db.io_queue();
    let mut b = db.io_queue();
    let wait_a = wait_of(cache_only(&db, &a).get(&key(700)));
    let wait_b = wait_of(cache_only(&db, &b).get(&key(700)));
    assert_eq!(wait_a.unit(), wait_b.unit());
    assert_eq!(wait_a.queue(), a.id());
    assert_eq!(wait_b.queue(), b.id());
    // A registers on the unit without running it.
    let progress = a.poll(IoBudget::units(0));
    assert_eq!(progress.completed, 0);
    assert!(progress.more_pending);
    // B runs the one read, and its own wait is complete.
    let before = device.reads();
    let progress = b.poll(IoBudget::ALL);
    assert_eq!(device.reads() - before, 1);
    assert_eq!(progress.completed, 1);
    assert!(wait_b.is_ready());
    // A's completion is in A's queue, not yet delivered: only A's poll does.
    assert!(!wait_a.is_ready(), "B's poll completed A's wait");
    let progress = a.poll(IoBudget::units(0));
    assert_eq!(progress.completed, 1);
    assert!(!progress.more_pending);
    assert!(wait_a.is_ready());
    assert_eq!(device.reads() - before, 1, "A did not read it again");
    assert_eq!(
        cache_only(&db, &a).get(&key(700)).unwrap(),
        Some(value(700))
    );
}

#[test]
fn a_queue_runs_only_the_reads_it_waits_on() {
    let (_dir, db, device) = cold();
    let mut a = db.io_queue();
    let mut b = db.io_queue();
    let wait_a = wait_of(cache_only(&db, &a).get(&key(300)));
    let wait_b = wait_of(cache_only(&db, &b).get(&key(1_500)));
    assert_ne!(wait_a.unit(), wait_b.unit());
    let before = device.reads();
    assert_eq!(b.poll(IoBudget::ALL).completed, 1);
    assert_eq!(
        device.reads() - before,
        1,
        "B ran its own read and no other"
    );
    assert!(wait_b.is_ready());
    assert!(!wait_a.is_ready());
    assert_eq!(a.poll(IoBudget::ALL).completed, 1);
    assert!(wait_a.is_ready());
}

#[test]
fn a_poll_with_nothing_pending_returns_at_once() {
    let (_dir, db, device) = cold();
    let mut queue = db.io_queue();
    let before = device.touches();
    let progress = queue.poll(IoBudget::ALL);
    assert_eq!(progress.completed, 0);
    assert!(!progress.more_pending);
    assert_eq!(device.touches(), before);
    assert_eq!(queue.pending_bytes(), 0);
}

#[test]
fn the_budget_bounds_the_reads_one_poll_runs() {
    let (_dir, db, device) = cold();
    let mut queue = db.io_queue();
    let snapshot = cache_only(&db, &queue);
    let waits: Vec<IoWait> = (0..4)
        .map(|i| wait_of(snapshot.get(&key(200 + i * 300))))
        .collect();
    let before = device.reads();
    let progress = queue.poll(IoBudget::units(1));
    assert_eq!(device.reads() - before, 1);
    assert_eq!(progress.completed, 1);
    assert!(progress.more_pending);
    let progress = queue.poll(IoBudget::units(2));
    assert_eq!(device.reads() - before, 3);
    assert_eq!(progress.completed, 2);
    let progress = queue.poll(IoBudget::ALL);
    assert_eq!(progress.completed, 1);
    assert!(!progress.more_pending);
    assert!(waits.iter().all(IoWait::is_ready));
}

#[test]
fn an_idle_owner_is_woken_once_and_a_busy_one_never() {
    let (_dir, db, device) = cold();
    let db = Arc::new(db);
    let mut a = db.io_queue();
    let wait_a = wait_of(cache_only(&db, &a).get(&key(900)));
    a.poll(IoBudget::units(0));

    // B claims the same unit and is held at the device mid-read.
    device.hold();
    let runner = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let mut b = db.io_queue();
            let wait_b = wait_of(cache_only(&db, &b).get(&key(900)));
            b.poll(IoBudget::ALL);
            assert!(wait_b.is_ready());
        })
    };
    device.wait_held(1);
    // A has nothing it can run itself, so it idles.
    let (idle, waker) = Counter::waker();
    a.idle_waker(&waker);
    assert_eq!(idle.wakes(), 0, "an owner with nothing to take in idles");
    device.release();
    runner.join().unwrap();
    assert_eq!(
        idle.wakes(),
        1,
        "the completion pushed to the idle owner woke it"
    );
    assert!(
        !wait_a.is_ready(),
        "the owner's poll delivers, not the push"
    );
    assert_eq!(a.poll(IoBudget::ALL).completed, 1);
    assert!(wait_a.is_ready());

    // Now busy: A registered as idle, then polled, so it is running when the
    // completion arrives.
    let wait_a = wait_of(cache_only(&db, &a).get(&key(1_900)));
    a.poll(IoBudget::units(0));
    device.hold();
    let runner = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let mut b = db.io_queue();
            let _wait_b = wait_of(cache_only(&db, &b).get(&key(1_900)));
            b.poll(IoBudget::ALL);
        })
    };
    device.wait_held(1);
    let (busy, waker) = Counter::waker();
    a.idle_waker(&waker);
    a.poll(IoBudget::units(0));
    device.release();
    runner.join().unwrap();
    assert_eq!(busy.wakes(), 0, "a busy owner is never woken");
    assert_eq!(a.poll(IoBudget::ALL).completed, 1);
    assert!(wait_a.is_ready());
}

#[test]
fn an_owner_with_a_read_of_its_own_to_run_is_told_at_once() {
    let (_dir, db, _device) = cold();
    let mut queue = db.io_queue();
    let _wait = wait_of(cache_only(&db, &queue).get(&key(42)));
    // The read is in the inbox: idling now would strand it.
    let (counter, waker) = Counter::waker();
    queue.idle_waker(&waker);
    assert_eq!(counter.wakes(), 1);
    // Taken in but not run: still the owner's to run.
    queue.poll(IoBudget::units(0));
    let (counter, waker) = Counter::waker();
    queue.idle_waker(&waker);
    assert_eq!(counter.wakes(), 1);
    queue.poll(IoBudget::ALL);
    let (counter, waker) = Counter::waker();
    queue.idle_waker(&waker);
    assert_eq!(counter.wakes(), 0, "nothing left to run or take in");
}

#[test]
fn a_poll_marks_the_owner_busy_even_with_nothing_to_do() {
    let (_dir, db, _device) = cold();
    let mut queue = db.io_queue();
    let (counter, waker) = Counter::waker();
    queue.idle_waker(&waker);
    // The owner runs again before anything arrived: it is busy now.
    assert!(!queue.poll(IoBudget::ALL).more_pending);
    let snapshot = cache_only(&db, &queue);
    std::thread::scope(|scope| {
        scope
            .spawn(|| wait_of(snapshot.get(&key(1_100))))
            .join()
            .unwrap()
    });
    assert_eq!(counter.wakes(), 0, "a push woke an owner that was running");
    assert_eq!(queue.poll(IoBudget::ALL).completed, 1);
}

#[test]
fn a_read_recorded_from_another_thread_wakes_the_idle_owner() {
    let (_dir, db, _device) = cold();
    let mut queue = db.io_queue();
    let (counter, waker) = Counter::waker();
    queue.idle_waker(&waker);
    assert_eq!(counter.wakes(), 0);
    let snapshot = cache_only(&db, &queue);
    let wait = std::thread::scope(|scope| {
        scope
            .spawn(|| wait_of(snapshot.get(&key(1_200))))
            .join()
            .unwrap()
    });
    assert_eq!(counter.wakes(), 1);
    assert_eq!(wait.queue(), queue.id());
    queue.poll(IoBudget::ALL);
    assert!(wait.is_ready());
    assert_eq!(snapshot.get(&key(1_200)).unwrap(), Some(value(1_200)));
}

#[test]
fn an_io_wait_is_woken_only_by_its_queues_poll() {
    let (_dir, db, _device) = cold();
    let mut a = db.io_queue();
    let mut b = db.io_queue();
    let mut wait = wait_of(cache_only(&db, &a).get(&key(1_000)));
    let _wait_b = wait_of(cache_only(&db, &b).get(&key(1_000)));
    let (counter, waker) = Counter::waker();
    let mut cx = Context::from_waker(&waker);
    assert_eq!(Pin::new(&mut wait).poll(&mut cx), Poll::Pending);
    // A clone listens on its own.
    let mut clone = wait.clone();
    let (clone_counter, clone_waker) = Counter::waker();
    assert_eq!(
        Pin::new(&mut clone).poll(&mut Context::from_waker(&clone_waker)),
        Poll::Pending
    );
    a.poll(IoBudget::units(0));
    b.poll(IoBudget::ALL);
    assert_eq!(counter.wakes(), 0, "another queue's poll ran the read");
    a.poll(IoBudget::ALL);
    assert_eq!(counter.wakes(), 1);
    assert_eq!(clone_counter.wakes(), 1);
    assert_eq!(Pin::new(&mut wait).poll(&mut cx), Poll::Ready(()));
    assert_eq!(
        Pin::new(&mut clone).poll(&mut Context::from_waker(&clone_waker)),
        Poll::Ready(())
    );
}

#[test]
fn close_completes_every_wait_and_the_read_run_again_is_closed() {
    let (_dir, db, device) = cold();
    let mut a = db.io_queue();
    let mut b = db.io_queue();
    let wait_a = wait_of(cache_only(&db, &a).get(&key(400)));
    let wait_b = wait_of(cache_only(&db, &b).get(&key(1_400)));
    let mut wait_late = wait_of(cache_only(&db, &a).get(&key(1_800)));
    // A has taken its reads in, B has not.
    a.poll(IoBudget::units(0));
    let (counter, waker) = Counter::waker();
    assert_eq!(
        Pin::new(&mut wait_late).poll(&mut Context::from_waker(&waker)),
        Poll::Pending
    );
    let before = device.reads();
    db.close().unwrap();
    assert_eq!(
        device.reads(),
        before,
        "close reads none of the pending blocks"
    );
    let progress = a.poll(IoBudget::ALL);
    assert_eq!(progress.completed, 2);
    assert!(!progress.more_pending);
    assert!(wait_a.is_ready() && wait_late.is_ready());
    assert_eq!(counter.wakes(), 1);
    b.poll(IoBudget::ALL);
    assert!(wait_b.is_ready());
    assert_eq!(
        device.reads(),
        before,
        "nothing was read after close either"
    );
    assert!(matches!(
        cache_only(&db, &a).get(&key(400)),
        Err(Error::Closed)
    ));
}

#[test]
fn a_dropped_queue_completes_its_waits_and_other_queues_read_again() {
    let (_dir, db, _device) = cold();
    let a = db.io_queue();
    let mut b = db.io_queue();
    let snapshot_a = cache_only(&db, &a);
    let wait_a = wait_of(snapshot_a.get(&key(600)));
    let wait_b = wait_of(cache_only(&db, &b).get(&key(600)));
    b.poll(IoBudget::units(0));
    drop(a);
    assert!(wait_a.is_ready(), "a dropped queue completes what it held");
    assert!(matches!(
        snapshot_a.get(&key(600)),
        Err(Error::InvalidArgument(_))
    ));
    // B's unit was A's too, and A closed it: B reads again.
    b.poll(IoBudget::ALL);
    assert!(wait_b.is_ready());
    let snapshot_b = cache_only(&db, &b);
    let mut found = None;
    for _ in 0..8 {
        match snapshot_b.get(&key(600)) {
            Err(Error::WouldBlock(_)) => {
                b.poll(IoBudget::ALL);
            }
            other => {
                found = Some(other.unwrap());
                break;
            }
        }
    }
    assert_eq!(found, Some(Some(value(600))));
}

#[test]
fn a_queue_owes_no_more_than_its_byte_bound() {
    let (_dir, db, device) = cold();
    let mut queue = db.io_queue();
    let snapshot = cache_only(&db, &queue);
    // The bound is 256 blocks of the configured 256-byte block size.
    let bound = 256 * 256;
    let mut units = 0;
    let mut room = None;
    for i in (16..KEYS).step_by(4) {
        let wait = wait_of(snapshot.get(&key(i)));
        assert!(queue.pending_bytes() <= bound, "{}", queue.pending_bytes());
        match wait.unit() {
            Some(_) => units += 1,
            None => {
                room = Some(wait);
                break;
            }
        }
    }
    let room = room.expect("misses past the bound wait for room");
    assert!(units > 100, "the bound admitted only {units} reads");
    assert!(!room.is_ready());
    let touched = device.touches();
    queue.poll(IoBudget::ALL);
    assert!(device.touches() > touched);
    assert!(room.is_ready(), "a poll that made room releases the wait");
    assert_eq!(queue.pending_bytes(), 0);
}

/// One thread, one queue: a task awaits its `IoWait`, and the thread polls
/// the queue when no task can run. Everything finishes.
#[test]
fn one_thread_with_one_queue_finishes_everything() {
    let (_dir, db, device) = cold();
    let mut queue = db.io_queue();
    let snapshot = Arc::new(cache_only(&db, &queue));
    type Task = Pin<Box<dyn Future<Output = Vec<u8>>>>;
    let mut tasks: Vec<Option<Task>> = (0..64)
        .map(|n| {
            let snapshot = Arc::clone(&snapshot);
            let task: Task = Box::pin(async move {
                let i = (n * 31) % KEYS;
                loop {
                    match snapshot.get(&key(i)) {
                        Ok(found) => return found.unwrap(),
                        Err(Error::WouldBlock(WouldBlock::Io(wait))) => wait.await,
                        Err(other) => panic!("{other}"),
                    }
                }
            });
            Some(task)
        })
        .collect();
    let (counter, waker) = Counter::waker();
    let mut cx = Context::from_waker(&waker);
    let mut done = 0;
    let mut polls = 0;
    while done < tasks.len() {
        for (n, slot) in tasks.iter_mut().enumerate() {
            if let Some(task) = slot
                && let Poll::Ready(found) = task.as_mut().poll(&mut cx)
            {
                assert_eq!(found, value((n * 31) % KEYS));
                *slot = None;
                done += 1;
            }
        }
        if done < tasks.len() {
            // No task can run: this is the thread's time to do its I/O.
            let progress = queue.poll(IoBudget::ALL);
            assert!(progress.completed > 0 || progress.more_pending);
            polls += 1;
            assert!(polls < 1_000, "the thread stopped making progress");
        }
    }
    assert!(
        counter.wakes() > 0,
        "the waits were woken by the queue's poll"
    );
    assert!(device.reads() > 0);
}

#[test]
fn a_read_that_failed_reports_its_error_when_run_again() {
    let (dir, db, _device) = cold();
    db.close().unwrap();
    drop(db);
    // Damage one data block of the largest table.
    let sst = std::fs::read_dir(dir.path().join("sst"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .max_by_key(|path| std::fs::metadata(path).unwrap().len())
        .unwrap();
    let mut bytes = std::fs::read(&sst).unwrap();
    let at = bytes.len() / 3;
    bytes[at] ^= 0xff;
    std::fs::write(&sst, bytes).unwrap();

    let (env, _device) = DeviceEnv::new();
    let db = Db::open(dir.path(), options(env)).unwrap();
    let broken = (16..KEYS)
        .find(|i| matches!(db.get(&key(*i)), Err(Error::Corruption(_))))
        .expect("a key in the damaged block");
    let mut queue = db.io_queue();
    let snapshot = cache_only(&db, &queue);
    for _ in 0..2 {
        // The failure is handed to the read run again once; the read after
        // that tries the device again, and fails again.
        let _wait = wait_of(snapshot.get(&key(broken)));
        queue.poll(IoBudget::ALL);
        assert!(
            matches!(snapshot.get(&key(broken)), Err(Error::Corruption(_))),
            "the queued read's failure reaches the read run again"
        );
    }
}
