//! What a call does about work in its way: wait for it, leave it, or park
//! (plan 4.10, D53).
//!
//! The pipeline hands nobody a turn. A call that finds a round or a unit in
//! its way runs it, or deals with the thread that already does, and what it
//! does then depends only on who is calling ([`Help`]):
//!
//! - **A blocking call waits.** It runs a unit nobody has, and waits for one
//!   another thread claimed, which is device I/O under way.
//! - **A `commit_nowait` call leaves.** It runs no unit and waits for none:
//!   the unit in its way goes on its queue and the call returns.
//! - **A queue's poll parks.** It runs what nobody has, as far as that goes
//!   without waiting. For anything another thread has, it registers its
//!   waker where that thread's work ends, gives back what it took, and
//!   parks its job (`engine::io::job`), so the poll goes on with the rest of
//!   its queue and never waits for another thread.
//!
//! # What a poll parks for
//!
//! Everything a job of the pipeline could wait for on another thread, and
//! where its waker goes instead:
//!
//! | in the way | the job is woken when | registered by |
//! |---|---|---|
//! | a round, a unit, a rotation check or a slot's decision another thread claimed | it lands, or that thread parks it | [`Job::watch`] |
//! | the version store's step, held by another thread | the step is given up | `Step::hold_or_park` |
//! | the flush exclusion, held by another thread | it is released | `Flusher::hold_or_park` |
//! | writes in flight on a log being rolled back | the last of them left | `Log::turn_away` |
//!
//! The first is met while helping, where every step is one any thread may
//! repeat: a job that parks there has changed nothing, and starts over when
//! it is woken. The last is met by a rollback before it has frozen anything.
//!
//! The other two are met inside a unit's body, which runs to its end once
//! begun. So the poll takes them *before* the body begins: a unit says what
//! its body may wait for ([`Needs`]), the poll takes exactly that, or parks,
//! and the body then finds it held by its own thread.
//!
//! A job that is no unit takes them the same way ([`RegolithEngine::keep`])
//! before each stretch of its work that flushes or changes the version set:
//! an owed background step, a stall's relief, and the parts of a foreground
//! job between its units (a drain's flushes, `compact_range`'s passes, an
//! ingest's file ids, a checkpoint's capture). A foreground job enters each
//! of its ordered units as any call does ([`Help`]), so a poll that runs it
//! parks at the unit's entry too (`engine::foreground`, "Where a poll
//! parks").
//!
//! # What still waits, and for whom
//!
//! A unit holds the version store's step and the flush exclusion for the
//! length of its body: a poll that needs either parks until the body ends,
//! and a blocking call waits for it.
//!
//! # Invariants
//!
//! - **K1, a parked job holds nothing.** Before it parks it lands the round
//!   it held and gives up the step and the exclusion it took, and nothing
//!   of a unit's body has run. Any thread may claim it and go on.
//! - **K2, a parked job is woken.** Every registration is followed by a
//!   second look at what it waits for, with a fence between that the side
//!   ending the wait matches, so one of the two sees the other.
//! - **K3, nothing a poll steps waits for another thread.** What a unit's
//!   body said it needs is held by its thread for the length of the body,
//!   and a job that is no unit keeps what a stretch of its work needs. Every
//!   blocking wait of the engine notes itself (`io::scope::waits`: the
//!   round, a unit, a commit that blocks, the version store's step, the
//!   flush exclusion, the compaction gate, a log's writes in flight), and a
//!   poll that reaches one fails a build with debug assertions.
//! - **K4, whoever waits on a parked job can run it.** A thread blocked on
//!   the job is woken when it parks and claims it; every queue registered
//!   on it is told when its wait ends, and never before the thread that
//!   parked it has let go of it, so a queue that is told finds it free to
//!   claim (`job.rs`).

use std::sync::Arc;
use std::task::Waker;

use super::super::RegolithEngine;
use super::super::flush::Flushing;
use super::super::io::job::{self, Job};
use super::super::io::scope;
use super::super::step::Hold;

/// What a call does about work in its way that another thread has, or that
/// nobody has yet. See the module docs.
#[derive(Clone, Copy)]
pub(crate) enum Help<'a> {
    /// A blocking call: runs what nobody has, waits for what another thread
    /// runs.
    Wait,
    /// A `commit_nowait` call: runs no unit and waits for none.
    Leave,
    /// A queue's poll: runs what nobody has as far as it goes without
    /// waiting, and has this waker woken when what another thread has ends.
    Park(&'a Waker),
}

impl Help<'_> {
    /// Whether the call may run a unit it finds unclaimed.
    pub(super) fn runs(self) -> bool {
        !matches!(self, Help::Leave)
    }
}

/// What a unit's body may wait for on another thread, said when the unit is
/// made: a queue's poll takes it before the body begins (K3).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Needs {
    /// The body changes the version set: it takes a file id, installs a
    /// table, rewrites the manifest.
    versions: bool,
    /// The body flushes a memtable, or must not run beside a flush.
    flushing: bool,
}

impl Needs {
    /// The body waits for no other thread.
    pub(crate) const NONE: Self = Self {
        versions: false,
        flushing: false,
    };
    /// The body changes the version set and flushes nothing: a seal.
    pub(crate) const VERSIONS: Self = Self {
        versions: true,
        flushing: false,
    };
    /// The body flushes, which changes the version set too.
    pub(crate) const FLUSH: Self = Self {
        versions: true,
        flushing: true,
    };
}

/// What a queue's poll took for a unit's body before running it (K3).
/// Empty for a blocking caller, whose body waits for what it needs.
pub(crate) struct Kept<'a> {
    flushing: Option<Flushing<'a>>,
    versions: Option<Hold<'a>>,
}

impl Kept<'_> {
    /// Give everything back. `true` when the flush exclusion was among it:
    /// the caller then refreshes the stall level, as every flush does once
    /// its exclusion is released.
    pub(crate) fn release(self) -> bool {
        let flushed = self.flushing.is_some();
        drop(self.versions);
        drop(self.flushing);
        flushed
    }
}

impl RegolithEngine {
    /// Deal with `job`, a round or a unit in this call's way. `true` once it
    /// has landed. `false` when the call does not get past it now: a
    /// `commit_nowait` call puts it on its queue; a poll parks, having
    /// arranged to be woken (K2).
    pub(super) fn pass(&self, job: &Arc<Job>, help: Help<'_>) -> bool {
        let waker = match help {
            Help::Wait => {
                self.run_or_wait(job);
                return true;
            }
            Help::Leave => return job.is_done(),
            Help::Park(waker) => waker,
        };
        loop {
            if job.is_done() {
                return true;
            }
            if job.claim() {
                // Nobody had it: it is stepped here, inside this poll's own
                // step. If it parks, the end of its wait wakes this job and
                // tells the queues registered on that one (K4).
                return self.io().step_within(job, &job::nested_waker(job, waker));
            }
            // Another thread has it. Woken when it lands or is parked,
            // unless that happened since the claim was tried.
            if job.watch(waker) {
                return false;
            }
        }
    }

    /// Run `job` if nobody claimed it, else wait for the thread that did:
    /// what a blocking call does about a unit in its way. The wait is for
    /// device I/O under way, and ends early when that thread parks the job,
    /// which this call then claims (K4).
    pub(super) fn run_or_wait(&self, job: &Arc<Job>) {
        scope::waits("a round or a unit in the way of a call that blocks");
        loop {
            if job.claim() {
                self.run_job(job);
                return;
            }
            if job.wait_turn() {
                return;
            }
        }
    }

    /// Take what a unit's body needs before it begins (K3). `None` parks
    /// the unit: what could not be taken wakes `help`'s waker when it is
    /// free, and what was taken first is given back.
    ///
    /// Also what a job that is no unit does before a stretch of work that
    /// flushes or changes the version set, when a queue's poll runs it: a
    /// background step, a drain, a foreground job's part.
    pub(crate) fn keep(&self, needs: Needs, help: Help<'_>) -> Option<Kept<'_>> {
        let Help::Park(waker) = help else {
            // A blocking caller's body waits for what it needs.
            return Some(Kept {
                flushing: None,
                versions: None,
            });
        };
        let flushing = match needs.flushing {
            true => Some(self.flusher.hold_or_park(waker)?),
            false => None,
        };
        let versions = match needs.versions {
            true => Some(self.versions.hold_or_park(waker)?),
            false => None,
        };
        Some(Kept { flushing, versions })
    }

    /// Give back what [`Self::keep`] took. A stopped writer that found the
    /// flush exclusion held looks again once it is released, as after every
    /// flush (`StallSignal`, relief handed back).
    pub(crate) fn unkeep(&self, kept: Kept<'_>) {
        if kept.release() {
            self.refresh_stall_level();
        }
    }
}
