//! One unit of device I/O, run once however many queues wait on it.
//!
//! A unit is born in the `Free` state when the first `CacheOnly` read misses
//! its block, and moves through the states `flight.rs` describes: one claim
//! by compare-and-swap, then one `finish`, which writes the outcome and lands
//! the flight, pushing one `Done` message to each queue that registered. A
//! release (close, a dropped queue) claims the unit and finishes it with
//! nothing read. A queue that registers after the landing is refused and
//! reads the outcome itself. So every queue that registered gets exactly one
//! completion, and none is lost.
//!
//! A run whose reopen under `max_open_files` found every slot busy does not
//! wait (D60): it puts its work back and parks the flight, and the slot
//! table tells the queue that ran it when a slot frees; that queue's poll
//! unparks the unit and runs it again. A parked unit is still in flight:
//! reads of its block join it, so the block is read once, and a release
//! closes it like a unit nobody claimed. So the work runs on one thread at
//! a time, and the unit finishes at most once.

#![allow(unsafe_code)]

use std::io;
use std::sync::Arc;

use super::Landed;
use super::flight::Flight;
use super::shared::{Message, QueueShared};
use crate::engine::block_cache::BlockCache;
use crate::sync::internal::UnsafeCell;

/// What names one unit in the shared table: the block cache's key for the
/// block, and the per-block allocation guard of a size-limited read. Reads of
/// one block with the same guard share one unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct UnitKey {
    pub(crate) file_id: u64,
    pub(crate) offset: u64,
    pub(crate) guard: Option<usize>,
}

/// The device read a unit performs, filling the block cache as the blocking
/// read does and handing back what it read. Run again after a park, so it
/// must leave nothing behind when its reopen parks.
pub(crate) type Work = Box<dyn FnMut(&BlockCache) -> io::Result<Landed> + Send>;

/// How one [`Unit::run`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ran {
    /// The unit is done, and its waiting queues have their completions.
    Finished,
    /// A reopen found no open-file slot: the unit waits, parked, for the
    /// queue that ran it to be told a slot freed.
    Parked,
}

/// How a unit ended.
pub(crate) enum Outcome {
    /// The read succeeded.
    Landed(Landed),
    /// The read failed; the error is handed to one read of each waiting queue.
    Failed(Arc<io::Error>),
    /// Closed without reading: the database closed or the queue that held the
    /// unit was dropped. A read run again decides what to do.
    Released,
}

pub(crate) struct Unit {
    key: UnitKey,
    /// What the unit reads from the device: the frame of the block.
    bytes: usize,
    flight: Flight,
    /// Taken by the claim holder, and put back by it before a park.
    work: UnsafeCell<Option<Work>>,
    /// Written by the claim holder before `Done`, read only after it.
    outcome: UnsafeCell<Option<Outcome>>,
}

// SAFETY: `work` is only touched by the thread whose claim succeeded, and a
// claim succeeds again only after the holder's park released the flight
// (Release store of `Parked`, read by the unpark CAS whose `Free` the next
// claim's Acquire CAS reads), so two holders never overlap and each sees
// the work the last one put back. `outcome` is written by the holder before
// `Done` is released and only read after `Done` is acquired. `Work` is
// `Send`, and an `Outcome` is `Send + Sync`.
unsafe impl Sync for Unit {}

impl Unit {
    pub(crate) fn new(key: UnitKey, bytes: usize, work: Work) -> Self {
        Self {
            key,
            bytes,
            flight: Flight::new(),
            work: UnsafeCell::new(Some(work)),
            outcome: UnsafeCell::new(None),
        }
    }

    pub(crate) fn key(&self) -> UnitKey {
        self.key
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn is_done(&self) -> bool {
        self.flight.is_done()
    }

    /// Nobody has claimed the unit yet: a waiting queue's owner may run it.
    pub(crate) fn is_free(&self) -> bool {
        self.flight.is_free()
    }

    /// Take the unit for this thread with one compare-and-swap. `true` for
    /// exactly one caller over the unit's life; that caller must then
    /// [`run`](Self::run) or [`finish`](Self::finish) it.
    pub(crate) fn claim(&self) -> bool {
        self.flight.claim()
    }

    /// Record `queue` as waiting. `false` when the unit already finished: the
    /// caller then reads [`outcome`](Self::outcome) itself.
    pub(crate) fn register(&self, queue: Arc<QueueShared>) -> bool {
        self.flight.register(queue)
    }

    /// Run the work on this thread and finish, or park when `parked` says a
    /// reopen in the work found no open-file slot (the work's error is then
    /// that, not the read's outcome). Only after this caller's
    /// [`claim`](Self::claim) succeeded.
    pub(crate) fn run(self: &Arc<Self>, cache: &BlockCache, parked: impl FnOnce() -> bool) -> Ran {
        // A panic in the work still finishes the unit, so no waiting queue
        // is left on a claim nobody will ever complete.
        struct Unwind<'a>(Option<&'a Arc<Unit>>);
        impl Drop for Unwind<'_> {
            fn drop(&mut self) {
                if let Some(unit) = self.0.take() {
                    unit.finish(Outcome::Released);
                }
            }
        }
        let mut unwind = Unwind(Some(self));
        // SAFETY: the claim gave this thread `work`.
        let work = self.work.with_mut(|work| unsafe { (*work).take() });
        let outcome = match work {
            Some(mut work) => match work(cache) {
                Ok(landed) => Outcome::Landed(landed),
                Err(err) => {
                    if parked() {
                        // SAFETY: still the claim holder; the park below
                        // publishes the work to the next one.
                        self.work.with_mut(|cell| unsafe { *cell = Some(work) });
                        unwind.0 = None;
                        self.park();
                        return Ran::Parked;
                    }
                    Outcome::Failed(Arc::new(err))
                }
            },
            None => Outcome::Released,
        };
        unwind.0 = None;
        self.finish(outcome);
        Ran::Finished
    }

    /// Park the unit this thread holds claimed: it is neither run nor
    /// finished until [`unpark`](Self::unpark) or
    /// [`release`](Self::release). The flight's `Release` store hands the
    /// work put back to whoever claims it next.
    pub(crate) fn park(&self) {
        self.flight.park_closed();
    }

    /// Make a parked unit claimable again, after a slot freed. `true` for
    /// the one call that did; `false` when the unit was not parked (it was
    /// unparked already, released, or finished).
    pub(crate) fn unpark(&self) -> bool {
        self.flight.unpark()
    }

    /// Whether the unit is parked for an open-file slot.
    #[cfg(test)]
    pub(crate) fn is_parked(&self) -> bool {
        self.flight.is_parked()
    }

    /// Close a unit nobody is running, parked or not, without reading.
    /// `false` when another thread holds it claimed; that thread finishes or
    /// parks it, and a park after close is released by the thread that
    /// parked it (`IoRuntime::run`).
    pub(crate) fn release(self: &Arc<Self>) -> bool {
        if !self.flight.claim_to_release() {
            return false;
        }
        self.finish(Outcome::Released);
        true
    }

    /// Publish `outcome` and hand one completion to every waiting queue.
    /// Only by the claim holder, once.
    fn finish(self: &Arc<Self>, outcome: Outcome) {
        // SAFETY: the claim holder is the only writer, and nobody reads the
        // cells before `Done` is published below.
        self.work.with_mut(|work| unsafe { *work = None });
        self.outcome
            .with_mut(|cell| unsafe { *cell = Some(outcome) });
        for queue in self.flight.land() {
            // A queue dropped since it registered refuses the message; it has
            // nobody left to tell.
            let _ = queue.deliver(Message::Done(Arc::clone(self)));
        }
    }

    /// How the unit ended, once it has.
    pub(crate) fn outcome(&self) -> Option<&Outcome> {
        if !self.is_done() {
            return None;
        }
        // SAFETY: `Done` was acquired above, after the one write; the cell is
        // never written again, and the reference lives no longer than `self`.
        self.outcome.with(|cell| unsafe { (*cell).as_ref() })
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn key(offset: u64) -> UnitKey {
        UnitKey {
            file_id: 1,
            offset,
            guard: None,
        }
    }

    fn failing() -> Work {
        Box::new(|_| Err(io::Error::other("device says no")))
    }

    #[test]
    fn exactly_one_claim_wins() {
        let unit = Arc::new(Unit::new(key(0), 10, failing()));
        let wins = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let unit = Arc::clone(&unit);
                    scope.spawn(move || unit.claim())
                })
                .collect();
            handles
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .filter(|won| *won)
                .count()
        });
        assert_eq!(wins, 1);
        assert!(!unit.release(), "a claimed unit is not released again");
    }

    #[test]
    fn a_run_publishes_its_outcome_and_refuses_later_registrations() {
        let cache = BlockCache::new(0);
        let unit = Arc::new(Unit::new(key(0), 10, failing()));
        assert!(unit.outcome().is_none());
        assert!(unit.claim());
        unit.run(&cache, || false);
        assert!(unit.is_done());
        assert!(
            matches!(unit.outcome(), Some(Outcome::Failed(err)) if err.to_string() == "device says no")
        );
        assert!(!unit.register(Arc::new(QueueShared::new(
            super::super::shared::test_id(),
            1 << 20
        ))));
    }

    #[test]
    fn a_panicking_run_still_finishes_the_unit() {
        let cache = BlockCache::new(0);
        let unit = Arc::new(Unit::new(key(0), 10, Box::new(|_| panic!("work panics"))));
        assert!(unit.claim());
        let caught =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unit.run(&cache, || false)));
        assert!(caught.is_err());
        assert!(matches!(unit.outcome(), Some(Outcome::Released)));
    }

    #[test]
    fn a_release_closes_a_free_unit_without_running_it() {
        let unit = Arc::new(Unit::new(
            key(0),
            10,
            Box::new(|_| unreachable!("a released unit never runs")),
        ));
        assert!(unit.release());
        assert!(matches!(unit.outcome(), Some(Outcome::Released)));
        assert!(!unit.claim());
    }

    /// A work that finds no slot on its first run and reads on its second,
    /// counting its runs.
    fn parks_once(runs: &Arc<std::sync::atomic::AtomicUsize>) -> Work {
        let runs = Arc::clone(runs);
        Box::new(move |_| {
            if runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                Err(io::Error::other("no open-file slot"))
            } else {
                Err(io::Error::other("the second run read"))
            }
        })
    }

    #[test]
    fn a_parked_run_keeps_its_work_and_runs_it_again_once_unparked() {
        let cache = BlockCache::new(0);
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let unit = Arc::new(Unit::new(key(0), 10, parks_once(&runs)));
        assert!(unit.claim());
        assert_eq!(unit.run(&cache, || true), Ran::Parked);
        assert!(unit.is_parked());
        assert!(!unit.is_done() && !unit.is_free());
        assert!(unit.outcome().is_none(), "a park is not an outcome");
        // Parked, nobody claims it: the poll that sees it skips it.
        assert!(!unit.claim());
        assert!(unit.unpark());
        assert!(!unit.unpark(), "one unpark per park");
        assert!(unit.claim());
        assert_eq!(unit.run(&cache, || false), Ran::Finished);
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(
            matches!(unit.outcome(), Some(Outcome::Failed(err)) if err.to_string() == "the second run read")
        );
        assert!(!unit.unpark(), "a finished unit is never unparked");
    }

    #[test]
    fn a_release_closes_a_parked_unit_and_a_late_unpark_does_nothing() {
        let cache = BlockCache::new(0);
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let unit = Arc::new(Unit::new(key(0), 10, parks_once(&runs)));
        let queue = Arc::new(QueueShared::new(super::super::shared::test_id(), 1 << 20));
        assert!(unit.register(Arc::clone(&queue)));
        assert!(unit.claim());
        assert_eq!(unit.run(&cache, || true), Ran::Parked);
        assert!(
            unit.release(),
            "close and a dropped queue release a parked unit"
        );
        assert!(matches!(unit.outcome(), Some(Outcome::Released)));
        assert!(!unit.unpark());
        assert!(!unit.claim());
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        let done = queue
            .take_inbox()
            .filter(|message| matches!(message, super::super::shared::Message::Done(_)))
            .count();
        assert_eq!(done, 1, "the waiting queue gets its completion");
    }

    #[test]
    fn a_failed_read_that_did_not_park_finishes() {
        let cache = BlockCache::new(0);
        let unit = Arc::new(Unit::new(key(0), 10, failing()));
        assert!(unit.claim());
        assert_eq!(unit.run(&cache, || false), Ran::Finished);
        assert!(unit.is_done());
    }
}
