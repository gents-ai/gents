//! A `CacheOnly` read never waits for an open-file slot (plan D60).
//!
//! Under `max_open_files`, a read whose table must be reopened while every
//! descriptor is in use by reads on other threads stays pending on its I/O
//! queue: the read and the poll both return at once. The read that frees a
//! descriptor tells the queue, and the next poll reads the block. A
//! `Blocking` read, which chose to block, still waits for one descriptor's
//! running reads, and completes.
//!
//! The reads that hold every descriptor are held at the device by
//! `DeviceEnv`'s gate, so each step here is ordered by the test, never by
//! timing. A step that must not wait runs beside a watchdog that lets the
//! device go after a deadline: had the step waited for a held read, the
//! watchdog's release is what would end it, and the test fails saying so.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::task::{Wake, Waker};
use std::time::Duration;

use common::device_env::Device;
use regolith::{Db, Error, IoBudget, IoWait, Options, ReadMode, WouldBlock};
use tempfile::TempDir;

/// Tables written, each its own flush.
const TABLES: usize = 3;
/// Keys per table.
const PER_TABLE: usize = 100;

fn key(i: usize) -> Vec<u8> {
    format!("k{i:04}").into_bytes()
}

fn value(i: usize) -> Vec<u8> {
    format!("value-{i:04}").into_bytes()
}

/// A key in the middle of table `t`. Opening reads the first block of every
/// table, so a key past it is cold.
fn middle(t: usize) -> usize {
    t * PER_TABLE + PER_TABLE / 2
}

/// `TABLES` tables with no compaction, behind `max_open_files` descriptors,
/// opened again with a cold block cache.
fn tables(max_open_files: usize) -> (TempDir, Db, Arc<Device>) {
    common::device_env::cold_db(
        |env| {
            Options::default()
                .env(env)
                .block_size(256)
                .max_open_files(max_open_files)
                .l0_compaction_trigger(64)
                .max_background_compactions(0)
        },
        |db| {
            for t in 0..TABLES {
                for i in t * PER_TABLE..(t + 1) * PER_TABLE {
                    db.put(&key(i), &value(i)).unwrap();
                }
                db.flush().unwrap();
            }
        },
    )
}

/// The wait a read that must not block returned.
fn pending(read: regolith::Result<Option<Vec<u8>>>) -> IoWait {
    match read {
        Err(Error::WouldBlock(WouldBlock::Io(wait))) => wait,
        other => panic!("expected the read to return WouldBlock, got {other:?}"),
    }
}

/// Run `step`, which must not wait for a read held at the device, beside a
/// watchdog that releases the device after a deadline. Fails if the
/// watchdog had to.
fn never_waits<T>(what: &str, device: &Device, step: impl FnOnce() -> T) -> T {
    let (finished, watched) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let watchdog = scope.spawn(move || {
            let fired = watched.recv_timeout(Duration::from_secs(30)).is_err();
            if fired {
                device.release();
            }
            fired
        });
        let out = step();
        let _ = finished.send(());
        assert!(
            !watchdog.join().unwrap(),
            "{what} waited for a read held at the device"
        );
        out
    })
}

/// Counts wakes.
struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// With one and with two descriptors, `Blocking` readers hold every one at
/// the device. A `CacheOnly` read of another table returns `WouldBlock` at
/// once, its poll returns at once with the read still pending, and no
/// descriptor is opened. Once the readers leave, one poll reads the block.
#[test]
fn a_cache_only_read_never_waits_while_every_descriptor_is_in_use() {
    for max in [1, 2] {
        let (_dir, db, device) = tables(max);
        let mut queue = db.io_queue();
        let snapshot = db
            .snapshot()
            .with_read_mode(ReadMode::CacheOnly(queue.id()));
        let target = middle(TABLES - 1);
        device.hold();
        let wait = std::thread::scope(|scope| {
            let readers: Vec<_> = (0..max)
                .map(|t| {
                    let db = &db;
                    scope.spawn(move || {
                        let i = middle(t);
                        assert_eq!(db.get(&key(i)).unwrap(), Some(value(i)));
                    })
                })
                .collect();
            device.wait_held(max);
            let opens = device.opens();
            let wait = never_waits("a CacheOnly read and its poll", &device, || {
                let wait = pending(snapshot.get(&key(target)));
                let progress = queue.poll(IoBudget::ALL);
                assert_eq!(progress.completed, 0, "max_open_files({max})");
                assert!(progress.more_pending);
                assert!(!wait.is_ready());
                // Run again while it is pending: it joins the pending read.
                let again = pending(snapshot.get(&key(target)));
                assert_eq!(again.unit(), wait.unit());
                assert_eq!(queue.poll(IoBudget::ALL).completed, 0);
                wait
            });
            assert_eq!(
                device.opens(),
                opens,
                "a descriptor was opened while every one was in use"
            );
            device.release();
            for reader in readers {
                reader.join().unwrap();
            }
            wait
        });
        // The reader that left told the queue: one poll reads the block.
        let progress = queue.poll(IoBudget::ALL);
        assert_eq!(progress.completed, 1, "max_open_files({max})");
        assert!(wait.is_ready());
        assert_eq!(snapshot.get(&key(target)).unwrap(), Some(value(target)));
    }
}

/// An owner that idles with its read pending is woken, once, by the read
/// that frees a descriptor.
#[test]
fn a_freed_descriptor_wakes_the_idle_owner_of_a_pending_read() {
    let (_dir, db, device) = tables(1);
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    let target = middle(1);
    let wakes = Arc::new(Wakes(AtomicUsize::new(0)));
    device.hold();
    std::thread::scope(|scope| {
        let db = &db;
        let reader = scope.spawn(move || db.get(&key(middle(0))).unwrap());
        device.wait_held(1);
        never_waits("a CacheOnly read and its poll", &device, || {
            pending(snapshot.get(&key(target)));
            assert!(queue.poll(IoBudget::ALL).more_pending);
            queue.idle_waker(&Waker::from(Arc::clone(&wakes)));
        });
        assert_eq!(wakes.0.load(Ordering::SeqCst), 0, "nothing freed yet");
        device.release();
        assert_eq!(reader.join().unwrap(), Some(value(middle(0))));
    });
    assert_eq!(
        wakes.0.load(Ordering::SeqCst),
        1,
        "the freed descriptor woke the idle owner once"
    );
    assert_eq!(queue.poll(IoBudget::ALL).completed, 1);
    assert_eq!(snapshot.get(&key(target)).unwrap(), Some(value(target)));
}

/// While a `Blocking` read holds the only descriptor at the device and a
/// `CacheOnly` read of another table is pending, a second `Blocking` read of
/// a third table waits for a descriptor, as it chose to. Once the device
/// lets go, both `Blocking` reads return their values and the pending read
/// is read at the next poll.
#[test]
fn a_blocking_read_still_waits_for_a_descriptor_and_completes() {
    let (_dir, db, device) = tables(1);
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    let queued = middle(2);
    device.hold();
    std::thread::scope(|scope| {
        let db = &db;
        let first = scope.spawn(move || db.get(&key(middle(0))).unwrap());
        device.wait_held(1);
        never_waits("a CacheOnly read and its poll", &device, || {
            pending(snapshot.get(&key(queued)));
            assert!(queue.poll(IoBudget::ALL).more_pending);
        });
        let second = scope.spawn(move || db.get(&key(middle(1))).unwrap());
        assert!(!second.is_finished(), "every read is held at the device");
        device.release();
        assert_eq!(first.join().unwrap(), Some(value(middle(0))));
        assert_eq!(second.join().unwrap(), Some(value(middle(1))));
    });
    assert_eq!(queue.poll(IoBudget::ALL).completed, 1);
    assert_eq!(snapshot.get(&key(queued)).unwrap(), Some(value(queued)));
}

/// Two queues miss one block while the only descriptor is held. They wait
/// on one unit; the queue that ran it and found no descriptor is the one
/// told when a descriptor frees, and its poll reads the block once, with one
/// open, for both.
#[test]
fn a_pending_read_stays_single_flight_across_queues() {
    let (_dir, db, device) = tables(1);
    let mut first = db.io_queue();
    let mut second = db.io_queue();
    let through_first = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(first.id()));
    let through_second = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(second.id()));
    let target = middle(1);
    device.hold();
    let (wait_first, wait_second) = std::thread::scope(|scope| {
        let db = &db;
        let reader = scope.spawn(move || db.get(&key(middle(0))).unwrap());
        device.wait_held(1);
        let waits = never_waits("two CacheOnly reads and their polls", &device, || {
            let a = pending(through_first.get(&key(target)));
            let b = pending(through_second.get(&key(target)));
            assert_eq!(a.unit(), b.unit(), "one block, one unit");
            assert_eq!(first.poll(IoBudget::ALL).completed, 0);
            assert_eq!(second.poll(IoBudget::ALL).completed, 0);
            (a, b)
        });
        device.release();
        reader.join().unwrap();
        waits
    });
    let (opens, reads) = (device.opens(), device.reads());
    // The second queue lost the claim to the first: it waits for the
    // first's poll, which the freed descriptor's message is for.
    assert_eq!(second.poll(IoBudget::ALL).completed, 0);
    assert_eq!(first.poll(IoBudget::ALL).completed, 1);
    assert_eq!(second.poll(IoBudget::ALL).completed, 1);
    assert!(wait_first.is_ready() && wait_second.is_ready());
    assert_eq!(device.opens() - opens, 1, "the table was reopened once");
    assert_eq!(device.reads() - reads, 1, "the block was read once");
    assert_eq!(
        through_first.get(&key(target)).unwrap(),
        Some(value(target))
    );
    assert_eq!(
        through_second.get(&key(target)).unwrap(),
        Some(value(target))
    );
}

/// Close settles a pending read without reading it: its wait becomes ready
/// at the next poll with no table opened, and the read run again answers
/// `Closed`. First with the descriptor freed but the queue not yet polled,
/// then with the descriptor still held.
#[test]
fn close_settles_a_pending_read_with_closed() {
    for still_held in [false, true] {
        let (_dir, db, device) = tables(1);
        let mut queue = db.io_queue();
        let snapshot = db
            .snapshot()
            .with_read_mode(ReadMode::CacheOnly(queue.id()));
        let target = middle(1);
        device.hold();
        let wait = std::thread::scope(|scope| {
            let db = &db;
            let reader = scope.spawn(move || db.get(&key(middle(0))));
            device.wait_held(1);
            let wait = never_waits("a CacheOnly read and its poll", &device, || {
                let wait = pending(snapshot.get(&key(target)));
                assert!(queue.poll(IoBudget::ALL).more_pending);
                wait
            });
            if still_held {
                never_waits("close", &device, || db.close().unwrap());
                device.release();
                reader.join().unwrap().unwrap();
            } else {
                device.release();
                reader.join().unwrap().unwrap();
                db.close().unwrap();
            }
            wait
        });
        let opens = device.opens();
        let progress = queue.poll(IoBudget::ALL);
        assert_eq!(progress.completed, 1, "still held: {still_held}");
        assert!(!progress.more_pending);
        assert!(wait.is_ready());
        assert_eq!(
            device.opens(),
            opens,
            "close settled the pending read; nothing ran it (still held: {still_held})"
        );
        assert!(
            matches!(snapshot.get(&key(target)), Err(Error::Closed)),
            "still held: {still_held}"
        );
    }
}
