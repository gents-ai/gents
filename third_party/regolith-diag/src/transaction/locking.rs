//! A pessimistic transaction's side of the lock table (`lock_table`): taking
//! a key's lock without waiting, or waiting for it up to the database's lock
//! timeout, and letting every lock go when the transaction ends or its commit
//! lands.
//!
//! Both kinds of call share one wait per key and transaction (a
//! `PendingLock`), kept in the transaction across calls. Publishing it is
//! one linearizable step of the transaction's wait map, so two calls that
//! find the lock taken at once never both queue a wait: the one that loses
//! gives its candidate's use back before the candidate could queue, and polls
//! the published wait instead (`LockTableCalls.tla`). A
//! [`try_get_for_update`](Transaction::try_get_for_update) that returned a
//! [`LockWait`] and runs again continues the same wait, with the losses it
//! counted toward the bypass bound, and so does a blocking call on the same
//! key. Each call records how it is to be told before it polls the wait (a
//! completion on its queue, or an unpark of its thread), so a wake that comes
//! while it polls is never missed.

use std::sync::Arc;
use std::time::Duration;

use super::lock_table::{KeyLock, LandingLocks, LockTable, LockWaits, PendingLock, Polled};
use super::{Transaction, TransactionError, TxResult, strip_cf_prefix};
use crate::engine::io::shared::{QueueShared, WaitSlot};
use crate::sync::{OwnerId, TryLock};
use crate::{Error, LockWait, WouldBlock};

/// Buckets a transaction's wait map starts with, made by its first wait: a
/// transaction waits on few keys at once, and the map grows past this on
/// demand.
const LOCK_WAIT_BUCKETS: usize = 8;

#[cfg(test)]
std::thread_local! {
    /// Run once by the next call on this thread that found the key held and
    /// has not published its wait yet: what another call of the same
    /// transaction could do in between.
    static BEFORE_PUBLISH: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

/// Have the next call on this thread that finds its key held run `hook`
/// before it publishes its wait.
#[cfg(test)]
pub(super) fn before_publish(hook: impl FnOnce() + 'static) {
    BEFORE_PUBLISH.set(Some(Box::new(hook)));
}

/// How a lock call waits when another transaction holds the lock.
#[derive(Clone, Copy)]
enum Waiting {
    /// Not at all: return a [`LockWait`].
    Never,
    /// Block the thread for up to the transaction's lock timeout, counted
    /// from the first time the call finds the lock taken; on a platform with
    /// no clock nothing bounds a wait, and the call fails at once instead.
    Until,
}

impl Transaction {
    /// Take `key`'s lock (column-family prefixed) for a pessimistic
    /// transaction, waiting up to the lock timeout; [`TransactionError::Busy`]
    /// when it passes. `true` when the transaction already held the lock. An
    /// optimistic transaction takes no locks.
    pub(super) fn lock_key(&self, key: &[u8]) -> TxResult<bool> {
        let (Some(table), Some(owner)) = (self.lock_table.as_ref(), self.lock_owner.as_ref())
        else {
            return Ok(false);
        };
        self.acquire(table, owner.id(), key, Waiting::Until)
    }

    /// [`Self::lock_key`] that never waits: a lock another transaction holds
    /// is [`WouldBlock::Locked`].
    pub(super) fn try_lock_key(&self, key: &[u8]) -> TxResult<bool> {
        let (Some(table), Some(owner)) = (self.lock_table.as_ref(), self.lock_owner.as_ref())
        else {
            return Ok(false);
        };
        self.acquire(table, owner.id(), key, Waiting::Never)
    }

    /// Take `key`'s lock for `owner`, this transaction's name. The loop's
    /// first look is whether the transaction holds the key already, so a call
    /// that does pays one lookup.
    fn acquire(
        &self,
        table: &LockTable,
        owner: OwnerId,
        key: &[u8],
        waiting: Waiting,
    ) -> TxResult<bool> {
        // Looked up before anything queues, so a queue that is not open fails
        // the call cleanly.
        let queue = match waiting {
            Waiting::Never => self.lock_wait_queue()?,
            Waiting::Until => None,
        };
        // When a blocking call stops waiting, in platform microseconds: set
        // the first time it finds the lock taken, so a call that takes the
        // lock at once reads no clock.
        let mut deadline: Option<Option<u64>> = None;
        loop {
            if self.held_locks.contains_key(key) {
                return Ok(true);
            }
            let pending = match self.live_wait(key) {
                Some(pending) => pending,
                None => {
                    let key_lock = table.pin(key);
                    match key_lock.lock.try_lock(owner) {
                        TryLock::Taken => {
                            self.held_locks.insert(key.to_vec(), key_lock);
                            return Ok(false);
                        }
                        TryLock::Mine => {
                            // Another call of this transaction took it, and
                            // records it in `held_locks` with its own use.
                            table.unpin(key, &key_lock);
                            return Ok(true);
                        }
                        TryLock::Busy => {
                            #[cfg(test)]
                            if let Some(hook) = BEFORE_PUBLISH.take() {
                                hook();
                            }
                            match self.publish_wait(key, key_lock) {
                                Ok(pending) => pending,
                                Err(lost) => {
                                    // Another call of the transaction published
                                    // its wait first; this candidate never
                                    // queued. Its use goes back, and the next
                                    // look finds that wait.
                                    table.unpin(key, lost.key_lock());
                                    continue;
                                }
                            }
                        }
                    }
                }
            };
            match waiting {
                Waiting::Never => {
                    let slot = Arc::new(WaitSlot::new());
                    pending.note(Arc::clone(&slot), queue.clone());
                    match self.poll_wait(table, owner, key, &pending, Some(&slot))? {
                        Polled::Acquired => return Ok(self.took(key, &pending)),
                        Polled::Mine => return Ok(true),
                        // Another call ended the wait: look again from the top.
                        Polled::Done => continue,
                        Polled::Pending | Polled::Busy => {
                            let queue = queue.as_ref().map(|queue| queue.id());
                            let wait = LockWait::new(slot, queue);
                            return Err(TransactionError::WouldBlock(WouldBlock::Locked(wait)));
                        }
                    }
                }
                Waiting::Until => {
                    let deadline = *deadline.get_or_insert_with(|| {
                        let timeout =
                            u64::try_from(self.lock_timeout.as_micros()).unwrap_or(u64::MAX);
                        crate::env::platform_micros().map(|now| now.saturating_add(timeout))
                    });
                    pending.sleep_here();
                    match self.poll_wait(table, owner, key, &pending, None)? {
                        Polled::Acquired => return Ok(self.took(key, &pending)),
                        Polled::Mine => return Ok(true),
                        Polled::Done => continue,
                        Polled::Pending | Polled::Busy => {}
                    }
                    let left = deadline.and_then(|deadline| {
                        crate::env::platform_micros()
                            .and_then(|now| deadline.checked_sub(now))
                            .filter(|&left| left > 0)
                    });
                    let Some(left) = left else {
                        if pending.abandon(owner) {
                            table.unpin(key, pending.key_lock());
                        }
                        return Err(TransactionError::Busy(strip_cf_prefix(key.to_vec())));
                    };
                    // Unparked by the lock's next wake; a timeout, or a
                    // spurious return, looks again and checks the deadline.
                    std::thread::park_timeout(Duration::from_micros(left));
                }
            }
        }
    }

    /// Record the lock `pending` acquired as held, with the wait's use.
    /// `false`: the transaction did not hold it before.
    fn took(&self, key: &[u8], pending: &PendingLock) -> bool {
        self.held_locks
            .insert(key.to_vec(), Arc::clone(pending.key_lock()));
        false
    }

    /// Poll `pending` once, counted in through the close gate: a wait polled
    /// once close began is abandoned, and the call fails with `Closed`.
    /// `told` is the completion the call recorded for itself, voided when the
    /// call does not wait after all.
    fn poll_wait(
        &self,
        table: &LockTable,
        owner: OwnerId,
        key: &[u8],
        pending: &PendingLock,
        told: Option<&WaitSlot>,
    ) -> TxResult<Polled> {
        let closed = |pending: &PendingLock| {
            if let Some(told) = told {
                told.complete();
            }
            if pending.abandon(owner) {
                table.unpin(key, pending.key_lock());
            }
            Err(Error::Closed.into())
        };
        if !table.enter() {
            return closed(pending);
        }
        let polled = pending.poll(owner, told);
        let closing = table.leave();
        match polled {
            // The wait's use went with it; the holder's use is the other
            // call's.
            Polled::Mine => table.unpin(key, pending.key_lock()),
            Polled::Pending | Polled::Busy if closing => return closed(pending),
            _ => {}
        }
        Ok(polled)
    }

    /// This transaction's wait for `key`'s lock, while one is under way.
    fn live_wait(&self, key: &[u8]) -> Option<Arc<PendingLock>> {
        self.lock_waits
            .get()?
            .get(key)
            .filter(|pending| !pending.is_done())
    }

    /// Make a wait on `key_lock`, which the caller pinned for it, this
    /// transaction's wait for `key`, unless another call of the transaction
    /// published a live one first. Inserting into an empty place, or
    /// replacing a wait that is over, is one linearizable step of the map,
    /// so at most one live wait per transaction and key is ever published,
    /// and only a published wait is polled (and so queued). `Err` hands back
    /// the candidate, never polled, for the caller to give its use back.
    fn publish_wait(
        &self,
        key: &[u8],
        key_lock: Arc<KeyLock>,
    ) -> Result<Arc<PendingLock>, Arc<PendingLock>> {
        let candidate = Arc::new(PendingLock::new(key_lock));
        let waits = self.lock_waits();
        let mut present = waits.insert_if_absent(key.to_vec(), Arc::clone(&candidate));
        loop {
            match present {
                None => return Ok(candidate),
                Some(live) if !live.is_done() => return Err(candidate),
                Some(_) => {
                    present = match waits
                        .replace_if(key.to_vec(), Arc::clone(&candidate), |held| held.is_done())
                    {
                        Ok(_) => None,
                        Err(Some(current)) => Some(current),
                        // Entries leave only when the transaction ends, so
                        // never; insert again all the same.
                        Err(None) => waits.insert_if_absent(key.to_vec(), Arc::clone(&candidate)),
                    };
                }
            }
        }
    }

    fn lock_waits(&self) -> &LockWaits {
        self.lock_waits
            .get_or_init(|| Box::new(LockWaits::with_capacity(LOCK_WAIT_BUCKETS)))
    }

    /// The queue a [`LockWait`] this transaction hands out completes on:
    /// the one it names, if any.
    fn lock_wait_queue(&self) -> TxResult<Option<Arc<QueueShared>>> {
        let Some(id) = self.io_queue() else {
            return Ok(None);
        };
        match self.engine.io().queue(id) {
            Some(queue) => Ok(Some(queue)),
            None => Err(Error::invalid_argument(
                "the I/O queue this transaction names is not open on this database",
            )
            .into()),
        }
    }

    /// Let go of every lock this transaction holds and every wait it left,
    /// as it ends.
    pub(super) fn release_locks(&mut self) {
        let (Some(table), Some(owner)) = (self.lock_table.as_ref(), self.lock_owner.as_ref())
        else {
            return;
        };
        let owner = owner.id();
        if let Some(waits) = self.lock_waits.take() {
            for (key, pending) in *waits {
                if pending.abandon(owner) {
                    table.unpin(&key, pending.key_lock());
                }
            }
        }
        // Taken off in place: no list of them is built to walk.
        self.held_locks.drain_with(|key, key_lock| {
            key_lock.lock.unlock(owner);
            table.unpin(&key, &key_lock);
        });
    }

    /// The locks this transaction holds, taken out of it for a commit that
    /// lands later: they are released by whoever lands it.
    pub(super) fn take_landing_locks(&mut self) -> Option<Arc<LandingLocks>> {
        let table = Arc::clone(self.lock_table.as_ref()?);
        let owner = Arc::clone(self.lock_owner.as_ref()?);
        let keys = self.held_locks.drain();
        (!keys.is_empty()).then(|| Arc::new(LandingLocks::new(table, owner, keys)))
    }
}
