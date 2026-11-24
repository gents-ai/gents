//! Models of the per-thread I/O queues (D53): the claim of a unit, the
//! completion pushed to every queue waiting on it, the owner's idle waker,
//! the wait a read hands back, and close meeting a miss under way.
//!
//! Each positive model drives the production types: `Unit`, `QueueShared`,
//! `WaitSlot` and `IoWait`, whose atomics and cells come from
//! `crate::sync::internal` and so are loom's under `--cfg loom`. Each
//! calibration rebuilds the one step it breaks out of plain loom atomics,
//! the way the bug would be written, and must fail.

use std::future::Future;
use std::io;
use std::num::NonZeroU64;
use std::sync::Arc as StdArc;
use std::task::{Context, Wake, Waker};

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::thread;

use super::super::block_cache::BlockCache;
use super::super::io::CloseGate;
use super::super::io::shared::{Message, QueueShared, WaitSlot};
use super::super::io::unit::{Unit, UnitKey};
use super::explore;
use crate::io_queue::{IoWait, QueueId};

fn queue_id(n: u64) -> QueueId {
    QueueId::new(NonZeroU64::new(n).expect("model ids start at 1"))
}

fn queue(n: u64) -> StdArc<QueueShared> {
    StdArc::new(QueueShared::new(queue_id(n), 1 << 20))
}

fn key() -> UnitKey {
    UnitKey {
        file_id: 1,
        offset: 0,
        guard: None,
    }
}

/// A unit whose work counts its runs and reads nothing.
fn counted_unit(runs: &Arc<AtomicUsize>) -> StdArc<Unit> {
    let runs = Arc::clone(runs);
    StdArc::new(Unit::new(
        key(),
        1,
        Box::new(move |_| {
            runs.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other("modelled read"))
        }),
    ))
}

/// Counts wakes, as a `Waker`.
struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: StdArc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &StdArc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn counting_waker() -> (StdArc<Wakes>, Waker) {
    let wakes = StdArc::new(Wakes(AtomicUsize::new(0)));
    (StdArc::clone(&wakes), Waker::from(wakes))
}

/// Three threads race for one unit: one claim wins, and the work runs once.
pub fn a_unit_is_claimed_once_and_runs_once() {
    explore("a_unit_is_claimed_once_and_runs_once", 6, 1, |witness| {
        let runs = Arc::new(AtomicUsize::new(0));
        let unit = counted_unit(&runs);
        let cache = StdArc::new(BlockCache::with_config(0, 0, false));
        let racers: Vec<_> = (0..2)
            .map(|_| {
                let (unit, cache) = (StdArc::clone(&unit), StdArc::clone(&cache));
                thread::spawn(move || {
                    let won = unit.claim();
                    if won {
                        unit.run(&cache, || false);
                    }
                    usize::from(won)
                })
            })
            .collect();
        let mut won = usize::from(unit.claim());
        if won == 1 {
            unit.run(&cache, || false);
        }
        for racer in racers {
            won += racer.join().expect("racer");
        }
        assert_eq!(won, 1, "two threads claimed one unit");
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the unit ran twice");
        assert!(unit.is_done());
        witness.record();
    });
}

/// Two queues register on a unit while a third thread finishes it: each
/// queue that registered gets exactly one completion, and one that was
/// refused sees the unit finished.
pub fn every_registered_queue_gets_one_completion() {
    explore(
        "every_registered_queue_gets_one_completion",
        6,
        1,
        |witness| {
            let runs = Arc::new(AtomicUsize::new(0));
            let unit = counted_unit(&runs);
            let (a, b) = (queue(1), queue(2));
            let registrars: Vec<_> = [&a, &b]
                .into_iter()
                .map(|q| {
                    let (unit, q) = (StdArc::clone(&unit), StdArc::clone(q));
                    thread::spawn(move || {
                        let registered = unit.register(q);
                        // A refused registration reads the outcome itself.
                        assert!(registered || unit.outcome().is_some());
                        registered
                    })
                })
                .collect();
            assert!(unit.release(), "nobody else claims the unit here");
            let registered: Vec<bool> = registrars
                .into_iter()
                .map(|r| r.join().expect("registrar"))
                .collect();
            for (q, registered) in [&a, &b].into_iter().zip(&registered) {
                let done = q
                    .take_inbox()
                    .filter(|message| matches!(message, Message::Done(_)))
                    .count();
                assert_eq!(
                    done,
                    usize::from(*registered),
                    "a queue got a completion it did not register for, or lost one"
                );
            }
            if registered.contains(&true) && registered.contains(&false) {
                witness.record();
            }
        },
    );
}

/// The owner idles while another thread pushes: the owner is woken exactly
/// once, by the push that found it idle or by `rest` itself when the push
/// came first.
pub fn an_idle_owner_is_woken_once() {
    explore("an_idle_owner_is_woken_once", 4, 1, |witness| {
        let q = queue(1);
        let (wakes, waker) = counting_waker();
        let pusher = {
            let q = StdArc::clone(&q);
            thread::spawn(move || {
                q.deliver(Message::Room(StdArc::new(WaitSlot::new())))
                    .is_ok_and(|woke| woke)
            })
        };
        q.rest(&waker);
        let woke = pusher.join().expect("pusher");
        assert_eq!(wakes.0.load(Ordering::SeqCst), 1, "an idle owner slept");
        if woke {
            witness.record();
        }
    });
}

/// The owner idles, then runs again, while another thread pushes: a push
/// after the owner woke on its own wakes nobody, so a busy owner is never
/// woken, and an idle one exactly once.
pub fn a_busy_owner_is_never_woken() {
    explore("a_busy_owner_is_never_woken", 4, 1, |witness| {
        let q = queue(1);
        let (wakes, waker) = counting_waker();
        let pusher = {
            let q = StdArc::clone(&q);
            thread::spawn(move || {
                q.deliver(Message::Room(StdArc::new(WaitSlot::new())))
                    .is_ok_and(|woke| woke)
            })
        };
        q.rest(&waker);
        let woke_itself = q.wake_up();
        let woken_by_push = pusher.join().expect("pusher");
        let wakes = wakes.0.load(Ordering::SeqCst);
        assert!(
            !(woke_itself && woken_by_push),
            "a push woke an owner that was already running"
        );
        assert_eq!(
            wakes,
            usize::from(!woke_itself),
            "a busy owner was woken, or an idle one was not"
        );
        if woke_itself {
            witness.record();
        }
    });
}

/// A task polls a read's wait, then polls it again with another waker (a
/// task that moved), while the owner completes it: a poll that returned
/// `Pending` is always woken through the waker it was last given. No wakeup
/// is lost.
pub fn a_wait_is_never_lost() {
    explore("a_wait_is_never_lost", 6, 1, |witness| {
        let q = queue(1);
        let slot = StdArc::new(WaitSlot::new());
        let wait = IoWait::new(&q, None, StdArc::clone(&slot));
        let completer = thread::spawn(move || slot.complete());
        let parked = poll_until_woken(wait, 2);
        completer.join().expect("completer");
        if parked {
            witness.record();
        }
    });
}

/// A wait and its clone are polled on two threads while the owner completes
/// the read: each is woken through its own registration.
pub fn a_cloned_wait_is_woken_too() {
    explore("a_cloned_wait_is_woken_too", 6, 1, |witness| {
        let q = queue(1);
        let slot = StdArc::new(WaitSlot::new());
        let wait = IoWait::new(&q, None, StdArc::clone(&slot));
        let clone = wait.clone();
        let other = thread::spawn(move || poll_until_woken(clone, 1));
        let completer = thread::spawn(move || slot.complete());
        let parked = poll_until_woken(wait, 1);
        let parked_clone = other.join().expect("clone");
        completer.join().expect("completer");
        if parked && parked_clone {
            witness.record();
        }
    });
}

/// Poll `wait` `polls` times, each with a new waker, then, if it is still
/// pending, wait for the last waker to fire and check the wait is ready.
/// Returns whether the wait was ever pending.
fn poll_until_woken(mut wait: IoWait, polls: usize) -> bool {
    let mut last = None;
    for _ in 0..polls {
        let (wakes, waker) = counting_waker();
        if std::pin::Pin::new(&mut wait)
            .poll(&mut Context::from_waker(&waker))
            .is_ready()
        {
            return last.is_some();
        }
        last = Some((wakes, waker));
    }
    let Some((wakes, waker)) = last else {
        return false;
    };
    // Wait for the completion the way a parked task does: only the wake
    // ends it. A wake that never comes leaves this spinning, which loom
    // reports as a livelock past its branch budget.
    while wakes.0.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }
    assert!(
        std::pin::Pin::new(&mut wait)
            .poll(&mut Context::from_waker(&waker))
            .is_ready(),
        "a woken wait was not ready"
    );
    true
}

/// A claim written as a load and a store instead of one compare-and-swap:
/// two threads both see the unit free and both run it.
pub fn calibration_a_claim_without_a_cas_runs_twice() {
    explore(
        "calibration_a_claim_without_a_cas_runs_twice",
        2,
        0,
        |_witness| {
            let state = Arc::new(AtomicUsize::new(0));
            let runs = Arc::new(AtomicUsize::new(0));
            let claim = |state: &AtomicUsize, runs: &AtomicUsize| {
                if state.load(Ordering::SeqCst) == 0 {
                    state.store(1, Ordering::SeqCst);
                    runs.fetch_add(1, Ordering::SeqCst);
                }
            };
            let racer = {
                let (state, runs) = (Arc::clone(&state), Arc::clone(&runs));
                thread::spawn(move || claim(&state, &runs))
            };
            claim(&state, &runs);
            racer.join().expect("racer");
            assert_eq!(runs.load(Ordering::SeqCst), 1, "the unit ran twice");
        },
    );
}

/// A finish that publishes "done" and then takes the waiter list in two
/// steps, and a registration that checks "done" before it pushes: a queue
/// that checked in between is pushed after the take and never completed.
pub fn calibration_a_close_apart_from_the_take_loses_a_completion() {
    explore(
        "calibration_a_close_apart_from_the_take_loses_a_completion",
        2,
        0,
        |_witness| {
            let done = Arc::new(AtomicBool::new(false));
            let waiting = Arc::new(AtomicBool::new(false));
            let delivered = Arc::new(AtomicBool::new(false));
            let registrar = {
                let (done, waiting) = (Arc::clone(&done), Arc::clone(&waiting));
                thread::spawn(move || {
                    if done.load(Ordering::SeqCst) {
                        return false;
                    }
                    waiting.store(true, Ordering::SeqCst);
                    true
                })
            };
            done.store(true, Ordering::SeqCst);
            if waiting.swap(false, Ordering::SeqCst) {
                delivered.store(true, Ordering::SeqCst);
            }
            let registered = registrar.join().expect("registrar");
            assert!(
                !registered || delivered.load(Ordering::SeqCst),
                "a registered queue got no completion"
            );
        },
    );
}

/// An owner that checks its inbox and then marks itself idle in a second
/// step: a push in between sees no idle owner and wakes nobody, and the
/// owner sleeps on a message.
pub fn calibration_an_idle_mark_apart_from_the_inbox_loses_a_wakeup() {
    explore(
        "calibration_an_idle_mark_apart_from_the_inbox_loses_a_wakeup",
        2,
        0,
        |_witness| {
            let inbox = Arc::new(AtomicUsize::new(0));
            let idle = Arc::new(AtomicBool::new(false));
            let woken = Arc::new(AtomicBool::new(false));
            let pusher = {
                let (inbox, idle, woken) =
                    (Arc::clone(&inbox), Arc::clone(&idle), Arc::clone(&woken));
                thread::spawn(move || {
                    inbox.fetch_add(1, Ordering::SeqCst);
                    if idle.swap(false, Ordering::SeqCst) {
                        woken.store(true, Ordering::SeqCst);
                    }
                })
            };
            let empty = inbox.load(Ordering::SeqCst) == 0;
            if empty {
                idle.store(true, Ordering::SeqCst);
            }
            pusher.join().expect("pusher");
            assert!(
                !empty || woken.load(Ordering::SeqCst),
                "an idle owner slept on a non-empty inbox"
            );
        },
    );
}

/// A wait that registers its waker without looking again: the completion
/// lands between the look and the registration, finds no waker, and the
/// task stays parked with nobody left to wake it.
pub fn calibration_a_wait_without_a_second_look_hangs() {
    explore(
        "calibration_a_wait_without_a_second_look_hangs",
        2,
        0,
        |_witness| {
            let ready = Arc::new(AtomicBool::new(false));
            let parked = Arc::new(loom::sync::Mutex::new(None::<Waker>));
            let (wakes, waker) = counting_waker();
            let completer = {
                let (ready, parked) = (Arc::clone(&ready), Arc::clone(&parked));
                thread::spawn(move || {
                    ready.store(true, Ordering::SeqCst);
                    if let Some(waker) = parked.lock().expect("waker").take() {
                        waker.wake();
                    }
                })
            };
            let pending = if ready.load(Ordering::SeqCst) {
                false
            } else {
                *parked.lock().expect("waker") = Some(waker);
                true
            };
            completer.join().expect("completer");
            assert!(
                !pending || wakes.0.load(Ordering::SeqCst) > 0,
                "a wait parked with nobody left to wake it"
            );
        },
    );
}

/// A read that checked the database open goes on to miss while close runs:
/// the miss counts itself in at the production `CloseGate`, puts its unit in
/// the table and counts itself out, while close marks the gate and then
/// sweeps the table. In every interleaving the unit is released once close
/// has returned and the miss has ended (the reader is joined), by close's
/// sweep or by the miss itself. That is the boundary the contract states,
/// not the instant close returns: the witness counts the runs where close
/// returned with the unit still in the table, because the sweep came before
/// the insert, and the miss let it go when it counted out.
/// The table is one flag here, standing for the map's insert and its
/// iteration, which loom does not see inside the map.
pub fn no_unit_outlives_close() {
    explore("no_unit_outlives_close", 3, 1, |witness| {
        let gate = StdArc::new(CloseGate::new());
        let in_table = Arc::new(AtomicBool::new(false));
        let runs = Arc::new(AtomicUsize::new(0));
        let unit = counted_unit(&runs);
        let reader = {
            let (gate, in_table, unit) = (
                StdArc::clone(&gate),
                Arc::clone(&in_table),
                StdArc::clone(&unit),
            );
            thread::spawn(move || {
                if !gate.enter() {
                    return (false, false);
                }
                in_table.store(true, Ordering::Release);
                let released_here = gate.leave() && unit.release();
                (true, released_here)
            })
        };
        gate.close();
        if in_table.load(Ordering::Acquire) {
            unit.release();
        }
        // Close has returned; the miss may still be under way.
        let done_when_close_returned = unit.is_done();
        let (inserted, released_here) = reader.join().expect("reader");
        assert!(!inserted || unit.is_done(), "a unit outlived close");
        assert_eq!(runs.load(Ordering::SeqCst), 0, "a released unit never runs");
        assert!(
            !inserted || done_when_close_returned || released_here,
            "a unit close left behind was not let go by its miss"
        );
        if inserted && !done_when_close_returned {
            witness.record();
        }
    });
}

/// A miss that checks for close only once, when it starts, and then puts
/// its unit in the table: close's sweep can come between the two, and the
/// unit outlives close.
pub fn calibration_a_miss_checked_only_when_it_starts_outlives_close() {
    explore(
        "calibration_a_miss_checked_only_when_it_starts_outlives_close",
        2,
        0,
        |_witness| {
            let closed = Arc::new(AtomicBool::new(false));
            let in_table = Arc::new(AtomicBool::new(false));
            let released = Arc::new(AtomicBool::new(false));
            let reader = {
                let (closed, in_table) = (Arc::clone(&closed), Arc::clone(&in_table));
                thread::spawn(move || {
                    if closed.load(Ordering::Acquire) {
                        return false;
                    }
                    in_table.store(true, Ordering::Release);
                    true
                })
            };
            closed.store(true, Ordering::Release);
            if in_table.load(Ordering::Acquire) {
                released.store(true, Ordering::SeqCst);
            }
            let inserted = reader.join().expect("reader");
            assert!(
                !inserted || released.load(Ordering::SeqCst),
                "a unit outlived close"
            );
        },
    );
}
