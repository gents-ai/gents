//! Loom models of the commit pipeline (plan 4.7): the ring's reserve, install
//! and void, a cell taking its next lap, deciding with helping, the round's
//! claim, publication, and a rollback meeting a decision under way.
//!
//! # What runs the production code and what mirrors it
//!
//! The ring ([`Ring`]), the tip cell ([`ArcSlot`]), a slot's decision cell
//! ([`OnceArc`]), the round's job ([`Job`]) and the read horizon
//! ([`ReadHorizon`]) are the production types: their atomics come from
//! `crate::sync::internal`, so under `--cfg loom` they are loom's, and a
//! blocked thread parks through loom. The steps *between* them (decide,
//! settle, the rollback's freeze) live on the engine, which owns files and
//! a memtable loom cannot run, so the models repeat those few lines against
//! the production cells: [`help`] and [`settle`] are `pipeline.rs`'s
//! `help_decide` and `settle` with the engine taken out, and [`roll_back`]
//! is `ordered.rs`'s with the log and the memtable taken out. They are
//! protocol models and say so.
//!
//! # What was shrunk
//!
//! Every ring has two cells, the smallest that has a next lap. Every model
//! is two threads, or two and a reader, each doing one step of the
//! protocol, and is bounded by an explicit preemption bound, never by
//! time: the bound is what makes the search finish, and each model says
//! which bound it runs at. A bound of `n` still reaches every schedule in
//! which threads are switched away from at most `n` times while they could
//! have gone on, which covers each race here: each is a window between two
//! steps of one thread that one switch opens and one closes.
//!
//! # Calibration
//!
//! Each positive model has a calibration that writes one step the wrong way
//! out of plain loom atomics and must fail: a void that is not a
//! compare-and-swap, a cleaner that takes the descriptor of an install
//! under way, a stamp that does not release the pointer stored before it,
//! a replacement that reads the pins before its swap, a frontier
//! passed before the tip moved, a claim that is a load and a store, a
//! frontier published before the horizon, and a settle that does not look
//! at the epoch.

#![allow(unsafe_code)]

use std::ptr;

use loom::cell::UnsafeCell;
use loom::sync::Arc;
use loom::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use loom::thread;

use super::descriptor::OnceArc;
use super::ring::{ArcSlot, Installed, Linked, Loaded, Ring};
use crate::engine::io::job::{Job, JobBody};
use crate::engine::loom_model::explore_bounded;
use crate::engine::read_horizon::ReadHorizon;

/// A descriptor that counts its drops.
struct Probe {
    id: u64,
    drops: Arc<AtomicUsize>,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

fn probe(id: u64, drops: &Arc<AtomicUsize>) -> Arc<Probe> {
    Arc::new(Probe {
        id,
        drops: Arc::clone(drops),
    })
}

/// R1 and R3: an owner installs the index it reserved while a helper voids
/// it. Exactly one wins, every thread sees the same fate afterwards, a
/// loaded descriptor is the one installed, and the descriptor is dropped
/// exactly once.
///
/// Bound: 3 preemptions.
pub fn an_index_is_installed_or_voided_never_both() {
    explore_bounded(
        "an_index_is_installed_or_voided_never_both",
        Some(3),
        8,
        1,
        |witness| {
            let drops = Arc::new(AtomicUsize::new(0));
            let ring: Arc<Ring<Probe>> = Arc::new(Ring::new(2));
            let index = ring.reserve();
            let owner = {
                let (ring, drops) = (Arc::clone(&ring), Arc::clone(&drops));
                thread::spawn(move || {
                    let desc = probe(index, &drops);
                    matches!(ring.install(index, &desc), Installed::Done)
                })
            };
            let helper = {
                let ring = Arc::clone(&ring);
                thread::spawn(move || ring.void(index))
            };
            let installed = owner.join().expect("owner");
            let voided = helper.join().expect("helper");
            assert!(
                installed != voided,
                "R1: an index was installed and voided, or neither"
            );
            match ring.load(index) {
                Loaded::Ready(desc) => {
                    assert!(installed, "R1: a voided index holds a descriptor");
                    assert_eq!(desc.id, index, "R3: another slot's descriptor");
                }
                Loaded::Void => {
                    assert!(voided, "R1: an installed index reads as void");
                    witness.record();
                }
                _ => panic!("R1: the index has no fate"),
            }
            drop(ring);
            assert_eq!(
                drops.load(Ordering::SeqCst),
                1,
                "R3: the descriptor was dropped twice, or never"
            );
        },
    );
}

/// The phases of the calibration cells below.
const FREE: u64 = 0;
const READY: u64 = 1;
const VOID: u64 = 2;

/// Calibration for [`an_index_is_installed_or_voided_never_both`]: an
/// install and a void that each read the stamp and then store it. Both find
/// the cell free, and the index gets two fates.
pub fn a_void_that_is_not_a_compare_and_swap_gives_an_index_two_fates() {
    explore_bounded(
        "a_void_that_is_not_a_compare_and_swap_gives_an_index_two_fates",
        Some(3),
        2,
        1,
        |witness| {
            let stamp = Arc::new(AtomicU64::new(FREE));
            let take = |phase: u64| {
                let stamp = Arc::clone(&stamp);
                thread::spawn(move || {
                    if stamp.load(Ordering::SeqCst) != FREE {
                        return false;
                    }
                    stamp.store(phase, Ordering::SeqCst);
                    true
                })
            };
            let (owner, helper) = (take(READY), take(VOID));
            let installed = owner.join().expect("owner");
            let voided = helper.join().expect("helper");
            witness.record();
            assert!(!(installed && voided), "an index was installed and voided");
        },
    );
}

/// R3 and R4: the thread that published a slot retires it while the owner
/// of the index one lap later installs into the same cell. The cleaner that
/// arrives between the owner's descriptor going in and its stamp moving
/// leaves that descriptor alone: an install that reported success is
/// loaded, and nothing is dropped twice.
///
/// Bound: 3 preemptions.
pub fn a_cell_retired_while_it_takes_the_next_lap_keeps_the_new_descriptor() {
    explore_bounded(
        "a_cell_retired_while_it_takes_the_next_lap_keeps_the_new_descriptor",
        Some(3),
        8,
        1,
        |witness| {
            let drops = Arc::new(AtomicUsize::new(0));
            let ring: Arc<Ring<Probe>> = Arc::new(Ring::new(2));
            let first = ring.reserve();
            let _second = ring.reserve();
            let lap = ring.reserve();
            assert_eq!(lap, first + ring.capacity());
            assert!(matches!(
                ring.install(first, &probe(first, &drops)),
                Installed::Done
            ));
            // Published, and not retired yet.
            assert!(ring.pass_decided(first));
            assert!(ring.pass_published(first));

            let retirer = {
                let ring = Arc::clone(&ring);
                thread::spawn(move || ring.retire(first))
            };
            let owner = {
                let (ring, drops) = (Arc::clone(&ring), Arc::clone(&drops));
                thread::spawn(move || {
                    let desc = probe(lap, &drops);
                    matches!(ring.install(lap, &desc), Installed::Done)
                })
            };
            retirer.join().expect("retirer");
            let installed = owner.join().expect("owner");
            if installed {
                witness.record();
                match ring.load(lap) {
                    Loaded::Ready(desc) => assert_eq!(desc.id, lap),
                    _ => panic!("R3: a cleaner took the descriptor of an install under way"),
                }
            }
            drop(ring);
            assert_eq!(
                drops.load(Ordering::SeqCst),
                2,
                "R3: a descriptor was dropped twice, or never"
            );
        },
    );
}

/// R3 for an install and a load at once: a helper loads a slot while its
/// owner installs it, as `help_decide` does when it reaches a slot whose
/// owner is still installing. The owner writes the descriptor's pointer
/// and the holder word with plain stores, and the stamp's compare-and-swap
/// publishes both: a load that finds the slot READY finds the descriptor
/// installed, never an empty cell or another slot's.
///
/// Bound: 3 preemptions.
pub fn a_descriptor_loaded_while_it_is_installed_is_the_one_installed() {
    explore_bounded(
        "a_descriptor_loaded_while_it_is_installed_is_the_one_installed",
        Some(3),
        8,
        1,
        |witness| {
            let drops = Arc::new(AtomicUsize::new(0));
            let ring: Arc<Ring<Probe>> = Arc::new(Ring::new(2));
            let index = ring.reserve();
            let owner = {
                let (ring, drops) = (Arc::clone(&ring), Arc::clone(&drops));
                thread::spawn(move || {
                    let desc = probe(index, &drops);
                    assert!(matches!(ring.install(index, &desc), Installed::Done));
                })
            };
            let helper = {
                let ring = Arc::clone(&ring);
                let witness = witness.clone();
                thread::spawn(move || match ring.load(index) {
                    Loaded::Ready(desc) => {
                        witness.record();
                        assert_eq!(desc.id, index, "R3: a READY slot held another descriptor");
                    }
                    Loaded::Empty => {}
                    _ => panic!("R1: a slot under install read as neither installed nor reserved"),
                })
            };
            owner.join().expect("owner");
            helper.join().expect("helper");
            drop(ring);
            assert_eq!(
                drops.load(Ordering::SeqCst),
                1,
                "R3: dropped twice, or never"
            );
        },
    );
}

/// Calibration for
/// [`a_descriptor_loaded_while_it_is_installed_is_the_one_installed`]: an
/// install whose stamp does not release what went in before it. A plain
/// store of the pointer, then a plain store of the stamp: a loader that
/// finds the stamp READY may find the pointer not there yet.
pub fn a_stamp_that_does_not_release_the_pointer_shows_an_empty_ready_cell() {
    explore_bounded(
        "a_stamp_that_does_not_release_the_pointer_shows_an_empty_ready_cell",
        Some(3),
        1,
        0,
        |_| {
            let stamp = Arc::new(AtomicU64::new(FREE));
            // The slot's descriptor, 0 for none.
            let desc = Arc::new(AtomicU64::new(0));
            let owner = {
                let (stamp, desc) = (Arc::clone(&stamp), Arc::clone(&desc));
                thread::spawn(move || {
                    desc.store(1, Ordering::Relaxed);
                    // The bug: the stamp moves with no release.
                    stamp.store(READY, Ordering::Relaxed);
                })
            };
            if stamp.load(Ordering::SeqCst) == READY {
                assert_eq!(
                    desc.load(Ordering::SeqCst),
                    1,
                    "a loader found the slot READY and its cell empty"
                );
            }
            owner.join().expect("owner");
        },
    );
}

/// A cell of the calibration below: the ring's stamp and holder words, and
/// the slot index whose descriptor the cell holds, zero for none.
struct LooseCell {
    stamp: AtomicU64,
    holder: AtomicU64,
    held: AtomicU64,
}

const UNHELD: u64 = 0;
const LOCKED: u64 = u64::MAX;

fn stamped(index: u64, phase: u64) -> u64 {
    (index << 2) | phase
}

impl LooseCell {
    /// The ring's `clean` with the rule it must not have: the descriptor
    /// goes unless the stamp names its slot as READY. A stamp that has not
    /// reached the slot yet, an install under way, passes that test.
    fn clean(&self) {
        let holder = self.holder.load(Ordering::SeqCst);
        if holder == UNHELD || holder == LOCKED {
            return;
        }
        let seen = self.stamp.load(Ordering::SeqCst);
        if seen == stamped(holder, READY) {
            return;
        }
        if self
            .holder
            .compare_exchange(holder, LOCKED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            self.held.store(0, Ordering::SeqCst);
            self.holder.store(UNHELD, Ordering::SeqCst);
        }
    }
}

/// Calibration for
/// [`a_cell_retired_while_it_takes_the_next_lap_keeps_the_new_descriptor`]:
/// the cleaner takes a descriptor whenever the stamp does not name it as
/// READY. It takes the one an owner has just put in, the owner's stamp then
/// moves, and the slot is READY with nothing in its cell.
pub fn a_cleaner_that_takes_an_install_under_way_empties_a_ready_cell() {
    explore_bounded(
        "a_cleaner_that_takes_an_install_under_way_empties_a_ready_cell",
        Some(3),
        2,
        1,
        |witness| {
            const OLD: u64 = 2;
            const NEW: u64 = 4;
            // The old slot is finished and its descriptor is still held.
            let cell = Arc::new(LooseCell {
                stamp: AtomicU64::new(stamped(OLD, FREE)),
                holder: AtomicU64::new(OLD),
                held: AtomicU64::new(OLD),
            });
            let retirer = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || cell.clean())
            };
            let owner = {
                let cell = Arc::clone(&cell);
                thread::spawn(move || {
                    let seen = cell.stamp.load(Ordering::SeqCst);
                    if cell
                        .holder
                        .compare_exchange(UNHELD, LOCKED, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
                        cell.clean();
                        if cell
                            .holder
                            .compare_exchange(UNHELD, LOCKED, Ordering::SeqCst, Ordering::SeqCst)
                            .is_err()
                        {
                            return false;
                        }
                    }
                    cell.held.store(NEW, Ordering::SeqCst);
                    cell.holder.store(NEW, Ordering::SeqCst);
                    cell.stamp
                        .compare_exchange(
                            seen,
                            stamped(NEW, READY),
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                })
            };
            retirer.join().expect("retirer");
            if owner.join().expect("owner") {
                witness.record();
                assert_eq!(
                    cell.held.load(Ordering::SeqCst),
                    NEW,
                    "a cleaner took the descriptor of an install under way"
                );
            }
        },
    );
}

/// R3, the pin: a thread loads a slot's descriptor while the slot's
/// publisher retires it. The loader either finds the slot gone or gets a
/// descriptor that is alive and is the slot's own: the cleaner that read
/// the pins as zero did so after the stamp said the slot was over, so a
/// loader that pins later never touches the pointer. loom's `Arc` fails the
/// model on a reference taken from a value already dropped.
///
/// Bound: 3 preemptions.
pub fn a_descriptor_loaded_while_its_slot_is_retired_is_alive() {
    explore_bounded(
        "a_descriptor_loaded_while_its_slot_is_retired_is_alive",
        Some(3),
        8,
        1,
        |witness| {
            let drops = Arc::new(AtomicUsize::new(0));
            let ring: Arc<Ring<Probe>> = Arc::new(Ring::new(2));
            let index = ring.reserve();
            assert!(matches!(
                ring.install(index, &probe(index, &drops)),
                Installed::Done
            ));
            assert!(ring.pass_decided(index));
            assert!(ring.pass_published(index));

            let retirer = {
                let ring = Arc::clone(&ring);
                thread::spawn(move || ring.retire(index))
            };
            let loader = {
                let (ring, drops) = (Arc::clone(&ring), Arc::clone(&drops));
                let witness = witness.clone();
                thread::spawn(move || {
                    if let Loaded::Ready(desc) = ring.load(index) {
                        witness.record();
                        assert_eq!(desc.id, index);
                        assert_eq!(
                            drops.load(Ordering::SeqCst),
                            0,
                            "R3: a loaded descriptor was already dropped"
                        );
                    }
                })
            };
            retirer.join().expect("retirer");
            loader.join().expect("loader");
            drop(ring);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        },
    );
}

/// Calibration for [`a_descriptor_loaded_while_its_slot_is_retired_is_alive`]:
/// a loader that does not count itself in. The cleaner cannot know it is
/// there, drops the descriptor between the loader's read of the stamp and
/// its use of the pointer, and the loader holds a dropped descriptor.
pub fn a_loader_that_does_not_pin_reads_a_dropped_descriptor() {
    explore_bounded(
        "a_loader_that_does_not_pin_reads_a_dropped_descriptor",
        Some(3),
        2,
        1,
        |witness| {
            let stamp = Arc::new(AtomicU64::new(READY));
            // 1 while the cell's descriptor is alive.
            let alive = Arc::new(AtomicU64::new(1));
            let retirer = {
                let (stamp, alive) = (Arc::clone(&stamp), Arc::clone(&alive));
                thread::spawn(move || {
                    stamp.store(FREE, Ordering::SeqCst);
                    alive.store(0, Ordering::SeqCst);
                })
            };
            let loader = {
                let (stamp, alive) = (Arc::clone(&stamp), Arc::clone(&alive));
                let witness = witness.clone();
                thread::spawn(move || {
                    if stamp.load(Ordering::SeqCst) == READY {
                        witness.record();
                        assert_eq!(
                            alive.load(Ordering::SeqCst),
                            1,
                            "a loader read a descriptor that was already dropped"
                        );
                    }
                })
            };
            retirer.join().expect("retirer");
            if let Err(payload) = loader.join() {
                std::panic::resume_unwind(payload);
            }
        },
    );
}

/// The tip cell: a thread loads the pipeline's state while another replaces
/// it. The load returns the old value or the new one, alive either way: the
/// replaced value is dropped only once no load that could hold its address
/// is counted in.
///
/// Bound: 3 preemptions.
pub fn a_tip_loaded_while_it_is_replaced_is_alive() {
    explore_bounded(
        "a_tip_loaded_while_it_is_replaced_is_alive",
        Some(3),
        8,
        1,
        |witness| {
            let tip = Arc::new(ArcSlot::new(Mark::new(1, 0, 1, 0)));
            let replacer = {
                let tip = Arc::clone(&tip);
                thread::spawn(move || {
                    let seen = tip.load();
                    assert!(tip.replace(&seen, Mark::new(2, 1, 3, 0)));
                })
            };
            let loader = {
                let tip = Arc::clone(&tip);
                let witness = witness.clone();
                thread::spawn(move || {
                    let seen = tip.load();
                    assert!(seen.index == 1 || seen.index == 2);
                    if seen.index == 1 {
                        witness.record();
                    }
                    assert_eq!(seen.next, if seen.index == 1 { 1 } else { 3 });
                })
            };
            replacer.join().expect("replacer");
            loader.join().expect("loader");
            assert_eq!(tip.load().index, 2);
        },
    );
}

/// A value for the tip cell that counts its drops, one counter per value.
struct Counted {
    /// Which value this is: 0, 1 or 2.
    id: usize,
    /// How many times each value was dropped.
    drops: Arc<[AtomicUsize; 3]>,
    link: AtomicPtr<Counted>,
}

impl Counted {
    fn new(id: usize, drops: &Arc<[AtomicUsize; 3]>) -> Arc<Self> {
        Arc::new(Self {
            id,
            drops: Arc::clone(drops),
            link: AtomicPtr::new(ptr::null_mut()),
        })
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.drops[self.id].fetch_add(1, Ordering::SeqCst);
    }
}

impl Linked for Counted {
    fn link(&self) -> &AtomicPtr<Self> {
        &self.link
    }
}

/// The tip cell's drop at the swap (`ArcSlot::replace`): a replacement
/// that finds no load counted in and nothing on the list drops the replaced
/// value at once; one that finds a load puts it on the list, where a later
/// replacement or the cell's own drop drops it. A thread replaces the tip
/// twice while another loads it: whichever value the load returns is not
/// dropped while it holds it, no value is dropped twice, and once the cell
/// is dropped every value is dropped exactly once.
///
/// Bound: 3 preemptions. The race is the load's window between counting
/// itself in and taking its reference, which one switch opens and one
/// closes; the second replacement is there for a value the first left on
/// the list, which a third switch reaches.
pub fn a_tip_replaced_twice_while_loaded_drops_each_value_once() {
    explore_bounded(
        "a_tip_replaced_twice_while_loaded_drops_each_value_once",
        Some(3),
        8,
        1,
        |witness| {
            let drops = Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
            let tip = Arc::new(ArcSlot::new(Counted::new(0, &drops)));
            let replacer = {
                let (tip, drops) = (Arc::clone(&tip), Arc::clone(&drops));
                thread::spawn(move || {
                    for next in 1..3 {
                        let seen = tip.load();
                        assert!(tip.replace(&seen, Counted::new(next, &drops)));
                    }
                })
            };
            let loader = {
                let (tip, drops) = (Arc::clone(&tip), Arc::clone(&drops));
                let witness = witness.clone();
                thread::spawn(move || {
                    let seen = tip.load();
                    // The load got a value the replacer goes on to replace.
                    if seen.id < 2 {
                        witness.record();
                    }
                    assert_eq!(
                        drops[seen.id].load(Ordering::SeqCst),
                        0,
                        "a loaded tip was dropped"
                    );
                })
            };
            replacer.join().expect("replacer");
            loader.join().expect("loader");
            assert_eq!(tip.load().id, 2);
            // A replaced value is dropped at once or waits on the list, never
            // twice, and the tip is never dropped while the cell holds it.
            let gone: Vec<usize> = drops.iter().map(|d| d.load(Ordering::SeqCst)).collect();
            assert!(gone[0] <= 1 && gone[1] <= 1 && gone[2] == 0, "{gone:?}");
            drop(tip);
            let gone: Vec<usize> = drops.iter().map(|d| d.load(Ordering::SeqCst)).collect();
            assert_eq!(gone, [1, 1, 1], "every value, each once");
        },
    );
}

/// Calibration for [`a_tip_replaced_twice_while_loaded_drops_each_value_once`]:
/// a replacement that looks at the pins before its swap instead of after.
/// A load that counts itself in and reads the old value between the look
/// and the swap is missed, the value is dropped under it, and the load holds
/// a dropped tip.
pub fn a_replace_that_reads_the_pins_before_its_swap_drops_a_loaded_tip() {
    explore_bounded(
        "a_replace_that_reads_the_pins_before_its_swap_drops_a_loaded_tip",
        Some(3),
        2,
        1,
        |witness| {
            // The tip names value 1 or value 2.
            let current = Arc::new(AtomicU64::new(1));
            let pins = Arc::new(AtomicUsize::new(0));
            // 1 while value 1 is alive.
            let alive = Arc::new(AtomicU64::new(1));
            let replacer = {
                let (current, pins, alive) =
                    (Arc::clone(&current), Arc::clone(&pins), Arc::clone(&alive));
                thread::spawn(move || {
                    let loads = pins.load(Ordering::SeqCst);
                    current.store(2, Ordering::SeqCst);
                    if loads == 0 {
                        alive.store(0, Ordering::SeqCst);
                    }
                })
            };
            let loader = {
                let (current, pins, alive) =
                    (Arc::clone(&current), Arc::clone(&pins), Arc::clone(&alive));
                let witness = witness.clone();
                thread::spawn(move || {
                    pins.fetch_add(1, Ordering::SeqCst);
                    if current.load(Ordering::SeqCst) == 1 {
                        witness.record();
                        assert_eq!(
                            alive.load(Ordering::SeqCst),
                            1,
                            "a loader holds a tip that was already dropped"
                        );
                    }
                    pins.fetch_sub(1, Ordering::SeqCst);
                })
            };
            replacer.join().expect("replacer");
            if let Err(payload) = loader.join() {
                std::panic::resume_unwind(payload);
            }
        },
    );
}

/// What a slot's decision carries in these models: the sequences it took
/// and the state after it, as `descriptor.rs`'s `Decision` does.
struct Mark {
    index: u64,
    /// The first sequence the slot took.
    base: u64,
    /// The sequence the next slot takes.
    next: u64,
    /// The pipeline's rollback count when this was computed.
    epoch: u64,
    link: AtomicPtr<Mark>,
}

impl Mark {
    fn new(index: u64, base: u64, next: u64, epoch: u64) -> Arc<Self> {
        Arc::new(Self {
            index,
            base,
            next,
            epoch,
            link: AtomicPtr::new(ptr::null_mut()),
        })
    }
}

impl Linked for Mark {
    fn link(&self) -> &AtomicPtr<Self> {
        &self.link
    }
}

/// A slot's descriptor in these models.
struct Slot {
    /// Sequences the slot takes.
    ops: u64,
    decision: OnceArc<Mark>,
    /// Set when the slot is discarded.
    failed: AtomicU64,
}

fn slot(ops: u64) -> Arc<Slot> {
    Arc::new(Slot {
        ops,
        decision: OnceArc::empty(),
        failed: AtomicU64::new(0),
    })
}

/// How [`settle`] is written.
#[derive(Clone, Copy, PartialEq)]
enum Settle {
    /// As `pipeline.rs` writes it.
    Sound,
    /// The frontier passes before the tip moves.
    FrontierFirst,
    /// The tip moves to a decision whatever its epoch.
    AnyEpoch,
}

/// `pipeline.rs`'s `help_decide` and `decide` over the production cells:
/// decide every slot up to `upto` in ring order, from the tip. Returns when
/// a rollback holds the frontier, as a call that may not wait does.
fn help(ring: &Ring<Slot>, tip: &ArcSlot<Mark>, upto: u64, how: Settle) {
    loop {
        let decided = ring.decided();
        if decided >= upto || ring.is_deciding_frozen() {
            return;
        }
        let index = decided + 1;
        match ring.load(index) {
            Loaded::Ready(slot) => {
                if slot.decision.get().is_none() {
                    let prev = tip.load();
                    if ring.is_next_to_decide(index) {
                        // A decision that lost the install is dropped (D1).
                        let _ = slot.decision.set(Mark::new(
                            index,
                            prev.next,
                            prev.next + slot.ops,
                            prev.epoch,
                        ));
                    }
                }
                settle(ring, tip, index, &slot, how);
            }
            Loaded::Void | Loaded::Gone => {
                ring.pass_decided(index);
            }
            Loaded::Empty => {
                ring.void(index);
            }
            Loaded::Full => unreachable!("no model fills its ring"),
        }
    }
}

/// `pipeline.rs`'s `settle`, without the apply: move the tip to the slot's
/// decision, or fail the slot when the decision is of a discarded state,
/// then pass the frontier.
fn settle(ring: &Ring<Slot>, tip: &ArcSlot<Mark>, index: u64, slot: &Slot, how: Settle) {
    let Some(decision) = slot.decision.get_arc() else {
        return;
    };
    if how == Settle::FrontierFirst {
        ring.pass_decided(index);
    }
    loop {
        let seen = tip.load();
        if seen.index >= decision.index {
            break;
        }
        if how != Settle::AnyEpoch && seen.epoch != decision.epoch {
            slot.failed.store(1, Ordering::SeqCst);
            break;
        }
        if tip.replace(&seen, Arc::clone(&decision)) {
            break;
        }
    }
    ring.pass_decided(index);
}

/// A ring holding two installed slots, and the tip before them.
fn two_slots() -> (Arc<Ring<Slot>>, Arc<ArcSlot<Mark>>, [Arc<Slot>; 2], u64) {
    let ring: Arc<Ring<Slot>> = Arc::new(Ring::new(2));
    let slots = [slot(2), slot(3)];
    let first = ring.reserve();
    assert!(matches!(ring.install(first, &slots[0]), Installed::Done));
    let second = ring.reserve();
    assert!(matches!(ring.install(second, &slots[1]), Installed::Done));
    // The state before the first slot: the next sequence is 1.
    let tip = Arc::new(ArcSlot::new(Mark::new(first - 1, 0, 1, 0)));
    (ring, tip, slots, second)
}

/// The sequences two slots took are one after the other with no gap: P1.
fn assert_dense(slots: &[Arc<Slot>; 2]) {
    let first = slots[0].decision.get().expect("the first slot is decided");
    let second = slots[1].decision.get().expect("the second slot is decided");
    assert_eq!(first.base, 1, "the first slot starts the sequences");
    assert_eq!(
        second.base, first.next,
        "two slots took overlapping sequences, or left a hole"
    );
    assert_eq!(second.next, 1 + slots[0].ops + slots[1].ops);
}

fn decide_with_two_helpers(name: &'static str, how: Settle, min_interleavings: usize) {
    explore_bounded(name, Some(2), min_interleavings, 1, move |witness| {
        let (ring, tip, slots, last) = two_slots();
        let helpers: Vec<_> = (0..2)
            .map(|_| {
                let (ring, tip) = (Arc::clone(&ring), Arc::clone(&tip));
                thread::spawn(move || help(&ring, &tip, last, how))
            })
            .collect();
        for helper in helpers {
            helper.join().expect("helper");
        }
        witness.record();
        assert_eq!(ring.decided(), last);
        assert_dense(&slots);
        assert_eq!(tip.load().index, last, "the tip is the newest decision");
    });
}

/// P1 and D1: two threads decide the same two slots at once, each helping
/// the other. Each slot gets one decision, computed from the slot before
/// it, so the sequences are dense, whichever thread decided which.
///
/// Bound: 2 preemptions.
pub fn two_helpers_decide_two_slots_in_ring_order() {
    decide_with_two_helpers(
        "two_helpers_decide_two_slots_in_ring_order",
        Settle::Sound,
        16,
    );
}

/// Calibration for [`two_helpers_decide_two_slots_in_ring_order`]: the
/// frontier passes a slot before the tip has moved to its decision. The
/// other helper then decides the next slot from the tip before it, and two
/// slots take the same sequences.
pub fn a_frontier_passed_before_the_tip_moved_gives_two_slots_one_sequence() {
    decide_with_two_helpers(
        "a_frontier_passed_before_the_tip_moved_gives_two_slots_one_sequence",
        Settle::FrontierFirst,
        2,
    );
}

/// The device of the round models: the log's durable offset, and a cell
/// every sync writes, so two syncs at once are a race loom reports.
struct Device {
    durable: AtomicU64,
    syncing: UnsafeCell<u64>,
}

// SAFETY: `syncing` is written only by the thread that holds the round's
// claim; the models exist to check exactly that, and loom tracks the cell.
unsafe impl Sync for Device {}

/// What is written and waits to be durable.
const WRITTEN: u64 = 9;

impl Device {
    fn sync(&self) {
        self.syncing.with_mut(|count| {
            // SAFETY: one sync at a time is the property under test; loom
            // tracks the cell and fails the model on an overlap.
            unsafe { *count += 1 }
        });
        self.durable.fetch_max(WRITTEN, Ordering::SeqCst);
    }
}

/// One round, as `round.rs` has it: a job and its link in the round cell.
struct Lap {
    job: std::sync::Arc<Job>,
    link: AtomicPtr<Lap>,
}

impl Linked for Lap {
    fn link(&self) -> &AtomicPtr<Self> {
        &self.link
    }
}

/// The rounds of one log.
struct Rounds {
    device: Device,
    current: ArcSlot<Lap>,
}

/// The body of a round's job: sync, then put the next round in place before
/// the job lands.
struct SyncLap(std::sync::Weak<Rounds>);

impl JobBody for SyncLap {
    fn run(self: Box<Self>) {
        let Some(rounds) = self.0.upgrade() else {
            return;
        };
        rounds.device.sync();
        let lap = rounds.current.load();
        rounds.current.replace(&lap, new_lap(&self.0));
    }

    fn release(self: Box<Self>) {
        self.run();
    }
}

fn new_lap(rounds: &std::sync::Weak<Rounds>) -> Arc<Lap> {
    Arc::new(Lap {
        job: Job::new(Box::new(SyncLap(rounds.clone()))),
        link: AtomicPtr::new(ptr::null_mut()),
    })
}

fn rounds() -> std::sync::Arc<Rounds> {
    std::sync::Arc::new_cyclic(|me| Rounds {
        device: Device {
            durable: AtomicU64::new(0),
            syncing: UnsafeCell::new(0),
        },
        current: ArcSlot::new(new_lap(me)),
    })
}

/// The watermark claim (D53): two commits need the log durable. One claims
/// the round with a compare-and-swap and syncs; the other finds the round
/// claimed, parks on it, and is woken by its landing. Nobody syncs while
/// another sync is under way, both find the log durable, and no wake is
/// lost: a parked thread nobody wakes is a deadlock loom reports.
///
/// Bound: 3 preemptions.
pub fn a_round_is_claimed_by_one_commit_and_wakes_the_other() {
    explore_bounded(
        "a_round_is_claimed_by_one_commit_and_wakes_the_other",
        Some(3),
        8,
        1,
        |witness| {
            let rounds = rounds();
            let commits: Vec<_> = (0..2)
                .map(|_| {
                    let rounds = std::sync::Arc::clone(&rounds);
                    thread::spawn(move || {
                        let mut waited = false;
                        while rounds.device.durable.load(Ordering::SeqCst) < WRITTEN {
                            let lap = rounds.current.load();
                            if lap.job.claim() {
                                lap.job.run();
                            } else {
                                waited = true;
                                lap.job.wait_landed();
                            }
                        }
                        waited
                    })
                })
                .collect();
            let waited: Vec<bool> = commits
                .into_iter()
                .map(|commit| commit.join().expect("commit"))
                .collect();
            if waited.iter().any(|waited| *waited) {
                witness.record();
            }
            assert_eq!(rounds.device.durable.load(Ordering::SeqCst), WRITTEN);
            assert!(
                !rounds.current.load().job.is_done(),
                "a round landed without the next one in place"
            );
        },
    );
}

/// Calibration for [`a_round_is_claimed_by_one_commit_and_wakes_the_other`]:
/// a claim made of a load and a store. Both commits find the round free and
/// both sync, at once.
pub fn a_claim_that_is_a_load_and_a_store_lets_two_syncs_overlap() {
    explore_bounded(
        "a_claim_that_is_a_load_and_a_store_lets_two_syncs_overlap",
        Some(3),
        2,
        1,
        |witness| {
            let device = Arc::new(Device {
                durable: AtomicU64::new(0),
                syncing: UnsafeCell::new(0),
            });
            let claimed = Arc::new(AtomicU64::new(0));
            let commits: Vec<_> = (0..2)
                .map(|_| {
                    let (device, claimed) = (Arc::clone(&device), Arc::clone(&claimed));
                    thread::spawn(move || {
                        if claimed.load(Ordering::SeqCst) == 0 {
                            claimed.store(1, Ordering::SeqCst);
                            device.sync();
                        }
                    })
                })
                .collect();
            witness.record();
            for commit in commits {
                if let Err(payload) = commit.join() {
                    std::panic::resume_unwind(payload);
                }
            }
        },
    );
}

/// The last sequence the published slot of the models below took.
const PUBLISHED_SEQ: u64 = 7;

fn publish_against_a_reader(name: &'static str, horizon_first: bool, min_interleavings: usize) {
    explore_bounded(name, Some(2), min_interleavings, 1, move |witness| {
        let ring: Arc<Ring<Probe>> = Arc::new(Ring::new(2));
        let drops = Arc::new(AtomicUsize::new(0));
        let index = ring.reserve();
        assert!(matches!(
            ring.install(index, &probe(index, &drops)),
            Installed::Done
        ));
        assert!(ring.pass_decided(index));
        let horizon = Arc::new(ReadHorizon::new(0));

        let publishers: Vec<_> = (0..2)
            .map(|_| {
                let (ring, horizon) = (Arc::clone(&ring), Arc::clone(&horizon));
                thread::spawn(move || {
                    if horizon_first {
                        horizon.publish(PUBLISHED_SEQ);
                    }
                    let passed = ring.pass_published(index);
                    if !horizon_first {
                        horizon.publish(PUBLISHED_SEQ);
                    }
                    if passed {
                        ring.retire(index);
                    }
                })
            })
            .collect();
        // A commit that sees its slot published reads its own writes.
        let reader = {
            let (ring, horizon) = (Arc::clone(&ring), Arc::clone(&horizon));
            let witness = witness.clone();
            thread::spawn(move || {
                if ring.published() >= index {
                    witness.record();
                    assert!(
                        horizon.visible() >= PUBLISHED_SEQ,
                        "a slot was published before the horizon reached it"
                    );
                }
            })
        };
        for publisher in publishers {
            publisher.join().expect("publisher");
        }
        if let Err(payload) = reader.join() {
            std::panic::resume_unwind(payload);
        }
        assert_eq!(ring.published(), index, "the frontier passed the slot once");
        assert_eq!(horizon.visible(), PUBLISHED_SEQ);
    });
}

/// P3: two helpers publish one slot while a reader watches. Each raises
/// the read horizon to the slot's sequence and then passes the published
/// frontier, so whoever sees the frontier past the slot finds the horizon
/// there. The frontier passes once, and the slot is retired once.
///
/// Bound: 2 preemptions (three threads).
pub fn a_published_slot_is_visible_to_whoever_sees_it_published() {
    publish_against_a_reader(
        "a_published_slot_is_visible_to_whoever_sees_it_published",
        true,
        16,
    );
}

/// Calibration for [`a_published_slot_is_visible_to_whoever_sees_it_published`]:
/// the frontier first, then the horizon. A reader between the two sees its
/// slot published and cannot read it.
pub fn a_frontier_passed_before_the_horizon_hides_a_published_slot() {
    publish_against_a_reader(
        "a_frontier_passed_before_the_horizon_hides_a_published_slot",
        false,
        2,
    );
}

/// `ordered.rs`'s `roll_back` over the production cells, with the log and
/// the memtable taken out: freeze the decided frontier, stop the tip with a
/// marker of the next epoch, discard what was decided and not published
/// (here everything decided, since nothing is published), leave the state
/// of the published prefix in the tip, and thaw.
fn roll_back(ring: &Ring<Slot>, tip: &ArcSlot<Mark>, slots: &[(u64, Arc<Slot>)], resume: u64) {
    let decided = ring.freeze_decided();
    let (seen, marker) = loop {
        let seen = tip.load();
        let marker = Mark::new(seen.index, seen.base, seen.next, seen.epoch + 1);
        if tip.replace(&seen, Arc::clone(&marker)) {
            break (seen, marker);
        }
    };
    let last = decided.max(seen.index);
    for (index, slot) in slots {
        if *index <= last {
            slot.failed.store(1, Ordering::SeqCst);
        }
    }
    let resumed = Mark::new(last, resume, resume, marker.epoch);
    // Only a rollback replaces its own marker; a model that lets another
    // thread do it is caught by what the tip holds at the end.
    tip.replace(&marker, resumed);
    ring.thaw_decided(last);
}

fn rollback_against_a_decider(name: &'static str, how: Settle, min_interleavings: usize) {
    explore_bounded(name, Some(3), min_interleavings, 1, move |witness| {
        let ring: Arc<Ring<Slot>> = Arc::new(Ring::new(2));
        let only = slot(2);
        let index = ring.reserve();
        assert!(matches!(ring.install(index, &only), Installed::Done));
        // Nothing is published: the published prefix ends at sequence 0.
        let tip = Arc::new(ArcSlot::new(Mark::new(index - 1, 0, 1, 0)));

        let decider = {
            let (ring, tip) = (Arc::clone(&ring), Arc::clone(&tip));
            thread::spawn(move || help(&ring, &tip, index, how))
        };
        let rollback = {
            let (ring, tip, only) = (Arc::clone(&ring), Arc::clone(&tip), Arc::clone(&only));
            thread::spawn(move || roll_back(&ring, &tip, &[(index, only)], 1))
        };
        decider.join().expect("decider");
        rollback.join().expect("rollback");
        // A decider that found the frontier frozen went away: finish here.
        help(&ring, &tip, index, how);

        let end = tip.load();
        assert_eq!(ring.decided(), index);
        assert_eq!(
            end.epoch, 1,
            "a decision of the discarded state became the tip"
        );
        let failed = only.failed.load(Ordering::SeqCst) == 1;
        if failed {
            witness.record();
            assert_eq!(end.next, 1, "a discarded slot kept its sequences");
        } else {
            let decision = only.decision.get().expect("the slot is decided");
            assert_eq!(decision.epoch, 1, "a stale decision was not discarded");
            assert_eq!(decision.base, 1);
            assert_eq!(end.next, 1 + only.ops);
        }
    });
}

/// P4: a rollback runs while a thread is deciding the slot after the
/// frontier. Whatever the thread had got to, the decision it computed from
/// the old state never becomes the tip and lands nothing: either the
/// rollback took the slot in and failed it, or the slot is settled as
/// failed by its epoch, or it is decided afresh from the new state. The
/// sequences it took are handed back.
///
/// Bound: 3 preemptions.
pub fn a_decision_computed_before_a_rollback_never_becomes_the_tip() {
    rollback_against_a_decider(
        "a_decision_computed_before_a_rollback_never_becomes_the_tip",
        Settle::Sound,
        16,
    );
}

/// Calibration for [`a_decision_computed_before_a_rollback_never_becomes_the_tip`]:
/// a settle that does not compare epochs. The thread's stale decision
/// replaces the tip the rollback left, and the pipeline goes on from a state
/// that was discarded.
pub fn a_settle_that_ignores_the_epoch_brings_a_discarded_state_back() {
    rollback_against_a_decider(
        "a_settle_that_ignores_the_epoch_brings_a_discarded_state_back",
        Settle::AnyEpoch,
        2,
    );
}
