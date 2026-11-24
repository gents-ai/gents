//! Tracks the sequence numbers of every live snapshot so that compaction
//! can decide which older versions of a user key are safe to drop.
//!
//! Every [`Db::snapshot`](crate::Db::snapshot) and every transaction begin
//! pins a sequence here, and the handle releases it when it drops.
//! [`SnapshotRegistry::live_seqs`] lists the pinned sequences. Compaction
//! cuts a key's versions into stripes at them: inside a stripe only the
//! newest state is visible to any reader, so what lies beneath it can be
//! discarded and its operands folded, while nothing crosses a stripe
//! boundary (see `engine::compaction::stripes`).
//!
//! # Per-thread slots (plan 4.6, D54)
//!
//! There is no lock. Each thread number (`crate::per_thread`) owns a
//! cache-padded slot, and a thread announces the sequences it pins in its
//! own slot's entries (`chain`). An entry holds one sequence and a count
//! of the pins on it, so many snapshots at one sequence share one entry. A
//! pin records which slot and entry it is in, and its release goes exactly
//! there, on whatever thread drops the handle: a `Snapshot` or a
//! `Transaction` created on one thread and dropped on another releases
//! exactly what it pinned. Threads past the pool of numbers share slots;
//! every entry update is an atomic read-modify-write, so sharing costs
//! contention, never correctness.
//!
//! # The protocol (proofs/tla/SnapshotRegistry.tla)
//!
//! Registration, in the calling thread's slot:
//! 1. **announce**: read the horizon `h` and put `h` in an entry: join the
//!    slot's last entry if it announces `h`, otherwise claim a free one;
//! 2. **sample**: a `SeqCst` fence, then read the horizon again;
//! 3. **confirm**: if it still reads `h`, the snapshot is live at `h`.
//!    Otherwise take the announce back and announce the value just read.
//!
//! A compaction samples (a `SeqCst` fence) once its inputs are fixed, then
//! reads every slot. A clone adds a pin to the entry of the pin it copies,
//! which holds the sequence already, so it needs no confirm of its own.
//!
//! # Why it holds
//!
//! An entry's sequence does not change while it holds a pin (E1 in
//! `chain`), so a scan that reads an entry after a snapshot's announce
//! lists the snapshot's sequence. A scan that read it before the announce
//! put its fence before the registration's fence in the single order of
//! `SeqCst` fences; the registration's confirming read therefore sees every
//! sequence published before the scan's fence, so the snapshot reads at or
//! above every version the compaction's inputs hold. Either way the
//! snapshot loses nothing. Without the confirm, a reader could announce a
//! horizon read long ago after a scan passed its slot
//! (`MC_SnapshotRegistry_Red_NoConfirm`).
//!
//! # Slack
//!
//! The list never misses a live snapshot, and holds nothing but sequences
//! some registration read from the horizon and announced. It may hold more
//! than the live set: a pin released while the scan runs, or a registration
//! whose confirm is about to fail. So the minimum a compaction uses is at
//! most the oldest live snapshot, and below it only by sequences that were
//! announced during the scan.
//!
//! # Progress and cost
//!
//! - A release is one `fetch_sub`, plus one `fetch_and` when it frees the
//!   entry: wait-free.
//! - A clone is one `fetch_add`: wait-free.
//! - A registration is one join or claim in its own slot, a fence and two
//!   horizon loads. It repeats only when a commit publishes between those
//!   two loads, so it is lock-free: each retry is another thread's commit
//!   making progress. In a shared slot a claim also retries its
//!   compare-and-swap when another thread changed the same word.
//! - A scan reads every slot: the width of the pool plus each slot's
//!   chunks, touching only announced entries.
//!
//! # Memory
//!
//! A slot allocates its first chunk on its first pin and appends one when
//! every entry is taken; chunks stay until the registry drops. The total is
//! the peak number of distinct sequences each slot held at once, rounded up
//! to a chunk.
//!
//! # Waiting for the last snapshot
//!
//! [`SnapshotRegistry::wait_until_drained`] parks the calling thread until
//! every entry is free, with no mutex and no condition variable: the
//! waiter marks each chunk watched in the same read-modify-write that reads
//! its taken bits, and a release that frees an entry learns of the mark in
//! the read-modify-write that clears the entry's bit, then wakes the
//! waiters through a [`Notify`]. [`SnapshotRegistry::until_drained`] is the
//! same wait for a task: it awaits the notification instead of parking.

mod chain;
#[cfg(all(test, not(loom)))]
mod model_tests;
#[cfg(all(test, not(loom)))]
mod tests;

use std::ops::ControlFlow;
use std::sync::Arc;

use kovan::CachePadded;

pub(crate) use self::chain::{Announced, At};
use self::chain::{Join, NO_TIME, Slot};
use super::read_horizon::ReadHorizon;
use crate::env::Env;
use crate::sync::internal::{AtomicUsize, Ordering, fence};
use crate::sync::{Notified, Notify};

/// Thread-safe registry of live snapshot sequence numbers. See the module
/// documentation.
pub(crate) struct SnapshotRegistry {
    /// One per thread number; the length is a power of two.
    slots: Box<[CachePadded<Slot>]>,
    /// Waits in progress: threads inside [`Self::wait_until_drained`] and
    /// tasks awaiting [`Self::until_drained`]. A release that frees an
    /// entry in a watched chunk wakes the waiters only when this is nonzero.
    watchers: AtomicUsize,
    /// Woken when an entry in a watched chunk goes free.
    drained: Notify,
    /// Wakes actually issued. Test-only: it is how a test proves the
    /// no-waiter path issued none and the waiter path issued one.
    #[cfg(test)]
    wakes: AtomicUsize,
    env: Arc<dyn Env>,
}

/// Where a registered snapshot's pin is announced, or nothing for a handle
/// that pinned nothing (one taken on a closed database).
///
/// Neither `Clone` nor `Copy`: a pin is released exactly once, by moving it
/// into [`SnapshotRegistry::release`]. A copy of the snapshot takes its own
/// pin with [`SnapshotRegistry::clone_pin`].
#[derive(Debug, Default)]
#[must_use = "a pin never released keeps compaction from dropping the versions it holds"]
pub(crate) struct SnapshotPin(Option<PinAt>);

#[derive(Debug, Clone, Copy)]
struct PinAt {
    slot: u32,
    at: At,
}

impl SnapshotPin {
    /// A pin that holds nothing: releasing it does nothing.
    pub(crate) fn none() -> Self {
        Self(None)
    }

    /// Takes the pin out, leaving one that holds nothing.
    pub(crate) fn take(&mut self) -> Self {
        Self(self.0.take())
    }

    /// Whether the pin holds an entry.
    pub(crate) fn is_registered(&self) -> bool {
        self.0.is_some()
    }

    /// The pin a [`SnapshotRegistry::announce`] in slot `slot` made, for a
    /// caller that runs the protocol's steps itself (tests and loom models)
    /// and confirms or takes it back.
    #[cfg(any(test, loom))]
    pub(crate) fn announced(slot: u32, at: At) -> Self {
        Self(Some(PinAt { slot, at }))
    }

    /// The slot and entry the pin holds. Model use only.
    #[cfg(loom)]
    pub(crate) fn location(&self) -> Option<(u32, At)> {
        self.0.map(|held| (held.slot, held.at))
    }
}

impl SnapshotRegistry {
    /// A registry with one slot per thread number, whose timestamps come
    /// from `env`.
    pub(crate) fn with_env(env: Arc<dyn Env>) -> Self {
        Self::with_width(env, crate::per_thread::width())
    }

    /// A registry with `width` slots (rounded up to a power of two).
    pub(crate) fn with_width(env: Arc<dyn Env>, width: usize) -> Self {
        let width = width.max(1).next_power_of_two();
        Self {
            slots: (0..width).map(|_| CachePadded::new(Slot::new())).collect(),
            watchers: AtomicUsize::new(0),
            drained: Notify::new(),
            #[cfg(test)]
            wakes: AtomicUsize::new(0),
            env,
        }
    }

    /// A registry timed by the standard environment.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_env(crate::env::std_env())
    }

    /// Pins a snapshot at the current horizon, in the calling thread's
    /// slot. Returns the pinned sequence and the pin, which must be released
    /// with [`Self::release`].
    ///
    /// A compaction that reads the live list after this returns lists the
    /// sequence; one that read it earlier saw a horizon no newer than it
    /// (see the module documentation).
    pub(crate) fn pin(&self, horizon: &ReadHorizon) -> (u64, SnapshotPin) {
        self.pin_in(crate::per_thread::index(), horizon)
    }

    /// [`Self::pin`] in slot `slot` (taken modulo the width).
    pub(crate) fn pin_in(&self, slot: usize, horizon: &ReadHorizon) -> (u64, SnapshotPin) {
        let slot = self.slot_no(slot);
        let mut seq = horizon.visible();
        loop {
            let at = self.announce(slot, seq);
            match self.confirm(horizon, seq) {
                Ok(()) => return (seq, SnapshotPin(Some(PinAt { slot, at }))),
                Err(now) => {
                    self.unpin(slot, at);
                    seq = now;
                }
            }
        }
    }

    /// Announce: put `seq` in an entry of slot `slot`, joining the slot's
    /// last entry when it announces `seq`, else claiming a free one.
    pub(crate) fn announce(&self, slot: u32, seq: u64) -> At {
        let lane = &self.slots[slot as usize];
        if let Some(at) = lane.hint()
            && let Some(chunk) = lane.chunk_at(at.depth)
        {
            match chunk.join(at.index, seq) {
                Join::Joined => return at,
                Join::Missed => {}
                Join::Undone(watched) => self.freed(watched),
            }
        }
        lane.claim(seq, || self.env.unix_secs().unwrap_or(NO_TIME))
    }

    /// Sample and confirm: `Ok` when the horizon still reads `seq` after
    /// the fence, else the value it reads now.
    pub(crate) fn confirm(&self, horizon: &ReadHorizon, seq: u64) -> Result<(), u64> {
        fence(Ordering::SeqCst);
        let now = horizon.visible();
        if now == seq { Ok(()) } else { Err(now) }
    }

    /// A pin for a copy of the snapshot `pin` holds: one more count on the
    /// same entry, which already announces the sequence, so the copy is
    /// covered from the moment the original was. Wait-free.
    pub(crate) fn clone_pin(&self, pin: &SnapshotPin) -> SnapshotPin {
        let Some(held) = pin.0 else {
            return SnapshotPin::none();
        };
        if let Some(chunk) = self.slots[held.slot as usize].chunk_at(held.at.depth) {
            chunk.add_pin(held.at.index);
            SnapshotPin(Some(held))
        } else {
            debug_assert!(false, "a pin names a chunk its slot never had");
            SnapshotPin::none()
        }
    }

    /// Releases `pin` exactly where it was announced, on any thread.
    pub(crate) fn release(&self, pin: SnapshotPin) {
        if let Some(held) = pin.0 {
            self.unpin(held.slot, held.at);
        }
    }

    fn unpin(&self, slot: u32, at: At) {
        // Chunks are never unlinked (E3 in `chain`), so the chunk a pin
        // names is always there.
        if let Some(chunk) = self.slots[slot as usize].chunk_at(at.depth) {
            let watched = chunk.unpin(at.index);
            self.freed(watched);
        } else {
            debug_assert!(false, "a pin names a chunk its slot never had");
        }
    }

    /// An entry went free; `watched` when its chunk is watched.
    fn freed(&self, watched: bool) {
        if watched && self.watchers.load(Ordering::Acquire) > 0 {
            #[cfg(test)]
            self.wakes.fetch_add(1, Ordering::Relaxed);
            self.drained.notify_waiters();
        }
    }

    fn slot_no(&self, slot: usize) -> u32 {
        (slot & (self.slots.len() - 1)) as u32
    }

    /// The compaction's sample: a `SeqCst` fence, so a registration whose
    /// announce a later scan misses confirms against a horizon at least as
    /// new as every publish before this point.
    pub(crate) fn sample(&self) {
        fence(Ordering::SeqCst);
    }

    /// Calls `each` for every announced entry of slot `slot`.
    pub(crate) fn scan_slot(&self, slot: usize, each: &mut impl FnMut(Announced)) {
        for chunk in self.slots[slot].chunks() {
            chunk.each_announced(each);
        }
    }

    /// Samples, then calls `each` for every announced entry of every slot.
    fn scan(&self, mut each: impl FnMut(Announced)) {
        self.sample();
        for slot in 0..self.slots.len() {
            self.scan_slot(slot, &mut each);
        }
    }

    /// The smallest pinned seq, or `u64::MAX` if no snapshot is live. Within
    /// the slack the module documentation states.
    pub(crate) fn oldest_live_seq(&self) -> u64 {
        let mut oldest = u64::MAX;
        self.scan(|entry| oldest = oldest.min(entry.seq));
        oldest
    }

    /// Every seq a snapshot is pinned at, ascending and distinct; empty
    /// when no snapshot is live. Compaction cuts a key's versions into
    /// stripes at these seqs.
    ///
    /// A pass reads this once its input tables are fixed, never before,
    /// and keeps the list for the whole merge; `perform_compaction_to`
    /// says why that loses no snapshot's version. A snapshot released
    /// after the read only makes the pass fold less than it could.
    pub(crate) fn live_seqs(&self) -> Vec<u64> {
        let mut live = Vec::new();
        self.scan(|entry| live.push(entry.seq));
        live.sort_unstable();
        live.dedup();
        #[cfg(test)]
        if let Some(hook) = AFTER_LIVE_READ.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        live
    }

    /// Number of live pins. Counts pins, not distinct seqs: two snapshots
    /// taken at the same seq contribute two. A registration in flight may
    /// be counted.
    pub(crate) fn live_count(&self) -> u64 {
        let mut pins = 0u64;
        self.scan(|entry| pins = pins.saturating_add(entry.pins));
        pins
    }

    /// Unix-seconds timestamp when the entry holding the oldest live
    /// sequence was first claimed.
    ///
    /// `None` when no snapshot is alive, and also `None` when the
    /// environment has no wall clock: regolith reports "not known" rather
    /// than inventing an epoch timestamp. Used to populate the
    /// `regolith.oldest-snapshot-time` property.
    pub(crate) fn oldest_snapshot_time_unix(&self) -> Option<u64> {
        let mut oldest: Option<(u64, u64)> = None;
        self.scan(|entry| {
            let candidate = (entry.seq, entry.since);
            oldest = Some(oldest.map_or(candidate, |best| best.min(candidate)));
        });
        oldest.and_then(|(_, since)| (since != NO_TIME).then_some(since))
    }

    /// Block until no snapshot is pinned, or until `timeout` elapses.
    ///
    /// Returns the number of pins still live, so `0` means the wait
    /// succeeded and anything else is what was still outstanding when
    /// the deadline passed. The caller decides whether that is an error.
    ///
    /// Waits for the release itself rather than polling: the thread parks
    /// and the release that frees the last entry it saw taken unparks it.
    /// No mutex and no condition variable are involved (see the module
    /// documentation).
    ///
    /// Time is read through the env (E15): `Instant::now` panics on
    /// `wasm32-unknown-unknown`. On a target with one thread no other thread
    /// can release a pin while this one waits, so it reports what is still
    /// pinned at once instead of waiting on a signal nobody can send.
    pub(crate) fn wait_until_drained(&self, timeout: std::time::Duration) -> u64 {
        self.wait_until_drained_on(timeout, crate::env::PLATFORM_THREADS)
    }

    /// [`Self::wait_until_drained`] on a target that has other threads
    /// (`threads`) or only this one.
    fn wait_until_drained_on(&self, timeout: std::time::Duration, threads: bool) -> u64 {
        if !threads {
            return self.live_count();
        }
        let timeout_micros = u64::try_from(timeout.as_micros()).unwrap_or(u64::MAX);
        // `None` where the env has no clock: the wait is then one wait of
        // the whole timeout, ended early only by a release.
        let deadline = self
            .env
            .now_micros()
            .map(|now| now.saturating_add(timeout_micros));
        let waker = std::task::Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut cx = std::task::Context::from_waker(&waker);
        let mut waited = false;
        self.wait_drained(|notified, pins| {
            let remaining = match deadline {
                Some(deadline) => match self.env.now_micros() {
                    Some(now) if now < deadline => std::time::Duration::from_micros(deadline - now),
                    _ => return ControlFlow::Break(pins),
                },
                None if waited => return ControlFlow::Break(pins),
                None => timeout,
            };
            waited = true;
            if std::future::Future::poll(notified, &mut cx).is_pending() {
                std::thread::park_timeout(remaining);
            }
            ControlFlow::Continue(())
        })
    }

    /// The drain wait's protocol, with the waiting itself left to `park`.
    ///
    /// Each round creates a notification future, then marks every chunk
    /// watched and reads it; when an entry is still taken it hands `park`
    /// the future and the pins outstanding. `park` waits for the future, or
    /// for as long as it chooses, and breaks with the count to give up. A
    /// release that frees an entry the round saw taken sees the mark and
    /// bumps the generation the future compares against, so the future is
    /// ready however the two interleave: no wake is lost.
    pub(crate) fn wait_drained(
        &self,
        mut park: impl FnMut(std::pin::Pin<&mut Notified<'_>>, u64) -> ControlFlow<u64>,
    ) -> u64 {
        let _watching = Watching::enter(&self.watchers);
        loop {
            let (notified, taken, pins) = self.round();
            if !taken {
                break 0;
            }
            let mut notified = std::pin::pin!(notified);
            if let ControlFlow::Break(pins) = park(notified.as_mut(), pins) {
                break pins;
            }
        }
    }

    /// The same rounds as [`Self::wait_drained`], with the waiting left to
    /// the task that awaits this: over once no snapshot is pinned.
    ///
    /// Nothing blocks and no clock is read. The release that frees the last
    /// entry a round saw taken wakes the task, on the releasing thread.
    /// Dropping the wait half way counts the waiter out and withdraws its
    /// notification, so a release after that wakes nobody for it.
    pub(crate) async fn until_drained(&self) {
        let _watching = Watching::enter(&self.watchers);
        loop {
            let (notified, taken, _) = self.round();
            if !taken {
                return;
            }
            notified.await;
        }
    }

    /// One round of the drain wait: the notification, then whether any entry
    /// is taken and the pins outstanding. The notification is created before
    /// the chunks are marked and read, which is what makes a release that
    /// lands in between show in it (`MC_SnapshotWait_Red_FutureAfterWatch`).
    fn round(&self) -> (Notified<'_>, bool, u64) {
        let notified = self.drained.notified();
        let (taken, pins) = self.watch();
        (notified, taken, pins)
    }

    /// Marks every chunk watched and reads it: whether any entry is taken,
    /// and the pins announced.
    fn watch(&self) -> (bool, u64) {
        let mut taken = false;
        let mut pins = 0u64;
        for slot in self.slots.iter() {
            for chunk in slot.chunks() {
                taken |= chunk.watch(&mut |entry: Announced| {
                    pins = pins.saturating_add(entry.pins);
                });
            }
        }
        (taken, pins)
    }

    /// The number of slots.
    #[cfg(test)]
    pub(crate) fn width(&self) -> usize {
        self.slots.len()
    }

    /// The notification drain waiters wait on, for a loom calibration that
    /// waits on it without marking any chunk.
    #[cfg(loom)]
    pub(crate) fn drained(&self) -> &Notify {
        &self.drained
    }

    /// How many chunks slot `slot`'s chain holds. Model use only.
    #[cfg(loom)]
    pub(crate) fn chunks_in(&self, slot: usize) -> usize {
        self.slots[slot].chunks().count()
    }

    /// Test-only: number of distinct seq values currently pinned.
    #[cfg(test)]
    pub(crate) fn pin_count(&self) -> usize {
        self.live_seqs().len()
    }

    /// Test-only: threads currently inside `wait_until_drained`.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.watchers.load(Ordering::Acquire)
    }

    /// Test-only: wakes issued so far.
    #[cfg(test)]
    pub(crate) fn wakes_issued(&self) -> usize {
        self.wakes.load(Ordering::Relaxed)
    }
}

/// One waiter counted in [`SnapshotRegistry::watchers`] for as long as this
/// lives, so a wait that ends early, by a timeout, a drop or a panic, is
/// counted out like one that ran to its end.
struct Watching<'a>(&'a AtomicUsize);

impl<'a> Watching<'a> {
    fn enter(watchers: &'a AtomicUsize) -> Self {
        watchers.fetch_add(1, Ordering::AcqRel);
        Self(watchers)
    }
}

impl Drop for Watching<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Unparks the thread waiting in [`SnapshotRegistry::wait_until_drained`].
struct Unpark(std::thread::Thread);

impl std::task::Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

#[cfg(test)]
thread_local! {
    /// Test seam: runs once, on this thread, right after the next
    /// [`SnapshotRegistry::live_seqs`] has taken its list. Thread-local so a
    /// parallel test never fires another test's hook.
    static AFTER_LIVE_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only: run `hook` once, on this thread, right after the next
/// [`SnapshotRegistry::live_seqs`] has taken its list.
///
/// A pin the hook registers is missing from that list. That is how a test
/// puts a snapshot, and the flush of a newer version beside it, after a
/// compaction pass has read the live snapshots.
#[cfg(test)]
pub(crate) fn after_next_live_read(hook: impl FnOnce() + 'static) {
    AFTER_LIVE_READ.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}
