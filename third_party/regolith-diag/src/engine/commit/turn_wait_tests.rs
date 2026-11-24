//! A blocking call far from its turn (`pipeline.rs`, P6): it parks on its
//! own slot instead of yielding to the thread deciding the next one, the
//! thread that passes the decided frontier names the slots whose turn now
//! nears, and a call whose commit is done wakes them. Each test drives the
//! pipeline step by step, or stops a thread at a hook, so nothing depends
//! on timing.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

use super::descriptor::{NEAR, PARKED};
use super::hook::{self, Stage};
use super::pipeline::{Pipeline, Turn};
use super::pipeline_tests::{eventual_put, install, key, open_engine, seq_of};
use super::*;
use crate::sync::internal::Ordering;

const DEADLINE: Duration = Duration::from_secs(30);

/// How many calls the test says sleep far from their turn.
fn pretend_sleepers(pipe: &Pipeline, n: usize) {
    pipe.turns.far.0.store(n, Ordering::SeqCst);
}

#[test]
fn a_pass_names_the_slots_whose_turn_nears_only_while_someone_sleeps() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    let (first, _) = install(&engine, eventual_put(b"a", b"v"));
    assert!(matches!(
        engine.help_decide(pipe, first, Help::Wait, None),
        Turn::Done
    ));
    assert_eq!(
        pipe.turns.near_to.0.load(Ordering::SeqCst),
        0,
        "nobody sleeps: no slot is named"
    );
    pretend_sleepers(pipe, 1);
    let (second, _) = install(&engine, eventual_put(b"b", b"v"));
    assert!(matches!(
        engine.help_decide(pipe, second, Help::Wait, None),
        Turn::Done
    ));
    assert_eq!(
        pipe.turns.near_to.0.load(Ordering::SeqCst),
        second + pipe.turns.lead(),
        "the pass names every slot up to the lead past it"
    );
    pretend_sleepers(pipe, 0);
}

#[test]
fn a_finished_commit_wakes_the_sleepers_its_frontier_named() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    pretend_sleepers(pipe, 1);
    let slots: Vec<_> = [b"a", b"b", b"c", b"d"]
        .iter()
        .map(|name| install(&engine, eventual_put(*name, b"v")))
        .collect();
    let (first, _) = &slots[0];
    assert!(matches!(
        engine.help_decide(pipe, *first, Help::Wait, None),
        Turn::Done
    ));
    let named = first + pipe.turns.lead();
    assert_eq!(pipe.turns.near_to.0.load(Ordering::SeqCst), named);
    for (index, desc) in &slots {
        assert!(!desc.is(NEAR), "nobody is told before a commit is done");
        let _ = index;
    }
    engine.wake_near(pipe);
    for (index, desc) in &slots[1..] {
        assert_eq!(
            desc.is(NEAR),
            *index <= named,
            "slot {index}: told exactly when the frontier named it"
        );
    }
    pretend_sleepers(pipe, 0);
}

/// A blocking slot that lands wakes whoever parked on it: its owner,
/// asleep far from its turn, whose slot has no record and so no other flag
/// that would wake it.
#[test]
fn a_blocking_slot_that_lands_wakes_its_owner() {
    let desc = super::spare::descriptor(Work::Request(eventual_put(b"a", b"v")), true);
    let (woke, woken) = mpsc::channel();
    thread::scope(|scope| {
        let sleeper = {
            let desc = &desc;
            scope.spawn(move || {
                // Patience far past the deadline: only a wake ends it in
                // time.
                assert!(desc.park(DEADLINE * 2, || false), "it parked");
                woke.send(()).unwrap();
            })
        };
        let deadline = std::time::Instant::now() + DEADLINE;
        while !desc.is(PARKED) {
            assert!(
                std::time::Instant::now() < deadline,
                "the owner never parked"
            );
            thread::yield_now();
        }
        desc.land();
        woken
            .recv_timeout(DEADLINE)
            .expect("the slot landed and its owner slept on");
        sleeper.join().unwrap();
    });
}

#[test]
fn a_call_near_its_turn_does_not_park() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    let (next, _) = install(&engine, eventual_put(b"a", b"v"));
    let own = super::spare::descriptor(Work::Request(eventual_put(b"b", b"v")), true);
    let index = engine.enter(pipe, &own);
    assert!(index <= next + pipe.turns.lead());
    assert!(
        !engine.park_far(pipe, &own, next),
        "a call within the lead of the frontier parked"
    );
    engine.drive(pipe, index, &own);
    seq_of(&own).unwrap();
}

/// P6 end to end: the owner of slot 1 stops while it decides it; the owner
/// of a slot far behind it counts itself in to park, and the test holds it
/// there so its patience cannot run out first. Another thread decides the
/// slots between, which names the far slot, and its commit done tells it;
/// the far call then finds its turn near and carries every slot before its
/// own through, the stopped owner's included.
#[test]
fn a_call_far_from_its_turn_is_woken_when_its_turn_nears() {
    let dir = TempDir::new().unwrap();
    let engine = open_engine(&dir);
    let pipe = engine.pipe().unwrap();
    let owner = super::spare::descriptor(Work::Request(eventual_put(b"owner", b"v")), true);
    let first = engine.enter(pipe, &owner);
    let between: Vec<_> = [b"b1", b"b2", b"b3"]
        .iter()
        .map(|name| install(&engine, eventual_put(*name, b"v")))
        .collect();
    let far = super::spare::descriptor(Work::Request(eventual_put(b"far", b"v")), true);
    let far_index = engine.enter(pipe, &far);
    assert!(
        far_index > first + 1 + pipe.turns.lead(),
        "far from its turn"
    );

    let (stopped, owner_stopped) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let (parked, far_parked) = mpsc::channel();
    let (wake, woken) = mpsc::channel::<()>();
    thread::scope(|scope| {
        let owner_thread = {
            let (engine, owner) = (&engine, &owner);
            scope.spawn(move || {
                let mut fired = false;
                hook::set(move |stage| {
                    if stage == Stage::Decide && !fired {
                        fired = true;
                        stopped.send(()).unwrap();
                        let _ = released.recv_timeout(DEADLINE);
                    }
                });
                engine.drive(engine.pipe().unwrap(), first, owner);
                hook::clear();
            })
        };
        owner_stopped
            .recv_timeout(DEADLINE)
            .expect("the owner never reached its decision");
        let far_thread = {
            let (engine, far) = (&engine, &far);
            scope.spawn(move || {
                let mut fired = false;
                hook::set(move |stage| {
                    if stage == Stage::TurnWait && !fired {
                        fired = true;
                        parked.send(()).unwrap();
                        // Held where it is counted in and about to sleep,
                        // so its patience cannot run out first.
                        let _ = woken.recv_timeout(DEADLINE);
                    }
                });
                engine.drive(engine.pipe().unwrap(), far_index, far);
                hook::clear();
            })
        };
        far_parked
            .recv_timeout(DEADLINE)
            .expect("the call far from its turn never parked");
        // This thread decides every slot before the far one, past the
        // stopped owner's decision, then finishes a commit of its own.
        let (last, last_desc) = between.last().unwrap();
        assert!(matches!(
            engine.help_decide(pipe, *last, Help::Wait, None),
            Turn::Done
        ));
        assert!(
            pipe.turns.near_to.0.load(Ordering::SeqCst) >= far_index,
            "the frontier named the far slot"
        );
        engine.drive(pipe, *last, last_desc);
        assert!(
            far.is(NEAR),
            "the finished commit told the sleeper its turn neared"
        );
        wake.send(()).unwrap();
        far_thread.join().unwrap();
        release.send(()).unwrap();
        owner_thread.join().unwrap();
    });
    seq_of(&far).unwrap();
    seq_of(&owner).unwrap();
    for (name, _) in [(b"owner".as_slice(), 0), (b"far", 0), (b"b1", 0)] {
        assert_eq!(
            engine.get(&key(name), u64::MAX).unwrap(),
            Some(b"v".to_vec())
        );
    }
}
