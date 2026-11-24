//! The round: the one claim on the log's device step (plan 4.7, D53).
//!
//! Syncing the log and rolling a failed log back are done by one thread at
//! a time, and by whichever thread claims the round, a job, with one
//! compare-and-swap:
//!
//! - **A sync round** writes what is decided and not written, syncs the
//!   written prefix of the log and raises the durable offset over it. Every
//!   commit that waits for durability waits on the round: a blocking call
//!   claims it or waits for the thread that did, a `commit_nowait` commit
//!   leaves it on its queue, whose poll claims it when nobody has. Nobody
//!   waits for one particular thread to sync. A poll that claimed the
//!   round runs it without waiting for another thread: its one wait, for
//!   the writes in flight on a log it must roll back, parks the round
//!   instead (`park.rs`).
//! - **A rollback** is what a round does when the log has failed
//!   (`ordered.rs`), so no sync runs while commits are being discarded.
//! - **A hold** is the round claimed by an ordered unit for the length of
//!   its body ([`RegolithEngine::hold_round`]): the body syncs the log
//!   itself, and no round of another thread runs under it.
//!
//! One round at a time because a sync error is reported to one caller of
//! one file handle and then cleared by the host: two syncs overlapping on
//! the handle could tell the later caller that the bytes of a failed one
//! were safe (`engine::wal_log`). Each round installs the next before it
//! lands, so a thread woken by the landing finds a round it can claim.

use std::sync::Arc;
use std::task::Waker;

use super::super::RegolithEngine;
use super::super::callback::InCommit;
use super::super::io::job::{Job, JobBody, Step};
use super::super::wal_log::Log;
use super::descriptor::{Decision, WRITTEN};
use super::hook;
use super::park::Help;
use super::pipeline::Pipeline;
use super::ring::{Linked, Loaded};
use super::spare;
use super::write_unit::Await;
use crate::perf_context::{PerfTimer, PerfTimerField};
use crate::statistics::{Histogram, Ticker};
use crate::sync::internal::{Arc as RingArc, AtomicPtr, AtomicU64, Ordering};

/// One round of the log's device step: the unit whoever needs the log
/// durable, or a failed log rolled back, claims.
pub(super) struct Round {
    pub(super) job: Arc<Job>,
    /// Set by a thread that claimed the round to do the device step itself
    /// (`RegolithEngine::hold_round`): the job's body then does nothing.
    held: AtomicU64,
    link: AtomicPtr<Round>,
}

impl Round {
    /// A round nobody claimed yet. Its allocations are the ones this thread
    /// kept from a round that is over, when it has them (`spare.rs`): a
    /// commit that waits for its sync allocates nothing for the round.
    pub(super) fn new(engine: std::sync::Weak<RegolithEngine>) -> RingArc<Self> {
        spare::round(SyncRound::boxed(engine))
    }

    /// A round whose job is `job`, before it is shared.
    pub(super) fn fresh(job: Arc<Job>) -> Self {
        Self {
            job,
            held: AtomicU64::new(0),
            link: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    /// The job of a round that is over, for its allocation. A loom build
    /// keeps none (`spare.rs`).
    #[cfg(not(loom))]
    pub(super) fn into_job(self) -> Arc<Job> {
        self.job
    }
}

/// A round held by [`RegolithEngine::hold_round`]: landed when dropped, on
/// return or on unwind, so nobody is left waiting on a claim nobody will
/// complete.
pub(super) struct HeldRound<'a> {
    engine: &'a RegolithEngine,
    round: RingArc<Round>,
}

impl Drop for HeldRound<'_> {
    fn drop(&mut self) {
        self.engine.run_job(&self.round.job);
    }
}

impl Linked for Round {
    fn link(&self) -> &AtomicPtr<Self> {
        &self.link
    }

    fn retired(value: RingArc<Self>) {
        spare::keep_round(value);
    }
}

impl RegolithEngine {
    /// The round to claim or wait on now. A round whose body ended without
    /// installing the next one (it unwound) is replaced here.
    pub(super) fn current_round(&self, pipe: &Pipeline) -> RingArc<Round> {
        loop {
            let round = pipe.round.load();
            if !round.job.is_done() {
                return round;
            }
            pipe.round.replace(&round, Round::new(self.me.clone()));
        }
    }

    /// Bring the log durable through what is written now, or a failed log
    /// rolled back: get past the current round as `help` says (`park.rs`).
    /// `Err` is the round, for a call that did not get past it.
    pub(super) fn sync_round(&self, pipe: &Pipeline, help: Help<'_>) -> Result<(), Arc<Job>> {
        let round = self.current_round(pipe);
        if help.runs() {
            hook::at(hook::Stage::Sync);
        }
        if !self.pass(&round.job, help) {
            return Err(Arc::clone(&round.job));
        }
        // The round is over and replaced: this may be its last reference.
        spare::keep_round(round);
        Ok(())
    }

    /// Claim the round and hold it until the returned guard drops. The
    /// holder does the log's device step itself: no sync and no rollback of
    /// another thread runs meanwhile. What an ordered unit's body runs under
    /// (`ordered.rs`).
    ///
    /// A blocking call waits out a round another thread has under way and
    /// always gets `Some`. `None` is for a call that may not wait: a poll's
    /// waker is then woken when that thread lets the round go.
    pub(super) fn hold_round<'a>(
        &'a self,
        pipe: &Pipeline,
        help: Help<'_>,
    ) -> Option<HeldRound<'a>> {
        loop {
            let round = self.current_round(pipe);
            if round.job.claim() {
                round.held.store(1, Ordering::SeqCst);
                return Some(HeldRound {
                    engine: self,
                    round,
                });
            }
            match help {
                Help::Wait => {
                    round.job.wait_turn();
                }
                Help::Leave => return None,
                Help::Park(waker) => {
                    if round.job.watch(waker) {
                        return None;
                    }
                }
            }
        }
    }

    /// One round, run by whichever thread claimed it: write what is decided
    /// and not written, sync the written prefix of the log, raise the
    /// durable offset over it, and publish what that made publishable. A
    /// log that failed, in this round or before it, is rolled back here
    /// (P4).
    ///
    /// `false` when a poll's round parked: writes are still in flight below
    /// the prefix it syncs, or the log failed and writes of the commits to
    /// discard are. The round is then as it was, for the next thread that
    /// claims it.
    fn run_sync_round(&self, pipe: &Pipeline, round: &RingArc<Round>, help: Help<'_>) -> bool {
        let tip = pipe.tip.load();
        if !self.sync_written(pipe, &tip, help) {
            return false;
        }
        if tip.after.log.is_poisoned() {
            if !self.turned_away(&tip, help) {
                return false;
            }
            self.roll_back(pipe);
        }
        // The next round is in place before this one lands, so a thread
        // woken by the landing finds a round it can claim.
        pipe.round.replace(round, Round::new(self.me.clone()));
        // The slots this round made durable, and the ones behind them that
        // waited only for those: bounded by what was decided when it began.
        let _ = self.publish_upto(pipe, tip.index, None, Await::Copy);
        true
    }

    /// Before a rollback of the failed log `tip` names, by the thread that
    /// holds the round: `false` when the caller is a poll and writes are
    /// still in flight on the log, which a rollback waits out. The poll's
    /// waker is then woken by the last of them, and the log already turns
    /// every later write of a commit to discard away. A blocking caller
    /// waits for them inside the rollback.
    pub(super) fn turned_away(&self, tip: &Decision, help: Help<'_>) -> bool {
        match help {
            // The epoch the rollback moves the pipeline to (`ordered.rs`):
            // only a rollback changes it, and this thread holds the round.
            Help::Park(waker) => tip.after.log.turn_away(tip.after.epoch + 1, waker),
            Help::Wait | Help::Leave => true,
        }
    }

    /// The sync of one round: the written prefix of the log `tip` names,
    /// unless the log already failed. `false` when a poll's round parked on
    /// writes in flight below the prefix (WG4).
    fn sync_written(&self, pipe: &Pipeline, tip: &Decision, help: Help<'_>) -> bool {
        let log = &tip.after.log;
        if log.is_poisoned() {
            return true;
        }
        // The written prefix ends where the first unwritten record starts,
        // or after the newest decided slot when every record is written. The
        // holder of the round writes what is not written, through the write
        // unit or as a copy, and waits for no other thread
        // (`write_unit.rs`, `Await::Copy`).
        let mut through = tip.after.next_off;
        let first = pipe.ring.published() + 1;
        for index in first..=tip.index {
            let Loaded::Ready(desc) = pipe.ring.load(index) else {
                continue;
            };
            let Some(decision) = desc.decision() else {
                continue;
            };
            let Some(place) = &decision.place else {
                continue;
            };
            if !Arc::ptr_eq(&decision.after.log, log) {
                continue;
            }
            let _ = self.bring_written(pipe, &desc, Await::Copy);
            if !desc.is(WRITTEN) {
                through = place.offset;
                break;
            }
        }
        if through > log.durable() && !log.is_poisoned() {
            let Some(through) = self.settle_writes(log, through, help) else {
                return false;
            };
            if through <= log.durable() || log.is_poisoned() {
                return true;
            }
            let _perf = PerfTimer::new(PerfTimerField::WriteWal);
            let start = self.timer_start();
            match log.sync_through(through) {
                Ok(()) => {
                    if let Some(s) = self.statistics() {
                        s.add(Ticker::WalSyncCount, 1);
                        if let Some(micros) = self.elapsed_micros(start) {
                            s.record(Histogram::WalWriteTime, micros);
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(error = %err, "syncing the write-ahead log failed");
                    log.poison(err);
                }
            }
        }
        true
    }

    /// Wait, as `help` says, until no write in flight may land below
    /// `through` on `log`, and return the offset a sync may cover now
    /// (`wal_gate.rs`, WG4 and WG5). `None` when a poll parked: its waker is
    /// woken by the last of those writes to leave. Called by the holder of
    /// the round.
    pub(super) fn settle_writes(&self, log: &Log, through: u64, help: Help<'_>) -> Option<u64> {
        match help {
            Help::Park(waker) => log.settle_or_park(through, waker),
            Help::Wait | Help::Leave => match log.settle(through) {
                Ok(upto) => Some(upto),
                // Only where one thread cannot wait for another, and there no
                // other thread has a write in flight: the log is failed rather
                // than synced over a write nobody can account for.
                Err(err) => {
                    tracing::error!(error = %err, "a sync of the write-ahead log could not wait for the writes in flight");
                    log.poison(err);
                    Some(log.durable())
                }
            },
        }
    }
}

/// The body of a sync round's job.
struct SyncRound {
    engine: std::sync::Weak<RegolithEngine>,
}

#[cfg(not(loom))]
std::thread_local! {
    /// The box of the body this thread ran last, kept for the next round it
    /// makes (`spare.rs`, S3). It names no engine while it waits here.
    static BODY: std::cell::Cell<Option<Box<SyncRound>>> = const { std::cell::Cell::new(None) };
}

impl SyncRound {
    /// The body of a round of `engine`'s, in the box this thread kept when
    /// it has one.
    fn boxed(engine: std::sync::Weak<RegolithEngine>) -> Box<dyn JobBody> {
        #[cfg(not(loom))]
        if let Ok(Some(mut body)) = BODY.try_with(std::cell::Cell::take) {
            body.engine = engine;
            return body;
        }
        Box::new(Self { engine })
    }

    /// Run the round, keeping the body's box for the round this run makes.
    /// `Err` hands the body back when a poll's round parked.
    fn sync(mut self: Box<Self>, help: Help<'_>) -> Result<(), Box<dyn JobBody>> {
        let weak = std::mem::take(&mut self.engine);
        #[cfg(not(loom))]
        let _ = BODY.try_with(|kept| kept.set(Some(self)));
        let Some(engine) = weak.upgrade() else {
            return Ok(());
        };
        let Some(pipe) = engine.pipeline.as_ref() else {
            return Ok(());
        };
        let _commit = InCommit::enter();
        // The round this job belongs to: the current one, since a round is
        // replaced only by its own body.
        let round = pipe.round.load();
        if round.held.load(Ordering::SeqCst) != 0 {
            // The thread that held the round did the device step itself.
            pipe.round.replace(&round, Round::new(engine.me.clone()));
            return Ok(());
        }
        if engine.run_sync_round(pipe, &round, help) {
            return Ok(());
        }
        Err(Self::boxed(weak))
    }
}

impl JobBody for SyncRound {
    fn run(self: Box<Self>) {
        let _ = self.sync(Help::Wait);
    }

    /// On a queue's poll: parks instead of waiting for another thread.
    fn step(self: Box<Self>, waker: &Waker) -> Step {
        match self.sync(Help::Park(waker)) {
            Ok(()) => Step::Done,
            Err(body) => Step::Parked(body),
        }
    }

    /// A sync commits wait on is never left undone: a release runs it too.
    fn release(self: Box<Self>) {
        let _ = self.sync(Help::Wait);
    }
}
