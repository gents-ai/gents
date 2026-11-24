//! The transactions still open that have something to run when the database
//! closes: the ones with an `on_abort` callback, and every one under a
//! database with transaction hooks.
//!
//! `close` aborts each of them (see `transaction::claim`), so a callback
//! never waits on a caller that will not return. Nothing is listed for a
//! transaction without callbacks or hooks, and the table is built by the
//! first one that is, so a database that uses neither pays nothing. Listing
//! one is a slot taken and given back ([`claim_slots`]).
//!
//! A pessimistic database also lists its lock table here: `close` releases
//! the key locks of the transactions it aborted and wakes every transaction
//! waiting for a lock, which then fails with `Closed`.

mod claim_slots;
#[cfg(loom)]
pub mod loom_model;

use std::sync::{Arc, OnceLock};

use super::RegolithEngine;
use crate::transaction::{Claim, LockTable};
use claim_slots::ClaimSlots;

pub(crate) struct OpenTransactions {
    open: OnceLock<ClaimSlots<Claim>>,
    /// The pessimistic lock table of the database, if it has one.
    locks: OnceLock<Arc<LockTable>>,
}

impl OpenTransactions {
    pub(crate) fn new() -> Self {
        Self {
            open: OnceLock::new(),
            locks: OnceLock::new(),
        }
    }

    /// A slot to list a claim in, with [`OpenTransactions::insert`]; see
    /// [`ClaimSlots::reserve`].
    pub(crate) fn reserve(&self) -> Option<u32> {
        self.open.get_or_init(ClaimSlots::new).reserve()
    }

    /// List `claim` in the `slot` reserved for it.
    pub(crate) fn insert(&self, slot: u32, claim: &Arc<Claim>) {
        self.open.get_or_init(ClaimSlots::new).fill(slot, claim);
    }

    /// End the listing in `slot` and give the slot back.
    pub(crate) fn remove(&self, slot: u32) {
        if let Some(open) = self.open.get() {
            open.remove(slot);
        }
    }

    /// List the database's pessimistic lock table, for `close` to settle.
    pub(crate) fn track_locks(&self, locks: Arc<LockTable>) {
        let _ = self.locks.set(locks);
    }

    /// Abort every transaction still open, on the calling thread. One that
    /// began to commit first is not touched, and one listed while this runs
    /// may be missed: its owner ends it, with the same outcome. Then the
    /// lock table: the aborted transactions' key locks are released, and
    /// every transaction waiting for a lock is woken to find the database
    /// closed.
    pub(crate) fn abort_all(&self, engine: &RegolithEngine) {
        let mut aborted = Vec::new();
        if let Some(open) = self.open.get() {
            open.drain(|claim| {
                if claim.abort_for_close(engine)
                    && let Some(owner) = claim.lock_owner()
                {
                    aborted.push(Arc::clone(owner));
                }
            });
        }
        if let Some(locks) = self.locks.get() {
            locks.close(&aborted);
        }
    }

    /// A close that failed and left the database open: lock waits queue
    /// again.
    pub(crate) fn reopen_locks(&self) {
        if let Some(locks) = self.locks.get() {
            locks.reopen();
        }
    }
}
