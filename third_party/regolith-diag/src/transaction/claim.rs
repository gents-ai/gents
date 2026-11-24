//! The single-winner claim on a transaction's abort, shared with the engine.
//!
//! A transaction with an `on_abort` callback, or under a database with
//! [`TransactionHooks`](super::TransactionHooks), has to be abortable by a
//! thread that does not own it: `close` aborts every transaction still open.
//! The owner and the closer settle who ends the transaction with one
//! compare-and-swap on the claim's state word, as the model in
//! `proofs/tla/TxnCallbacks.tla` does:
//!
//! - `OPEN -> COMMITTING` is the owner starting its commit. After it nothing
//!   else aborts the transaction, and the owner alone runs its outcome.
//! - `OPEN -> ABORTED` is whoever ends the transaction without a commit: the
//!   owner by rollback or drop, or `close`. The winner runs the abort
//!   callbacks and the database hook; a loser runs neither.
//!
//! The `on_abort` callbacks live here, in a lock-free [`FirstFour`]: the
//! owner pushes, and whoever ends the transaction takes what is in the list
//! and runs it, so each runs once however many threads reach for it. One
//! registered after the winner took the list stays in it, and the owner
//! takes it at the end of the transaction. The first four sit in the claim
//! itself, so registering them allocates nothing beyond the claim.

use std::sync::{Arc, OnceLock};

use super::TransactionHooks;
use super::callbacks::{AbortReason, survive_after};
use super::first_four::FirstFour;
use super::in_place::OnAbort;
use super::lock_table::LockOwner;
use crate::engine::RegolithEngine;
use crate::portability::{AtomicU8, Ordering};

const OPEN: u8 = 0;
const COMMITTING: u8 = 1;
const ABORTED: u8 = 2;

/// The shared half of a transaction that has something to run when it aborts.
pub(crate) struct Claim {
    /// Where the engine lists it for `close`; `None` when it is not listed.
    pub(crate) slot: Option<u32>,
    state: AtomicU8,
    on_abort: FirstFour<OnAbort>,
    /// The name a pessimistic transaction's key locks carry, once it has
    /// one: `close` releases the locks of the transactions it aborts.
    lock_owner: OnceLock<Arc<LockOwner>>,
}

impl Claim {
    /// A claim on a transaction, listed with the engine so `close` finds it.
    /// Nothing is listed once the database has begun to close: the closer has
    /// already swept, and the owner ends the transaction itself.
    ///
    /// `committing` is for a transaction whose commit has begun but which had
    /// no claim until now: the claim starts as committing and is not listed,
    /// since `close` never aborts a commit that began.
    pub(super) fn track(engine: &RegolithEngine, committing: bool) -> Arc<Self> {
        let slot = (!committing && !engine.is_closed())
            .then(|| engine.open_transactions().reserve())
            .flatten();
        let claim = Arc::new(Self {
            slot,
            state: AtomicU8::new(if committing { COMMITTING } else { OPEN }),
            on_abort: FirstFour::default(),
            lock_owner: OnceLock::new(),
        });
        if let Some(slot) = slot {
            engine.open_transactions().insert(slot, &claim);
        }
        claim
    }

    /// Record the name the transaction's key locks carry.
    pub(super) fn hold_locks_of(&self, owner: Arc<LockOwner>) {
        let _ = self.lock_owner.set(owner);
    }

    /// The name the transaction's key locks carry, if it took any.
    pub(crate) fn lock_owner(&self) -> Option<&Arc<LockOwner>> {
        self.lock_owner.get()
    }

    fn win(&self, to: u8) -> bool {
        self.state
            .compare_exchange(OPEN, to, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Claim the transaction for its commit. `false` means `close` aborted it
    /// first, and the owner's commit fails with [`crate::Error::Closed`].
    pub(super) fn begin_commit(&self) -> bool {
        self.win(COMMITTING)
    }

    /// Claim the transaction's abort. `false` means someone else already ended
    /// it.
    pub(super) fn abort(&self) -> bool {
        self.win(ABORTED)
    }

    /// [`Claim::abort`] for the owner, who also ends a transaction whose commit
    /// it claimed and then abandoned by unwinding.
    pub(super) fn owner_abort(&self) -> bool {
        match self
            .state
            .compare_exchange(OPEN, ABORTED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(state) => state == COMMITTING,
        }
    }

    pub(super) fn push(&self, f: OnAbort) {
        self.on_abort.push(f);
    }

    /// Run the `on_abort` callbacks still registered, oldest first, each at
    /// most once: this call takes each before it runs it, so no other thread
    /// has it. A panic is reported to `report`'s listeners.
    pub(super) fn run_callbacks(&self, report: Option<&RegolithEngine>, reason: &AbortReason<'_>) {
        for f in self.on_abort.take() {
            survive_after(report, "on_abort", || f.call(reason));
        }
    }

    /// What an abort owes: the transaction's `on_abort` callbacks, then the
    /// database hook's. The one function the owner, a ticket's delivery and
    /// `close` all call.
    pub(super) fn run_abort(
        &self,
        report: Option<&RegolithEngine>,
        hooks: Option<&std::sync::Arc<dyn TransactionHooks>>,
        reason: &AbortReason<'_>,
    ) {
        self.run_callbacks(report, reason);
        if let Some(hooks) = hooks {
            survive_after(report, "TransactionHooks::on_abort", || {
                hooks.on_abort(reason)
            });
        }
    }

    /// `close` ends the transaction if it is still open, on the closing
    /// thread. `true` when this call ended it.
    pub(crate) fn abort_for_close(&self, engine: &RegolithEngine) -> bool {
        let won = self.abort();
        if won {
            self.run_abort(
                Some(engine),
                engine.transaction_hooks(),
                &AbortReason::Closed,
            );
        }
        won
    }
}
