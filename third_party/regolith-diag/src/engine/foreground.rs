//! Foreground jobs: `compact_range`, an ingest and a checkpoint, run as
//! background jobs instead of on the caller's call (plan 4.10).
//!
//! Each of them holds the compaction gate alone for part of its work
//! (`compaction_gate.rs`). A caller never waits on the gate: the call
//! returns a [`JobTicket`] at once, and the job runs
//!
//! - **on the compaction worker**, when the database has one: the worker
//!   runs one job between its own passes and waits for the gate on its own
//!   thread;
//! - **as a job on the caller's queue**, with no worker: the caller's thread
//!   runs it when it polls its [`IoQueue`](crate::IoQueue), as the I/O it
//!   asked for. The poll waits for no other thread: not for the gate, and
//!   not for anything the job needs before or under it (see below). Where
//!   another thread has what the job needs next, the job parks on the
//!   queue, and the queue's owner is woken once it may go on;
//! - **inline**, with neither: the call runs it before it returns, as the
//!   blocking calls do their own I/O, and waits for the gate on its own
//!   thread, as [`JobTicket::wait`] does for a job of another queue that it
//!   runs itself. (A job of the waiting thread's own queue is stepped by
//!   that wait, with the queue's other jobs, as its poll steps them.)
//!
//! A job has two parts. What needs no gate comes first, with the gate free
//! for compaction: an ingest copies and validates its files, a checkpoint
//! readies its target and drains the memtables. Then the job takes the gate
//! and does the rest: the range's passes, the install, the capture and its
//! links.
//!
//! # Where a poll parks
//!
//! One function takes a job from wherever it stands as far as it goes
//! ([`RegolithEngine::advance_foreground`]): to its end for a worker or a
//! blocking caller, which wait where they must, and to the first thing
//! another thread holds for a queue's poll (`commit::park`). The places a
//! job can stand are the [`Stage`]s, and a poll parks at each of them
//! holding only what the stage says:
//!
//! | the job needs | a poll that does not get it |
//! |---|---|
//! | the version store's step, for the file ids of an ingest's staging | parks with nothing staged |
//! | its drain's unit seen through, then each flush's exclusion and step (a checkpoint before the gate, `compact_range` under it) | parks in the drain (`drain.rs`) |
//! | the gate | parks keeping its place among the gate's waiters |
//! | the step, for `compact_range`'s passes and for a checkpoint's capture | parks under the gate |
//! | the unit that installs an ingested table seen through | parks under the gate, the unit in the ring or not yet |
//!
//! Under the gate a parked job keeps the gate: giving it up would lose the
//! levels the job has read, or its place. Nothing that holds what such a
//! job parks for ever waits for the gate: the gate is taken before a unit
//! is entered and before the flush exclusion and the step, by every user
//! of it, never after.
//!
//! The ticket is completed on the queue of the thread that made the call (at
//! that queue's poll), or, for a caller with no queue, by whichever thread
//! finished the job. Close settles a job not yet run with
//! [`Error::Closed`]: the workers' queue when they stop, a queue's job in the
//! job table's sweep, parked or not.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::task::{Poll, Waker};

use super::RegolithEngine;
use super::commit::{Help, Needs};
use super::compaction_gate::{Admission, Exclusive};
use super::drain::Drain;
use super::ingest::{Install, Staged};
use super::io::job::{Delivery, Job, JobBody, Step};
use super::io::shared::{Message, QueueShared};
use crate::io_queue::completion::Completion;
use crate::{Error, IngestOptions, JobTicket};

/// Checks one column-family-prefixed key of an ingested file.
pub(crate) type KeyCheck = Box<dyn FnMut(&[u8]) -> io::Result<()> + Send>;

/// What a foreground job does.
pub(crate) enum Work {
    /// Compact every table overlapping `[lower, upper)`, both bounds column
    /// family prefixed.
    CompactRange { lower: Vec<u8>, upper: Vec<u8> },
    /// Ingest `files`, checking each key with `check`.
    Ingest {
        files: Vec<PathBuf>,
        options: IngestOptions,
        check: KeyCheck,
    },
    /// Create a checkpoint in `target`.
    Checkpoint { target: PathBuf },
}

/// A job whose part before the gate is done: what it does holding the gate.
enum Gated {
    CompactRange {
        lower: Vec<u8>,
        upper: Vec<u8>,
    },
    Ingest {
        staged: Staged,
        options: IngestOptions,
    },
    Checkpoint {
        target: PathBuf,
    },
}

/// Where a job stands: each is a place a queue's poll can park at, holding
/// what the stage holds and nothing else.
enum Stage {
    /// Nothing done yet.
    Fresh(Work),
    /// A checkpoint whose target is ready, draining the memtables before it
    /// asks for the gate.
    Draining { target: PathBuf, drain: Drain },
    /// The part before the gate is done, and the job waits to hold the gate
    /// alone, keeping its place among the gate's waiters.
    Admitting(Gated, Admission),
    /// The job holds the gate, and is this far in its work under it.
    Under(Under, Exclusive),
}

/// What a job has left to do holding the gate.
enum Under {
    /// `compact_range`: bring the memtables that may overlap to L0.
    Rotating {
        lower: Vec<u8>,
        upper: Vec<u8>,
        drain: Drain,
    },
    /// `compact_range`: push the range's tables down, level by level.
    Compacting { lower: Vec<u8>, upper: Vec<u8> },
    /// An ingest: install the staged tables, one unit each.
    Installing(Install),
    /// A checkpoint: capture the version and link its tables.
    Capturing { target: PathBuf },
}

/// What a job's part before the gate left.
enum Before {
    /// Nothing is left to do under the gate.
    Nothing,
    /// The gate is next.
    Gate(Gated),
    /// A checkpoint's drain is next, then the gate.
    Drain { target: PathBuf, drain: Drain },
    /// A queue's poll parked before anything was done.
    Parked(Work),
}

/// How far [`RegolithEngine::advance_foreground`] got.
enum Advanced {
    /// The job is over, with this outcome.
    Done(Result<(), Error>),
    /// A queue's poll parked the job at this stage.
    Parked(Stage),
}

/// Where a finished job's outcome is told.
enum Tell {
    /// The job is a unit on the caller's queue: the queue tells it as the
    /// job's waiter when the job lands.
    AsWaiter,
    /// The job ran on a worker for a caller with this queue: pushed there,
    /// told at its poll.
    Queue(Arc<QueueShared>),
    /// The job ran on a worker for a caller with no queue: told on the
    /// worker.
    Here,
}

/// The job's body: the work, and how its outcome reaches the ticket.
struct Foreground {
    engine: Weak<RegolithEngine>,
    stage: Stage,
    finish: Finish,
}

/// The ticket's shared half, and where its outcome is told.
struct Finish {
    completion: Arc<Completion<Result<(), Error>>>,
    tell: Tell,
}

impl Finish {
    fn finish(self, result: Result<(), Error>) {
        self.completion.decide(result);
        match self.tell {
            Tell::AsWaiter => {}
            Tell::Queue(queue) => {
                let waiter = Arc::clone(&self.completion) as Arc<dyn Delivery>;
                // A queue dropped since the call refuses the push: its drop
                // has told everything it held, so this is told here.
                if queue.deliver(Message::Deliver(waiter)).is_err() {
                    self.completion.deliver_decided();
                }
            }
            Tell::Here => self.completion.deliver_decided(),
        }
    }
}

impl JobBody for Foreground {
    /// On a worker, or for a blocking caller: waits here for the gate and
    /// for whatever else the job needs.
    fn run(self: Box<Self>) {
        let Foreground {
            engine,
            stage,
            finish,
        } = *self;
        let result = match engine.upgrade() {
            Some(engine) => engine.run_foreground(stage),
            None => Err(Error::Closed),
        };
        finish.finish(result);
    }

    /// On a queue's poll: parks instead of waiting for another thread (the
    /// module docs say where).
    fn step(self: Box<Self>, waker: &Waker) -> Step {
        let Foreground {
            engine: weak,
            stage,
            finish,
        } = *self;
        let Some(engine) = weak.upgrade() else {
            finish.finish(Err(Error::Closed));
            return Step::Done;
        };
        match engine.advance_foreground(stage, Help::Park(waker)) {
            Advanced::Done(result) => {
                finish.finish(result);
                Step::Done
            }
            Advanced::Parked(stage) => Step::Parked(Box::new(Foreground {
                engine: weak,
                stage,
                finish,
            })),
        }
    }

    /// Settled without running to its end: the database closed, or the
    /// queue that would have run it was dropped. Dropping the stage gives
    /// back what it holds: an ingest's staged files, the job's place at the
    /// gate, and the gate itself if it held it or was handed it meanwhile.
    /// An ingested table whose unit is already in the ring is seen through
    /// first: whichever thread reaches a unit runs it, and the table must
    /// not go in after its caller was told the ingest failed. When it was
    /// the last table, the ingest is complete, and the ticket says so.
    fn release(mut self: Box<Self>) {
        let complete = match (&mut self.stage, self.engine.upgrade()) {
            (Stage::Under(Under::Installing(install), _), Some(engine)) => {
                engine.ingest_install_settle(install)
            }
            _ => false,
        };
        let closed = self
            .engine
            .upgrade()
            .is_none_or(|engine| engine.is_closed());
        let result = if complete {
            Ok(())
        } else if closed {
            Err(Error::Closed)
        } else {
            Err(Error::invalid_argument(
                "the I/O queue that would have run this job was dropped before it ran; call again",
            ))
        };
        let held_a_place = matches!(self.stage, Stage::Admitting(..) | Stage::Under(..));
        drop(self.stage);
        // The gate, handed to the job while it was parked, kept stopped
        // writers' steps contended: with it given back they look again
        // (`StallSignal`, relief handed back).
        if held_a_place && let Some(engine) = self.engine.upgrade() {
            engine.refresh_stall_level();
        }
        self.finish.finish(result);
    }
}

impl RegolithEngine {
    /// Start `work` without waiting for it (see the module docs), and return
    /// its ticket.
    pub(crate) fn start_foreground(&self, work: Work) -> JobTicket {
        let completion = JobTicket::completion(self.me.clone());
        let ticket = JobTicket::new(Arc::clone(&completion));
        if let Err(err) = self.ensure_writable() {
            completion.decide(Err(err.into()));
            completion.deliver_decided();
            return ticket;
        }
        let queue = self.current_queue();
        if self.has_worker {
            let body = Box::new(Foreground {
                engine: self.me.clone(),
                stage: Stage::Fresh(work),
                finish: Finish {
                    completion: Arc::clone(&completion),
                    tell: queue.map_or(Tell::Here, Tell::Queue),
                },
            });
            if let Err(body) = self.compaction.submit(body) {
                // The workers stopped: the database is closing.
                body.release();
            }
            return ticket;
        }
        let Some(queue) = queue else {
            // No worker and no queue: the caller runs it, as a blocking call
            // does its own I/O.
            completion.decide(self.run_foreground(Stage::Fresh(work)));
            completion.deliver_decided();
            return ticket;
        };
        let job = Job::new(Box::new(Foreground {
            engine: self.me.clone(),
            stage: Stage::Fresh(work),
            finish: Finish {
                completion: Arc::clone(&completion),
                tell: Tell::AsWaiter,
            },
        }));
        let waiter = Arc::clone(&completion) as Arc<dyn Delivery>;
        match self
            .io()
            .submit(&queue, Arc::clone(&job), Some(Arc::clone(&waiter)))
        {
            Ok(()) => JobTicket::of_job(completion, job),
            Err(job) => {
                // Not left on the queue (closing, or the queue went away):
                // settle it here and tell the ticket.
                job.release();
                waiter.deliver();
                ticket
            }
        }
    }

    /// Do a job from `stage` to its end on this thread, waiting here for the
    /// gate and for whatever else it needs: the worker, a blocking caller,
    /// or the call itself.
    fn run_foreground(&self, stage: Stage) -> Result<(), Error> {
        match self.advance_foreground(stage, Help::Wait) {
            Advanced::Done(result) => result,
            // A call that waits is never parked.
            Advanced::Parked(_) => Err(Error::Io(io::Error::other(
                "a background job stopped before its end on a call that waits for it",
            ))),
        }
    }

    /// Take a job from `stage` as far as it goes: to its end, or, for a
    /// queue's poll, to the first thing another thread holds, where it
    /// parks with its waker registered. The one path every way of running a
    /// job takes, so a poll and a blocking caller do the same work in the
    /// same order.
    fn advance_foreground(&self, mut stage: Stage, help: Help<'_>) -> Advanced {
        loop {
            stage = match stage {
                Stage::Fresh(work) => match self.before_gate(work, help) {
                    Ok(Before::Nothing) => return Advanced::Done(Ok(())),
                    Ok(Before::Gate(gated)) => {
                        Stage::Admitting(gated, self.compaction_gate.admit())
                    }
                    Ok(Before::Drain { target, drain }) => Stage::Draining { target, drain },
                    Ok(Before::Parked(work)) => return Advanced::Parked(Stage::Fresh(work)),
                    Err(err) => return Advanced::Done(Err(err)),
                },
                Stage::Draining { target, mut drain } => {
                    match self.drain_advance(&mut drain, help) {
                        Ok(true) => Stage::Admitting(
                            Gated::Checkpoint { target },
                            self.compaction_gate.admit(),
                        ),
                        Ok(false) => return Advanced::Parked(Stage::Draining { target, drain }),
                        Err(err) => return Advanced::Done(Err(err.into())),
                    }
                }
                Stage::Admitting(gated, mut admission) => {
                    let gate = match help {
                        Help::Park(waker) => match admission.poll(waker) {
                            Poll::Ready(gate) => gate,
                            Poll::Pending => {
                                return Advanced::Parked(Stage::Admitting(gated, admission));
                            }
                        },
                        Help::Wait | Help::Leave => match admission.wait() {
                            Ok(gate) => gate,
                            Err(err) => return Advanced::Done(Err(err.into())),
                        },
                    };
                    match self.begin_under(gated) {
                        Ok(under) => Stage::Under(under, gate),
                        Err(err) => {
                            drop(gate);
                            self.relieve_after_gate(help);
                            return Advanced::Done(Err(err));
                        }
                    }
                }
                Stage::Under(under, gate) => match self.under_gate(under, gate, help) {
                    Ok(Some((under, gate))) => {
                        return Advanced::Parked(Stage::Under(under, gate));
                    }
                    // The gate was released on the way out.
                    Ok(None) => {
                        self.relieve_after_gate(help);
                        return Advanced::Done(Ok(()));
                    }
                    Err(err) => {
                        self.relieve_after_gate(help);
                        return Advanced::Done(Err(err));
                    }
                },
            };
        }
    }

    /// A job's part before the gate. A panic in code run for the caller (an
    /// ingest's key check) fails the job alone.
    fn before_gate(&self, work: Work, help: Help<'_>) -> Result<Before, Error> {
        contained(|| match work {
            Work::CompactRange { lower, upper } => {
                self.ensure_writable()?;
                Ok(Before::Gate(Gated::CompactRange { lower, upper }))
            }
            Work::Ingest {
                files,
                options,
                mut check,
            } => {
                let Some(staged) = self.ingest_stage(&files, &options, &mut check, help)? else {
                    return Ok(Before::Parked(Work::Ingest {
                        files,
                        options,
                        check,
                    }));
                };
                Ok(if staged.is_empty() {
                    Before::Nothing
                } else {
                    Before::Gate(Gated::Ingest { staged, options })
                })
            }
            Work::Checkpoint { target } => {
                crate::checkpoint::prepare_target(self, &target)?;
                let drain = self.checkpoint_drain_start()?;
                Ok(Before::Drain { target, drain })
            }
        })
    }

    /// What a job that was just given the gate has to do under it.
    fn begin_under(&self, gated: Gated) -> Result<Under, Error> {
        Ok(match gated {
            Gated::CompactRange { lower, upper } => match self.compact_range_drain()? {
                Some(drain) => Under::Rotating {
                    lower,
                    upper,
                    drain,
                },
                None => Under::Compacting { lower, upper },
            },
            Gated::Ingest { staged, options } => Under::Installing(staged.into_install(options)),
            Gated::Checkpoint { target } => Under::Capturing { target },
        })
    }

    /// A job's part holding the gate, as far as it goes. `Ok(None)` when it
    /// is done, the gate released. `Ok(Some(..))` when a queue's poll
    /// parked: what is left, and the gate, which the parked job keeps (the
    /// module docs say why that holds nobody up). An error releases the
    /// gate too.
    ///
    /// A panic in code the job runs for the caller (a compaction filter, a
    /// merge operator, a listener) fails the job alone. Once the gate is
    /// free again its caller relieves the stop it kept writers from
    /// relieving ([`Self::relieve_after_gate`]).
    fn under_gate(
        &self,
        mut under: Under,
        gate: Exclusive,
        help: Help<'_>,
    ) -> Result<Option<(Under, Exclusive)>, Error> {
        // The closure owns the gate: it is released when the closure
        // returns without handing it back, or unwinds.
        contained(move || {
            loop {
                under = match under {
                    Under::Rotating {
                        lower,
                        upper,
                        mut drain,
                    } => {
                        let drained = self.drain_advance(&mut drain, help);
                        if matches!(drained, Ok(false)) {
                            let parked = Under::Rotating {
                                lower,
                                upper,
                                drain,
                            };
                            return Ok(Some((parked, gate)));
                        }
                        // Even when a flush failed: the memtable it left
                        // frozen counts toward the stall thresholds, and
                        // skipping this would let writers keep sealing
                        // memtables, without back-pressure, for as long as
                        // flushes keep failing.
                        if drain.has_targets() {
                            self.refresh_stall_level();
                        }
                        drained?;
                        Under::Compacting { lower, upper }
                    }
                    Under::Compacting { lower, upper } => {
                        // Every pass hands sections to the version store's
                        // step: a poll holds it for the passes, or parks.
                        let Some(kept) = self.keep(Needs::VERSIONS, help) else {
                            return Ok(Some((Under::Compacting { lower, upper }, gate)));
                        };
                        let compacted = self.compact_range_levels(Some(&lower), Some(&upper));
                        self.unkeep(kept);
                        compacted?;
                        return Ok(None);
                    }
                    Under::Installing(mut install) => {
                        if !self.ingest_install_advance(&mut install, help)? {
                            return Ok(Some((Under::Installing(install), gate)));
                        }
                        return Ok(None);
                    }
                    Under::Capturing { target } => {
                        // The capture is one section of the step, which a
                        // poll holds for it and gives back before the links.
                        let Some(kept) = self.keep(Needs::VERSIONS, help) else {
                            return Ok(Some((Under::Capturing { target }, gate)));
                        };
                        crate::checkpoint::capture_into(self, gate, &target, || {
                            self.unkeep(kept);
                        })?;
                        return Ok(None);
                    }
                };
            }
        })
    }

    /// Relieve a stop that a sole hold of the compaction gate, or a wait
    /// owed it, kept writers from relieving. Called once that hold or wait
    /// is over.
    ///
    /// On a database with no worker and inline compaction on, a stopped
    /// writer relieves the stop with compaction steps on its own thread. A
    /// step that finds the gate held alone, or owed to a waiter, is
    /// contended, and the writer waits for the thread in its way to relieve
    /// the stop. A foreground job holds the gate for its gated part, a job
    /// parked on a queue is owed it from the handoff to its owner's next
    /// poll, and an ingest or a checkpoint only adds L0 tables, so here that
    /// thread is this one: once the gate is released it runs the same
    /// bounded steps a stopped writer would. A stop that stays (nothing left
    /// to compact, the bound reached, a step failing) lands the waiters'
    /// unit anyway, as every piece of progress does here (`StallSignal`,
    /// relief handed back), so each writer looks again and relieves it or
    /// reports why it cannot, instead of waiting for a relief nobody is
    /// running. A step contended by another thread's work leaves the stop
    /// to that thread, which hands it back the same way.
    /// `proofs/tla/StallRelief.tla` checks that no stopped writer is left
    /// waiting once nobody is relieving.
    ///
    /// A queue's poll runs these steps too, and one that finds the version
    /// store's step held by another thread does not wait for it: the
    /// relief stops there, and the unit is landed as for a stop that stays.
    pub(super) fn relieve_after_gate(&self, help: Help<'_>) {
        if self.has_worker || !self.inline_compaction {
            return;
        }
        let mut passes = 0usize;
        let mut idle = 0usize;
        while matches!(self.stall_state(), Some((_, true))) {
            match self.run_one_background_step(help) {
                // The poll would have parked: the job it runs is over, so
                // the writers look again and relieve the stop themselves.
                Ok(None) => break,
                Ok(Some(super::compaction::CompactionOutcome::DidWork)) => {
                    idle = 0;
                    passes += 1;
                    if passes > Self::MAX_INLINE_PASSES {
                        break;
                    }
                }
                Ok(Some(super::compaction::CompactionOutcome::Idle)) => {
                    idle += 1;
                    if idle > Self::MAX_IDLE_PASSES {
                        break;
                    }
                }
                Ok(Some(super::compaction::CompactionOutcome::Contended)) => return,
                Err(err) => {
                    tracing::debug!(error = %err, "a step relieving a stop after a job failed");
                    break;
                }
            }
        }
        // Lands the unit, the stop cleared or not.
        self.refresh_stall_level();
    }
}

#[cfg(test)]
#[path = "foreground_tests.rs"]
mod tests;

/// Run one part of a job, turning a panic in it into the job's failure.
fn contained<T>(part: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(part)).unwrap_or_else(|_| {
        tracing::error!("a background job panicked");
        Err(Error::Io(io::Error::other(
            "the job panicked in code it ran on the caller's behalf, such as a compaction filter or a listener",
        )))
    })
}
