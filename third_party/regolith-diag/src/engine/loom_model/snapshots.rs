//! Loom models of the snapshot registry (`engine::snapshot_registry`): the
//! slot and thread-number claims, the release of a moved pin, the sample
//! and confirm, and the drain waiter's wake.
//!
//! Every atomic the registry touches comes from `crate::sync::internal`, so
//! loom sees the taken bits, the pin counts, the sequences, the chain links,
//! the hint, the horizon and the `SeqCst` fences. A chunk holds three
//! entries under loom (`chain::ENTRIES`), so a few pins fill one and the
//! models also grow chains. Loom regards `SeqCst` loads and stores as
//! `AcqRel` but models `SeqCst` fences, which is why the protocol's
//! ordering rests on fences alone: the models check the code as built.
//!
//! Every model runs at a preemption bound of three (`LOOM_MAX_PREEMPTIONS`
//! overrides it), except the largest, three pinning threads in one slot,
//! which takes two: at three it explores about 0.7 million schedules in over
//! a minute of release build, at two about 0.1 million in ten seconds.
//! Unbounded, the three-thread searches run for hours without reaching a new
//! kind of interleaving.
//!
//! Four calibrations must fail: a registration that skips its confirm, a
//! confirm with no fence after the announce, and a scan with no sample
//! fence are each missed by a scan; and a drain waiter that does not mark
//! the chunks it read is never told of the release it waits for.

use std::future::Future;
use std::ops::ControlFlow;

use loom::sync::atomic::AtomicBool;
use loom::thread;

use super::super::read_horizon::ReadHorizon;
use super::super::snapshot_registry::{Announced, SnapshotPin, SnapshotRegistry};
use super::explore_bounded;
use crate::per_thread::IndexPool;
use crate::sync::internal::{Arc, Ordering};

/// Wakes a loom thread parked in [`block_on`].
struct Unpark(loom::sync::Notify);

impl std::task::Wake for Unpark {
    fn wake(self: std::sync::Arc<Self>) {
        self.0.notify();
    }

    fn wake_by_ref(self: &std::sync::Arc<Self>) {
        self.0.notify();
    }
}

/// Drives `future` on this loom thread, parking it between polls until its
/// waker fires. A wake that never comes parks it for good, which loom
/// reports as a deadlock.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let park = std::sync::Arc::new(Unpark(loom::sync::Notify::new()));
    let waker = std::task::Waker::from(std::sync::Arc::clone(&park));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        park.0.wait();
    }
}

/// A registry of `width` slots.
fn registry(width: usize) -> SnapshotRegistry {
    SnapshotRegistry::with_width(crate::env::std_env(), width)
}

/// A pin at exactly `seq` in slot `slot`, against a horizon standing still.
fn pin_at(r: &SnapshotRegistry, slot: usize, seq: u64) -> SnapshotPin {
    let (pinned, pin) = r.pin_in(slot, &ReadHorizon::new(seq));
    assert_eq!(pinned, seq);
    pin
}

/// Every announced entry of slot `slot`, as `(seq, pins)`, sorted.
fn entries(r: &SnapshotRegistry, slot: usize) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    r.scan_slot(slot, &mut |entry: Announced| {
        out.push((entry.seq, entry.pins))
    });
    out.sort_unstable();
    out
}

/// Slot claim: three threads pin in one shared slot at once, two of them
/// at the same sequence. Each claim's compare-and-swap takes its own entry
/// or joins the matching one; no pin is lost or counted twice.
pub fn concurrent_pins_in_one_slot_keep_their_own_counts() {
    explore_bounded(
        "concurrent_pins_in_one_slot_keep_their_own_counts",
        // SHRUNK: two preemptions where the other models take three (700
        // thousand interleavings, 78 s; now about 100 thousand). The
        // scenario is unchanged: three threads pin in one slot at once, two
        // at the same sequence, and the witness (a join that raced a claim)
        // is still reached.
        Some(2),
        16,
        1,
        |witness| {
            let r = Arc::new(registry(1));
            let spawn = |seq: u64| {
                let r = Arc::clone(&r);
                thread::spawn(move || pin_at(&r, 0, seq))
            };
            let threads = [spawn(4), spawn(5), spawn(4)];
            let pins: Vec<SnapshotPin> = threads.into_iter().map(|t| t.join().unwrap()).collect();

            assert_eq!(r.live_seqs(), vec![4, 5]);
            assert_eq!(r.live_count(), 3, "every pin counted once");
            let held = entries(&r, 0);
            if held.contains(&(4, 2)) {
                // The two pins at 4 shared one entry: a join raced a claim.
                witness.record();
            }
            for pin in pins {
                r.release(pin);
            }
            assert!(r.live_seqs().is_empty(), "every release freed its count");
        },
    );
}

/// Slot claim, growing: the slot's only chunk is full, and two threads pin
/// new sequences at once. Both must grow the chain; the loser of the link's
/// compare-and-swap frees its chunk and uses the winner's.
pub fn racing_claims_grow_the_chain_by_one_chunk() {
    explore_bounded(
        "racing_claims_grow_the_chain_by_one_chunk",
        Some(3),
        8,
        1,
        |witness| {
            let r = Arc::new(registry(1));
            let full: Vec<SnapshotPin> = (1..=3).map(|seq| pin_at(&r, 0, seq)).collect();
            let spawn = |seq: u64| {
                let r = Arc::clone(&r);
                thread::spawn(move || pin_at(&r, 0, seq))
            };
            let (a, b) = (spawn(10), spawn(11));
            let (a, b) = (a.join().unwrap(), b.join().unwrap());

            assert_eq!(r.chunks_in(0), 2, "one chunk appended, never two");
            assert_eq!(r.live_seqs(), vec![1, 2, 3, 10, 11]);
            let depth = |pin: &SnapshotPin| pin.location().map(|(_, at)| at.depth);
            if depth(&a) == Some(1) && depth(&b) == Some(1) {
                witness.record();
            }
            for pin in full.into_iter().chain([a, b]) {
                r.release(pin);
            }
            assert_eq!(r.live_count(), 0);
        },
    );
}

/// The thread-number pool: three threads claim from a pool of two while a
/// fourth gives a number back. A number is never owned by two threads at
/// once, and a thread that finds the pool full gets a shared number.
pub fn the_index_pool_never_hands_one_number_to_two_owners() {
    explore_bounded(
        "the_index_pool_never_hands_one_number_to_two_owners",
        Some(3),
        16,
        1,
        |witness| {
            let pool = Arc::new(IndexPool::new());
            let (first, owned) = pool.claim(2);
            assert_eq!((first, owned), (0, true));
            let leaver = {
                let pool = Arc::clone(&pool);
                thread::spawn(move || pool.give_back(0))
            };
            let claimers: Vec<_> = (0..2)
                .map(|_| {
                    let pool = Arc::clone(&pool);
                    thread::spawn(move || pool.claim(2))
                })
                .collect();
            leaver.join().unwrap();
            let claims: Vec<(usize, bool)> =
                claimers.into_iter().map(|t| t.join().unwrap()).collect();

            let owned: Vec<usize> = claims.iter().filter(|c| c.1).map(|c| c.0).collect();
            let mut distinct = owned.clone();
            distinct.dedup();
            assert_eq!(
                owned.len(),
                distinct.len(),
                "one number, two owners: {claims:?}"
            );
            assert!(claims.iter().all(|&(i, _)| i < 2));
            for &i in &owned {
                assert!(pool.is_taken(i));
            }
            if owned.len() == 2 {
                // The given-back number was claimed again.
                witness.record();
            }
        },
    );
}

/// Release of a moved pin: a pin taken on one thread is released on
/// another, while a third thread joins (or, too late, claims) at the same
/// sequence through the slot's hint and a fourth claims a new sequence that
/// may recycle the freed entry. That is the join's recycle race: a join
/// that counted itself on a recycled entry must take the count back.
/// Whatever the order, exactly the live pins remain.
pub fn a_moved_release_frees_exactly_its_entry_beside_a_join() {
    explore_bounded(
        "a_moved_release_frees_exactly_its_entry_beside_a_join",
        Some(3),
        64,
        1,
        |witness| {
            let r = Arc::new(registry(2));
            let moved = pin_at(&r, 0, 5);
            let releaser = {
                let r = Arc::clone(&r);
                thread::spawn(move || r.release(moved))
            };
            let joiner = {
                let r = Arc::clone(&r);
                thread::spawn(move || pin_at(&r, 0, 5))
            };
            let claimer = {
                let r = Arc::clone(&r);
                thread::spawn(move || pin_at(&r, 0, 6))
            };
            releaser.join().unwrap();
            let (joined, claimed) = (joiner.join().unwrap(), claimer.join().unwrap());

            assert_eq!(
                entries(&r, 0),
                vec![(5, 1), (6, 1)],
                "exactly the live pins"
            );
            assert_eq!(entries(&r, 1), vec![], "the other slot was never touched");
            if joined.location().map(|(_, at)| at.index) != Some(0) {
                // The join missed the original entry and claimed afresh.
                witness.record();
            }
            r.release(joined);
            r.release(claimed);
            assert_eq!(r.live_count(), 0);
        },
    );
}

/// What a compaction scan and a concurrent registration end with.
struct Race {
    /// The sequence the registration pinned.
    seq: u64,
    /// Whether the compaction saw the commit published before its sample.
    saw_publish: bool,
    /// The scan's list.
    list: Vec<u64>,
}

/// One registration (`register`) against a commit that publishes
/// sequence 2 and a compaction that samples (unless `sample` is false),
/// then scans both slots.
fn race(register: fn(&SnapshotRegistry, &ReadHorizon) -> (u64, SnapshotPin), sample: bool) -> Race {
    let r = Arc::new(registry(2));
    let horizon = Arc::new(ReadHorizon::new(1));
    let published = Arc::new(AtomicBool::new(false));
    let writer = {
        let (horizon, published) = (Arc::clone(&horizon), Arc::clone(&published));
        thread::spawn(move || {
            horizon.publish(2);
            published.store(true, Ordering::Release);
        })
    };
    let reader = {
        let (r, horizon) = (Arc::clone(&r), Arc::clone(&horizon));
        thread::spawn(move || register(&r, &horizon))
    };
    let compaction = {
        let (r, published) = (Arc::clone(&r), Arc::clone(&published));
        thread::spawn(move || {
            // The commit's publish happens before this sample when the flag
            // reads true: the inputs a real pass fixes are of that kind.
            let saw_publish = published.load(Ordering::Acquire);
            if sample {
                r.sample();
            }
            let mut list = Vec::new();
            for slot in 0..2 {
                r.scan_slot(slot, &mut |entry: Announced| list.push(entry.seq));
            }
            (saw_publish, list)
        })
    };
    writer.join().unwrap();
    let (seq, pin) = reader.join().unwrap();
    let (saw_publish, list) = compaction.join().unwrap();
    r.release(pin);
    Race {
        seq,
        saw_publish,
        list,
    }
}

/// Sample and confirm: a snapshot a scan missed reads at or above every
/// sequence published before the scan's sample, so it loses no version
/// the compaction's inputs hold. The registration is the real one:
/// announce, fence, confirm, retry.
pub fn a_scan_never_misses_a_confirmed_snapshot() {
    explore_bounded(
        "a_scan_never_misses_a_confirmed_snapshot",
        Some(3),
        64,
        1,
        |witness| {
            let race = race(|r, horizon| r.pin_in(0, horizon), true);
            if race.saw_publish && !race.list.contains(&race.seq) {
                witness.record();
                assert!(
                    race.seq >= 2,
                    "the scan missed a confirmed snapshot below a publish it saw: {} not in {:?}",
                    race.seq,
                    race.list
                );
            }
        },
    );
}

/// Calibration for [`a_scan_never_misses_a_confirmed_snapshot`]: the
/// registration goes live at the horizon it announced, with no confirm.
/// It read 1, the commit published 2, the compaction sampled and scanned
/// the still-empty slot, and only then the announce landed: the snapshot
/// at 1 is missed. Loom must find that schedule.
pub fn a_snapshot_announced_without_its_confirm_is_missed() {
    explore_bounded(
        "a_snapshot_announced_without_its_confirm_is_missed",
        Some(3),
        4,
        1,
        |witness| {
            let race = race(
                |r, horizon| {
                    let seq = horizon.visible();
                    let at = r.announce(0, seq);
                    (seq, SnapshotPin::announced(0, at))
                },
                true,
            );
            if race.saw_publish && !race.list.contains(&race.seq) {
                witness.record();
                assert!(
                    race.seq >= 2,
                    "the scan missed a confirmed snapshot below a publish it saw: {} not in {:?}",
                    race.seq,
                    race.list
                );
            }
        },
    );
}

/// The check [`a_scan_never_misses_a_confirmed_snapshot`] makes, for a
/// race run with or without the scan's sample.
fn assert_not_missed(race: &Race, witness: &super::Witness) {
    if race.saw_publish && !race.list.contains(&race.seq) {
        witness.record();
        assert!(
            race.seq >= 2,
            "the scan missed a confirmed snapshot below a publish it saw: {} not in {:?}",
            race.seq,
            race.list
        );
    }
}

/// Calibration for the compaction's half of the fences: the scan reads
/// the slots without its `SeqCst` sample. The registration's own fence is
/// then no help: its confirming load may read the old horizon while the
/// scan, which already saw the publish, reads the slot from before the
/// announce. Loom must find that schedule.
pub fn a_scan_without_its_sample_fence_misses_a_snapshot() {
    explore_bounded(
        "a_scan_without_its_sample_fence_misses_a_snapshot",
        Some(3),
        2,
        1,
        |witness| {
            let race = race(|r, horizon| r.pin_in(0, horizon), false);
            assert_not_missed(&race, witness);
        },
    );
}

/// Calibration for the registration's half of the fences: announce, then
/// confirm with a plain load and no fence between. The confirming load may
/// be satisfied before the announce is visible to the scan, so the scan
/// misses the entry while the confirm still reads the old horizon. Loom
/// must find that schedule.
pub fn a_confirm_without_its_fence_is_missed() {
    explore_bounded(
        "a_confirm_without_its_fence_is_missed",
        Some(3),
        2,
        1,
        |witness| {
            let race = race(
                |r, horizon| {
                    let mut seq = horizon.visible();
                    loop {
                        let at = r.announce(0, seq);
                        let now = horizon.visible();
                        if now == seq {
                            return (seq, SnapshotPin::announced(0, at));
                        }
                        r.release(SnapshotPin::announced(0, at));
                        seq = now;
                    }
                },
                true,
            );
            assert_not_missed(&race, witness);
        },
    );
}

/// The drain wait: a waiter that saw an entry taken is woken by the
/// release that frees it, however the two interleave. A lost wake leaves
/// the waiter blocked forever, which loom reports as a deadlock.
pub fn a_drain_waiter_is_always_woken() {
    explore_bounded("a_drain_waiter_is_always_woken", Some(3), 8, 1, |witness| {
        let r = Arc::new(registry(1));
        let pin = pin_at(&r, 0, 3);
        let releaser = {
            let r = Arc::clone(&r);
            thread::spawn(move || r.release(pin))
        };
        let waiter = {
            let (r, witness) = (Arc::clone(&r), witness.clone());
            thread::spawn(move || {
                r.wait_drained(|notified, _| {
                    witness.record();
                    block_on(notified);
                    ControlFlow::Continue(())
                })
            })
        };
        releaser.join().unwrap();
        assert_eq!(waiter.join().unwrap(), 0, "drained");
    });
}

/// Calibration for [`a_drain_waiter_is_always_woken`]: a waiter that reads
/// the entries without marking the chunk watched, and without counting
/// itself as a watcher. When its read saw the entry taken, the release that
/// frees it afterwards sees no mark and wakes nobody, so the notification
/// the waiter holds never completes. Polled again once the release is done,
/// it is still pending. (Blocking on it instead would end in loom's
/// deadlock report, which aborts the test binary rather than failing one
/// test.)
pub fn a_waiter_that_does_not_mark_the_chunk_is_never_woken() {
    explore_bounded(
        "a_waiter_that_does_not_mark_the_chunk_is_never_woken",
        Some(3),
        2,
        1,
        |witness| {
            let r = Arc::new(registry(1));
            let pin = pin_at(&r, 0, 3);
            let releaser = {
                let r = Arc::clone(&r);
                thread::spawn(move || r.release(pin))
            };
            let mut notified = std::pin::pin!(r.drained().notified());
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            let taken = !entries(&r, 0).is_empty();
            let early = notified.as_mut().poll(&mut cx).is_ready();
            releaser.join().unwrap();
            if taken && !early {
                witness.record();
                assert!(
                    notified.as_mut().poll(&mut cx).is_ready(),
                    "the waiter saw the entry taken and was never told it went free"
                );
            }
        },
    );
}
