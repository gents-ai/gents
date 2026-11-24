//! Models of the commit side of the per-thread I/O queues (D53, plan 4.10):
//! a job (a sync round of the commit pipeline) claimed by one
//! compare-and-swap among the queues of the commits that race for it, the one completion it pushes to each
//! member queue, and a ticket's callbacks racing the delivery of its outcome.
//!
//! Each positive model drives the production types: `Job`, `QueueShared`
//! and `Completion`, whose atomics and cells come from `crate::sync::internal`
//! and so are loom's under `--cfg loom`. Each calibration rebuilds the one
//! step it breaks out of plain loom atomics and must fail.

use std::num::NonZeroU64;
use std::sync::Arc as StdArc;
use std::sync::Weak as StdWeak;

use loom::sync::Arc;
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::thread;

use super::super::io::job::{Job, JobBody};
use super::super::io::shared::{Message, QueueShared};
use super::super::io::stack::{SHUT, Stack};
use super::explore;
use crate::io_queue::QueueId;
use crate::io_queue::completion::Completion;

fn queue(n: u64) -> StdArc<QueueShared> {
    StdArc::new(QueueShared::new(
        QueueId::new(NonZeroU64::new(n).expect("model ids start at 1")),
        1 << 20,
    ))
}

/// A job body that counts its runs and its releases.
struct Counted {
    runs: Arc<AtomicUsize>,
}

impl JobBody for Counted {
    fn run(self: Box<Self>) {
        self.runs.fetch_add(1, Ordering::SeqCst);
    }

    fn release(self: Box<Self>) {
        self.runs.fetch_add(1, Ordering::SeqCst);
    }
}

fn counted_job(runs: &Arc<AtomicUsize>) -> StdArc<Job> {
    Job::new(Box::new(Counted {
        runs: Arc::clone(runs),
    }))
}

/// Three member threads poll at once and race for the group's job: one
/// claim wins, the work runs once, and the job lands.
pub fn a_job_is_claimed_once_and_runs_once() {
    explore("a_job_is_claimed_once_and_runs_once", 6, 1, |witness| {
        let runs = Arc::new(AtomicUsize::new(0));
        let job = counted_job(&runs);
        let racers: Vec<_> = (0..2)
            .map(|_| {
                let job = StdArc::clone(&job);
                thread::spawn(move || {
                    let won = job.claim();
                    if won {
                        job.run();
                    }
                    usize::from(won)
                })
            })
            .collect();
        let mut won = usize::from(job.claim());
        if won == 1 {
            job.run();
        }
        for racer in racers {
            won += racer.join().expect("racer");
        }
        assert_eq!(won, 1, "two member threads claimed one group's sync");
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the group synced twice");
        assert!(job.is_done());
        witness.record();
    });
}

/// Two member queues register on the group's job while a third member runs
/// it: each queue that registered is pushed exactly one completion, and one
/// whose registration was refused finds the job landed.
pub fn every_member_queue_gets_one_completion() {
    explore("every_member_queue_gets_one_completion", 6, 1, |witness| {
        let runs = Arc::new(AtomicUsize::new(0));
        let job = counted_job(&runs);
        let (a, b) = (queue(1), queue(2));
        let registrars: Vec<_> = [&a, &b]
            .into_iter()
            .map(|q| {
                let (job, q) = (StdArc::clone(&job), StdArc::clone(q));
                thread::spawn(move || {
                    let registered = job.register(q);
                    // A refused registration finds the job landed.
                    assert!(registered || job.is_done());
                    registered
                })
            })
            .collect();
        assert!(job.claim(), "nobody else claims the job here");
        job.run();
        let registered: Vec<bool> = registrars
            .into_iter()
            .map(|r| r.join().expect("registrar"))
            .collect();
        for (q, registered) in [&a, &b].into_iter().zip(&registered) {
            let done = q
                .take_inbox()
                .filter(|message| matches!(message, Message::JobDone(_)))
                .count();
            assert_eq!(done, usize::from(*registered), "completions for one queue");
        }
        if registered.iter().any(|r| *r) {
            witness.record();
        }
    });
}

/// A ticket's `on_complete` registered on one thread races the delivery of
/// the outcome on the queue's thread: the callback runs exactly once, with
/// the outcome, whichever side gets there first. It runs on the registering
/// thread only once the ticket is ready, and on the delivering thread only
/// before: the public contract. The witness is the registering thread
/// running it, which needs the delivery to have closed the list first.
pub fn a_ticket_callback_runs_exactly_once() {
    explore("a_ticket_callback_runs_exactly_once", 4, 1, |witness| {
        let completion: StdArc<Completion<u32>> = Completion::new(StdWeak::new(), "on_complete");
        let ran = Arc::new(AtomicUsize::new(0));
        // What the callback saw. Recorded and checked after the join: a
        // panic inside a callback is caught and reported, not propagated.
        let (on_registrar, saw_ready) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let registrar = {
            let (completion, ran) = (StdArc::clone(&completion), Arc::clone(&ran));
            let (on_registrar, saw_ready) = (Arc::clone(&on_registrar), Arc::clone(&saw_ready));
            thread::spawn(move || {
                let me = thread::current().id();
                let ticket = StdArc::downgrade(&completion);
                completion.on_complete(Box::new(move |outcome| {
                    let ready = ticket.upgrade().is_some_and(|ticket| ticket.is_ready());
                    on_registrar.store(thread::current().id() == me, Ordering::SeqCst);
                    saw_ready.store(ready, Ordering::SeqCst);
                    if *outcome == 7 {
                        ran.fetch_add(1, Ordering::SeqCst);
                    }
                }));
            })
        };
        assert!(completion.decide(7));
        completion.deliver_decided();
        registrar.join().expect("registrar");
        assert_eq!(ran.load(Ordering::SeqCst), 1, "the callback ran once");
        assert!(completion.is_ready());
        let here = on_registrar.load(Ordering::SeqCst);
        assert_eq!(
            here,
            saw_ready.load(Ordering::SeqCst),
            "a callback ran on the registering thread before the ticket was ready, \
             or on the delivering thread after"
        );
        if here {
            witness.record();
        }
    });
}

/// Calibration: a claim that loads the state and then stores it, instead of
/// one compare-and-swap, lets two member threads both run the group's sync.
pub fn calibration_a_job_claimed_without_a_cas_runs_twice() {
    explore(
        "calibration_a_job_claimed_without_a_cas_runs_twice",
        1,
        0,
        |_| {
            let state = Arc::new(AtomicUsize::new(0));
            let runs = Arc::new(AtomicUsize::new(0));
            let claim = |state: &AtomicUsize| {
                if state.load(Ordering::SeqCst) == 0 {
                    state.store(1, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            };
            let other = {
                let (state, runs) = (Arc::clone(&state), Arc::clone(&runs));
                thread::spawn(move || {
                    if claim(&state) {
                        runs.fetch_add(1, Ordering::SeqCst);
                    }
                })
            };
            if claim(&state) {
                runs.fetch_add(1, Ordering::SeqCst);
            }
            other.join().expect("other");
            assert_eq!(runs.load(Ordering::SeqCst), 1, "the group synced twice");
        },
    );
}

/// Calibration: a delivery that closes the callback list when it first takes
/// it, and marks the ticket ready only after running what it took, refuses a
/// registration in between, and the refused callback runs on the
/// registering thread while the ticket is not ready yet.
pub fn calibration_a_list_closed_before_readiness_runs_a_callback_early() {
    explore(
        "calibration_a_list_closed_before_readiness_runs_a_callback_early",
        1,
        0,
        |_| {
            let ready = Arc::new(AtomicBool::new(false));
            let callbacks: StdArc<Stack<usize>> = StdArc::new(Stack::new());
            let registrar = {
                let (ready, callbacks) = (Arc::clone(&ready), StdArc::clone(&callbacks));
                thread::spawn(move || {
                    if !ready.load(Ordering::SeqCst) && callbacks.push(1, SHUT).is_err() {
                        // Refused: the callback runs here, now.
                        assert!(
                            ready.load(Ordering::SeqCst),
                            "a callback ran on the registering thread before the ticket was ready"
                        );
                    }
                })
            };
            // The bug: close on the first take, mark ready after running.
            for _ in callbacks.take(SHUT) {}
            ready.store(true, Ordering::SeqCst);
            registrar.join().expect("registrar");
        },
    );
}

/// Calibration: an `on_complete` that checks the ticket ready and, finding
/// it not, pushes its callback onto a list delivery has already taken and
/// will not look at again, loses the callback.
pub fn calibration_a_callback_pushed_after_the_take_is_lost() {
    explore(
        "calibration_a_callback_pushed_after_the_take_is_lost",
        1,
        0,
        |_| {
            let ready = Arc::new(AtomicBool::new(false));
            let callbacks: StdArc<Stack<usize>> = StdArc::new(Stack::new());
            let ran = Arc::new(AtomicUsize::new(0));
            let registrar = {
                let (ready, callbacks) = (Arc::clone(&ready), StdArc::clone(&callbacks));
                thread::spawn(move || {
                    if !ready.load(Ordering::SeqCst) {
                        // The bug: a push the delivery's take does not refuse.
                        let _ = callbacks.push(1, 0);
                    }
                })
            };
            // Delivery: take the callbacks, then mark ready.
            for _ in callbacks.take(SHUT) {
                ran.fetch_add(1, Ordering::SeqCst);
            }
            ready.store(true, Ordering::SeqCst);
            registrar.join().expect("registrar");
            assert_eq!(ran.load(Ordering::SeqCst), 1, "the callback was lost");
        },
    );
}
