use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::*;

/// A pin at exactly `seq`, through the real protocol against a horizon
/// standing at `seq`.
fn pin_at(r: &SnapshotRegistry, seq: u64) -> SnapshotPin {
    let (pinned, pin) = r.pin(&ReadHorizon::new(seq));
    assert_eq!(pinned, seq);
    pin
}

/// [`pin_at`] in slot `slot`.
fn pin_in(r: &SnapshotRegistry, slot: usize, seq: u64) -> SnapshotPin {
    let (pinned, pin) = r.pin_in(slot, &ReadHorizon::new(seq));
    assert_eq!(pinned, seq);
    pin
}

#[test]
fn register_release_refcounts_correctly() {
    let r = SnapshotRegistry::new();
    assert_eq!(r.oldest_live_seq(), u64::MAX);
    assert_eq!(r.pin_count(), 0);

    let ten_a = pin_at(&r, 10);
    let ten_b = pin_at(&r, 10);
    let five = pin_at(&r, 5);
    let twenty = pin_at(&r, 20);

    assert_eq!(r.oldest_live_seq(), 5);
    assert_eq!(r.pin_count(), 3);

    r.release(five);
    assert_eq!(r.oldest_live_seq(), 10);
    assert_eq!(r.pin_count(), 2);

    r.release(ten_a);
    assert_eq!(
        r.oldest_live_seq(),
        10,
        "the second pin at 10 is still alive"
    );
    assert_eq!(r.pin_count(), 2);

    r.release(ten_b);
    assert_eq!(r.oldest_live_seq(), 20);
    assert_eq!(r.pin_count(), 1);

    r.release(twenty);
    assert_eq!(r.oldest_live_seq(), u64::MAX);
    assert_eq!(r.pin_count(), 0);
}

#[test]
fn live_seqs_lists_each_pinned_seq_once_in_ascending_order() {
    let r = SnapshotRegistry::new();
    assert!(r.live_seqs().is_empty());

    let a = pin_at(&r, 20);
    let b = pin_at(&r, 5);
    let c = pin_at(&r, 20);
    let d = pin_at(&r, 10);
    assert_eq!(r.live_seqs(), vec![5, 10, 20]);

    r.release(a);
    assert_eq!(r.live_seqs(), vec![5, 10, 20], "a pin at 20 remains");
    r.release(c);
    r.release(b);
    assert_eq!(r.live_seqs(), vec![10]);
    r.release(d);
    assert!(r.live_seqs().is_empty());
}

#[test]
fn the_live_read_hook_runs_once_after_the_list_is_taken() {
    let r = Arc::new(SnapshotRegistry::new());
    let five = pin_at(&r, 5);
    let hooked = Arc::clone(&r);
    let late = Arc::new(std::sync::Mutex::new(None));
    let landed = Arc::clone(&late);
    after_next_live_read(move || *landed.lock().unwrap() = Some(pin_at(&hooked, 9)));

    assert_eq!(
        r.live_seqs(),
        vec![5],
        "the pin the hook registers is not in the list it follows"
    );
    assert_eq!(
        r.live_seqs(),
        vec![5, 9],
        "the hook is spent after one read"
    );
    r.release(five);
    r.release(late.lock().unwrap().take().unwrap());
}

#[test]
fn releasing_a_pin_that_holds_nothing_is_a_no_op() {
    let r = SnapshotRegistry::new();
    let kept = pin_at(&r, 3);
    r.release(SnapshotPin::none());
    assert_eq!(r.live_seqs(), vec![3]);
    r.release(kept);
    assert_eq!(r.oldest_live_seq(), u64::MAX);
}

#[test]
fn live_count_tracks_pins_not_distinct_seqs() {
    let r = SnapshotRegistry::new();
    let a = pin_at(&r, 5);
    let b = pin_at(&r, 5);
    let c = pin_at(&r, 9);
    assert_eq!(r.live_count(), 3);
    assert_eq!(r.pin_count(), 2);
    r.release(a);
    assert_eq!(r.live_count(), 2);
    r.release(b);
    r.release(c);
    assert_eq!(r.live_count(), 0);
}

#[test]
fn oldest_snapshot_time_is_none_when_empty_and_some_when_pinned() {
    let r = SnapshotRegistry::new();
    assert!(r.oldest_snapshot_time_unix().is_none());
    let pin = pin_at(&r, 7);
    assert!(r.oldest_snapshot_time_unix().is_some());
    r.release(pin);
    assert!(r.oldest_snapshot_time_unix().is_none());
}

#[test]
fn oldest_snapshot_time_reports_the_oldest_sequences_claim_and_none_without_a_clock() {
    let env = crate::env::MemEnv::new();
    let r = SnapshotRegistry::with_width(Arc::new(env.clone()), 2);
    env.advance_micros(5_000_000);
    let late = pin_in(&r, 0, 9);
    let at_late = r.oldest_snapshot_time_unix().unwrap();
    env.advance_micros(10_000_000);
    let early = pin_in(&r, 1, 4);
    let at_early = r.oldest_snapshot_time_unix().unwrap();
    assert!(
        at_early >= at_late + 10,
        "the sequence 4 entry was claimed later"
    );
    r.release(early);
    assert_eq!(r.oldest_snapshot_time_unix(), Some(at_late));
    r.release(late);

    env.set_clocks(None, None);
    let unknown = pin_at(&r, 2);
    assert_eq!(
        r.oldest_snapshot_time_unix(),
        None,
        "no clock is reported as unknown, never as the epoch"
    );
    r.release(unknown);
}

#[test]
fn pins_at_one_sequence_in_one_slot_share_an_entry() {
    let r = SnapshotRegistry::with_width(crate::env::std_env(), 1);
    let pins: Vec<SnapshotPin> = (0..100).map(|_| pin_in(&r, 0, 42)).collect();
    let mut entries = 0;
    r.scan_slot(0, &mut |entry: Announced| {
        entries += 1;
        assert_eq!((entry.seq, entry.pins), (42, 100));
    });
    assert_eq!(entries, 1, "a join, not a claim per pin");
    for pin in pins {
        r.release(pin);
    }
    assert_eq!(r.live_count(), 0);
}

#[test]
fn a_slot_grows_past_a_chunk_and_lists_every_sequence() {
    let r = SnapshotRegistry::with_width(crate::env::std_env(), 1);
    let pins: Vec<SnapshotPin> = (0..300u64).map(|seq| pin_in(&r, 0, seq)).collect();
    assert_eq!(r.live_seqs(), (0..300).collect::<Vec<_>>());
    assert_eq!(r.live_count(), 300);
    for pin in pins {
        r.release(pin);
    }
    assert!(r.live_seqs().is_empty());
}

#[test]
fn a_pin_released_on_another_thread_frees_exactly_its_entry() {
    let r = Arc::new(SnapshotRegistry::with_width(crate::env::std_env(), 4));
    let mine = pin_in(&r, 1, 7);
    let theirs = pin_in(&r, 2, 7);
    let moved = Arc::clone(&r);
    // The handle crosses to a thread whose own slot is a different one, and
    // is released there.
    thread::spawn(move || {
        let _own = moved.pin_in(3, &ReadHorizon::new(11));
        moved.release(mine);
    })
    .join()
    .unwrap();
    let mut by_slot = Vec::new();
    for slot in 0..4 {
        r.scan_slot(slot, &mut |entry: Announced| {
            by_slot.push((slot, entry.seq, entry.pins))
        });
    }
    assert_eq!(
        by_slot,
        vec![(2, 7, 1), (3, 11, 1)],
        "slot 1 freed, slot 2 untouched; the other thread's own pin was never released"
    );
    r.release(theirs);
}

#[test]
fn a_clone_shares_the_entry_and_outlives_the_original() {
    let r = SnapshotRegistry::with_width(crate::env::std_env(), 2);
    let original = pin_in(&r, 0, 5);
    let copy = r.clone_pin(&original);
    let mut seen = Vec::new();
    r.scan_slot(0, &mut |entry: Announced| {
        seen.push((entry.seq, entry.pins))
    });
    assert_eq!(seen, vec![(5, 2)], "one entry, two pins");
    r.release(original);
    assert_eq!(r.live_seqs(), vec![5], "the copy still holds the sequence");
    r.release(copy);
    assert!(r.live_seqs().is_empty());
    assert!(!r.clone_pin(&SnapshotPin::none()).is_registered());
}

#[test]
fn a_confirm_that_sees_the_horizon_move_re_announces_at_the_new_value() {
    let r = SnapshotRegistry::with_width(crate::env::std_env(), 1);
    let horizon = ReadHorizon::new(3);
    let at = r.announce(0, 3);
    horizon.publish(4);
    assert_eq!(r.confirm(&horizon, 3), Err(4), "the horizon moved");
    r.release(SnapshotPin::announced(0, at));
    assert!(r.live_seqs().is_empty(), "the stale announce is taken back");
    let (seq, pin) = r.pin_in(0, &horizon);
    assert_eq!(seq, 4);
    assert_eq!(r.live_seqs(), vec![4]);
    r.release(pin);
}

#[test]
fn concurrent_pins_and_cross_thread_releases_stay_exact() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 300;
    // Width 2 under 8 threads: every slot is shared by four.
    let r = Arc::new(SnapshotRegistry::with_width(crate::env::std_env(), 2));
    let horizon = Arc::new(ReadHorizon::new(1));
    let barrier = Arc::new(std::sync::Barrier::new(THREADS + 1));
    let (to_release, released_by) = std::sync::mpsc::channel::<SnapshotPin>();
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let (r, horizon, barrier, to_release) = (
                Arc::clone(&r),
                Arc::clone(&horizon),
                Arc::clone(&barrier),
                to_release.clone(),
            );
            thread::spawn(move || {
                let mut held = Vec::new();
                for round in 0..ROUNDS {
                    if round % 7 == t % 7 {
                        horizon.publish(horizon.visible() + 1);
                    }
                    let (seq, pin) = r.pin_in(t, &horizon);
                    assert!(seq <= horizon.visible());
                    if round % 3 == 0 {
                        // Moved: released by the main thread.
                        to_release.send(pin).unwrap();
                    } else {
                        held.push(pin);
                    }
                }
                barrier.wait();
                barrier.wait();
                for pin in held {
                    r.release(pin);
                }
            })
        })
        .collect();
    drop(to_release);
    barrier.wait();
    // Every thread holds its pins and is parked at the barrier: the list and
    // the count are exact now.
    let moved: Vec<SnapshotPin> = released_by.try_iter().collect();
    let total = r.live_count();
    assert_eq!(total as usize, THREADS * ROUNDS, "every pin counted once");
    for pin in moved {
        r.release(pin);
    }
    barrier.wait();
    for h in handles {
        h.join().unwrap();
    }
    // Late moved pins sent after `try_iter` drained the channel.
    for pin in released_by.try_iter() {
        r.release(pin);
    }
    assert_eq!(r.live_count(), 0);
    assert!(r.live_seqs().is_empty());
    assert_eq!(r.oldest_live_seq(), u64::MAX);
}

#[test]
fn release_wakes_a_registered_waiter() {
    let r = Arc::new(SnapshotRegistry::new());
    let pin = pin_at(&r, 7);

    thread::scope(|scope| {
        let waiter = {
            let r = Arc::clone(&r);
            scope.spawn(move || {
                let started = Instant::now();
                let remaining = r.wait_until_drained(Duration::from_secs(60));
                (remaining, started.elapsed())
            })
        };

        let deadline = Instant::now() + Duration::from_secs(10);
        while r.waiting() == 0 {
            assert!(Instant::now() < deadline, "waiter never registered itself");
            thread::yield_now();
        }
        r.release(pin);

        let (remaining, elapsed) = waiter.join().expect("waiter thread");
        assert_eq!(remaining, 0, "the release must drain the last pin");
        assert!(r.wakes_issued() <= 1);
        assert!(
            elapsed < Duration::from_secs(20),
            "a missed wake would only return at the 60s timeout, took {elapsed:?}"
        );
    });
}

#[test]
fn a_release_after_the_waiter_watched_issues_the_wake() {
    let r = Arc::new(SnapshotRegistry::new());
    let pin = pin_at(&r, 7);
    thread::scope(|scope| {
        let waiter = {
            let r = Arc::clone(&r);
            scope.spawn(move || r.wait_until_drained(Duration::from_secs(60)))
        };
        // Wait until the waiter has watched the chunk: a release now must
        // see the mark and wake it.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut pause = Duration::from_micros(50);
        loop {
            let watched = r.slots[crate::per_thread::index() & (r.width() - 1)]
                .chunks()
                .any(|chunk| chunk.is_watched());
            if r.waiting() > 0 && watched {
                break;
            }
            assert!(Instant::now() < deadline, "the waiter never watched");
            thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_millis(5));
        }
        r.release(pin);
        assert_eq!(waiter.join().unwrap(), 0);
        assert_eq!(r.wakes_issued(), 1, "the freeing release woke the watcher");
    });
}

#[test]
fn release_with_nobody_waiting_issues_no_wake() {
    let r = SnapshotRegistry::new();
    let one = pin_at(&r, 1);
    r.release(one);
    assert_eq!(r.wakes_issued(), 0);

    let two = pin_at(&r, 2);
    let three = pin_at(&r, 3);
    r.release(two);
    r.release(three);
    assert_eq!(r.wakes_issued(), 0);
}

#[test]
fn a_waiter_leaves_no_count_behind_and_later_releases_wake_nobody() {
    let r = SnapshotRegistry::new();
    let pin = pin_at(&r, 3);
    assert_eq!(
        r.wait_until_drained(Duration::from_millis(10)),
        1,
        "the pin is still live, so the wait must time out rather than drain"
    );
    assert_eq!(r.waiting(), 0);

    r.release(pin);
    assert_eq!(
        r.wakes_issued(),
        0,
        "the chunk stays watched, but nobody waits"
    );
    assert_eq!(r.wait_until_drained(Duration::from_secs(1)), 0);
    assert_eq!(r.waiting(), 0);
}

/// E15: on a target with one thread nothing can release a pin while the
/// wait runs, so it reports the pins at once, however long the timeout.
#[test]
fn with_one_thread_the_wait_reports_what_is_pinned_at_once() {
    let r = SnapshotRegistry::new();
    let a = pin_at(&r, 3);
    let b = pin_at(&r, 3);
    assert_eq!(r.wait_until_drained_on(Duration::from_secs(3600), false), 2);
    assert_eq!(r.waiting(), 0, "nothing waited");
    r.release(a);
    r.release(b);
    assert_eq!(r.wait_until_drained_on(Duration::from_secs(3600), false), 0);
}

/// E15: the timeout is measured on the env's clock, not the host's. With
/// the env's clock stopped the wait outlives its timeout in host time,
/// and moving the env's clock past the deadline ends it.
#[test]
fn the_wait_reads_time_through_the_env() {
    let env = crate::env::MemEnv::new();
    let r = Arc::new(SnapshotRegistry::with_env(Arc::new(env.clone())));
    let pin = pin_at(&r, 3);
    let (done, waited) = std::sync::mpsc::channel();
    let waiter = {
        let r = Arc::clone(&r);
        thread::spawn(move || {
            done.send(r.wait_until_drained(Duration::from_millis(5)))
                .unwrap();
        })
    };
    assert!(
        waited.recv_timeout(Duration::from_millis(200)).is_err(),
        "the wait ended on the host's clock while the env's stood still"
    );
    env.advance_micros(1_000_000);
    let remaining = waited
        .recv_timeout(Duration::from_secs(30))
        .expect("the env's clock passed the deadline");
    assert_eq!(remaining, 1);
    waiter.join().unwrap();
    r.release(pin);
}

/// E15: with no clock the wait is one wait of the timeout, ended early by
/// the last release.
#[test]
fn with_no_clock_the_wait_is_one_wait_of_the_timeout() {
    let env = crate::env::MemEnv::new();
    env.set_clocks(None, None);
    let r = Arc::new(SnapshotRegistry::with_env(Arc::new(env)));
    let pin = pin_at(&r, 3);
    assert_eq!(r.wait_until_drained(Duration::from_millis(5)), 1);

    let releaser = {
        let r = Arc::clone(&r);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut pause = Duration::from_micros(100);
            while r.waiting() == 0 && Instant::now() < deadline {
                thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(10));
            }
            r.release(pin);
        })
    };
    assert_eq!(r.wait_until_drained(Duration::from_secs(60)), 0);
    releaser.join().unwrap();
}

/// Counts the times it is woken.
#[derive(Default)]
struct Wakes(std::sync::atomic::AtomicUsize);

impl std::task::Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Wakes {
    fn count(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The awaited wait runs the same rounds: it counts itself in, is woken by
/// the release of an entry it saw taken, looks again, and is over only when
/// nothing is pinned.
#[test]
fn an_awaited_wait_is_woken_by_each_release_and_ends_with_the_last() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let r = SnapshotRegistry::new();
    let wakes = Arc::new(Wakes::default());
    let waker = Waker::from(Arc::clone(&wakes));
    let mut cx = Context::from_waker(&waker);
    let first = pin_at(&r, 3);
    let second = pin_at(&r, 4);

    let mut wait = std::pin::pin!(r.until_drained());
    assert_eq!(r.waiting(), 0, "nothing is counted before the first poll");
    assert_eq!(wait.as_mut().poll(&mut cx), Poll::Pending);
    assert_eq!(r.waiting(), 1);

    r.release(first);
    assert_eq!((r.wakes_issued(), wakes.count()), (1, 1));
    assert_eq!(
        wait.as_mut().poll(&mut cx),
        Poll::Pending,
        "one pin is still held"
    );

    r.release(second);
    assert_eq!((r.wakes_issued(), wakes.count()), (2, 2));
    assert_eq!(wait.as_mut().poll(&mut cx), Poll::Ready(()));
    assert_eq!(r.waiting(), 0, "a finished wait is counted out");
}

/// Dropping a pending wait counts it out and withdraws its notification:
/// the release after it issues no wake and wakes no task.
#[test]
fn a_dropped_awaited_wait_leaves_no_count_and_is_not_woken() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let r = SnapshotRegistry::new();
    let wakes = Arc::new(Wakes::default());
    let waker = Waker::from(Arc::clone(&wakes));
    let mut cx = Context::from_waker(&waker);
    let pin = pin_at(&r, 3);
    {
        let mut wait = std::pin::pin!(r.until_drained());
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(r.waiting(), 1);
    }
    assert_eq!(r.waiting(), 0);
    r.release(pin);
    assert_eq!((r.wakes_issued(), wakes.count()), (0, 0));
}

/// With nothing pinned the awaited wait is over at its first poll.
#[test]
fn an_awaited_wait_with_nothing_pinned_is_ready_at_once() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let r = SnapshotRegistry::new();
    let waker = Waker::from(Arc::new(Wakes::default()));
    let mut wait = std::pin::pin!(r.until_drained());
    assert_eq!(
        wait.as_mut().poll(&mut Context::from_waker(&waker)),
        Poll::Ready(())
    );
    assert_eq!(r.waiting(), 0);
}
