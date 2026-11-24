//! The pessimistic lock table: one exclusive lock per key, held by a
//! transaction from the call that takes it until the transaction ends or its
//! commit lands (plan 4.6, 4.10, 4.11).
//!
//! A key's lock is an [`OwnedLock`] in a lock-free map. Taking it is one
//! compare-and-swap that writes the transaction's [`LockOwner`] name into the
//! lock's state word; releasing it is one more. A transaction that finds the
//! lock taken either returns at once with a [`LockWait`](crate::LockWait)
//! (`try_get_for_update`) or waits for it (`get_for_update`, `put`, `delete`,
//! `merge`), and both wait the same way: in the lock's queue, with barging and
//! bounded bypass (D49), so a free lock goes to whoever asks first and a
//! waiter passed over [`MAX_BYPASS`](crate::sync::MAX_BYPASS) times is handed
//! it.
//!
//! # Invariants
//!
//! - **One holder per key.** The holder's name is in the lock's state word,
//!   written by the compare-and-swap that takes the lock, so two owners
//!   cannot both hold it, and one owner's second call on any thread reads its
//!   own name back. A key has at most one lock in the table: an entry leaves
//!   only once its use count is retired from zero, by one compare-and-swap
//!   inside the map's removal of exactly that entry, and a use is taken by a
//!   compare-and-swap that refuses a retired count. So nobody can hold or wait
//!   on an entry that left the table while another one stands for its key.
//! - **What bounds the table.** An entry is in the table only while it is
//!   used: by its holder, by each waiting transaction, and by each call
//!   between its lookup and its outcome. The last use out removes it. So the
//!   table holds the keys locked or waited on now, never every key ever
//!   locked.
//! - **A wait completes on the waiter's own queue.** A waiting transaction's
//!   lock waker delivers each [`LockWait`](crate::LockWait) it handed out to
//!   the queue that wait names, as one completion its `poll` takes in; it
//!   wakes an idle owner once and a busy one never (`QueueShared::deliver`).
//!   A blocking wait parks the waiting thread, and the waker unparks it.
//! - **No waiter outlives close.** A call that may queue a waiter counts
//!   itself in through a [`CloseGate`] before and out after; close marks the
//!   gate, releases the locks of the transactions it aborted, and wakes every
//!   waiter in the table. A call under way when the mark lands abandons its
//!   own wait. Every wait therefore becomes ready, and the call run again
//!   finds the gate closed and fails with `Closed`.

#![allow(unsafe_code)]

use std::sync::Arc;
use std::task::{Wake, Waker};
use std::thread::Thread;

use kovan_map::HashMap;

use crate::engine::io::CloseGate;
use crate::engine::io::job::Delivery;
use crate::engine::io::shared::{Message, QueueShared, WaitSlot};
use crate::engine::io::stack::Stack;
use crate::sync::internal::{AtomicUsize, Ordering, UnsafeCell};
use crate::sync::{OwnedLock, OwnedWait, OwnerId};

/// Buckets the table starts with; it grows past this on demand.
const KEY_BUCKETS: usize = 64;

/// The use count of an entry taken out of the table.
const RETIRED: usize = usize::MAX;

/// One transaction's waits, one per key: a wait is published by one
/// linearizable insert, or by replacing a wait that is over
/// (`Transaction::publish_wait`), and stays until the transaction ends.
pub(super) type LockWaits = HashMap<Vec<u8>, Arc<PendingLock>>;

/// The table of key locks one [`TransactionDb`](super::TransactionDb) keeps.
pub(crate) struct LockTable {
    keys: HashMap<Vec<u8>, Arc<KeyLock>>,
    /// Where close and the calls that may queue a waiter meet.
    gate: CloseGate,
}

/// One key's lock and the count of what uses it.
pub(crate) struct KeyLock {
    pub(super) lock: OwnedLock,
    /// The holder, each waiting transaction and each call between lookup and
    /// outcome, one each; [`RETIRED`] once the entry left the table.
    uses: AtomicUsize,
}

impl KeyLock {
    /// A lock used once already, by the call that makes it.
    fn new() -> Self {
        Self {
            lock: OwnedLock::new(),
            uses: AtomicUsize::new(1),
        }
    }

    /// A lock outside any table, used once by the model that makes it.
    #[cfg(loom)]
    pub(super) fn for_model() -> Self {
        Self::new()
    }

    /// Give back a use and retire the entry if it was the last, as
    /// [`LockTable::unpin`] does around the map's removal.
    #[cfg(loom)]
    pub(super) fn unpin_and_retire(&self) -> bool {
        self.uses.fetch_sub(1, Ordering::AcqRel) == 1 && self.retire()
    }

    /// Take a use, unless the entry left the table.
    pub(super) fn pin(&self) -> bool {
        let mut uses = self.uses.load(Ordering::Acquire);
        loop {
            if uses == RETIRED {
                return false;
            }
            match self.uses.compare_exchange_weak(
                uses,
                uses + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => uses = actual,
            }
        }
    }

    /// Mark an unused entry as leaving the table. `false` when a use came in
    /// first.
    fn retire(&self) -> bool {
        self.uses
            .compare_exchange(0, RETIRED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

impl LockTable {
    pub(crate) fn new() -> Self {
        Self {
            keys: HashMap::with_capacity(KEY_BUCKETS),
            gate: CloseGate::new(),
        }
    }

    /// `key`'s lock with a use taken for the caller, made when the key has
    /// none. Every pin is matched by one [`unpin`](Self::unpin).
    pub(super) fn pin(&self, key: &[u8]) -> Arc<KeyLock> {
        loop {
            let found = match self.keys.get(key) {
                Some(lock) => lock,
                None => {
                    let fresh = Arc::new(KeyLock::new());
                    match self.keys.insert_if_absent(key.to_vec(), Arc::clone(&fresh)) {
                        None => return fresh,
                        Some(lock) => lock,
                    }
                }
            };
            if found.pin() {
                return found;
            }
            // Retired: the thread that retired it is taking it out of the
            // table right now, in the same removal; look again once it is out.
            core::hint::spin_loop();
        }
    }

    /// Give back a use of `key`'s `lock`; the last one out takes the entry
    /// out of the table.
    pub(super) fn unpin(&self, key: &[u8], lock: &Arc<KeyLock>) {
        if lock.uses.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.keys
                .remove_if(key, |held| Arc::ptr_eq(held, lock) && held.retire());
        }
    }

    /// Count a call that may queue a waiter in. `false` once close began:
    /// the call then queues nothing and fails with `Closed`.
    pub(super) fn enter(&self) -> bool {
        self.gate.enter()
    }

    /// Count the call out. `true` when close began while it was under way:
    /// close's sweep may have missed its waiter, so the call abandons it.
    pub(super) fn leave(&self) -> bool {
        self.gate.leave()
    }

    /// `close`: refuse new waiters, release the locks `aborted` (the
    /// transactions close aborted) hold, and wake every waiter, so each runs
    /// its call again and fails with `Closed`.
    pub(crate) fn close(&self, aborted: &[Arc<LockOwner>]) {
        self.gate.close();
        let names: std::collections::HashMap<usize, OwnerId> = aborted
            .iter()
            .map(|owner| (owner.id().word(), owner.id()))
            .collect();
        for lock in self.keys.values() {
            if let Some(&owner) = names.get(&lock.lock.holder()) {
                lock.lock.unlock(owner);
            }
            lock.lock.wake_all();
        }
    }

    /// A close that failed and left the database open: waiters queue again.
    pub(crate) fn reopen(&self) {
        self.gate.reopen();
    }

    /// Entries in the table now.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }
}

/// A pessimistic transaction's name in the key locks it holds or waits for.
///
/// Its address is the name, so it is aligned past the lock's queue bits and
/// its handoff mark ([`OwnerId::ALIGN`]) and has a size of its own. Held
/// through an `Arc` by the transaction, by the commit whose locks wait for it
/// to land, and by the transaction's close claim, so the name stays unique
/// while any lock can carry it.
#[repr(align(128))]
pub(crate) struct LockOwner {
    /// Gives the owner a size, so no two owners share an address.
    _place: u8,
}

/// Owners a thread keeps for its next pessimistic transactions. A thread
/// runs one transaction at a time as a rule; the few beyond that cover
/// transactions it holds open side by side.
const OWNER_POOL: usize = 4;

std::thread_local! {
    /// The owners this thread's ended transactions gave back
    /// ([`LockOwner::give_back`]), each held by nothing else.
    static OWNERS: std::cell::RefCell<Vec<Arc<LockOwner>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl LockOwner {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self { _place: 0 })
    }

    /// A name for a pessimistic transaction that begins on this thread: one
    /// an earlier transaction of the thread gave back, else a new one. So a
    /// thread that runs transactions one after another allocates no owner
    /// and changes no count in steady state.
    pub(super) fn take() -> Arc<Self> {
        OWNERS
            .try_with(|pool| pool.borrow_mut().pop())
            .ok()
            .flatten()
            .unwrap_or_else(Self::new)
    }

    /// Give `owner` back as its transaction ends, for this thread's next
    /// transaction, if nothing else holds it. A commit whose locks are
    /// released at its landing (`LandingLocks`) and a close claim each hold a
    /// clone while they may still carry or release the name, so an owner one
    /// of them holds is dropped instead, and the name is never lent out while
    /// a lock can still carry it for the old transaction. A name a handoff
    /// wrote for the old transaction and has not cleared yet carries the
    /// handoff's mark, which the next transaction's calls never take for a
    /// hold (`OwnedLock`).
    pub(super) fn give_back(owner: Arc<Self>) {
        // Nothing ever makes a `Weak` of an owner, so a strong count of one is
        // every reference. The fence pairs with the release the last other
        // holder's drop made, so what it did with the name happened before
        // the name is lent again.
        if Arc::strong_count(&owner) != 1 {
            return;
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        // A thread that is exiting has no pool left; the owner is dropped.
        let _ = OWNERS.try_with(|pool| {
            let mut pool = pool.borrow_mut();
            if pool.len() < OWNER_POOL {
                pool.push(owner);
            }
        });
    }

    /// The name this owner's locks carry.
    pub(crate) fn id(&self) -> OwnerId {
        OwnerId::of(self)
    }
}

/// A [`PendingLock`] no call is polling.
const IDLE: usize = 0;
/// A call is polling the wait; another call of the transaction waits for the
/// lock's next wake instead.
const POLLING: usize = 1;
/// The wait is over: the lock was taken, found held by the transaction, or
/// the wait was abandoned.
const DONE: usize = 2;
/// Set beside [`POLLING`] by a wake that came while the poll ran. Such a
/// wake told only the notes recorded before it, so the poll that leaves the
/// wait pending finds it here and wakes again (`PendingLock::poll`).
const WOKEN: usize = 4;

/// What one poll of a [`PendingLock`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Polled {
    /// The transaction now holds the lock, and the wait's use is the
    /// holder's.
    Acquired,
    /// Another call of the transaction holds the lock; the wait left the
    /// queue and its use is the caller's to give back.
    Mine,
    /// Still waiting.
    Pending,
    /// Another call of the transaction is polling the wait right now.
    Busy,
    /// The wait was over before this poll.
    Done,
}

/// One transaction's wait for one key's lock, shared by every call of the
/// transaction that wants the key, and kept across calls so its place in
/// the queue and the losses it counted stay the transaction's.
pub(super) struct PendingLock {
    /// The key's lock, with a use taken for this wait.
    key_lock: Arc<KeyLock>,
    /// The place in the lock's queue. Touched only by the call that moved
    /// `Wakeup::state` from `IDLE`.
    wait: UnsafeCell<OwnedWait>,
    wakeup: Arc<Wakeup>,
    waker: Waker,
}

// SAFETY: `wait` is touched only by the one call that moved `Wakeup::state` from
// `IDLE` (to `POLLING`, or to `DONE` when it abandons the wait), so no two
// threads ever reach it at once; everything else is atomic or `Sync`.
unsafe impl Send for PendingLock {}
// SAFETY: as above.
unsafe impl Sync for PendingLock {}

impl PendingLock {
    /// A wait on `key_lock`, which the caller pinned for it.
    pub(super) fn new(key_lock: Arc<KeyLock>) -> Self {
        let wakeup = Arc::new(Wakeup {
            state: AtomicUsize::new(IDLE),
            notes: Stack::new(),
            sleepers: Stack::new(),
        });
        Self {
            key_lock,
            wait: UnsafeCell::new(OwnedWait::new()),
            waker: Waker::from(Arc::clone(&wakeup)),
            wakeup,
        }
    }

    pub(super) fn key_lock(&self) -> &Arc<KeyLock> {
        &self.key_lock
    }

    pub(super) fn is_done(&self) -> bool {
        self.wakeup.state.load(Ordering::Acquire) == DONE
    }

    /// Complete `slot` on `queue` (directly with none) at the lock's next
    /// wake. Recorded before the poll it is for, so no wake falls between.
    pub(super) fn note(&self, slot: Arc<WaitSlot>, queue: Option<Arc<QueueShared>>) {
        if let Err(note) = self.wakeup.notes.push(Note { slot, queue }, 0) {
            note.deliver();
        }
    }

    /// Unpark this thread at the lock's next wake. Recorded before the poll
    /// it is for, so no wake falls between.
    pub(super) fn sleep_here(&self) {
        let _ = self.wakeup.sleepers.push(std::thread::current(), 0);
    }

    /// Poll the wait once for `owner`. `told` is the completion the calling
    /// call recorded for itself with [`note`](Self::note), if it did: a poll
    /// that ends the wait voids it, since that call does not wait after all.
    ///
    /// A call that finds another polling returns [`Polled::Busy`] with its
    /// note recorded. A wake that came while that poll ran told only the
    /// notes recorded before it, and its nudge is still on the wait, so it
    /// marks the poll [`WOKEN`], and the poll that leaves the wait pending
    /// and finds the mark wakes again: a call that met it is told even if
    /// the poller's own caller never comes back.
    pub(super) fn poll(&self, owner: OwnerId, told: Option<&WaitSlot>) -> Polled {
        self.poll_checked::<true, false>(owner, told)
    }

    /// [`poll`](Self::poll) as it was before the hand-back: the loom
    /// calibration that shows a call meeting the poll is otherwise left
    /// untold.
    #[cfg(loom)]
    pub(super) fn poll_without_recheck(&self, owner: OwnerId, told: Option<&WaitSlot>) -> Polled {
        self.poll_checked::<false, false>(owner, told)
    }

    /// [`poll`](Self::poll) as it was before defect F1's fix: it takes the
    /// transaction's name for a hold even while a handoff wrote it and has
    /// not granted it. The loom calibration that shows that breaks mutual
    /// exclusion.
    #[cfg(loom)]
    pub(super) fn poll_trusting_a_half_made_handoff(
        &self,
        owner: OwnerId,
        told: Option<&WaitSlot>,
    ) -> Polled {
        self.poll_checked::<true, true>(owner, told)
    }

    /// [`poll`](Self::poll) with the hand-back of a wake that came while it
    /// ran (`RECHECK`), or without it, and with the name a handoff is still
    /// granting taken for a hold (`TRUST_MARK`) or not: the production poll
    /// is `<true, false>`, and the other forms are the loom calibrations that
    /// show each rule is needed.
    fn poll_checked<const RECHECK: bool, const TRUST_MARK: bool>(
        &self,
        owner: OwnerId,
        told: Option<&WaitSlot>,
    ) -> Polled {
        let state = &self.wakeup.state;
        match state.compare_exchange(IDLE, POLLING, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {}
            Err(DONE) => {
                if let Some(told) = told {
                    told.complete();
                }
                return Polled::Done;
            }
            // Another call of the transaction is polling: this one's
            // completion stays recorded, for the lock's next wake.
            Err(_) => return Polled::Busy,
        }
        let lock = &self.key_lock.lock;
        // SAFETY: `POLLING` gives this call the wait (see the `Sync` impl).
        let ready = self
            .wait
            .with_mut(|wait| unsafe { (*wait).poll(lock, owner, &self.waker) });
        let polled = if ready.is_ready() {
            Polled::Acquired
        } else if lock.holder() == owner.word() || (TRUST_MARK && trusts_mark(lock, owner)) {
            // Another call of the transaction took it while this one waited;
            // the lock cannot be handed to this wait while its own owner
            // holds it, so the cancel only withdraws. A handoff to this wait
            // that is not granted yet names it with `GRANTING`, which this
            // comparison never matches: the wait stays, and the grant wakes it.
            // SAFETY: as above.
            self.wait
                .with_mut(|wait| unsafe { (*wait).cancel(lock, owner) });
            Polled::Mine
        } else {
            Polled::Pending
        };
        if polled == Polled::Pending {
            // The swap reads the mark a wake that came while this poll ran
            // left (`Wakeup::wake_all`). A call this poll turned away noted
            // itself before it looked, so what that wake missed is told here.
            let ended = state.swap(IDLE, Ordering::AcqRel);
            if RECHECK && ended & WOKEN != 0 {
                self.wakeup.wake_all();
            }
        } else {
            if let Some(told) = told {
                told.complete();
            }
            self.finish();
        }
        polled
    }

    /// End the wait if no call is polling it: leave the queue, giving back a
    /// handoff that reached it. `true` when this call ended it, and the
    /// wait's use is the caller's to give back.
    pub(super) fn abandon(&self, owner: OwnerId) -> bool {
        if self
            .wakeup
            .state
            .compare_exchange(IDLE, DONE, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        // SAFETY: moving `state` from `IDLE` gave this call the wait.
        self.wait
            .with_mut(|wait| unsafe { (*wait).cancel(&self.key_lock.lock, owner) });
        self.wakeup.wake_all();
        true
    }

    /// The wait is over: every other call of the transaction waiting on it
    /// runs again.
    fn finish(&self) {
        self.wakeup.state.store(DONE, Ordering::Release);
        self.wakeup.wake_all();
    }
}

/// Whether the lock names `owner`, a handoff's marked name included: only the
/// loom calibration of defect F1 asks, and outside loom nothing does.
#[cfg(loom)]
fn trusts_mark(lock: &crate::sync::OwnedLock, owner: OwnerId) -> bool {
    lock.names(owner)
}

/// Outside loom no poll takes a marked name for a hold.
#[cfg(not(loom))]
fn trusts_mark(_lock: &crate::sync::OwnedLock, _owner: OwnerId) -> bool {
    false
}

/// What a wake of a [`PendingLock`] tells: every [`LockWait`](crate::LockWait)
/// handed out since the last wake, each on its own queue, and every thread
/// that blocks on the wait.
struct Wakeup {
    /// The wait's [`IDLE`], [`POLLING`] (with [`WOKEN`] when a wake came
    /// while it ran) or [`DONE`]. Here rather than in the wait, so a wake
    /// can mark the poll it falls into.
    state: AtomicUsize,
    notes: Stack<Note>,
    sleepers: Stack<Thread>,
}

impl Wakeup {
    fn wake_all(&self) {
        // Mark a poll under way before telling the notes: one this wake
        // misses was recorded after it, by a call the poll turned away.
        let mut state = self.state.load(Ordering::Acquire);
        while state == POLLING {
            match self.state.compare_exchange_weak(
                state,
                POLLING | WOKEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => state = actual,
            }
        }
        for note in self.notes.take(0) {
            note.deliver();
        }
        for sleeper in self.sleepers.take(0) {
            sleeper.unpark();
        }
    }
}

impl Wake for Wakeup {
    fn wake(self: Arc<Self>) {
        self.wake_all();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_all();
    }
}

/// One handed-out wait to complete: on its queue, at that queue's poll, or
/// directly when it has none or the queue was dropped.
struct Note {
    slot: Arc<WaitSlot>,
    queue: Option<Arc<QueueShared>>,
}

impl Note {
    fn deliver(self) {
        let Self { slot, queue } = self;
        // Voided by the call it was for, which did not wait after all.
        if slot.is_ready() {
            return;
        }
        let Some(queue) = queue else {
            slot.complete();
            return;
        };
        let own = Arc::clone(&slot);
        if queue
            .deliver(Message::Deliver(slot as Arc<dyn Delivery>))
            .is_err()
        {
            own.complete();
        }
    }
}

/// The key locks a pessimistic commit holds until it lands, released by the
/// thread that lands it: once the commit is applied and visible (and
/// durable at `Immediate`), whether or not its outcome was delivered.
pub(super) struct LandingLocks {
    table: Arc<LockTable>,
    owner: Arc<LockOwner>,
    keys: Vec<(Vec<u8>, Arc<KeyLock>)>,
    released: AtomicUsize,
}

impl LandingLocks {
    pub(super) fn new(
        table: Arc<LockTable>,
        owner: Arc<LockOwner>,
        keys: Vec<(Vec<u8>, Arc<KeyLock>)>,
    ) -> Self {
        Self {
            table,
            owner,
            keys,
            released: AtomicUsize::new(0),
        }
    }

    /// Release every lock, once whoever calls it.
    pub(super) fn release(&self) {
        if self.released.swap(1, Ordering::AcqRel) != 0 {
            return;
        }
        let owner = self.owner.id();
        for (key, key_lock) in &self.keys {
            key_lock.lock.unlock(owner);
            self.table.unpin(key, key_lock);
        }
    }
}

impl Delivery for LandingLocks {
    fn deliver(&self) {
        self.release();
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::sync::TryLock;

    #[test]
    fn an_entry_leaves_the_table_with_its_last_use() {
        let table = LockTable::new();
        let owner = LockOwner::new();
        let lock = table.pin(b"k");
        assert_eq!(lock.lock.try_lock(owner.id()), TryLock::Taken);
        let again = table.pin(b"k");
        assert!(Arc::ptr_eq(&lock, &again), "one lock per key");
        assert_eq!(table.len(), 1);
        table.unpin(b"k", &again);
        assert_eq!(table.len(), 1, "still held");
        assert!(lock.lock.unlock(owner.id()));
        table.unpin(b"k", &lock);
        assert_eq!(table.len(), 0, "the last use took it out");
        let fresh = table.pin(b"k");
        assert!(!Arc::ptr_eq(&lock, &fresh), "a new entry for the key");
        table.unpin(b"k", &fresh);
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn a_retired_entry_refuses_a_use() {
        let lock = KeyLock::new();
        assert_eq!(lock.uses.fetch_sub(1, Ordering::AcqRel), 1);
        assert!(lock.retire());
        assert!(!lock.pin(), "a retired entry is never used again");
        let busy = KeyLock::new();
        assert!(!busy.retire(), "a used entry is not retired");
    }

    #[test]
    fn pins_and_unpins_across_threads_leave_the_table_empty() {
        let table = Arc::new(LockTable::new());
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let table = Arc::clone(&table);
                std::thread::spawn(move || {
                    for i in 0..if cfg!(miri) { 30u32 } else { 2_000 } {
                        let key = (i % 3).to_le_bytes();
                        let lock = table.pin(&key);
                        table.unpin(&key, &lock);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("worker");
        }
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn close_releases_the_aborted_owners_locks_and_only_theirs() {
        let table = LockTable::new();
        let (aborted, kept) = (LockOwner::new(), LockOwner::new());
        let a = table.pin(b"a");
        let b = table.pin(b"b");
        assert_eq!(a.lock.try_lock(aborted.id()), TryLock::Taken);
        assert_eq!(b.lock.try_lock(kept.id()), TryLock::Taken);
        table.close(std::slice::from_ref(&aborted));
        assert_eq!(a.lock.holder(), 0, "the aborted owner's lock is free");
        assert_eq!(b.lock.holder(), kept.id().word(), "the other stays held");
        assert!(!table.enter(), "no waiter queues after close");
        table.reopen();
        assert!(table.enter());
        assert!(!table.leave());
        table.unpin(b"a", &a);
        assert!(b.lock.unlock(kept.id()));
        table.unpin(b"b", &b);
        assert_eq!(table.len(), 0);
    }

    /// E42. An owner is lent to the thread's next transaction only when its
    /// transaction held the last reference: one a landing commit or a close
    /// claim still holds is never lent, and its name stays its own.
    #[test]
    fn an_owner_is_lent_again_only_when_nothing_else_holds_it() {
        OWNERS.with(|pool| pool.borrow_mut().clear());
        let owner = LockOwner::take();
        let name = owner.id().word();
        let landing = Arc::clone(&owner);
        LockOwner::give_back(owner);
        let next = LockOwner::take();
        assert_ne!(next.id().word(), name, "lent while another holder had it");
        drop(landing);
        let name = next.id().word();
        LockOwner::give_back(next);
        let again = LockOwner::take();
        assert_eq!(again.id().word(), name, "the only holder gave it back");
        for _ in 0..OWNER_POOL + 2 {
            LockOwner::give_back(LockOwner::new());
        }
        assert_eq!(
            OWNERS.with(|pool| pool.borrow().len()),
            OWNER_POOL,
            "the pool is bounded"
        );
        OWNERS.with(|pool| pool.borrow_mut().clear());
    }

    /// Make `waiter`'s wait owed: it loses every race the bound allows while
    /// `holder` releases and barges back in. `holder` holds the lock after.
    fn owe(lock: &OwnedLock, holder: OwnerId, waiter: OwnerId, pending: &PendingLock) {
        assert_eq!(pending.poll(waiter, None), Polled::Pending, "queued");
        for _ in 0..crate::sync::MAX_BYPASS {
            assert!(lock.unlock(holder));
            assert_eq!(lock.try_lock(holder), TryLock::Taken, "barged");
            assert_eq!(pending.poll(waiter, None), Polled::Pending, "a lost race");
        }
    }

    /// F1. A release hands the lock to an owed wait in two steps: it writes
    /// the waiter's name, then grants the waiter. The waiting transaction
    /// polls between the two (on another thread, in the code; here inside
    /// the hook) and must not take the name for a hold of its own: that
    /// would withdraw the wait, the grant would then fail and free the lock,
    /// and the transaction would go on believing it holds the key.
    #[test]
    fn a_poll_between_the_name_and_the_grant_does_not_conclude_mine() {
        let lock = Arc::new(KeyLock::new());
        let (holder, waiter) = (LockOwner::new(), LockOwner::new());
        let (h, w) = (holder.id(), waiter.id());
        assert_eq!(lock.lock.try_lock(h), TryLock::Taken);
        let pending = Arc::new(PendingLock::new(Arc::clone(&lock)));
        owe(&lock.lock, h, w, &pending);

        let seen = Arc::new(std::sync::Mutex::new(None));
        {
            let (pending, seen, lock) =
                (Arc::clone(&pending), Arc::clone(&seen), Arc::clone(&lock));
            crate::sync::between_take_and_grant(move || {
                let polled = pending.poll(w, None);
                let tried = lock.lock.try_lock(w);
                *seen.lock().unwrap() = Some((polled, tried));
            });
        }
        assert!(
            lock.lock.unlock(h),
            "the release hands the lock to the owed wait"
        );
        let (polled, tried) = seen
            .lock()
            .unwrap()
            .take()
            .expect("the handoff ran the hook");
        assert_ne!(
            polled,
            Polled::Mine,
            "the poll took the half-made handoff for a hold"
        );
        assert_ne!(
            tried,
            TryLock::Mine,
            "a try took the half-made handoff for a hold"
        );
        // The grant went through: the wait is the holder.
        assert_eq!(lock.lock.holder(), w.word(), "the handoff was undone");
        assert_eq!(pending.poll(w, None), Polled::Acquired);
        assert!(lock.lock.unlock(w));
    }

    #[test]
    fn a_note_completes_on_its_queue_and_a_dropped_queue_completes_it_directly() {
        use crate::engine::io::shared::test_id;
        let queue = Arc::new(QueueShared::new(test_id(), 1 << 20));
        let slot = Arc::new(WaitSlot::new());
        Note {
            slot: Arc::clone(&slot),
            queue: Some(Arc::clone(&queue)),
        }
        .deliver();
        assert!(!slot.is_ready(), "ready only at the queue's poll");
        for message in queue.take_inbox() {
            match message {
                Message::Deliver(waiter) => waiter.deliver(),
                _ => panic!("a lock wait is delivered, nothing else"),
            }
        }
        assert!(slot.is_ready());
        assert_eq!(queue.shut_inbox().count(), 0);
        let late = Arc::new(WaitSlot::new());
        Note {
            slot: Arc::clone(&late),
            queue: Some(queue),
        }
        .deliver();
        assert!(late.is_ready(), "a dropped queue's wait completes directly");
    }
}
