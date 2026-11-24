//! P6, a blocking call far from its turn: who sleeps, which slots the
//! decided frontier names as near, and which of those a finished commit has
//! told (`pipeline.rs`, `RegolithEngine::park_far` and `wake_near`).
//!
//! A blocking call whose slot is more than [`Turns::lead`] slots past the
//! next slot to decide, and that finds that slot being decided by another
//! thread, parks on its own slot instead of yielding to that thread over
//! and over. It counts itself in [`Turns::far`] first. A thread that passes
//! the frontier over slot `i` while anybody is counted names every slot up
//! to `i + lead` ([`Turns::name`]), with no host call on the frontier's
//! path; the next blocking call whose commit is done claims the slots named
//! since the last claim ([`Turns::claim_told`]) and marks each `NEAR`, which
//! wakes its owner.
//!
//! The invariants:
//!
//! - **T1, nobody sleeps through its turn unnamed.** The sleeper counts in
//!   and then looks at the frontier; the passer passes and then looks at the
//!   count, each behind a sequentially consistent handshake. One of the two
//!   sees the other: the sleeper sees its turn near and does not sleep, or
//!   the passer names its slot.
//! - **T2, each named slot is claimed once.** `woken_to` moves only by a
//!   compare-and-swap from what the claimer read, so two finished commits
//!   never tell the same slots, and none is skipped: the claimed range
//!   starts one past the last claimed.
//! - **T3, a sleeper always wakes.** Named and told, its slot landing or
//!   failing (`Descriptor::land`, `FAILED`), or [`TURN_PATIENCE`] passed.
//!
//! TLA+: `TurnPark`, `TurnWake`, `WakeNear` and `Named` in
//! `CommitPipelineSteps.tla` and `CommitPipelineBase.tla`; Lean:
//! `a_turn_park_is_bounded` in `PipelineHelping.lean`; loom:
//! `loom_turn_wait.rs`.

use super::descriptor::{Descriptor, FAILED, NEAR};
use super::write_unit::Padded;
use crate::sync::internal::{AtomicU64, AtomicUsize, Ordering, seq_cst_handshake};

/// How many slots before its own a call parked far from its turn is woken
/// at the least: enough for it to be running again before its turn comes
/// on a quiet host.
pub(super) const LEAD: u64 = 2;
/// Calls asleep far from their turn for each slot more of lead.
pub(super) const LEAD_PER_SLEEPER: u64 = 4;
/// How long a call parked far from its turn waits to be woken before it
/// wakes by itself and helps.
pub(super) const TURN_PATIENCE: std::time::Duration = std::time::Duration::from_micros(200);

/// The calls asleep far from their turn and the slots named and told, each
/// on a cache line of its own.
pub(crate) struct Turns {
    /// How many calls are counted in to sleep far from their turn now.
    pub(super) far: Padded<AtomicUsize>,
    /// Every slot up to this one has its turn near.
    pub(super) near_to: Padded<AtomicU64>,
    /// Every slot up to this one was claimed by a finished commit to tell.
    woken_to: Padded<AtomicU64>,
}

impl Turns {
    /// Nobody asleep, nothing named.
    pub(super) fn new() -> Self {
        Self {
            far: Padded(AtomicUsize::new(0)),
            near_to: Padded(AtomicU64::new(0)),
            woken_to: Padded(AtomicU64::new(0)),
        }
    }

    /// How many slots before its turn a call parked far from it is woken
    /// now: [`LEAD`], and one more for every [`LEAD_PER_SLEEPER`] calls
    /// asleep, since the more threads wait, the longer a woken one takes to
    /// run again.
    pub(super) fn lead(&self) -> u64 {
        LEAD + self.far.0.load(Ordering::Relaxed) as u64 / LEAD_PER_SLEEPER
    }

    /// The decided frontier reached `index`: while anybody is counted in,
    /// name every slot up to [`Self::lead`] past it (T1).
    pub(super) fn name(&self, index: u64) {
        // Against the sleeper's count in and its look at the frontier, both
        // after its count: one of the two sees the other.
        seq_cst_handshake();
        if self.far.0.load(Ordering::SeqCst) != 0 {
            self.near_to
                .0
                .fetch_max(index + self.lead(), Ordering::SeqCst);
        }
    }

    /// Claim the slots named since the last claim, for this call to tell:
    /// `(from, to]`, or `None` when there are none (T2).
    pub(super) fn claim_told(&self) -> Option<(u64, u64)> {
        let to = self.near_to.0.load(Ordering::SeqCst);
        let mut from = self.woken_to.0.load(Ordering::Acquire);
        while from < to {
            match self.woken_to.0.compare_exchange_weak(
                from,
                to,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some((from, to)),
                Err(now) => from = now,
            }
        }
        None
    }

    /// Count a call in to sleep far from its turn; it looks at the frontier
    /// only after this (T1).
    pub(super) fn count_in(&self) {
        self.far.0.fetch_add(1, Ordering::SeqCst);
        seq_cst_handshake();
    }

    /// Count a call out once it is awake.
    pub(super) fn count_out(&self) {
        self.far.0.fetch_sub(1, Ordering::SeqCst);
    }

    /// Whether the call owning `own`, installed, may stop sleeping: told
    /// its turn is near, its slot failed, or its slot named or within the
    /// lead of the decided frontier `decided` (the look after it registered
    /// on its slot, T1 and T3).
    pub(super) fn near(&self, own: &Descriptor, decided: u64) -> bool {
        own.is(NEAR)
            || own.is(FAILED)
            || own.index() <= self.near_to.0.load(Ordering::SeqCst)
            || own.index() <= decided + self.lead()
    }
}
