#![cfg(loom)]

//! loom models of the lock-free CLOCK block cache (`src/engine/block_cache`).
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_block_cache
//! ```
//!
//! # What these models check, and what they do not
//!
//! The cache's map is kovan's `HopscotchMap` and its pin is a `std` `Arc`,
//! neither of which loom can see, so like `loom_read_view.rs` these models
//! transcribe the protocol rather than call the cache: each one names the
//! code it mirrors. They check the three rules the cache stands on:
//!
//! * **The pin check.** A reader pins a block by upgrading the map's weak
//!   handle, a CAS of the block's strong count from `n > 0` to `n + 1`
//!   (`WeakEntry::upgrade`). The hand evicts by a CAS of that count from
//!   one, the cache's own reference, to zero (`release_if_unpinned`). A
//!   reader never holds a freed block; the RED evicts ignoring pins.
//! * **The claim and the reference bit.** Hands claim a slot by one CAS of
//!   its state word (`Ring::step`), so one entry is removed at most once
//!   however many hands race, and a first revolution clears a reference
//!   bit a reader set instead of evicting.
//! * **The byte bound.** Every reservation is one bounded CAS on the total
//!   (`bounded_add`), so racing inserts never take it past the budget; the
//!   RED checks then adds in two steps.
//!
//! Every model asserts a floor on the interleavings loom explored and a
//! witness count for the schedule it exists for, and every RED must fail.

use std::panic::AssertUnwindSafe;
use std::sync::Arc as StdArc;
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence};
use loom::thread;

/// An entry is published in the slot (`ring::OCCUPIED`).
const OCCUPIED: u64 = 1;
/// One thread holds the slot (`ring::BUSY`).
const BUSY: u64 = 1 << 1;
/// Read since the hand last passed (`ring::REF`).
const REF: u64 = 1 << 3;

/// One cached block: its slot word, its strong count (the cache holds
/// one), and what the model watches.
struct Entry {
    word: AtomicU64,
    strong: AtomicUsize,
    freed: AtomicBool,
    removed: AtomicUsize,
    /// Set when the hand's pin check found a reader holding the block.
    met_pin: AtomicBool,
}

impl Entry {
    fn cached() -> Self {
        Self {
            word: AtomicU64::new(OCCUPIED),
            strong: AtomicUsize::new(1),
            freed: AtomicBool::new(false),
            removed: AtomicUsize::new(0),
            met_pin: AtomicBool::new(false),
        }
    }

    /// `WeakEntry::upgrade`: `Weak::upgrade`'s CAS loop on the strong count.
    fn pin(&self) -> bool {
        let mut n = self.strong.load(Ordering::Relaxed);
        loop {
            if n == 0 {
                return false;
            }
            match self
                .strong
                .compare_exchange_weak(n, n + 1, Ordering::Acquire, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(actual) => n = actual,
            }
        }
    }

    /// Dropping a reader's `Arc`: the last reference frees the block.
    fn unpin(&self) {
        if self.strong.fetch_sub(1, Ordering::Release) == 1 {
            fence(Ordering::Acquire);
            self.freed.store(true, Ordering::Relaxed);
        }
    }

    /// `Ring::touch`: one load, one CAS when the bit is clear.
    fn touch(&self) {
        let word = self.word.load(Ordering::Relaxed);
        if word & OCCUPIED != 0 && word & REF == 0 {
            let _ =
                self.word
                    .compare_exchange(word, word | REF, Ordering::Relaxed, Ordering::Relaxed);
        }
    }

    /// One step of the hand on this entry (`Ring::step` then
    /// `evict_or_keep`). `ignore_pins` is the RED. Returns whether this
    /// step removed the entry.
    fn hand(&self, forced: bool, ignore_pins: bool) -> bool {
        let word = self.word.load(Ordering::Acquire);
        if word & OCCUPIED == 0 || word & BUSY != 0 {
            return false;
        }
        if !forced && word & REF != 0 {
            let _ =
                self.word
                    .compare_exchange(word, word & !REF, Ordering::Relaxed, Ordering::Relaxed);
            return false;
        }
        if self
            .word
            .compare_exchange(word, word | BUSY, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        let evicted = if ignore_pins {
            // The defect: drop the cache's reference and free the block as
            // if nobody else could hold it.
            self.strong.fetch_sub(1, Ordering::AcqRel);
            true
        } else {
            // `Arc::try_unwrap`: one CAS from the cache's lone reference to
            // zero, then an acquire fence.
            let won = self
                .strong
                .compare_exchange(1, 0, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok();
            if won {
                fence(Ordering::Acquire);
            } else {
                self.met_pin.store(true, Ordering::Relaxed);
            }
            won
        };
        if evicted {
            self.freed.store(true, Ordering::Release);
            self.removed.fetch_add(1, Ordering::Relaxed);
            self.word.store(0, Ordering::Release);
        } else {
            self.word.fetch_and(!BUSY, Ordering::Release);
        }
        evicted
    }
}

fn run_model(name: &str, floor: usize, body: impl Fn() + Send + Sync + 'static) {
    let executions = StdArc::new(StdAtomicUsize::new(0));
    let counter = StdArc::clone(&executions);
    loom::model(move || {
        counter.fetch_add(1, StdOrdering::Relaxed);
        body();
    });
    let explored = executions.load(StdOrdering::Relaxed);
    println!("loom model `{name}`: {explored} interleavings explored");
    assert!(
        explored >= floor,
        "`{name}` explored only {explored} interleavings (floor {floor})"
    );
}

fn expect_violation(name: &str, body: impl Fn() + Send + Sync + 'static) {
    println!("negative control `{name}`: loom must report a violation below");
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| loom::model(body)));
    assert!(
        outcome.is_err(),
        "negative control `{name}` passed: the model cannot tell the defect from the real protocol"
    );
}

/// A reader pinning a block while the hand tries to evict it.
fn pin_model(ignore_pins: bool, witnessed: &StdArc<StdAtomicUsize>) {
    let entry = Arc::new(Entry::cached());

    let reader = {
        let entry = Arc::clone(&entry);
        thread::spawn(move || {
            if entry.pin() {
                entry.touch();
                assert!(
                    !entry.freed.load(Ordering::Acquire),
                    "a reader holds a block the hand freed"
                );
                entry.unpin();
            }
        })
    };
    let hand = {
        let entry = Arc::clone(&entry);
        thread::spawn(move || {
            entry.hand(true, ignore_pins);
        })
    };
    reader.join().unwrap();
    hand.join().unwrap();

    // The schedule this model exists for: the hand's pin check ran while
    // the reader held the block.
    if entry.met_pin.load(Ordering::Relaxed) {
        witnessed.fetch_add(1, StdOrdering::Relaxed);
    }
    let removed = entry.removed.load(Ordering::Relaxed);
    let strong = entry.strong.load(Ordering::Relaxed);
    if !ignore_pins {
        assert!(removed <= 1, "one entry was removed twice");
        if removed == 1 {
            assert_eq!(strong, 0, "an evicted entry still has a reference");
        } else {
            assert_eq!(strong, 1, "a kept entry lost the cache's reference");
            assert!(
                !entry.freed.load(Ordering::Relaxed),
                "a kept block was freed"
            );
        }
    }
}

/// The hand never evicts a block a reader holds.
#[test]
fn the_hand_never_evicts_a_pinned_block() {
    let witnessed = StdArc::new(StdAtomicUsize::new(0));
    let seen = StdArc::clone(&witnessed);
    run_model("pin_check", 16, move || pin_model(false, &seen));
    assert!(
        witnessed.load(StdOrdering::Relaxed) > 0,
        "the hand never met a pinned block"
    );
}

/// RED: an eviction that ignores pins frees a block a reader holds.
#[test]
fn an_eviction_that_ignores_pins_is_caught() {
    let witnessed = StdArc::new(StdAtomicUsize::new(0));
    expect_violation("pin_check/ignore_pins", move || pin_model(true, &witnessed));
}

/// Two hands racing for one entry while a reader re-references it: the
/// first revolution clears the bit, at most one hand removes the entry,
/// and the bytes it charged come back once.
#[test]
fn racing_hands_remove_an_entry_at_most_once() {
    let witnessed = StdArc::new(StdAtomicUsize::new(0));
    let seen = StdArc::clone(&witnessed);
    run_model("racing_hands", 16, move || {
        let entry = Arc::new(Entry::cached());
        entry.word.store(OCCUPIED | REF, Ordering::Relaxed);
        let charged = Arc::new(AtomicUsize::new(1));
        let hands: Vec<_> = [false, true]
            .into_iter()
            .map(|forced| {
                let (entry, charged) = (Arc::clone(&entry), Arc::clone(&charged));
                thread::spawn(move || {
                    // `remove` returns the charge of exactly the entry its
                    // own claim took out.
                    if entry.hand(forced, false) {
                        charged.fetch_sub(1, Ordering::AcqRel);
                    }
                })
            })
            .collect();
        let reader = {
            let entry = Arc::clone(&entry);
            thread::spawn(move || entry.touch())
        };
        for hand in hands {
            hand.join().unwrap();
        }
        reader.join().unwrap();
        let removed = entry.removed.load(Ordering::Relaxed);
        assert!(removed <= 1, "two hands removed one entry");
        assert_eq!(
            charged.load(Ordering::Relaxed),
            1 - removed,
            "the entry's bytes came back a different number of times than it was removed"
        );
        if removed == 1 {
            seen.fetch_add(1, StdOrdering::Relaxed);
        }
    });
    assert!(
        witnessed.load(StdOrdering::Relaxed) > 0,
        "no schedule evicted the entry"
    );
}

/// `bounded_add`, or the two-step check-then-add the RED makes of it.
fn reserve(total: &AtomicUsize, size: usize, limit: usize, two_steps: bool) -> bool {
    if two_steps {
        if total.load(Ordering::Acquire) + size > limit {
            return false;
        }
        total.fetch_add(size, Ordering::AcqRel);
        return true;
    }
    let mut current = total.load(Ordering::Acquire);
    loop {
        if current + size > limit {
            return false;
        }
        match total.compare_exchange_weak(
            current,
            current + size,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(actual) => current = actual,
        }
    }
}

/// Two inserters racing for the last room in the budget.
fn budget_model(two_steps: bool, witnessed: &StdArc<StdAtomicUsize>) {
    const LIMIT: usize = 3;
    let total = Arc::new(AtomicUsize::new(1));
    let inserters: Vec<_> = (0..2)
        .map(|_| {
            let total = Arc::clone(&total);
            thread::spawn(move || reserve(&total, 2, LIMIT, two_steps))
        })
        .collect();
    let admitted = inserters
        .into_iter()
        .map(|inserter| inserter.join().unwrap())
        .filter(|ok| *ok)
        .count();
    let total = total.load(Ordering::Relaxed);
    assert!(
        total <= LIMIT,
        "racing reservations took the total to {total} past {LIMIT}"
    );
    assert_eq!(total, 1 + 2 * admitted);
    if admitted == 1 {
        witnessed.fetch_add(1, StdOrdering::Relaxed);
    }
}

/// Racing reservations never take the total past the budget.
#[test]
fn racing_reservations_hold_the_byte_bound() {
    let witnessed = StdArc::new(StdAtomicUsize::new(0));
    let seen = StdArc::clone(&witnessed);
    run_model("byte_bound", 4, move || budget_model(false, &seen));
    assert!(
        witnessed.load(StdOrdering::Relaxed) > 0,
        "no schedule refused one inserter"
    );
}

/// RED: checking the room and taking it in two steps overruns the budget.
#[test]
fn a_two_step_reservation_is_caught() {
    let witnessed = StdArc::new(StdAtomicUsize::new(0));
    expect_violation("byte_bound/two_steps", move || {
        budget_model(true, &witnessed)
    });
}
