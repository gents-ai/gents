//! The gate every write of a positional log passes: what keeps the write of
//! a discarded commit from landing after the rollback that discarded it cut
//! the file, and any write from landing on a range a sync has begun to make
//! durable (`wal_log.rs`).
//!
//! One word holds the log's write epoch and the number of writes in flight.
//! A write is counted in by a compare-and-swap that succeeds only while the
//! word still carries the epoch the write was decided in, and counted out
//! when its bytes are with the host. A rollback moves the epoch on, keeping
//! the count, and cuts the file once the count is zero.
//!
//! # Invariants
//!
//! - **WG1, a write of an older epoch never starts.** Its count in compares
//!   the epoch in the same step that counts it, so once the epoch moved no
//!   such write is counted in, and none writes uncounted.
//! - **WG2, after the epoch moved the count only falls.** Nothing is decided
//!   in the new epoch until the rollback that moved it is over, so the
//!   writes in flight then are the ones let in before, each of which leaves
//!   once. The rollback waits for exactly those, and for no thread that
//!   merely looked at the gate.
//! - **WG3, a rollback that may not wait is woken by the last write to
//!   leave.** A job a queue's poll steps registers its waker and looks at
//!   the count a last time; the write that takes the count to zero under a
//!   moved epoch wakes the waker registered. One of the two sees the other.
//!
//! # A sync and the writes in flight (WG4)
//!
//! A sector being written when the power goes may come back garbled,
//! whatever bytes were being written to it: a disk without power-loss
//! protection promises nothing for a sector in flight. So no write may land
//! on a range of the log once a sync that covers the range has begun.
//! Putting synced bytes back in flight, even the very same bytes, would let
//! a power cut take commits that were durable. The pipeline writes one
//! record from two threads when the first is slow (a thread whose patience
//! with the write unit's holder ran out writes the record itself,
//! `commit::write_unit`), so a late copy of a record, or a run that holds
//! it, is the case to rule out.
//!
//! So a write also counts in on one of two sides, the one the gate names
//! when it enters, and then looks again at whether its record is written
//! already: one that is leaves without writing. A sync of the written
//! prefix first turns the gate to the other side, then waits until every
//! write counted on the side it turned away from has left. A write counted
//! in before the turn has returned before the sync begins; one counted in
//! after it finds every record of the prefix written, and writes none of
//! them. Writes of records past the prefix go on meanwhile, on the new
//! side: a sync waits for the writes in flight when it began, never for a
//! stream of new ones.
//!
//! - **WG4, no write lands below a sync's prefix after the sync began.**
//!   The turn and a write's count in are each a read-modify-write of the
//!   side word, and so is a sync's look at the side word when it finds
//!   nothing in flight and turns nothing. Read-modify-writes of one word
//!   are ordered one after another, so every write is on exactly one side
//!   of the sync's: before it, and the sync sees it in flight and waits; or
//!   after it, and then everything the sync knew before it, the written
//!   records among it, is known to the write once it counted in (the sync's
//!   read-modify-write releases, the write's acquires), so the write sees
//!   its record written and writes nothing. No fence on the write's side:
//!   the cost is one read-modify-write per sync, not a fence per write.
//! - **WG5, a turn is waited out once.** The turn and the prefix it was
//!   made for are kept until the side it turned away from has drained, so a
//!   sync that parked and runs again finishes its own turn rather than
//!   turning again behind newer writes, and syncs no further than that
//!   prefix. Only the holder of the pipeline's round turns or settles the
//!   gate, so one turn is outstanding at most.

use std::task::{Poll, Waker};

use super::io::atomic_waker::AtomicWaker;
use crate::sync::internal::{AtomicU64, Ordering, fence};

/// Bits of the word that count the writes in flight; the epoch is above
/// them.
const WRITER_BITS: u32 = 24;
const WRITERS: u64 = (1 << WRITER_BITS) - 1;

/// The bit of the side word that names the side a write counts in on now.
const SIDE_BIT: u32 = 63;
/// Bits of the side word that count the writes in flight of one side: the
/// low side's from bit 0, the high side's above them.
const SIDE_BITS: u32 = 31;
const SIDE_COUNT: u64 = (1 << SIDE_BITS) - 1;

/// The side a write counting in on `sides` now joins: 0 or 1.
fn current(sides: u64) -> u64 {
    sides >> SIDE_BIT
}

/// The writes in flight counted on `side`.
fn on_side(sides: u64, side: u64) -> u64 {
    (sides >> (side * u64::from(SIDE_BITS))) & SIDE_COUNT
}

/// One write counted on `side`.
fn one_on(side: u64) -> u64 {
    1 << (side * u64::from(SIDE_BITS))
}

/// A write let in: what [`Gate::leave`] counts out.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    epoch: u64,
    side: u64,
}

/// See the module docs.
pub(crate) struct Gate {
    /// `epoch << WRITER_BITS | writes in flight`.
    word: AtomicU64,
    /// The side writes count in on now (bit 63), and the writes in flight
    /// on each side (WG4).
    sides: AtomicU64,
    /// The prefix a sync turned the gate for, while the side it turned away
    /// from has not drained; 0 when no turn is outstanding (WG5).
    turned: AtomicU64,
    /// The rollback or the sync parked until the writes it waits for have
    /// left (WG3, WG4). One at a time: either holds the pipeline's round.
    cleared: AtomicWaker,
}

impl Gate {
    /// A gate at epoch zero with nothing in flight.
    pub(crate) fn new() -> Self {
        Self {
            word: AtomicU64::new(0),
            sides: AtomicU64::new(0),
            turned: AtomicU64::new(0),
            cleared: AtomicWaker::new(),
        }
    }

    /// The epoch a write must have been decided in to be let in now.
    pub(crate) fn epoch(&self) -> u64 {
        self.word.load(Ordering::SeqCst) >> WRITER_BITS
    }

    /// Count a write decided in `epoch` in. `None` when the gate has moved
    /// on: the write is of a discarded commit and is not made (WG1).
    ///
    /// The caller looks at whether its record is written already only after
    /// this returns, and writes nothing when it is (WG4).
    pub(crate) fn enter(&self, epoch: u64) -> Option<Entry> {
        let mut word = self.word.load(Ordering::SeqCst);
        loop {
            if word >> WRITER_BITS != epoch {
                return None;
            }
            debug_assert!(
                word & WRITERS < WRITERS,
                "more writes in flight than the gate counts"
            );
            match self.word.compare_exchange_weak(
                word,
                word + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(now) => word = now,
            }
        }
        let mut sides = self.sides.load(Ordering::SeqCst);
        let side = loop {
            let side = current(sides);
            debug_assert!(
                on_side(sides, side) < SIDE_COUNT,
                "more writes in flight than a side counts"
            );
            match self.sides.compare_exchange_weak(
                sides,
                sides + one_on(side),
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break side,
                Err(now) => sides = now,
            }
        };
        // WG4 needs no fence here: see `settle`.
        Some(Entry { epoch, side })
    }

    /// Count out a write that [`Self::enter`] let in.
    pub(crate) fn leave(&self, entry: Entry) {
        let sides = self.sides.fetch_sub(one_on(entry.side), Ordering::SeqCst) - one_on(entry.side);
        // The last write of a side a sync turned away from: the sync goes on
        // (WG4).
        let drained = on_side(sides, entry.side) == 0 && current(sides) != entry.side;
        let left = self.word.fetch_sub(1, Ordering::SeqCst) - 1;
        // The gate moved on under this write, and it was the last one in
        // flight: a rollback parked for it goes on (WG3).
        let cleared = left & WRITERS == 0 && left >> WRITER_BITS != entry.epoch;
        if drained || cleared {
            // With the fence in `clear_or_park` and in `settle`, one side
            // sees the other.
            fence(Ordering::SeqCst);
            self.cleared.wake();
        }
    }

    /// Move the gate on to `epoch`: no write decided before is let in from
    /// here on (WG1). The writes in flight stay counted.
    pub(crate) fn turn(&self, epoch: u64) {
        debug_assert!(epoch < 1 << (u64::BITS - WRITER_BITS));
        let mut word = self.word.load(Ordering::SeqCst);
        loop {
            let turned = epoch << WRITER_BITS | word & WRITERS;
            match self
                .word
                .compare_exchange_weak(word, turned, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return,
                Err(now) => word = now,
            }
        }
    }

    /// Whether no write is in flight.
    pub(crate) fn is_clear(&self) -> bool {
        self.word.load(Ordering::SeqCst) & WRITERS == 0
    }

    /// Forget a turn a sync made: a rollback cut the log back, and the
    /// prefix it was made for is gone. Called with no write in flight.
    pub(crate) fn forget_turn(&self) {
        debug_assert!(self.is_clear());
        self.turned.store(0, Ordering::SeqCst);
    }

    /// WG4 for a sync of the records written below `through`, called by the
    /// holder of the pipeline's round once it knows them written: `Ready`
    /// with the offset the sync may cover now. Every write that may land
    /// below it has returned, and none will start.
    ///
    /// That is `through` itself when nothing was in flight or this call made
    /// the turn and waited it out. A turn an earlier call made and parked on
    /// is finished first, for its own prefix, and the offset is then the
    /// lower of the two (WG5). `Pending` registers `waker`, which the last
    /// write of the side turned away from wakes.
    pub(crate) fn settle(&self, through: u64, waker: &Waker) -> Poll<u64> {
        debug_assert!(through > 0, "a prefix past the stamp");
        loop {
            let turned = self.turned.load(Ordering::SeqCst);
            if turned != 0 {
                let away = current(self.sides.load(Ordering::SeqCst)) ^ 1;
                if on_side(self.sides.load(Ordering::SeqCst), away) != 0 {
                    self.cleared.register(waker);
                    fence(Ordering::SeqCst);
                    if on_side(self.sides.load(Ordering::SeqCst), away) != 0 {
                        return Poll::Pending;
                    }
                }
                self.turned.store(0, Ordering::SeqCst);
                return Poll::Ready(turned.min(through));
            }
            // A read-modify-write that changes nothing, not a load (WG4): a
            // write that counts in after it acquires what this thread knew
            // before it, the records it found written, and so writes none of
            // them; one that counted in before it is seen here, in flight.
            let sides = self.sides.fetch_add(0, Ordering::SeqCst);
            let now = current(sides);
            debug_assert_eq!(
                on_side(sides, now ^ 1),
                0,
                "a side turned away from drained before its turn was let go"
            );
            if on_side(sides, now) == 0 {
                return Poll::Ready(through);
            }
            self.turned.store(through, Ordering::SeqCst);
            // Only this side's count can change under the turn, so one
            // exchange that fails is retried with what it read.
            let mut seen = sides;
            while let Err(now) = self.sides.compare_exchange_weak(
                seen,
                seen ^ (1 << SIDE_BIT),
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                seen = now;
            }
        }
    }

    /// [`Self::is_clear`] for a rollback that may not wait: when a write is
    /// still in flight, `waker` is woken by the last one to leave, and
    /// `false` tells the caller to park (WG3). Called after [`Self::turn`].
    pub(crate) fn clear_or_park(&self, waker: &Waker) -> bool {
        if self.is_clear() {
            return true;
        }
        self.cleared.register(waker);
        fence(Ordering::SeqCst);
        self.is_clear()
    }
}

#[cfg(all(test, not(loom)))]
#[path = "wal_gate_tests.rs"]
mod tests;
