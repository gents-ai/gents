//! Models of the compaction gate (`engine::compaction_gate`, plan 4.6, E28)
//! and of a job that parks on its queue instead of waiting for it
//! (`engine::io::job`, Parking).
//!
//! The positive models drive the production types: `CompactionGate`, whose
//! state is the `regolith::sync::RwLock` word and wait queue, and `Job`,
//! `QueueShared` and the waker a queue steps a job with. Their atomics and
//! cells are loom's under `--cfg loom`, and every wait goes through
//! `loom::future::block_on`, which parks the model's thread until its waker
//! fires, so a wake that never comes fails the model as a deadlock.
//!
//! - **Exclusion**: what the gate guards is a `loom::cell::UnsafeCell`, read
//!   by a pass and written by a job, so a pass and a job inside together
//!   fail the model as a data race.
//! - **E28, bounded bypass**: `MAX_BYPASS` is 2 under loom. A pass that
//!   waits while a job thread keeps taking the gate back is polled and loses
//!   at most `MAX_BYPASS` times before it is let in.
//! - **No lost retry**: a job parked on its queue is pushed back to it when
//!   the gate frees, whatever the gate's release races.
//!
//! Each is paired with a calibration that gets the one step wrong and must
//! fail: a job that skips the gate, the gate this one replaced (a waiting
//! exclusive user holds every new pass back), and a parked job stepped with
//! a waker that reaches no queue.

#![allow(unsafe_code)]

use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::Arc as StdArc;
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};
use std::task::{Poll, Waker};

use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Condvar, Mutex};
use loom::thread;

use super::super::compaction_gate::{Admission, CompactionGate};
use super::super::io::job::{Job, JobBody, Step, unpark_waker};
use super::super::io::shared::{Message, QueueShared};
use crate::io_queue::QueueId;
use crate::sync::MAX_BYPASS;
use crate::sync::internal::block_on;

/// What the gate guards in these models: read by a pass, written by a job.
/// Tracked by loom, so an access the gate did not order against the others
/// fails the model.
struct Tables(UnsafeCell<u32>);

// SAFETY: every access goes through the tracked cell, which is what checks
// that the gate ordered it; the calibrations break that on purpose.
unsafe impl Send for Tables {}
// SAFETY: as above.
unsafe impl Sync for Tables {}

impl Tables {
    fn new() -> Arc<Self> {
        Arc::new(Self(UnsafeCell::new(0)))
    }

    /// What a pass does: read.
    fn read(&self) -> u32 {
        // SAFETY: the caller holds the gate shared, or is a calibration.
        self.0.with(|tables| unsafe { *tables })
    }

    /// What a job does: write.
    fn write(&self) {
        // SAFETY: the caller holds the gate alone, or is a calibration.
        self.0.with_mut(|tables| unsafe { *tables += 1 });
    }
}

/// Run `model` under loom with at most `preemptions` preemptions per
/// schedule (`LOOM_MAX_PREEMPTIONS` overrides it), and fail it when the
/// search was implausibly small or never reached the state it checks.
fn explore(
    name: &'static str,
    preemptions: usize,
    min_interleavings: usize,
    min_witnesses: usize,
    model: impl Fn(&StdAtomicUsize) + Sync + Send + 'static,
) {
    let runs = StdArc::new(StdAtomicUsize::new(0));
    let witnessed = StdArc::new(StdAtomicUsize::new(0));
    let counted = {
        let (runs, witnessed) = (StdArc::clone(&runs), StdArc::clone(&witnessed));
        move || {
            model(&witnessed);
            runs.fetch_add(1, StdOrdering::Relaxed);
        }
    };
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = builder.preemption_bound.or(Some(preemptions));
    builder.check(counted);
    let (explored, witnessed) = (
        runs.load(StdOrdering::Relaxed),
        witnessed.load(StdOrdering::Relaxed),
    );
    println!("loom model {name}: {explored} interleavings explored, {witnessed} witnessed");
    assert!(
        explored >= min_interleavings,
        "{name} explored only {explored} interleavings, below the {min_interleavings} floor"
    );
    assert!(
        witnessed >= min_witnesses,
        "{name} reached its interesting state {witnessed} times, below the {min_witnesses} floor"
    );
}

/// A pass and a job meet at the gate: the job never overlaps the pass, and
/// neither is left waiting. Two threads, so the search covers every
/// interleaving within three preemptions; with a second pass thread it did
/// not finish in five minutes at two.
pub fn a_job_never_overlaps_a_pass() {
    explore("a_job_never_overlaps_a_pass", 3, 20, 1, |witness| {
        let gate = CompactionGate::new();
        let tables = Tables::new();
        let pass = {
            let (gate, tables) = (gate.clone(), Arc::clone(&tables));
            thread::spawn(move || {
                let _pass = gate.shared();
                tables.read()
            })
        };
        {
            let _held = gate.exclusive().expect("the model has threads");
            tables.write();
        }
        // The pass came after the job, having waited for it or not.
        if pass.join().expect("pass") == 1 {
            witness.fetch_add(1, StdOrdering::Relaxed);
        }
    });
}

/// A job that goes to the tables without the gate while a pass reads them.
pub fn calibration_a_job_that_skips_the_gate_races_a_pass() {
    explore(
        "calibration_a_job_that_skips_the_gate_races_a_pass",
        2,
        1,
        0,
        |_witness| {
            let gate = CompactionGate::new();
            let tables = Tables::new();
            let pass = {
                let (gate, tables) = (gate.clone(), Arc::clone(&tables));
                thread::spawn(move || {
                    let _pass = gate.shared();
                    tables.read()
                })
            };
            // The job never took the gate.
            tables.write();
            pass.join().expect("pass");
        },
    );
}

/// How many times in a row the job thread takes the gate: enough for a
/// waiting pass to lose `MAX_BYPASS` times and then see what follows.
const JOBS: usize = 4;

/// E28. A job thread takes the gate, lets go and takes it straight back,
/// `JOBS` times, while a pass waits. However the two interleave, the pass is
/// polled and found short at most once when it arrives and `MAX_BYPASS`
/// times after: then it is owed the gate, the job thread's next attempt
/// queues behind it, and the next release lets the pass in.
pub fn a_waiting_pass_is_passed_over_at_most_max_bypass_times() {
    explore(
        "a_waiting_pass_is_passed_over_at_most_max_bypass_times",
        3,
        100,
        1,
        |witness| {
            let gate = CompactionGate::new();
            let jobs = {
                let gate = gate.clone();
                thread::spawn(move || {
                    for _ in 0..JOBS {
                        drop(gate.exclusive().expect("the model has threads"));
                    }
                })
            };
            let mut short = 0usize;
            let mut wait = gate.lock().read();
            let pass = block_on(std::future::poll_fn(|cx| {
                let polled = Pin::new(&mut wait).poll(cx);
                if polled.is_pending() {
                    short += 1;
                }
                polled
            }));
            assert!(
                short <= 1 + MAX_BYPASS as usize,
                "the pass was found short {short} times: passed over more than MAX_BYPASS"
            );
            if short == 1 + MAX_BYPASS as usize {
                witness.fetch_add(1, StdOrdering::Relaxed);
            }
            drop(pass);
            jobs.join().expect("jobs");
        },
    );
}

/// The gate this one replaced: a mutex and a condition variable, with a
/// waiting exclusive user holding every new pass back.
#[derive(Default)]
struct WriterPreferring {
    state: Mutex<(bool, usize)>,
    changed: Condvar,
}

impl WriterPreferring {
    /// Enter shared, and report how often the pass looked and was held back.
    fn pass(&self) -> usize {
        let mut held_back = 0;
        let mut state = self.state.lock().expect("state");
        while state.0 || state.1 > 0 {
            held_back += 1;
            state = self.changed.wait(state).expect("state");
        }
        held_back
    }

    fn job(&self) {
        let mut state = self.state.lock().expect("state");
        state.1 += 1;
        while state.0 {
            state = self.changed.wait(state).expect("state");
        }
        state.1 -= 1;
        state.0 = true;
        drop(state);
        // Inside the gate: where a pass that looks now finds it held.
        thread::yield_now();
        self.state.lock().expect("state").0 = false;
        self.changed.notify_all();
    }
}

/// The same stream against the old gate: the pass is held back every time
/// it looks for as long as the job thread keeps coming, past any bound.
pub fn calibration_the_old_gate_holds_a_pass_back_without_bound() {
    explore(
        "calibration_the_old_gate_holds_a_pass_back_without_bound",
        3,
        1,
        0,
        |_witness| {
            let gate = Arc::new(WriterPreferring::default());
            let jobs = {
                let gate = Arc::clone(&gate);
                thread::spawn(move || {
                    for _ in 0..JOBS + 1 {
                        gate.job();
                    }
                })
            };
            let held_back = gate.pass();
            assert!(
                held_back <= 1 + MAX_BYPASS as usize,
                "the pass was held back {held_back} times"
            );
            jobs.join().expect("jobs");
        },
    );
}

/// A job that needs the gate alone: it parks while the gate is pending,
/// keeping its admission, and counts its runs.
struct Gated {
    admission: Admission,
    tables: Arc<Tables>,
    runs: Arc<AtomicUsize>,
}

impl JobBody for Gated {
    fn run(self: Box<Self>) {
        let Gated {
            admission,
            tables,
            runs,
        } = *self;
        let _held = admission.wait().expect("the model has threads");
        tables.write();
        runs.fetch_add(1, Ordering::SeqCst);
    }

    fn step(mut self: Box<Self>, waker: &Waker) -> Step {
        match self.admission.poll(waker) {
            Poll::Ready(_held) => {
                self.tables.write();
                self.runs.fetch_add(1, Ordering::SeqCst);
                Step::Done
            }
            Poll::Pending => Step::Parked(self),
        }
    }

    fn release(self: Box<Self>) {}
}

fn queue() -> StdArc<QueueShared> {
    StdArc::new(QueueShared::new(QueueId::new(NonZeroU64::MIN), 1 << 20))
}

/// The owner of `queue` runs `job` as a poll does and idles as `block_on`
/// does: step it under a claim with `waker_for`'s waker; when it parks, rest
/// on the queue until a message arrives, and step it again when the message
/// is the job's `Ready`.
fn own_until_done(
    queue: &StdArc<QueueShared>,
    job: &StdArc<Job>,
    waker_for: impl Fn(&StdArc<Job>, &StdArc<QueueShared>) -> Waker,
) -> usize {
    let mut steps = 0;
    let mut runnable = true;
    block_on(std::future::poll_fn(|cx| {
        for message in queue.take_inbox() {
            if matches!(&message, Message::Ready(ready) if StdArc::ptr_eq(ready, job)) {
                runnable = true;
            }
        }
        if runnable {
            runnable = false;
            steps += 1;
            assert!(job.claim(), "only the owner claims in this model");
            if job.step(&waker_for(job, queue)) {
                return Poll::Ready(());
            }
        }
        // Wakes at once when a message came in meanwhile.
        queue.rest(cx.waker());
        Poll::Pending
    }));
    steps
}

/// A pass holds the gate when a job on a queue is first stepped, or takes
/// and frees it around the step. The job parks instead of waiting, and the
/// gate's release pushes it back to its queue, which wakes the idle owner:
/// in every interleaving the job runs, once, and never beside the pass.
pub fn a_parked_job_is_pushed_back_when_the_gate_frees() {
    explore(
        "a_parked_job_is_pushed_back_when_the_gate_frees",
        3,
        20,
        1,
        |witness| {
            let gate = CompactionGate::new();
            let tables = Tables::new();
            let runs = Arc::new(AtomicUsize::new(0));
            let queue = queue();
            let job = Job::new(Box::new(Gated {
                admission: gate.admit(),
                tables: Arc::clone(&tables),
                runs: Arc::clone(&runs),
            }));
            let pass = {
                let (gate, tables) = (gate.clone(), Arc::clone(&tables));
                thread::spawn(move || {
                    let _pass = gate.shared();
                    tables.read();
                })
            };
            let steps = own_until_done(&queue, &job, unpark_waker);
            pass.join().expect("pass");
            assert!(job.is_done());
            assert_eq!(runs.load(Ordering::SeqCst), 1, "the job ran once");
            if steps > 1 {
                witness.fetch_add(1, StdOrdering::Relaxed);
            }
        },
    );
}

/// A parked job stepped with a waker that reaches no queue: the gate's
/// release wakes nothing, and the owner idles for good.
pub fn calibration_a_parked_job_with_no_way_back_is_never_run() {
    explore(
        "calibration_a_parked_job_with_no_way_back_is_never_run",
        3,
        1,
        0,
        |_witness| {
            let gate = CompactionGate::new();
            let tables = Tables::new();
            let runs = Arc::new(AtomicUsize::new(0));
            let queue = queue();
            let job = Job::new(Box::new(Gated {
                admission: gate.admit(),
                tables: Arc::clone(&tables),
                runs: Arc::clone(&runs),
            }));
            let pass = {
                let gate = gate.clone();
                thread::spawn(move || drop(gate.shared()))
            };
            own_until_done(&queue, &job, |_, _| Waker::noop().clone());
            pass.join().expect("pass");
        },
    );
}
