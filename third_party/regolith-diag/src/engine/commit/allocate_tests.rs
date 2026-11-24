//! Reservations in the pipeline (`allocate.rs`): a log failure that
//! discards a reservation after its range was handed out, and a
//! reservation that never waits finding a unit in its way.
//!
//! The failures are the log's sync fault. A durable commit is held at its
//! sync while the reservations are made, so they are decided behind it and
//! not published when its sync fails: the rollback, which runs on the
//! committing thread, discards them with it.

use std::sync::mpsc;
use std::time::Duration;

use tempfile::TempDir;

use super::allocate::fail_write_back;
use super::hook::Stage;
use super::pipeline_tests::{arm_sync_failure, durable_put, key, stopped_at};
use super::*;
use crate::IoBudget;
use crate::engine::EngineOptions;

/// No worker: what a commit leaves to the pipeline runs on the test's
/// thread.
fn no_worker(dir: &TempDir) -> Arc<RegolithEngine> {
    RegolithEngine::open(
        dir.path(),
        EngineOptions {
            max_background_compactions: 0,
            ..EngineOptions::default()
        },
    )
    .unwrap()
}

/// The counter at `name` as a read at the published horizon sees it.
fn published_counter(engine: &RegolithEngine, name: &[u8]) -> Option<u64> {
    engine
        .get(&key(name), engine.snapshot_seq())
        .unwrap()
        .map(|bytes| u64::from_be_bytes(bytes.try_into().unwrap()))
}

/// A durable commit is held at its sync while `reserve` runs, and its sync
/// then fails: the rollback, on the committing thread, discards the commit
/// and every reservation `reserve` made behind it. With `failing` the
/// rollback's write-back fails too.
fn discarded_behind_a_failing_sync(
    dir: &TempDir,
    engine: &RegolithEngine,
    failing: bool,
    reserve: impl FnOnce(),
) {
    let _fault = arm_sync_failure(dir);
    stopped_at(
        Stage::Sync,
        || {
            let _fails = failing.then(fail_write_back);
            engine.submit(durable_put(b"lost", b"x"))
        },
        reserve,
    )
    .expect_err("the sync fails and the commit is discarded");
}

/// The range is handed out when the slot is decided, behind a durable
/// commit that then fails its sync; the rollback discards both. The
/// database goes on: the counter was written back, so the next reservation
/// starts after the range handed out, and the key shows it.
#[test]
fn a_reservation_a_rollback_discards_after_handing_it_out_is_not_handed_out_again() {
    let dir = TempDir::new().unwrap();
    let engine = no_worker(&dir);
    let mut handed_out = None;
    discarded_behind_a_failing_sync(&dir, &engine, false, || {
        handed_out = Some((
            engine.allocate(key(b"ids"), 3),
            published_counter(&engine, b"ids"),
        ));
    });
    let (range, seen) = handed_out.unwrap();
    assert_eq!(range.unwrap(), 1..4);
    assert_eq!(seen, None, "not published when it was handed out");
    assert!(!engine.is_latched(), "a discarded commit latches nothing");
    assert_eq!(engine.get(&key(b"lost"), u64::MAX).unwrap(), None);
    assert_eq!(
        published_counter(&engine, b"ids"),
        Some(3),
        "the counter was written back and published"
    );
    assert_eq!(
        engine.allocate(key(b"ids"), 2).unwrap(),
        4..6,
        "not 1..3 again"
    );
    // A commit using the values goes through, and so does a reopen after it.
    engine.submit(durable_put(b"used", b"4")).unwrap();
    drop(engine);
    let engine = no_worker(&dir);
    assert_eq!(engine.allocate(key(b"ids"), 1).unwrap(), 6..7);
}

/// The write-back itself fails: the database can no longer vouch that no
/// value is handed out twice, so it latches, and no reservation is made.
#[test]
fn a_write_back_that_fails_latches_the_database() {
    let dir = TempDir::new().unwrap();
    let engine = no_worker(&dir);
    let mut handed_out = None;
    discarded_behind_a_failing_sync(&dir, &engine, true, || {
        handed_out = Some(engine.allocate(key(b"ids"), 3));
    });
    assert_eq!(handed_out.unwrap().unwrap(), 1..4);
    assert!(engine.is_latched(), "the write-back failed: latched");
    engine
        .allocate(key(b"ids"), 2)
        .expect_err("a latched database reserves nothing");
}

/// Two reservations of one key discarded together are written back at the
/// last value either reserved, as one record.
#[test]
fn several_discarded_reservations_write_back_the_last_value() {
    let dir = TempDir::new().unwrap();
    let engine = no_worker(&dir);
    let mut handed_out = Vec::new();
    discarded_behind_a_failing_sync(&dir, &engine, false, || {
        handed_out.push(engine.allocate(key(b"ids"), 3));
        handed_out.push(engine.allocate(key(b"ids"), 4));
        handed_out.push(engine.allocate(key(b"other"), 1));
    });
    let handed_out: Vec<_> = handed_out.into_iter().map(Result::unwrap).collect();
    assert_eq!(handed_out, vec![1..4, 4..8, 1..2]);
    assert_eq!(published_counter(&engine, b"ids"), Some(7));
    assert_eq!(published_counter(&engine, b"other"), Some(1));
    assert_eq!(engine.allocate(key(b"ids"), 1).unwrap(), 8..9);
    assert_eq!(engine.allocate(key(b"other"), 1).unwrap(), 2..3);
}

/// A unit ahead of the reservation that another thread is running: the
/// non-blocking form gives its slot up and hands back a wait on its queue,
/// completed at the queue's poll once the unit is done. Run again, it
/// reserves, and the slot it gave up took nothing: no value is skipped.
#[test]
fn allocate_nowait_hands_back_a_wait_for_a_unit_in_its_way() {
    let dir = TempDir::new().unwrap();
    let engine = no_worker(&dir);
    assert_eq!(engine.allocate(key(b"ids"), 2).unwrap(), 1..3);
    let mut queue = engine.io_queue();
    let (entered, has_entered) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    std::thread::scope(|scope| {
        let unit = scope.spawn(|| {
            engine.ordered(move |_, _| {
                entered.send(()).unwrap();
                let _ = released.recv_timeout(Duration::from_secs(60));
                Ok(())
            })
        });
        has_entered
            .recv_timeout(Duration::from_secs(60))
            .expect("the unit never began");
        let wait = match engine.allocate_nowait(queue.id(), key(b"ids"), 3) {
            Err(err) => match crate::Error::from(err) {
                crate::Error::WouldBlock(crate::WouldBlock::Io(wait)) => wait,
                other => panic!("expected a wait for the unit, got {other:?}"),
            },
            Ok(range) => panic!("reserved {range:?} past a unit in its way"),
        };
        assert_eq!(wait.unit(), None, "a wait for work, not for a block");
        queue.poll(IoBudget::ALL);
        assert!(!wait.is_ready(), "the unit is still running");
        release.send(()).unwrap();
        unit.join().unwrap().unwrap();
        for _ in 0..8 {
            if wait.is_ready() {
                break;
            }
            queue.poll(IoBudget::ALL);
        }
        assert!(wait.is_ready(), "the poll completed the wait");
    });
    assert_eq!(
        engine.allocate_nowait(queue.id(), key(b"ids"), 3).unwrap(),
        3..6,
        "the slot given up took nothing"
    );
}
