//! Loom models of the slots the open transactions' claims sit in
//! (`claim_slots.rs`).
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --release --test loom_claim_slots
//! ```
//!
//! Each model drives the production [`ClaimSlots`], whose free word, links,
//! counter and slots come from `crate::sync::internal` and so are loom's
//! under `--cfg loom`. Its chunks sit in `std::sync::OnceLock`s, which loom
//! does not see: a chunk one model thread made would reach another with no
//! edge loom knows of, and loom would let that thread read the chunk's slots
//! as they were made, before stores C11 orders ahead of its read. So every
//! chunk a model uses is made before its threads start (the spawn is the
//! edge). A chunk made while close runs is `ClaimSlots.tla`'s
//! (`MC_ClaimSlots_Green_ReachesLateChunk`). `fill`'s store is a swap
//! under loom (`sync::internal::store_ptr`): loom 0.7 lets a plain store's
//! thread and a racing swap both read past it, which C11 forbids.
//! A claim is a [`Probe`]: an id, and a loom cell written when the probe is
//! made, after its slot is reserved (as `Claim::track` makes its claim). A
//! thread that reads a probe it took from a slot without the slot's Release
//! and Acquire ordering it after that write fails the model.
//!
//! What each proves, against the real table:
//!
//! - **Every reference dropped once**: two owners list and end a claim each
//!   while close drains. No slot is held by two owners at once, close sees
//!   each claim at most once and reads it after it was made, and each
//!   claim's reference is dropped exactly once, by its owner or by close.
//! - **Close finds what was listed before it began**, in the second chunk,
//!   past a first chunk with nothing in it.
//! - **No ABA**: with slots 0 and 1 free, one owner takes a slot while
//!   another takes both, gives the first back and takes again. No slot is
//!   held twice, and none is lost from the free stack.
//! - **A pushed slot keeps its link**: an owner gives a slot back while
//!   another takes one; the taker reads the slot below the top that the
//!   giver wrote, so no free slot is lost.
//!
//! Each is paired with a calibration that plants one step wrong
//! ([`Mutant`]) and must fail. `proofs/tla/ClaimSlots.tla` checks the same
//! protocol with every step atomic and in one order; these check the
//! orderings that make the steps so.

#![allow(unsafe_code)]

use std::sync::Arc as StdArc;
use std::sync::atomic::{
    AtomicBool as StdAtomicBool, AtomicUsize as StdAtomicUsize, Ordering as StdOrdering,
};

use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, Ordering};
use loom::thread;

use super::claim_slots::{ClaimSlots, Mutant};
use crate::engine::loom_model::{Witness, explore_bounded};

/// Slots of the first chunk (`FIRST` in `claim_slots.rs`): the next slot
/// handed out after these is the first of the second chunk.
const FIRST_CHUNK: usize = 64;

/// A claim: its id, and a cell written when it is made.
struct Probe {
    id: usize,
    made: UnsafeCell<usize>,
}

// SAFETY: the cell is written once, when the probe is made, and only read
// after; loom checks that every read is ordered after that write.
unsafe impl Sync for Probe {}

impl Probe {
    /// A new claim, made by the calling thread.
    fn new(id: usize) -> StdArc<Self> {
        StdArc::new(Self {
            id,
            made: UnsafeCell::new(id),
        })
    }

    /// Read the cell. Loom fails the model unless the probe's making is
    /// ordered before this read.
    fn check(&self) {
        // SAFETY: the cell is written only when the probe is made.
        let made = self.made.with(|cell| unsafe { *cell });
        assert_eq!(
            made, self.id,
            "a claim read through a slot is not the one listed"
        );
    }
}

/// Which slots an owner holds now, to catch one held twice. Plain atomics:
/// loom runs one thread at a time, so these see every earlier write, and
/// they add no schedules of their own.
struct Held([StdAtomicBool; 4]);

impl Held {
    fn new() -> Self {
        Self(std::array::from_fn(|_| StdAtomicBool::new(false)))
    }

    fn take(&self, slot: u32) {
        assert!(
            !self.0[slot as usize].swap(true, StdOrdering::SeqCst),
            "free stack broken: slot {slot} held twice"
        );
    }

    fn give(&self, slot: u32) {
        self.0[slot as usize].store(false, StdOrdering::SeqCst);
    }
}

/// The reference count of a claim whose slot reference was dropped exactly
/// once, with the model holding two: one to return, one so that a reference
/// dropped twice leaves a count of one instead of freeing the claim.
const ONCE_DROPPED: usize = 2;

/// An owner: reserve, make the claim, list it, end it. Returns two handles
/// to the claim.
fn owner(
    slots: &Arc<ClaimSlots<Probe>>,
    held: &StdArc<Held>,
    id: usize,
) -> thread::JoinHandle<(StdArc<Probe>, StdArc<Probe>)> {
    let (slots, held) = (Arc::clone(slots), StdArc::clone(held));
    thread::spawn(move || {
        let slot = slots.reserve().expect("the table has room");
        held.take(slot);
        let probe = Probe::new(id);
        slots.fill(slot, &probe);
        held.give(slot);
        slots.remove(slot);
        let keep = StdArc::clone(&probe);
        (probe, keep)
    })
}

/// Two owners list and end a claim each while close drains.
fn owners_and_close(mutant: Mutant, witness: &Witness) {
    let slots = Arc::new(ClaimSlots::with_mutant(mutant));
    // The first chunk is made, and slot 0 freed, before the threads start.
    let zero = slots.reserve().expect("the table has room");
    slots.remove(zero);
    let held = StdArc::new(Held::new());
    let visits: StdArc<[StdAtomicUsize; 2]> =
        StdArc::new(std::array::from_fn(|_| StdAtomicUsize::new(0)));
    let first = owner(&slots, &held, 0);
    let second = owner(&slots, &held, 1);
    let closer = {
        let (slots, visits) = (Arc::clone(&slots), StdArc::clone(&visits));
        thread::spawn(move || {
            slots.drain(|claim| {
                claim.check();
                visits[claim.id].fetch_add(1, StdOrdering::SeqCst);
            })
        })
    };
    let claims = [first.join().expect("owner"), second.join().expect("owner")];
    closer.join().expect("closer");
    for (probe, keep) in claims {
        let seen = visits[probe.id].load(StdOrdering::SeqCst);
        assert!(seen <= 1, "close saw claim {} {seen} times", probe.id);
        if seen == 1 {
            witness.record();
        }
        let count = StdArc::strong_count(&probe);
        if count < ONCE_DROPPED {
            // One handle stands for the reference dropped twice: letting both
            // go would free the claim twice.
            std::mem::forget(keep);
            panic!("claim {}'s slot reference was dropped twice", probe.id);
        }
        assert!(
            count == ONCE_DROPPED,
            "claim {}'s slot reference was never dropped",
            probe.id
        );
    }
}

/// Two owners list and end a claim each while close drains: no slot held
/// twice, close sees each claim at most once and after it was made, and each
/// slot reference is dropped exactly once.
pub fn every_claim_is_dropped_once() {
    explore_bounded(
        "claim slots: owners and close",
        Some(3),
        10_000,
        1_000,
        |witness| owners_and_close(Mutant::None, witness),
    );
}

/// Calibration: a slot emptied by a load and a store lets the owner and
/// close both take its reference.
pub fn calibration_a_load_and_store_drops_a_claim_twice() {
    explore_bounded(
        "claim slots: take by load (calibration)",
        Some(3),
        1,
        0,
        |witness| owners_and_close(Mutant::TakeByLoad, witness),
    );
}

/// Calibration: a slot given back before it is emptied is filled again
/// over the claim it still holds, which is never dropped.
pub fn calibration_giving_back_first_loses_a_claim() {
    explore_bounded(
        "claim slots: give back first (calibration)",
        Some(3),
        1,
        0,
        |witness| owners_and_close(Mutant::GiveBackFirst, witness),
    );
}

/// Calibration: a claim stored with `Relaxed` is read by close before its
/// making is visible.
pub fn calibration_a_relaxed_fill_hides_the_claim() {
    explore_bounded(
        "claim slots: relaxed fill (calibration)",
        Some(3),
        1,
        0,
        |witness| owners_and_close(Mutant::RelaxedFill, witness),
    );
}

/// An owner lists a claim in the first slot of the second chunk, then says
/// so; close, if it heard, must take that claim.
fn listed_before_close(mutant: Mutant, witness: &Witness) {
    let slots = Arc::new(ClaimSlots::with_mutant(mutant));
    // Slots 0 to 63 are handed out and their chunk is never made; slot 64,
    // the first of the second chunk, is handed out and given back, which
    // makes its chunk before the threads start. The owner takes slot 64.
    for _ in 0..FIRST_CHUNK {
        slots.reserve().expect("the table has room");
    }
    let opener = slots.reserve().expect("the table has room");
    slots.remove(opener);
    let listed = Arc::new(AtomicBool::new(false));
    let lister = {
        let (slots, listed) = (Arc::clone(&slots), Arc::clone(&listed));
        thread::spawn(move || {
            let slot = slots.reserve().expect("the table has room");
            assert_eq!(slot as usize, FIRST_CHUNK, "the owner's slot opens a chunk");
            let probe = Probe::new(0);
            slots.fill(slot, &probe);
            listed.store(true, Ordering::Release);
            (slot, probe)
        })
    };
    let closer = {
        let (slots, listed) = (Arc::clone(&slots), Arc::clone(&listed));
        thread::spawn(move || {
            let heard = listed.load(Ordering::Acquire);
            let mut took = false;
            slots.drain(|claim| {
                claim.check();
                took = true;
            });
            (heard, took)
        })
    };
    let (slot, probe) = lister.join().expect("owner");
    let (heard, took) = closer.join().expect("closer");
    if heard {
        witness.record();
        assert!(took, "close missed a claim listed before it began");
    }
    // The owner ends the transaction: its slot reference is gone whether
    // close took it or not.
    slots.remove(slot);
    assert_eq!(
        StdArc::strong_count(&probe),
        1,
        "the slot reference was not dropped once"
    );
}

/// Close takes a claim listed before it began, in a chunk made while it ran.
pub fn close_finds_a_claim_listed_before_it() {
    explore_bounded(
        "claim slots: listed before close",
        Some(3),
        50,
        10,
        |witness| listed_before_close(Mutant::None, witness),
    );
}

/// Calibration: a drain that stops at the first slot with nothing to take
/// misses the claim.
pub fn calibration_a_drain_that_stops_early_misses_a_claim() {
    explore_bounded(
        "claim slots: drain stops at empty (calibration)",
        Some(3),
        1,
        0,
        |witness| listed_before_close(Mutant::StopAtEmpty, witness),
    );
}

/// Reserve three slots on a table that handed out slots 0, 1 and maybe 2
/// and holds none of them: they come back as exactly those, each once.
fn assert_free_stack_whole(slots: &ClaimSlots<Probe>) {
    let mut back: Vec<u32> = (0..3).map(|_| slots.reserve().expect("room")).collect();
    back.sort_unstable();
    assert!(
        back.windows(2).all(|pair| pair[0] != pair[1]),
        "free stack broken: a slot came back twice: {back:?}"
    );
    assert_eq!(back, vec![0, 1, 2], "free stack broken: a slot was lost");
}

/// Slots 0 and 1 free, 0 on top. Owner 1 takes a slot; owner 2 takes two,
/// gives the first back and takes again. Owner 1 may read "top 0, below 1"
/// and be paused while the stack goes from "0 on 1" to "0 alone".
fn aba(mutant: Mutant, witness: &Witness) {
    let slots = Arc::new(ClaimSlots::with_mutant(mutant));
    let held = StdArc::new(Held::new());
    let (zero, one) = (
        slots.reserve().expect("room"),
        slots.reserve().expect("room"),
    );
    slots.remove(one);
    slots.remove(zero);
    let single = {
        let (slots, held) = (Arc::clone(&slots), StdArc::clone(&held));
        thread::spawn(move || {
            let slot = slots.reserve().expect("room");
            held.take(slot);
            held.give(slot);
            slots.remove(slot);
            slot
        })
    };
    let juggler = {
        let (slots, held) = (Arc::clone(&slots), StdArc::clone(&held));
        thread::spawn(move || {
            let a = slots.reserve().expect("room");
            held.take(a);
            let b = slots.reserve().expect("room");
            held.take(b);
            held.give(a);
            slots.remove(a);
            let c = slots.reserve().expect("room");
            held.take(c);
            held.give(b);
            slots.remove(b);
            held.give(c);
            slots.remove(c);
            a
        })
    };
    let got = single.join().expect("owner");
    let first = juggler.join().expect("owner");
    if got == first {
        witness.record();
    }
    assert_free_stack_whole(&slots);
}

/// No slot is held twice, and none is lost, when a taker is paused across
/// another's take, take and give back.
pub fn a_reused_slot_is_never_held_twice() {
    explore_bounded("claim slots: ABA", Some(2), 300, 100, |witness| {
        aba(Mutant::None, witness)
    });
}

/// Calibration: without the change count the paused taker's CAS lands and
/// puts a held slot on the free stack.
pub fn calibration_without_the_tag_a_slot_is_held_twice() {
    explore_bounded(
        "claim slots: ABA without the tag (calibration)",
        Some(2),
        1,
        0,
        |witness| aba(Mutant::NoTag, witness),
    );
}

/// Slot 1 free, slot 0 held by owner 1, which gives it back while owner 2
/// takes a slot and gives it back.
fn push_and_pop(mutant: Mutant, witness: &Witness) {
    let slots = Arc::new(ClaimSlots::with_mutant(mutant));
    let (zero, one) = (
        slots.reserve().expect("room"),
        slots.reserve().expect("room"),
    );
    slots.remove(one);
    let giver = {
        let slots = Arc::clone(&slots);
        thread::spawn(move || slots.remove(zero))
    };
    let taker = {
        let slots = Arc::clone(&slots);
        thread::spawn(move || {
            let slot = slots.reserve().expect("room");
            slots.remove(slot);
            slot
        })
    };
    giver.join().expect("owner");
    if taker.join().expect("owner") == zero {
        witness.record();
    }
    assert_free_stack_whole(&slots);
}

/// A slot given back is taken with the link its giver wrote: no free slot
/// is lost.
pub fn a_pushed_slot_keeps_its_link() {
    explore_bounded("claim slots: push and pop", None, 50, 10, |witness| {
        push_and_pop(Mutant::None, witness)
    });
}

/// Calibration: a push made with `Relaxed` lets the taker read the slot's
/// link from before it, and the slot below is lost.
pub fn calibration_a_relaxed_push_loses_a_slot() {
    explore_bounded(
        "claim slots: relaxed push (calibration)",
        None,
        1,
        0,
        |witness| push_and_pop(Mutant::RelaxedPush, witness),
    );
}
