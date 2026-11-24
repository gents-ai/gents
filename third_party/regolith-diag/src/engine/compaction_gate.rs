//! The compaction gate: what keeps a compaction pass and the operations that
//! must not see a table replaced underneath them from running at once.
//!
//! A compaction pass, on a worker or on a caller's thread, holds the gate
//! shared, so passes run side by side; they keep off each other's tables
//! through their claims (`compaction/claims.rs`). `compact_range`, an
//! ingest, a checkpoint, `drop_all` and a backup's capture hold it alone.
//! Reads, writes and flushes never touch it.
//!
//! # Fair both ways (E28)
//!
//! The gate is a [`regolith::sync::RwLock`](crate::sync::RwLock), so it
//! keeps that lock's policy (D49): whoever finds it free takes it, even
//! past waiters, and a waiter passed over [`MAX_BYPASS`](crate::sync::MAX_BYPASS)
//! times is handed it next, with nobody getting in ahead meanwhile. That
//! bounds the wait on both sides. The gate it replaced let a waiting
//! exclusive user hold back every new pass, so a stream of ingests or
//! checkpoints whose waits overlapped kept the compaction worker out for as
//! long as the stream lasted, and writers stalled behind the L0 it no
//! longer compacted (E28). Now a pass waits at most for the exclusive users
//! queued ahead of it and [`MAX_BYPASS`](crate::sync::MAX_BYPASS) more, and
//! an exclusive user waits likewise for passes. `CompactionGate.tla` and
//! `CompactionGate.lean` model it, with that old policy as a RED.
//!
//! The lock is built without spin (E48): a contended caller queues at once.
//! The gate is held for a whole compaction pass, a job or a checkpoint's
//! copy, far longer than a spin could wait out, so a spin would only burn
//! the caller's time; and the queue is where the gate's bounded waits and
//! `CompactionGate.tla` place every waiter.
//!
//! # Who waits, and how
//!
//! - **A compaction worker** waits on its own thread: [`CompactionGate::shared`]
//!   for a pass, [`CompactionGate::exclusive`] for a job it runs.
//! - **A call that chose to block** (`drop_all`, a backup, a job run inline
//!   or by [`JobTicket::wait`](crate::JobTicket::wait)) waits on its own
//!   thread through [`CompactionGate::exclusive`].
//! - **A job on a caller's [`IoQueue`](crate::IoQueue)** never waits: it
//!   polls an [`Admission`], which keeps its place among the waiters, and
//!   parks on the queue while it is pending. The gate wakes the queue's
//!   owner once the job may go on, and the next poll runs it.
//! - **A pass a caller's call runs** ([`crate::Db::compact_step`], a step a
//!   write owes) only tries, and reports
//!   [`CompactionOutcome::Contended`](crate::CompactionOutcome::Contended)
//!   when the gate is held alone or owed to an exclusive user.

use std::future::Future as _;
use std::io;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use crate::sync::internal::wait_here;
use crate::sync::{OwnedRwLockWriteGuard, RwLock, RwLockReadGuard, WriteOwned};

/// The gate, shared by the engine and its compaction workers.
#[derive(Clone)]
pub(crate) struct CompactionGate(Arc<RwLock<()>>);

impl Default for CompactionGate {
    fn default() -> Self {
        Self(Arc::new(RwLock::new_without_spin(())))
    }
}

/// A compaction pass's share of the gate, released on drop.
pub(crate) type Shared<'a> = RwLockReadGuard<'a, ()>;

/// Sole hold of the gate, released on drop. It owns its hold, so a job can
/// keep it across the calls that make up its work, and a checkpoint for as
/// long as it copies.
pub(crate) type Exclusive = OwnedRwLockWriteGuard<()>;

impl CompactionGate {
    /// A gate nobody holds.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A pass's share at once, or `None` while an exclusive user holds the
    /// gate or is owed it.
    pub(crate) fn try_shared(&self) -> Option<Shared<'_>> {
        self.0.try_read()
    }

    /// A pass's share, waited for on this thread: a compaction worker's.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn shared(&self) -> Shared<'_> {
        super::io::scope::waits("a share of the compaction gate");
        crate::sync::internal::block_on(self.0.read())
    }

    /// Sole hold, waited for on this thread: a worker's job, or a call that
    /// chose to block. Fails only on `wasm32`, when this same thread already
    /// holds the gate further up its stack.
    pub(crate) fn exclusive(&self) -> io::Result<Exclusive> {
        self.admit().wait()
    }

    /// The lock under the gate, for a model that polls a pass's wait by hand.
    #[cfg(loom)]
    pub(crate) fn lock(&self) -> &RwLock<()> {
        &self.0
    }

    /// A wait for sole hold that a job polls from its queue, never blocking.
    pub(crate) fn admit(&self) -> Admission {
        Admission(self.0.write_owned())
    }
}

/// A pending wait for sole hold of the gate. It keeps its place among the
/// waiters and the bypasses it counted for as long as it lives; dropping it
/// withdraws the wait, and passes on a handoff made to it meanwhile.
///
/// Once it was passed over [`MAX_BYPASS`](crate::sync::MAX_BYPASS) times the
/// gate is handed to it at the next release, whether or not anything polls
/// it then (D49). A job parked on a queue therefore holds the gate from
/// that handoff until its owner's next poll runs it: the handoff wakes an
/// idle owner, and an owner that drops the queue or closes the database
/// gives the gate back, but one that keeps the queue and stops polling
/// keeps every other user of the gate waiting.
pub(crate) struct Admission(WriteOwned<()>);

impl Admission {
    /// The hold, once the gate admits it; otherwise `Pending`, with `waker`
    /// woken when it may admit it.
    pub(crate) fn poll(&mut self, waker: &Waker) -> Poll<Exclusive> {
        std::pin::Pin::new(&mut self.0).poll(&mut Context::from_waker(waker))
    }

    /// The hold, waited for on this thread. See [`CompactionGate::exclusive`].
    pub(crate) fn wait(self) -> io::Result<Exclusive> {
        super::io::scope::waits("sole hold of the compaction gate");
        wait_here(self.0)
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::sync::MAX_BYPASS;
    use crate::sync::test_support::Polled;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// How long a stream runs at most before a test gives up on the waiter
    /// it keeps out: far past the few scheduling rounds the waiter needs.
    const STREAM_DEADLINE: Duration = Duration::from_secs(if cfg!(miri) { 300 } else { 30 });

    /// E48. The gate's lock never spins: a contended caller queues at once.
    /// Its budget is zero from the start and stays zero through a pass that
    /// waits behind a job, the job's release, and the pass's retry.
    #[test]
    fn the_gate_never_spins() {
        let gate = CompactionGate::new();
        assert_eq!(gate.0.spin_rounds(), 0);
        let job = gate.exclusive().expect("free");
        let mut pass = Polled::new(gate.0.read());
        pass.pending();
        drop(job);
        assert_eq!(pass.wakes(), 1, "the job's release wakes the queued pass");
        drop(pass.ready());
        let mut waiting = admission(&gate);
        drop(waiting.ready());
        assert_eq!(gate.0.spin_rounds(), 0, "the gate never spins");
    }

    /// An admission polled by hand.
    fn admission(gate: &CompactionGate) -> Polled<impl std::future::Future<Output = Exclusive>> {
        let mut admission = gate.admit();
        Polled::new(std::future::poll_fn(move |cx| admission.poll(cx.waker())))
    }

    #[test]
    fn passes_share_and_an_exclusive_hold_excludes_them() {
        let gate = CompactionGate::new();
        let a = gate.try_shared().expect("free");
        let b = gate.try_shared().expect("passes share");
        let mut waiting = admission(&gate);
        waiting.pending();
        drop((a, b));
        let held = waiting.ready();
        assert!(gate.try_shared().is_none(), "a pass is kept out");
        drop(held);
        assert!(gate.try_shared().is_some());
    }

    /// E28, step by step: a fresh exclusive user takes the gate back each
    /// time it frees, past the pass that waits. The pass counts each loss and
    /// is handed the gate once it lost `MAX_BYPASS` times; the gate this
    /// replaced held every new pass back while any exclusive user waited.
    #[test]
    fn a_waiting_pass_is_bypassed_at_most_max_bypass_times() {
        let gate = CompactionGate::new();
        let mut holder = admission(&gate).ready();
        let mut pass = Polled::new(gate.0.read());
        pass.pending();
        let mut bypassed = 0;
        loop {
            let mut next = admission(&gate);
            drop(holder);
            match next.poll() {
                Poll::Ready(taken) => {
                    bypassed += 1;
                    assert!(
                        bypassed <= MAX_BYPASS,
                        "the pass was bypassed {bypassed} times"
                    );
                    holder = taken;
                    pass.pending();
                }
                Poll::Pending => break,
            }
        }
        assert_eq!(
            bypassed, MAX_BYPASS,
            "barging stopped only once the pass was owed"
        );
        drop(pass.ready());
    }

    /// The other way, step by step: passes that keep the gate shared, each
    /// entering before the last leaves, let a waiting exclusive user in once
    /// it lost `MAX_BYPASS` times.
    #[test]
    fn a_waiting_exclusive_user_is_bypassed_at_most_max_bypass_times() {
        let gate = CompactionGate::new();
        let mut inside = gate.try_shared().expect("free");
        let mut user = admission(&gate);
        user.pending();
        let mut bypassed = 0;
        while let Some(next) = gate.try_shared() {
            drop(inside);
            inside = next;
            user.pending();
            bypassed += 1;
            assert!(bypassed <= MAX_BYPASS, "bypassed {bypassed} times");
        }
        assert_eq!(bypassed, MAX_BYPASS);
        drop(inside);
        drop(user.ready());
    }

    /// E28 under threads: exclusive users that keep taking the gate back,
    /// their waits overlapping, still let a waiting pass in while they go on.
    /// With the gate this replaced the pass got in only once the stream
    /// stopped, at the deadline.
    #[test]
    fn a_stream_of_exclusive_users_lets_a_waiting_pass_in() {
        stream_lets_the_waiter_in(Side::Exclusive);
    }

    /// Phase fairness the other way, under threads.
    #[test]
    fn a_stream_of_passes_lets_a_waiting_exclusive_user_in() {
        stream_lets_the_waiter_in(Side::Shared);
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Side {
        Shared,
        Exclusive,
    }

    /// Three threads take `stream`'s side of the gate over and over, each
    /// holding it long enough that the next is already waiting when it lets
    /// go, until the waiter on the other side got in or the deadline passed.
    fn stream_lets_the_waiter_in(stream: Side) {
        const THREADS: usize = 3;
        // How many holds the stream makes before the waiter arrives: fewer
        // under miri, which runs each a hundred times slower.
        const UNDER_WAY: usize = if cfg!(miri) { 4 } else { 64 } * THREADS;
        let gate = CompactionGate::new();
        let taken = AtomicUsize::new(0);
        let waiter_in = AtomicBool::new(false);
        let deadline = Instant::now() + STREAM_DEADLINE;
        let mut in_before_deadline = false;
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    while !waiter_in.load(Ordering::SeqCst) && Instant::now() < deadline {
                        let hold = || {
                            taken.fetch_add(1, Ordering::SeqCst);
                            for _ in 0..256 {
                                std::hint::spin_loop();
                            }
                        };
                        match stream {
                            Side::Shared => {
                                let _pass = gate.shared();
                                hold();
                            }
                            Side::Exclusive => {
                                let _held = gate.exclusive().expect("native");
                                hold();
                            }
                        }
                    }
                });
            }
            // Wait until the stream is under way, by the deadline.
            while taken.load(Ordering::SeqCst) < UNDER_WAY && Instant::now() < deadline {
                std::hint::spin_loop();
            }
            match stream {
                Side::Shared => drop(gate.exclusive().expect("native")),
                Side::Exclusive => drop(gate.shared()),
            }
            in_before_deadline = Instant::now() < deadline;
            waiter_in.store(true, Ordering::SeqCst);
        });
        assert!(
            taken.load(Ordering::SeqCst) >= UNDER_WAY,
            "the stream never got under way"
        );
        assert!(
            in_before_deadline,
            "the waiter got in only once the stream stopped at the deadline: it starved"
        );
    }

    #[test]
    fn a_dropped_admission_handed_the_gate_passes_it_on() {
        let gate = CompactionGate::new();
        let mut inside = gate.try_shared().expect("free");
        let mut user = admission(&gate);
        user.pending();
        for _ in 0..MAX_BYPASS {
            let next = gate.try_shared().expect("a pass barges");
            drop(inside);
            inside = next;
            user.pending();
        }
        assert!(gate.try_shared().is_none(), "the admission is owed now");
        // Handed the gate as the last pass leaves, but its job is settled
        // before it runs.
        drop(inside);
        drop(user);
        assert!(
            gate.try_shared().is_some(),
            "the handoff went back to the gate"
        );
    }
}
