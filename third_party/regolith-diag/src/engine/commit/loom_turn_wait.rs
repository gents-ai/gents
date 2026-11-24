//! Loom models of a blocking call parked far from its turn (`turn_wait.rs`,
//! P6, E45 step 2): the count against the frontier's pass, the naming and
//! telling of the slots whose turn is near, and the wake a landing gives.
//!
//! # The story
//!
//! A blocking call whose slot is far behind the decided frontier parks on
//! its own slot rather than yield to the thread deciding, over and over. It
//! must be woken when its turn nears, or it sleeps out its whole patience
//! while the slots before it wait for nobody. It counts itself in first,
//! then registers on its slot and looks at the frontier. A thread that
//! passes the frontier looks at the count after the pass and, if anybody is
//! counted, names the slots up to the lead past it; a thread whose commit
//! is done claims the named slots and marks each near, which wakes its
//! owner. A slot that lands, published by whoever helps, wakes its owner
//! too: one with no record has no other flag that would.
//!
//! # What runs the production code and what mirrors it
//!
//! The count, the naming, the claim of the named slots and the look of the
//! sleeper ([`Turns`]: `count_in`, `name`, `claim_told`, `near`), the
//! decided frontier ([`Ring`]: `pass_decided`, `decided`) and the slot's
//! park, marks and landing ([`Descriptor`]: `park`, `register`, `mark`,
//! `land`) are the production types: under `--cfg loom` their atomics and
//! parking are loom's. The few lines that tie them together live on the
//! engine, which owns files loom cannot run (`pipeline.rs`:
//! `pass_decided`, `park_far`, `wake_near`), so the models repeat them line
//! for line. They are protocol models and say so.
//!
//! # What was shrunk
//!
//! One sleeper and one thread that passes the frontier over the two slots
//! before the sleeper's turn nears. Every model is bounded by an explicit
//! preemption bound, never by time; a park has no time out under loom, so
//! a wake that never comes is a deadlock loom reports. Each positive model
//! has a calibration that gets one step wrong and must fail its assertion;
//! a calibration does not park for real, it registers as a park does and
//! the model then asks whether anybody woke it. The sleeper runs on a
//! thread of its own and the model's thread only joins: loom does not
//! model an unpark that reaches a thread blocked in a join.

use std::time::Duration;

use loom::sync::Arc;
use loom::thread;

use super::descriptor::{Descriptor, NEAR, Work};
use super::ring::Ring;
use super::turn_wait::Turns;
use crate::engine::loom_model::explore_bounded;
use crate::sync::internal::Ordering;

/// A blocking call's slot, installed at `index`.
fn blocking_slot(index: u64) -> Arc<Descriptor> {
    let desc = Descriptor::new(
        Work::Allocate {
            key: Vec::new(),
            n: 0,
            read_queue: None,
        },
        true,
    );
    desc.set_index(index);
    Arc::new(desc)
}

/// A frontier and the slot of a call far from its turn: more than the lead
/// past the next slot to decide. Returns the ring, the first slot to
/// decide and the sleeper's slot.
fn far_setup(turns: &Turns) -> (Arc<Ring<Descriptor>>, u64, Arc<Descriptor>) {
    let ring: Arc<Ring<Descriptor>> = Arc::new(Ring::new(2));
    let next = ring.decided() + 1;
    let own = blocking_slot(next + turns.lead() + 1);
    (ring, next, own)
}

/// `RegolithEngine::pass_decided` over slots `from..=to`: each pass, then
/// the naming (`Turns::name`).
fn pass(ring: &Ring<Descriptor>, turns: &Turns, from: u64, to: u64) {
    for index in from..=to {
        assert!(ring.pass_decided(index), "this thread passes every slot");
        turns.name(index);
    }
}

/// `RegolithEngine::wake_near` for the one slot the model has: claim the
/// named slots and mark the sleeper's near if it is among them.
fn tell(turns: &Turns, own: &Descriptor) {
    if let Some((from, to)) = turns.claim_told()
        && from < own.index()
        && own.index() <= to
    {
        own.mark(NEAR);
    }
}

/// `RegolithEngine::park_far` past its first test: count in, park on the
/// slot until its turn is near, count out. `true` when it slept.
fn park_far(ring: &Ring<Descriptor>, turns: &Turns, own: &Descriptor) -> bool {
    turns.count_in();
    let slept = own.park(Duration::ZERO, || turns.near(own, ring.decided()));
    turns.count_out();
    slept
}

/// T1 and T2: the sleeper counts in and parks while the other thread
/// passes the two slots that bring its turn near, then, its commit done,
/// tells the slots it named. The sleeper is always woken: it looks at the
/// frontier after its count and the passer at the count after its pass, so
/// one sees the other, and a slot named is told by the next claim.
///
/// Bound: 3 preemptions.
pub fn a_call_far_from_its_turn_is_named_and_told() {
    explore_bounded(
        "a_call_far_from_its_turn_is_named_and_told",
        Some(3),
        8,
        1,
        |witness| {
            let turns = Arc::new(Turns::new());
            let (ring, next, own) = far_setup(&turns);
            let passer = {
                let (ring, turns, own) = (Arc::clone(&ring), Arc::clone(&turns), Arc::clone(&own));
                thread::spawn(move || {
                    pass(&ring, &turns, next, next + 1);
                    tell(&turns, &own);
                })
            };
            let sleeper = {
                let (ring, turns, own) = (Arc::clone(&ring), Arc::clone(&turns), Arc::clone(&own));
                thread::spawn(move || park_far(&ring, &turns, &own))
            };
            passer.join().expect("the passer");
            if sleeper.join().expect("the sleeper") {
                witness.record();
            }
        },
    );
}

/// Calibration for [`a_call_far_from_its_turn_is_named_and_told`]: a pass
/// that reads the count before it passes. The sleeper counts in after that
/// read and looks at the frontier before the pass: it sleeps, nobody names
/// its slot, and nobody tells it.
pub fn calibration_a_pass_that_reads_the_count_first_leaves_a_sleeper_untold() {
    explore_bounded(
        "calibration_a_pass_that_reads_the_count_first_leaves_a_sleeper_untold",
        Some(3),
        1,
        0,
        |_| {
            let turns = Arc::new(Turns::new());
            let (ring, next, own) = far_setup(&turns);
            let passer = {
                let (ring, turns, own) = (Arc::clone(&ring), Arc::clone(&turns), Arc::clone(&own));
                thread::spawn(move || {
                    // The bug: the count is read before the pass.
                    let anybody = turns.far.0.load(Ordering::SeqCst) != 0;
                    for index in next..=next + 1 {
                        assert!(ring.pass_decided(index));
                        if anybody {
                            turns
                                .near_to
                                .0
                                .fetch_max(index + turns.lead(), Ordering::SeqCst);
                        }
                    }
                    tell(&turns, &own);
                })
            };
            let sleeper = {
                let (ring, turns, own) = (Arc::clone(&ring), Arc::clone(&turns), Arc::clone(&own));
                thread::spawn(move || {
                    turns.count_in();
                    own.register(|| turns.near(&own, ring.decided()))
                })
            };
            passer.join().expect("the passer");
            let sleeps = sleeper.join().expect("the sleeper");
            assert!(
                !(sleeps && own.has_sleepers()),
                "a call far from its turn was never told"
            );
        },
    );
}

/// T3, the landing: the other thread passes every slot up to the
/// sleeper's and publishes the sleeper's slot, as a thread that helps
/// carries a slot with no record all the way; no commit of its is done, so
/// nobody tells. The landing wakes the sleeper: it sets `PARKED` and then
/// looks at the frontier, the landing comes after the pass and looks at
/// `PARKED`, so one sees the other.
///
/// Bound: 3 preemptions.
pub fn a_call_far_from_its_turn_is_woken_when_its_slot_lands() {
    explore_bounded(
        "a_call_far_from_its_turn_is_woken_when_its_slot_lands",
        Some(3),
        8,
        1,
        |witness| {
            let turns = Arc::new(Turns::new());
            let (ring, next, own) = far_setup(&turns);
            let passer = {
                let (ring, turns, own) = (Arc::clone(&ring), Arc::clone(&turns), Arc::clone(&own));
                thread::spawn(move || {
                    pass(&ring, &turns, next, own.index());
                    own.land();
                })
            };
            let sleeper = {
                let (ring, turns, own) = (Arc::clone(&ring), Arc::clone(&turns), Arc::clone(&own));
                thread::spawn(move || park_far(&ring, &turns, &own))
            };
            passer.join().expect("the passer");
            if sleeper.join().expect("the sleeper") {
                witness.record();
            }
        },
    );
}

/// Calibration for [`a_call_far_from_its_turn_is_woken_when_its_slot_lands`]:
/// a sleeper that looks at the frontier before it registers. The landing
/// comes between the look and the registration, finds nobody parked, and
/// the sleeper sleeps through it.
pub fn calibration_a_call_that_looks_before_it_registers_sleeps_through_its_landing() {
    explore_bounded(
        "calibration_a_call_that_looks_before_it_registers_sleeps_through_its_landing",
        Some(3),
        1,
        0,
        |_| {
            let turns = Arc::new(Turns::new());
            let (ring, next, own) = far_setup(&turns);
            let passer = {
                let (ring, turns, own) = (Arc::clone(&ring), Arc::clone(&turns), Arc::clone(&own));
                thread::spawn(move || {
                    // Nobody counted: the passes name nothing.
                    pass(&ring, &turns, next, own.index());
                    own.land();
                })
            };
            let sleeper = {
                let (ring, turns, own) = (Arc::clone(&ring), Arc::clone(&turns), Arc::clone(&own));
                thread::spawn(move || {
                    // The bug: the look comes first, the registration after.
                    let near = turns.near(&own, ring.decided());
                    own.register(|| near)
                })
            };
            passer.join().expect("the passer");
            let sleeps = sleeper.join().expect("the sleeper");
            assert!(
                !(sleeps && own.has_sleepers()),
                "a call far from its turn slept through its landing"
            );
        },
    );
}
