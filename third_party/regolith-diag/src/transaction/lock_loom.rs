//! Loom models of the pessimistic lock table: taking a key's lock, releasing
//! it, and waiting for it; and of the first four `on_abort` callbacks a
//! transaction's claim holds in place, pushed by its owner while `close`
//! takes them (`tests/loom_locks.rs` runs them).
//!
//! Each positive model drives the production types: [`OwnedLock`] and
//! [`OwnedWait`], [`KeyLock`]'s use count, [`PendingLock`] and the queue a
//! wait is delivered to, whose atomics and cells come from
//! `crate::sync::internal` and so are loom's under `--cfg loom`. Each
//! calibration writes the one step it breaks the wrong way and must fail;
//! without it, a search that never reached the bad interleaving would pass
//! for the same reason a broken protocol does.
//!
//! The map the table keeps its locks in is not loom's, so the models hold a
//! key's lock directly; what the map adds, one entry per key, rests on the
//! use count modelled in `a_retired_entry_is_never_used`.

#![allow(unsafe_code)]

use std::future::Future;
use std::num::NonZeroU64;
use std::sync::Arc as StdArc;
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering as StdOrdering};
use std::task::{Context, Poll, Wake, Waker};

use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{Arc, Notify};
use loom::thread;

use super::first_four::{FirstFour, IN_PLACE, TakeFirst};
use super::lock_table::{KeyLock, LockOwner, PendingLock, Polled};
use crate::engine::io::CloseGate;
use crate::engine::io::shared::{Message, QueueShared, WaitSlot};
use crate::io_queue::QueueId;
use crate::sync::{MAX_BYPASS, OwnedLock, OwnedWait, OwnerId, TryLock};

/// How many executions reached the state a model exists to check.
#[derive(Clone)]
struct Witness(StdArc<StdAtomicUsize>);

impl Witness {
    fn record(&self) {
        self.0.fetch_add(1, StdOrdering::Relaxed);
    }
}

/// Run `model` under loom with at most `preemptions` preemptions per
/// schedule, and fail it when the search was implausibly small or never
/// reached the state it checks.
fn explore(
    name: &'static str,
    preemptions: usize,
    min_interleavings: usize,
    min_witnesses: usize,
    model: impl Fn(&Witness) + Sync + Send + 'static,
) {
    let runs = StdArc::new(StdAtomicUsize::new(0));
    let witness = Witness(StdArc::new(StdAtomicUsize::new(0)));
    let counted = {
        let (runs, witness) = (StdArc::clone(&runs), witness.clone());
        move || {
            model(&witness);
            runs.fetch_add(1, StdOrdering::Relaxed);
        }
    };
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = builder.preemption_bound.or(Some(preemptions));
    builder.check(counted);
    let explored = runs.load(StdOrdering::Relaxed);
    let witnessed = witness.0.load(StdOrdering::Relaxed);
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

struct Unpark(Notify);

impl Wake for Unpark {
    fn wake(self: StdArc<Self>) {
        self.0.notify();
    }

    fn wake_by_ref(self: &StdArc<Self>) {
        self.0.notify();
    }
}

/// Drives `future` on this loom thread, parking it between polls until its
/// waker fires. A wake that never comes parks it for good, which loom
/// reports as a deadlock.
fn block_on<F: Future>(future: F) -> F::Output {
    let park = StdArc::new(Unpark(Notify::new()));
    let waker = Waker::from(StdArc::clone(&park));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        park.0.wait();
    }
}

/// A waker that also records that it fired, for a model that must tell a
/// poll after a real wake from one after a wait that returned on its own.
struct Told {
    park: Notify,
    fired: std::sync::atomic::AtomicBool,
}

impl Wake for Told {
    fn wake(self: StdArc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &StdArc<Self>) {
        self.fired.store(true, StdOrdering::SeqCst);
        self.park.notify();
    }
}

/// Wait in `lock`'s queue for `owner` and return how many races it lost: the
/// polls that left it pending after a wake that really fired. loom's
/// `Notify` may return from a wait once with no notification, as a real park
/// may, and a poll after that tries nothing and loses nothing, so it is not
/// counted. `queued` is raised once the wait is in the queue.
fn wait_counting_losses(lock: &OwnedLock, owner: OwnerId, queued: &AtomicBool) -> usize {
    let told = StdArc::new(Told {
        park: Notify::new(),
        fired: std::sync::atomic::AtomicBool::new(false),
    });
    let waker = Waker::from(StdArc::clone(&told));
    let mut wait = OwnedWait::new();
    let mut lost = 0;
    loop {
        let woken = told.fired.swap(false, StdOrdering::SeqCst);
        if wait.poll(lock, owner, &waker).is_ready() {
            return lost;
        }
        if woken {
            lost += 1;
        }
        queued.store(true, Ordering::Release);
        told.park.wait();
    }
}

/// Take `lock` for `owner`, waiting in its queue when it is taken. Returns
/// how many polls came back pending: the first is the queueing, each one
/// after it a race the waiter lost.
fn lock_for(lock: &OwnedLock, owner: OwnerId) -> usize {
    if lock.try_lock(owner) == TryLock::Taken {
        return 0;
    }
    let mut wait = OwnedWait::new();
    let mut pending = 0;
    block_on(std::future::poll_fn(|cx| {
        let polled = wait.poll(lock, owner, cx.waker());
        if polled.is_pending() {
            pending += 1;
        }
        polled
    }));
    pending
}

fn queue(n: u64) -> StdArc<QueueShared> {
    let id = QueueId::new(NonZeroU64::new(n).expect("model ids start at 1"));
    StdArc::new(QueueShared::new(id, 1 << 20))
}

/// The completions pushed to `queue`, delivered as its owner's poll would.
fn take_in(queue: &QueueShared) -> usize {
    let mut delivered = 0;
    for message in queue.take_inbox() {
        if let Message::Deliver(waiter) = message {
            waiter.deliver();
            delivered += 1;
        }
    }
    delivered
}

/// Two owners take, hold and release one key's lock, each waiting in its
/// queue when it finds it taken. The value they guard is a loom cell, so
/// two holders at once fail the model as a data race, and a waiter the
/// release never wakes parks for good and fails it as a deadlock.
pub fn two_owners_never_hold_one_key() {
    explore("two_owners_never_hold_one_key", 3, 10, 1, |witness| {
        let lock = Arc::new((OwnedLock::new(), UnsafeCell::new(0usize)));
        let (a, b) = (LockOwner::new(), LockOwner::new());
        let other = {
            let lock = Arc::clone(&lock);
            thread::spawn(move || {
                let waited = lock_for(&lock.0, b.id());
                lock.1.with_mut(|value| unsafe { *value += 1 });
                assert!(lock.0.unlock(b.id()));
                waited
            })
        };
        let waited = lock_for(&lock.0, a.id());
        lock.1.with_mut(|value| unsafe { *value += 1 });
        assert!(lock.0.unlock(a.id()));
        if waited + other.join().expect("owner") > 0 {
            witness.record();
        }
        assert_eq!(lock.1.with(|value| unsafe { *value }), 2);
        assert_eq!(lock.0.holder(), 0, "the lock is free at the end");
    });
}

/// A waiter keeps its place while another owner takes the lock back after
/// every release: it loses at most `MAX_BYPASS` races, and is then handed
/// the lock.
pub fn a_waiter_loses_at_most_the_bound() {
    explore("a_waiter_loses_at_most_the_bound", 2, 10, 1, |witness| {
        let lock = Arc::new(OwnedLock::new());
        let (barger, waiter) = (LockOwner::new(), LockOwner::new());
        assert_eq!(lock.try_lock(barger.id()), TryLock::Taken);
        let queued = Arc::new(AtomicBool::new(false));
        let waiting = {
            let (lock, queued) = (Arc::clone(&lock), Arc::clone(&queued));
            thread::spawn(move || {
                let lost = wait_counting_losses(&lock, waiter.id(), &queued);
                assert!(lock.unlock(waiter.id()));
                lost
            })
        };
        // The barging starts once the waiter is in the queue.
        while !queued.load(Ordering::Acquire) {
            thread::yield_now();
        }
        // Barge back in after every release, once more than the bound
        // allows: past it the release must go to the waiter.
        let mut held = true;
        for _ in 0..=MAX_BYPASS {
            if held {
                assert!(lock.unlock(barger.id()));
            }
            held = lock.try_lock(barger.id()) == TryLock::Taken;
        }
        if held {
            assert!(lock.unlock(barger.id()));
        }
        let lost = waiting.join().expect("waiter");
        assert!(
            lost <= MAX_BYPASS as usize,
            "a waiter lost more races than the bound allows"
        );
        if lost == MAX_BYPASS as usize {
            witness.record();
        }
        assert_eq!(lock.holder(), 0);
    });
}

/// A transaction that found the lock taken records its wait on queue A and
/// polls; the holder releases meanwhile. If the poll left it waiting, the
/// release told it: exactly one completion is on queue A, none on queue B,
/// and the wait is ready only once queue A's owner takes it in.
pub fn a_lock_wait_reaches_its_own_queue() {
    explore("a_lock_wait_reaches_its_own_queue", 3, 10, 1, |witness| {
        let key_lock = StdArc::new(KeyLock::for_model());
        let (holder, waiter) = (LockOwner::new(), LockOwner::new());
        assert_eq!(key_lock.lock.try_lock(holder.id()), TryLock::Taken);
        let (mine, other) = (queue(1), queue(2));
        let pending = StdArc::new(PendingLock::new(StdArc::clone(&key_lock)));
        let slot = StdArc::new(WaitSlot::new());
        let releaser = {
            let key_lock = StdArc::clone(&key_lock);
            thread::spawn(move || {
                assert!(key_lock.lock.unlock(holder.id()));
            })
        };
        pending.note(StdArc::clone(&slot), Some(StdArc::clone(&mine)));
        let polled = pending.poll(waiter.id(), Some(&slot));
        releaser.join().expect("releaser");
        match polled {
            Polled::Acquired => {}
            Polled::Pending => {
                assert!(!slot.is_ready(), "a lock wait completed off its queue");
                assert_eq!(take_in(&other), 0, "a lock wait reached another queue");
                assert_eq!(take_in(&mine), 1, "a lock wait was never told");
                assert!(slot.is_ready());
                assert_eq!(pending.poll(waiter.id(), None), Polled::Acquired);
                witness.record();
            }
            other => panic!("one caller polls this wait: {other:?}"),
        }
        assert_eq!(key_lock.lock.holder(), waiter.id().word());
        assert!(key_lock.lock.unlock(waiter.id()));
    });
}

/// One transaction's wait map for one key, as the loom models see it. The
/// production map is not loom's, so a loom mutex stands for its one
/// linearizable step: publishing into an empty place or over a wait that is
/// over (`Transaction::publish_wait`). Newest last.
struct Waits {
    published: loom::sync::Mutex<Vec<StdArc<PendingLock>>>,
    /// The old buffer's publication, for the calibration: every wait is
    /// added, and a lookup finds the newest.
    shadows: bool,
    /// Publications lost to another call's: the race the model is about.
    lost: AtomicUsize,
}

impl Waits {
    fn live(&self) -> Option<StdArc<PendingLock>> {
        let published = self.published.lock().expect("the map");
        published.iter().rev().find(|wait| !wait.is_done()).cloned()
    }

    fn publish(&self, candidate: StdArc<PendingLock>) -> Option<StdArc<PendingLock>> {
        let mut published = self.published.lock().expect("the map");
        if !self.shadows && published.iter().any(|wait| !wait.is_done()) {
            self.lost.fetch_add(1, Ordering::SeqCst);
            return None;
        }
        published.push(StdArc::clone(&candidate));
        Some(candidate)
    }
}

/// One call of the transaction, as `Transaction::acquire` makes it, up to
/// its return: the slot it waits on, or `None` once the transaction holds
/// the lock.
fn lock_call(key_lock: &StdArc<KeyLock>, waits: &Waits, me: OwnerId) -> Option<StdArc<WaitSlot>> {
    loop {
        let pending = match waits.live() {
            Some(pending) => pending,
            None => {
                assert!(key_lock.pin(), "the holder's use keeps the entry");
                match key_lock.lock.try_lock(me) {
                    TryLock::Taken => return None,
                    TryLock::Mine => {
                        key_lock.unpin_and_retire();
                        return None;
                    }
                    TryLock::Busy => {
                        let candidate = StdArc::new(PendingLock::new(StdArc::clone(key_lock)));
                        match waits.publish(candidate) {
                            Some(pending) => pending,
                            None => {
                                // Lost: given back unpolled, and looked up again.
                                key_lock.unpin_and_retire();
                                continue;
                            }
                        }
                    }
                }
            }
        };
        let slot = StdArc::new(WaitSlot::new());
        pending.note(StdArc::clone(&slot), None);
        match pending.poll(me, Some(&slot)) {
            Polled::Acquired | Polled::Mine => return None,
            Polled::Done => continue,
            Polled::Pending | Polled::Busy => return Some(slot),
        }
    }
}

/// Two calls of one transaction ask for a key another transaction holds, at
/// once: both may find no wait and make one. Then the holder releases, and
/// each call that was told calls again. The transaction must take the freed
/// lock: the release tells the wait in the line, and a told call's lookup
/// finds that wait. `shadows` plants the old publication for the
/// calibration. The witness is a call that lost the publication.
fn two_calls_on_one_transaction(name: &'static str, shadows: bool) {
    explore(name, 2, 10, usize::from(!shadows), move |witness| {
        let key_lock = StdArc::new(KeyLock::for_model());
        let (holder, waiter) = (LockOwner::new(), LockOwner::new());
        let me = waiter.id();
        assert_eq!(key_lock.lock.try_lock(holder.id()), TryLock::Taken);
        let waits = StdArc::new(Waits {
            published: loom::sync::Mutex::new(Vec::new()),
            shadows,
            lost: AtomicUsize::new(0),
        });
        let other = {
            let (key_lock, waits) = (StdArc::clone(&key_lock), StdArc::clone(&waits));
            thread::spawn(move || lock_call(&key_lock, &waits, me))
        };
        let slots = [
            lock_call(&key_lock, &waits, me),
            other.join().expect("call"),
        ];
        if waits.lost.load(Ordering::SeqCst) > 0 {
            witness.record();
        }
        // The holder lets go; each told call calls again, once.
        assert!(key_lock.lock.unlock(holder.id()));
        for slot in slots.iter().flatten() {
            if slot.is_ready() {
                let _ = lock_call(&key_lock, &waits, me);
            }
        }
        let took = key_lock.lock.holder() == me.word();
        // Settle before the verdict, so a failure is the assertion alone.
        for wait in waits.published.lock().expect("the map").iter() {
            wait.abandon(me);
        }
        key_lock.lock.unlock(me);
        assert!(took, "the transaction never took the freed lock");
    });
}

/// Two calls of one transaction behind a holder share one wait, and the
/// release reaches it.
pub fn two_calls_on_one_transaction_share_one_wait() {
    two_calls_on_one_transaction("two_calls_on_one_transaction_share_one_wait", false);
}

/// Its calibration: the old publication, where each call adds its own wait
/// and a lookup finds the newest. The release tells the older wait, at the
/// front of the line, and the told call's lookup finds the newer one.
pub fn calibration_two_published_waits_hide_the_told_one() {
    two_calls_on_one_transaction("calibration_two_published_waits_hide_the_told_one", true);
}

/// Two calls of one transaction poll one wait that is already queued, while
/// the holder releases: one polls, and the other may meet that poll and
/// return waiting with its note recorded. `poll` is the production poll, or
/// the old one without the hand-back for the calibration. Whatever the
/// order, once nobody took the freed lock every call that returned waiting
/// was told, so the transaction's next poll is never left to the poller's own
/// caller, which may never come back. The witness is a call that met the poll.
fn two_pollers_on_one_wait(
    name: &'static str,
    poll: fn(&PendingLock, OwnerId, Option<&WaitSlot>) -> Polled,
) {
    explore(name, 2, 10, 1, move |witness| {
        let key_lock = StdArc::new(KeyLock::for_model());
        let (holder, waiter) = (LockOwner::new(), LockOwner::new());
        let me = waiter.id();
        assert_eq!(key_lock.lock.try_lock(holder.id()), TryLock::Taken);
        let pending = StdArc::new(PendingLock::new(StdArc::clone(&key_lock)));
        // Queued already: an earlier call polled it and returned waiting.
        let earlier = StdArc::new(WaitSlot::new());
        pending.note(StdArc::clone(&earlier), None);
        assert_eq!(poll(&pending, me, Some(&earlier)), Polled::Pending);
        let call = move |pending: &PendingLock| {
            let slot = StdArc::new(WaitSlot::new());
            pending.note(StdArc::clone(&slot), None);
            (poll(pending, me, Some(&slot)), slot)
        };
        let releaser = {
            let key_lock = StdArc::clone(&key_lock);
            thread::spawn(move || assert!(key_lock.lock.unlock(holder.id())))
        };
        let other = {
            let pending = StdArc::clone(&pending);
            thread::spawn(move || call(&pending))
        };
        let mine = call(&pending);
        releaser.join().expect("releaser");
        let outcomes = [mine, other.join().expect("call")];
        if outcomes
            .iter()
            .all(|(polled, _)| *polled != Polled::Acquired)
        {
            let told = outcomes.iter().all(|(_, slot)| slot.is_ready());
            if outcomes.iter().any(|(polled, _)| *polled == Polled::Busy) {
                witness.record();
            }
            // Settle the wait before the verdict, so a failure is the
            // assertion and nothing else: the freed lock is the wait's.
            assert_eq!(pending.poll(me, None), Polled::Acquired);
            assert!(told, "a call that met the poll was never told");
        }
        assert!(key_lock.lock.unlock(me));
    });
}

/// The two-poller model on the production poll.
pub fn two_pollers_on_one_wait_are_both_told() {
    two_pollers_on_one_wait("two_pollers_on_one_wait_are_both_told", PendingLock::poll);
}

/// Its calibration: the poll without the hand-back of a wake that came
/// while it ran. A release whose wake falls inside the first call's poll,
/// with the second call noting itself after that wake and meeting the poll,
/// leaves the second call untold.
pub fn calibration_a_poll_without_the_hand_back_leaves_a_call_untold() {
    two_pollers_on_one_wait(
        "calibration_a_poll_without_the_hand_back_leaves_a_call_untold",
        PendingLock::poll_without_recheck,
    );
}

/// Defect F1. A release hands the lock to an owed wait in two steps, the
/// name and then the grant, while the waiting transaction polls. `poll` is
/// the production poll, or for the calibration one that takes the name a
/// handoff is still granting for a hold. Whatever the order, a poll that says
/// the transaction holds the lock is right, and the lock ends with the
/// transaction. The witness is a poll that left the wait pending while the
/// handoff went through; the calibration, at the same bound, shows the search
/// reaches the poll between the handoff's two steps.
fn a_poll_racing_a_handoff(
    name: &'static str,
    poll: fn(&PendingLock, OwnerId, Option<&WaitSlot>) -> Polled,
) {
    explore(name, 2, 10, 1, move |witness| {
        let key_lock = StdArc::new(KeyLock::for_model());
        let (holder, waiter) = (LockOwner::new(), LockOwner::new());
        let (h, me) = (holder.id(), waiter.id());
        assert_eq!(key_lock.lock.try_lock(h), TryLock::Taken);
        let pending = StdArc::new(PendingLock::new(StdArc::clone(&key_lock)));
        // Owed: the wait loses every race the bound allows.
        assert_eq!(poll(&pending, me, None), Polled::Pending);
        for _ in 0..MAX_BYPASS {
            assert!(key_lock.lock.unlock(h));
            assert_eq!(key_lock.lock.try_lock(h), TryLock::Taken);
            assert_eq!(poll(&pending, me, None), Polled::Pending);
        }
        let releaser = {
            let key_lock = StdArc::clone(&key_lock);
            thread::spawn(move || assert!(key_lock.lock.unlock(h)))
        };
        let polled = poll(&pending, me, None);
        releaser.join().expect("releaser");
        let says_held = matches!(polled, Polled::Acquired | Polled::Mine);
        let holds = key_lock.lock.holder() == me.word();
        if polled == Polled::Pending && holds {
            witness.record();
        }
        // Settle before the verdict: the wait, if still pending, is granted.
        if !says_held && polled != Polled::Done {
            assert_eq!(pending.poll(me, None), Polled::Acquired);
        }
        key_lock.lock.unlock(me);
        assert!(
            !says_held || holds,
            "a poll said the transaction holds a lock the handoff then freed"
        );
    });
}

/// The F1 model on the production poll.
pub fn a_poll_racing_a_handoff_is_never_fooled() {
    a_poll_racing_a_handoff("a_poll_racing_a_handoff_is_never_fooled", PendingLock::poll);
}

/// Its calibration: the poll before the fix, which took the name a handoff
/// had written, and not granted yet, for the transaction's own hold.
pub fn calibration_a_poll_trusting_a_half_made_handoff_breaks_exclusion() {
    a_poll_racing_a_handoff(
        "calibration_a_poll_trusting_a_half_made_handoff_breaks_exclusion",
        PendingLock::poll_trusting_a_half_made_handoff,
    );
}

/// The last use of an entry is given back, and the entry retired, while
/// another caller looks the entry up and takes a use: never both. Either
/// the use came first and the entry stays, or the retirement did and the
/// caller sees it and looks the key up again.
pub fn a_retired_entry_is_never_used() {
    explore("a_retired_entry_is_never_used", 3, 2, 1, |witness| {
        let key_lock = StdArc::new(KeyLock::for_model());
        let user = {
            let key_lock = StdArc::clone(&key_lock);
            thread::spawn(move || key_lock.pin())
        };
        let retired = key_lock.unpin_and_retire();
        let pinned = user.join().expect("user");
        assert!(
            !(retired && pinned),
            "an entry out of the table was used again"
        );
        if retired {
            witness.record();
        }
    });
}

/// A transaction queues its wait while close marks the gate and wakes every
/// waiter. In every interleaving the wait is settled: close's wake reached
/// it, or the call saw the close on its way out and abandoned it, or it
/// never queued.
pub fn no_lock_wait_outlives_close() {
    explore("no_lock_wait_outlives_close", 3, 10, 1, |witness| {
        let key_lock = StdArc::new(KeyLock::for_model());
        let gate = StdArc::new(CloseGate::new());
        let (holder, waiter) = (LockOwner::new(), LockOwner::new());
        // The waiter's name; the owner itself lives until the model ends.
        let me = waiter.id();
        assert_eq!(key_lock.lock.try_lock(holder.id()), TryLock::Taken);
        let pending = StdArc::new(PendingLock::new(StdArc::clone(&key_lock)));
        let slot = StdArc::new(WaitSlot::new());
        let caller = {
            let (gate, pending, slot) = (
                StdArc::clone(&gate),
                StdArc::clone(&pending),
                StdArc::clone(&slot),
            );
            thread::spawn(move || {
                if !gate.enter() {
                    return true;
                }
                pending.note(StdArc::clone(&slot), None);
                let polled = pending.poll(me, Some(&slot));
                assert_eq!(polled, Polled::Pending, "the holder never lets go");
                let closing = gate.leave();
                if closing {
                    pending.abandon(me);
                }
                closing
            })
        };
        gate.close();
        key_lock.lock.wake_all();
        let abandoned = caller.join().expect("caller");
        assert!(abandoned || slot.is_ready(), "a lock wait outlived close");
        if !abandoned {
            witness.record();
            assert!(pending.abandon(me));
        }
        assert!(key_lock.lock.unlock(holder.id()));
    });
}

/// Mutual exclusion's calibration: a take that looks and then writes, as two
/// steps, lets two owners in.
pub fn calibration_a_take_without_a_cas_admits_two_holders() {
    explore(
        "calibration_a_take_without_a_cas_admits_two_holders",
        3,
        1,
        0,
        |_witness| {
            let holder = Arc::new(AtomicUsize::new(0));
            let inside = Arc::new(AtomicUsize::new(0));
            let take = |holder: &AtomicUsize, me: usize| {
                if holder.load(Ordering::SeqCst) == 0 {
                    holder.store(me, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            };
            let other = {
                let (holder, inside) = (Arc::clone(&holder), Arc::clone(&inside));
                thread::spawn(move || {
                    if take(&holder, 2) {
                        inside.fetch_add(1, Ordering::SeqCst);
                    }
                })
            };
            if take(&holder, 1) {
                inside.fetch_add(1, Ordering::SeqCst);
            }
            other.join().expect("owner");
            assert!(
                inside.load(Ordering::SeqCst) <= 1,
                "two owners hold one key"
            );
        },
    );
}

/// Bounded bypass's calibration: a lock whose release only ever wakes the
/// waiter to compete, and never hands over, lets a barger pass it without
/// end.
pub fn calibration_a_release_that_never_hands_off_starves_the_waiter() {
    explore(
        "calibration_a_release_that_never_hands_off_starves_the_waiter",
        3,
        1,
        0,
        |_witness| {
            let locked = Arc::new(AtomicBool::new(true));
            let lost = Arc::new(AtomicUsize::new(0));
            let nudge = Arc::new(Notify::new());
            let waiting = {
                let (locked, lost, nudge) =
                    (Arc::clone(&locked), Arc::clone(&lost), Arc::clone(&nudge));
                thread::spawn(move || {
                    // Woken by each release, it tries; a loss is only
                    // counted, and nothing is ever owed to it.
                    loop {
                        nudge.wait();
                        if locked
                            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                            .is_ok()
                        {
                            return;
                        }
                        lost.fetch_add(1, Ordering::SeqCst);
                    }
                })
            };
            for _ in 0..=MAX_BYPASS {
                locked.store(false, Ordering::SeqCst);
                nudge.notify();
                // The barger asks again at once.
                let _ = locked.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst);
            }
            // The barger is done: the lock is free, and the waiter is told.
            locked.store(false, Ordering::SeqCst);
            nudge.notify();
            waiting.join().expect("waiter");
            assert!(
                lost.load(Ordering::SeqCst) <= MAX_BYPASS as usize,
                "a waiter lost more races than the bound allows"
            );
        },
    );
}

/// No lost wake-up's calibration: a call that records how it is to be told
/// only after it polled. The release's wake falls between the two, finds
/// nothing to tell, and the wait is never completed.
pub fn calibration_a_wait_recorded_after_the_poll_is_never_told() {
    explore(
        "calibration_a_wait_recorded_after_the_poll_is_never_told",
        3,
        1,
        0,
        |_witness| {
            let key_lock = StdArc::new(KeyLock::for_model());
            let (holder, waiter) = (LockOwner::new(), LockOwner::new());
            assert_eq!(key_lock.lock.try_lock(holder.id()), TryLock::Taken);
            let mine = queue(1);
            let pending = StdArc::new(PendingLock::new(StdArc::clone(&key_lock)));
            let slot = StdArc::new(WaitSlot::new());
            let releaser = {
                let key_lock = StdArc::clone(&key_lock);
                thread::spawn(move || {
                    assert!(key_lock.lock.unlock(holder.id()));
                })
            };
            let polled = pending.poll(waiter.id(), None);
            pending.note(StdArc::clone(&slot), Some(StdArc::clone(&mine)));
            releaser.join().expect("releaser");
            if polled == Polled::Pending {
                take_in(&mine);
                let told = slot.is_ready();
                // Settle the wait before the verdict, so a failure is this
                // assertion and nothing else.
                pending.abandon(waiter.id());
                assert!(told, "a lock wait was never told");
            } else {
                key_lock.lock.unlock(waiter.id());
            }
        },
    );
}

/// The use count's calibration: a retirement that looks and then writes, as
/// two steps, retires an entry a caller took a use of in between.
pub fn calibration_a_retire_without_a_cas_retires_a_used_entry() {
    explore(
        "calibration_a_retire_without_a_cas_retires_a_used_entry",
        3,
        1,
        0,
        |_witness| {
            const RETIRED: usize = usize::MAX;
            let uses = Arc::new(AtomicUsize::new(0));
            let user = {
                let uses = Arc::clone(&uses);
                thread::spawn(move || {
                    let mut seen = uses.load(Ordering::SeqCst);
                    loop {
                        if seen == RETIRED {
                            return false;
                        }
                        match uses.compare_exchange(
                            seen,
                            seen + 1,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        ) {
                            Ok(_) => return true,
                            Err(actual) => seen = actual,
                        }
                    }
                })
            };
            let retired = uses.load(Ordering::SeqCst) == 0;
            if retired {
                uses.store(RETIRED, Ordering::SeqCst);
            }
            let pinned = user.join().expect("user");
            assert!(
                !(retired && pinned),
                "an entry out of the table was used again"
            );
        },
    );
}

/// Close's calibration: a call that does not look at the gate on its way
/// out. Close's wake comes before the wait is queued, and nothing settles
/// it.
pub fn calibration_a_wait_that_ignores_the_gate_outlives_close() {
    explore(
        "calibration_a_wait_that_ignores_the_gate_outlives_close",
        3,
        1,
        0,
        |_witness| {
            let key_lock = StdArc::new(KeyLock::for_model());
            let gate = StdArc::new(CloseGate::new());
            let (holder, waiter) = (LockOwner::new(), LockOwner::new());
            // The waiter's name; the owner itself lives until the model ends.
            let me = waiter.id();
            assert_eq!(key_lock.lock.try_lock(holder.id()), TryLock::Taken);
            let pending = StdArc::new(PendingLock::new(StdArc::clone(&key_lock)));
            let slot = StdArc::new(WaitSlot::new());
            let caller = {
                let (gate, pending, slot) = (
                    StdArc::clone(&gate),
                    StdArc::clone(&pending),
                    StdArc::clone(&slot),
                );
                thread::spawn(move || {
                    if !gate.enter() {
                        return true;
                    }
                    pending.note(StdArc::clone(&slot), None);
                    let _ = pending.poll(me, Some(&slot));
                    // The defect: the close seen on the way out is ignored.
                    let _ = gate.leave();
                    false
                })
            };
            gate.close();
            key_lock.lock.wake_all();
            let refused = caller.join().expect("caller");
            let settled = refused || slot.is_ready();
            pending.abandon(me);
            key_lock.lock.unlock(holder.id());
            assert!(settled, "a lock wait outlived close");
        },
    );
}

/// The first four's models explore every schedule at this bound: a bound of
/// 100 explores no more (7 and 12 interleavings), and a bound of 3 misses one
/// and three.
const FIRST_FOUR_PREEMPTIONS: usize = 4;

/// One thread pushes a transaction's `on_abort` callbacks into the first
/// four its claim holds while another takes them. The take hands out only
/// slots whose push it saw complete (each value lives in a loom cell, so a
/// read not ordered after its write fails the model), and it and the take
/// after the pushes hand out every entry once, oldest first.
pub fn a_take_racing_a_push_keeps_the_order() {
    explore(
        "a_take_racing_a_push_keeps_the_order",
        FIRST_FOUR_PREEMPTIONS,
        // 7 interleavings measured.
        6,
        1,
        |witness| take_racing_two_pushes(0, FirstFour::<usize>::take, witness),
    );
}

/// The same across the edge to the overflow: three entries are in place
/// before the race, so the two pushes fill the last slot and start the
/// overflow while the take runs.
pub fn a_take_racing_a_push_keeps_the_order_across_the_edge() {
    explore(
        "a_take_racing_a_push_keeps_the_order_across_the_edge",
        FIRST_FOUR_PREEMPTIONS,
        // 12 interleavings measured.
        11,
        1,
        |witness| take_racing_two_pushes(IN_PLACE - 1, FirstFour::<usize>::take, witness),
    );
}

/// The first four's calibration: a take that runs on past a slot not filled
/// yet hands a later entry out before an earlier one.
pub fn calibration_a_take_past_a_slot_not_filled_breaks_the_order() {
    explore(
        "calibration_a_take_past_a_slot_not_filled_breaks_the_order",
        FIRST_FOUR_PREEMPTIONS,
        1,
        0,
        |witness| take_racing_two_pushes(0, FirstFour::<usize>::take_past_empty, witness),
    );
}

/// `before` entries pushed, then two more pushed on another thread while
/// this one takes with `take`, then a plain take of what is left. Recorded
/// when the racing take handed out the first of the two and met the slot of
/// the second before its push.
fn take_racing_two_pushes(
    before: usize,
    take: for<'a> fn(&'a FirstFour<usize>) -> TakeFirst<'a, usize>,
    witness: &Witness,
) {
    let list = Arc::new(FirstFour::default());
    for entry in 0..before {
        list.push(entry);
    }
    let pusher = {
        let list = Arc::clone(&list);
        thread::spawn(move || {
            list.push(before);
            list.push(before + 1);
        })
    };
    let raced: Vec<usize> = take(&list).collect();
    pusher.join().expect("pusher");
    if raced.len() == before + 1 {
        witness.record();
    }
    let all: Vec<usize> = raced.into_iter().chain(list.take()).collect();
    assert!(
        all == (0..before + 2).collect::<Vec<_>>(),
        "a take handed an entry out of order or not exactly once: {all:?}"
    );
}
