//! The ring's own rules, without a pipeline on top: one fate per index, a
//! loaded descriptor is alive, nobody waits for a cell, and the frontiers
//! pass every slot once.

use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};

use super::*;

/// A descriptor that counts its drops.
struct Probe {
    id: u64,
    drops: std::sync::Arc<StdAtomicUsize>,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.drops.fetch_add(1, StdOrdering::SeqCst);
    }
}

fn probe(id: u64, drops: &std::sync::Arc<StdAtomicUsize>) -> Arc<Probe> {
    Arc::new(Probe {
        id,
        drops: std::sync::Arc::clone(drops),
    })
}

fn drops() -> std::sync::Arc<StdAtomicUsize> {
    std::sync::Arc::new(StdAtomicUsize::new(0))
}

#[test]
fn an_installed_slot_is_loaded_until_it_is_retired() {
    let dropped = drops();
    let ring: Ring<Probe> = Ring::new(4);
    let index = ring.reserve();
    assert_eq!(index, 4, "indices start at the capacity");
    assert!(matches!(ring.load(index), Loaded::Empty));
    let desc = probe(index, &dropped);
    assert!(matches!(ring.install(index, &desc), Installed::Done));
    match ring.load(index) {
        Loaded::Ready(loaded) => assert_eq!(loaded.id, index),
        _ => panic!("an installed slot loads its descriptor"),
    }
    ring.retire(index);
    assert!(matches!(ring.load(index), Loaded::Gone));
    drop(desc);
    assert_eq!(
        dropped.load(StdOrdering::SeqCst),
        1,
        "a retired slot's cell gives its reference up"
    );
}

#[test]
fn a_voided_slot_is_lost_to_its_owner_and_an_installed_one_cannot_be_voided() {
    let dropped = drops();
    let ring: Ring<Probe> = Ring::new(4);
    let first = ring.reserve();
    // R1: the helper's void wins, so the owner's install loses.
    assert!(ring.void(first));
    assert!(ring.void(first), "voiding a void slot says it is void");
    assert!(matches!(ring.load(first), Loaded::Void));
    let desc = probe(first, &dropped);
    assert!(matches!(ring.install(first, &desc), Installed::Lost));
    assert_eq!(
        Arc::strong_count(&desc),
        1,
        "a lost install leaves no reference behind"
    );

    // R1 the other way: the owner's install wins, so the void fails.
    let second = ring.reserve();
    assert!(matches!(ring.install(second, &desc), Installed::Done));
    assert!(!ring.void(second));
    assert!(matches!(ring.load(second), Loaded::Ready(_)));
}

#[test]
fn a_cell_takes_the_next_lap_only_once_its_slot_is_finished() {
    let dropped = drops();
    let ring: Ring<Probe> = Ring::new(2);
    let a = ring.reserve();
    let b = ring.reserve();
    let lap = ring.reserve();
    assert_eq!(lap, a + 2, "the third index maps to the first cell again");
    let first = probe(a, &dropped);
    assert!(matches!(ring.install(a, &first), Installed::Done));
    assert!(ring.void(b));

    let next = probe(lap, &dropped);
    assert!(matches!(ring.load(lap), Loaded::Full));
    assert!(matches!(ring.install(lap, &next), Installed::Full));
    assert!(
        !ring.void(lap),
        "a slot cannot be voided into a cell still in use"
    );

    ring.retire(a);
    assert!(matches!(ring.load(lap), Loaded::Empty));
    assert!(matches!(ring.install(lap, &next), Installed::Done));
    assert!(matches!(ring.load(a), Loaded::Gone));
    // A void cell is free for its next lap too.
    let lap_b = ring.reserve();
    assert_eq!(lap_b, b + 2);
    assert!(matches!(ring.install(lap_b, &next), Installed::Done));
}

/// R4: an owner that finds the cell's descriptor still held by a load gives
/// its index up instead of waiting, and the slot reads as void.
#[test]
fn an_owner_voids_its_own_slot_when_the_cell_is_busy() {
    let dropped = drops();
    let ring: Ring<Probe> = Ring::new(2);
    let a = ring.reserve();
    let _b = ring.reserve();
    let first = probe(a, &dropped);
    assert!(matches!(ring.install(a, &first), Installed::Done));
    // A thread stopped inside a load of this cell.
    ring.cell(a).pins.fetch_add(1, Ordering::SeqCst);
    ring.retire(a);
    drop(first);
    assert_eq!(
        dropped.load(StdOrdering::SeqCst),
        0,
        "a descriptor a load may still be taking is not dropped"
    );

    let lap = a + 2;
    while ring.reserve() != lap {}
    let next = probe(lap, &dropped);
    assert!(matches!(ring.install(lap, &next), Installed::Lost));
    assert!(matches!(ring.load(lap), Loaded::Void));

    // The stopped thread runs again: the cell is cleaned by whoever passes.
    ring.cell(a).pins.fetch_sub(1, Ordering::SeqCst);
    let again = lap + 2;
    while ring.reserve() != again {}
    assert!(matches!(ring.install(again, &next), Installed::Done));
    assert_eq!(dropped.load(StdOrdering::SeqCst), 1);
}

#[test]
fn the_frontiers_pass_each_slot_once_and_in_order() {
    let ring: Ring<Probe> = Ring::new(4);
    assert_eq!(ring.decided(), 3);
    assert_eq!(ring.published(), 3);
    assert!(
        !ring.pass_decided(5),
        "a slot is not passed before the one before it"
    );
    assert!(ring.pass_decided(4));
    assert!(!ring.pass_decided(4), "a slot is passed once");
    assert!(ring.pass_published(4));
    assert!(!ring.pass_published(4));
    assert_eq!((ring.decided(), ring.published()), (4, 4));
}

#[test]
fn a_frozen_frontier_is_moved_only_by_the_thread_that_froze_it() {
    let ring: Ring<Probe> = Ring::new(4);
    assert_eq!(ring.freeze(), 3);
    assert!(ring.is_frozen());
    assert!(!ring.pass_published(4), "nothing is published while frozen");
    ring.set_frozen(4);
    assert_eq!(ring.published(), 4);
    assert!(ring.is_frozen());
    ring.thaw(6);
    assert!(!ring.is_frozen());
    assert!(ring.pass_published(7));
}

/// R4: a thread that moved the published frontier past a slot and stopped
/// before retiring it does not hold the slot's cell: the thread that needs
/// the cell a lap later retires the slot itself.
#[test]
fn a_slot_published_and_never_retired_does_not_hold_its_cell() {
    let dropped = drops();
    let ring: Ring<Probe> = Ring::new(2);
    let first = ring.reserve();
    assert!(matches!(
        ring.install(first, &probe(first, &dropped)),
        Installed::Done
    ));
    let second = ring.reserve();
    assert!(matches!(
        ring.install(second, &probe(second, &dropped)),
        Installed::Done
    ));
    let lap = ring.reserve();
    assert_eq!(lap, first + ring.capacity());
    assert!(
        matches!(ring.load(lap), Loaded::Full),
        "the slot a lap before is not published"
    );
    assert!(matches!(
        ring.install(lap, &probe(lap, &dropped)),
        Installed::Full
    ));
    // Published, and its publisher goes no further.
    assert!(ring.pass_decided(first));
    assert!(ring.pass_published(first));
    let dropped_before = dropped.load(StdOrdering::SeqCst);
    assert!(
        matches!(ring.load(lap), Loaded::Empty),
        "a published slot still held its cell"
    );
    assert_eq!(
        dropped.load(StdOrdering::SeqCst),
        dropped_before + 1,
        "the cell gave the published slot's descriptor up"
    );
    assert!(matches!(
        ring.install(lap, &probe(lap, &dropped)),
        Installed::Done
    ));
    assert!(matches!(ring.load(lap), Loaded::Ready(desc) if desc.id == lap));
}

/// R6: the cell of a void slot takes the index a lap later before either
/// frontier has passed the void one, and the void one then reads as gone.
#[test]
fn a_void_slot_does_not_keep_its_cell() {
    let dropped = drops();
    let ring: Ring<Probe> = Ring::new(2);
    let first = ring.reserve();
    let second = ring.reserve();
    assert!(ring.void(first));
    assert!(ring.void(second));
    let lap = ring.reserve();
    assert_eq!(lap, first + ring.capacity());
    assert!(
        matches!(ring.install(lap, &probe(lap, &dropped)), Installed::Done),
        "a void slot held its cell"
    );
    assert_eq!(ring.decided(), first - 1, "no frontier has moved");
    assert!(matches!(ring.load(first), Loaded::Gone));
    assert!(matches!(ring.load(second), Loaded::Void));
    assert!(matches!(ring.load(lap), Loaded::Ready(desc) if desc.id == lap));
}

/// R1 and R3 under threads: owners install and helpers void the same
/// indices at once. Every index ends installed or void, never both, every
/// descriptor loaded is the one installed at that index, and every
/// descriptor is dropped exactly once.
#[test]
fn installs_voids_and_loads_race_without_losing_or_leaking_a_descriptor() {
    const THREADS: u64 = 4;
    // Interpreted, each round costs thousands of times what it does
    // native; fifty rounds a thread still race every step of the cell.
    #[cfg(not(miri))]
    const EACH: u64 = 2_000;
    #[cfg(miri)]
    const EACH: u64 = 50;
    let dropped = drops();
    let installed = std::sync::Arc::new(StdAtomicUsize::new(0));
    let made = std::sync::Arc::new(StdAtomicUsize::new(0));
    let ring: Ring<Probe> = Ring::new(8);
    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            let (ring, dropped, installed, made) = (&ring, &dropped, &installed, &made);
            scope.spawn(move || {
                for _ in 0..EACH {
                    // Take a slot, as an owner: install, or lose to a void.
                    let index = loop {
                        let index = ring.reserve();
                        let desc = probe(index, dropped);
                        made.fetch_add(1, StdOrdering::SeqCst);
                        let mut done = None;
                        loop {
                            match ring.install(index, &desc) {
                                Installed::Done => {
                                    done = Some(index);
                                    break;
                                }
                                Installed::Lost => break,
                                Installed::Full => {
                                    // Help the lap before through.
                                    finish(ring, index - ring.capacity());
                                }
                            }
                        }
                        if let Some(index) = done {
                            installed.fetch_add(1, StdOrdering::SeqCst);
                            break index;
                        }
                    };
                    finish(ring, index);
                }
            });
        }
    });
    // Whatever is still in a cell is dropped with the ring.
    drop(ring);
    assert_eq!(
        dropped.load(StdOrdering::SeqCst),
        made.load(StdOrdering::SeqCst),
        "every descriptor is dropped exactly once"
    );
    assert_eq!(installed.load(StdOrdering::SeqCst) as u64, THREADS * EACH);
}

/// Decide and publish every slot up to `upto`, as a helper: void the empty
/// ones, check the loaded ones, pass both frontiers.
fn finish(ring: &Ring<Probe>, upto: u64) {
    loop {
        let decided = ring.decided();
        if decided >= upto {
            break;
        }
        let index = decided + 1;
        match ring.load(index) {
            Loaded::Empty => {
                ring.void(index);
            }
            Loaded::Ready(desc) => {
                assert_eq!(
                    desc.id, index,
                    "a cell handed out another slot's descriptor"
                );
                ring.pass_decided(index);
            }
            // R6: a void slot's cell may have moved on.
            Loaded::Void | Loaded::Gone => {
                ring.pass_decided(index);
            }
            Loaded::Full => finish_published(ring, index - ring.capacity()),
        }
    }
    finish_published(ring, upto);
}

fn finish_published(ring: &Ring<Probe>, upto: u64) {
    loop {
        let published = ring.published();
        if published >= upto.min(ring.decided()) {
            return;
        }
        let index = published + 1;
        if ring.pass_published(index) {
            ring.retire(index);
        }
    }
}

/// A value for an [`ArcSlot`].
struct Tip {
    value: u64,
    drops: std::sync::Arc<StdAtomicUsize>,
    link: AtomicPtr<Tip>,
}

impl Linked for Tip {
    fn link(&self) -> &AtomicPtr<Self> {
        &self.link
    }
}

impl Drop for Tip {
    fn drop(&mut self) {
        self.drops.fetch_add(1, StdOrdering::SeqCst);
    }
}

fn tip(value: u64, drops: &std::sync::Arc<StdAtomicUsize>) -> Arc<Tip> {
    Arc::new(Tip {
        value,
        drops: std::sync::Arc::clone(drops),
        link: AtomicPtr::new(ptr::null_mut()),
    })
}

#[test]
fn a_slot_is_replaced_only_from_the_value_it_holds() {
    let dropped = drops();
    let slot = ArcSlot::new(tip(1, &dropped));
    let one = slot.load();
    assert!(slot.replace(&one, tip(2, &dropped)));
    assert_eq!(slot.load().value, 2);
    // `one` is stale now: a replacement from it loses, and its value goes.
    assert!(!slot.replace(&one, tip(3, &dropped)));
    assert_eq!(dropped.load(StdOrdering::SeqCst), 1, "the refused value");
    assert_eq!(slot.load().value, 2);
    drop(one);
    assert_eq!(dropped.load(StdOrdering::SeqCst), 2);
}

/// R3 for the slot: a value replaced while a load is under way is kept
/// until a later replacement finds no load, and is then dropped.
#[test]
fn a_replaced_value_waits_for_a_load_under_way() {
    let dropped = drops();
    let slot = ArcSlot::new(tip(1, &dropped));
    // A thread stopped inside a load.
    slot.pins.fetch_add(1, Ordering::SeqCst);
    let one = slot.load();
    assert!(slot.replace(&one, tip(2, &dropped)));
    drop(one);
    assert_eq!(
        dropped.load(StdOrdering::SeqCst),
        0,
        "the slot still holds its reference to the replaced value"
    );
    let two = slot.load();
    assert!(slot.replace(&two, tip(3, &dropped)));
    drop(two);
    assert_eq!(dropped.load(StdOrdering::SeqCst), 0);

    slot.pins.fetch_sub(1, Ordering::SeqCst);
    let three = slot.load();
    assert!(slot.replace(&three, tip(4, &dropped)));
    drop(three);
    assert_eq!(
        dropped.load(StdOrdering::SeqCst),
        3,
        "every replaced value is dropped once no load is under way"
    );
    drop(slot);
    assert_eq!(dropped.load(StdOrdering::SeqCst), 4);
}

#[test]
fn loads_and_replacements_race_without_a_lost_or_double_drop() {
    const THREADS: u64 = 4;
    // Sized for the interpreter as the ring's race above is.
    #[cfg(not(miri))]
    const EACH: u64 = 5_000;
    #[cfg(miri)]
    const EACH: u64 = 100;
    let dropped = drops();
    let slot = ArcSlot::new(tip(0, &dropped));
    let made = std::sync::Arc::new(StdAtomicUsize::new(1));
    std::thread::scope(|scope| {
        for _ in 0..THREADS {
            let (slot, dropped, made) = (&slot, &dropped, &made);
            scope.spawn(move || {
                for _ in 0..EACH {
                    let seen = slot.load();
                    made.fetch_add(1, StdOrdering::SeqCst);
                    // Only ever forwards: a stale replacement loses.
                    let _ = slot.replace(&seen, tip(seen.value + 1, dropped));
                    assert!(slot.load().value >= seen.value);
                }
            });
        }
    });
    drop(slot);
    assert_eq!(
        dropped.load(StdOrdering::SeqCst),
        made.load(StdOrdering::SeqCst)
    );
}
