//! The owner's side of a per-thread I/O queue.

use crate::portability::{AtomicU64, Ordering};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::sync::Arc;
use std::task::Waker;
use std::time::Duration;

use super::ledger::Ledger;
use super::timers::Timers;
use super::{IoBudget, IoProgress, QueueId};
use crate::engine::block_cache::BlockCache;
use crate::engine::io::IoRuntime;
use crate::engine::io::job::{Delivery, Job};
use crate::engine::io::shared::{Landing, Message, QueueShared, WaitSlot};
use crate::engine::io::unit::{Outcome, Ran, Unit};
use crate::env::Env;

/// Queue ids are unique in the process, so a handle that names a queue of
/// another database is caught instead of read through a stranger's queue.
static NEXT_QUEUE: AtomicU64 = AtomicU64::new(1);

/// One thread's pending I/O: the blocks its `CacheOnly` handles missed, the
/// fsyncs its [`commit_nowait`](crate::Transaction::commit_nowait) commits
/// owe, the stalls its writes wait out, the background work its calls
/// left owing with no worker, and the timed waits of its
/// [`transact_async`](crate::OptimisticTransactionDb::transact_async)
/// attempts. Made by [`Db::io_queue`](crate::Db::io_queue).
///
/// The thread that holds the queue polls it, and only that thread: it is
/// `Send`, so it can move to the thread that will own it, but not `Sync`, so
/// two threads cannot poll it at once. Handles name it by [`IoQueue::id`] in
/// [`ReadMode::CacheOnly`](crate::ReadMode::CacheOnly); a read through such a
/// handle may run on any thread, and its miss is recorded here all the same.
///
/// [`poll`](Self::poll) runs the reads and jobs this thread waits on and
/// delivers the completions other threads pushed. Every completion of work
/// recorded here arrives here and nowhere else, and wakes only the waits
/// recorded here: a read's [`IoWait`](crate::IoWait), a commit's
/// [`CommitTicket`](crate::CommitTicket), a stall's
/// [`StallWait`](crate::StallWait), a job's [`JobTicket`](crate::JobTicket).
/// A thread with nothing to run registers [`idle_waker`](Self::idle_waker)
/// and idles; the next completion pushed to it wakes it, once.
///
/// A timed wait is the one thing no other thread completes: it is over at
/// the first poll that finds the database's clock at its due time. Each
/// poll says how long until the earliest one
/// ([`IoProgress::wake_in`](crate::IoProgress::wake_in)), and an owner that
/// idles polls again by then. A queue that holds none reads no clock.
///
/// The queue is its thread's for the I/O regolith starts itself on that
/// thread's calls: made on a thread, or polled on one, it takes that
/// thread's owed steps, stalls and foreground jobs. A thread with no queue
/// open runs that I/O inline, bounded.
///
/// What a queue owes is bounded by bytes: once the reads it waits on reach
/// the bound (a few hundred blocks of the database's block size), a further
/// miss waits for room instead of adding a read, and records its read when it
/// is run again. One read larger than the bound is still admitted when the
/// queue owes nothing else. What landed for the queue is kept within the
/// same bound, except what a call run again still needs: a call that reads
/// more blocks at once than the bound holds still finishes when it is run
/// again after each poll (`ledger.rs`).
///
/// Dropping the queue completes every wait recorded on it, on the dropping
/// thread; a read run again through a handle that still names it fails with
/// [`Error::InvalidArgument`](crate::Error::InvalidArgument). A read the
/// queue had not finished is closed, and any other queue waiting on it runs
/// its own read again. A commit's fsync the queue waits on is run here if
/// nobody runs it yet, and waited for if another thread does, so every
/// ticket on the queue still gets its true outcome and every callback on it
/// still runs exactly once. A job the queue owns alone is settled without
/// running: the step a write left owing is owed again, and a foreground
/// job's ticket fails, saying the queue that would have run it is gone.
///
/// Two threads cannot share one:
///
/// ```compile_fail
/// fn shared<T: Sync>() {}
/// shared::<regolith::IoQueue>();
/// ```
pub struct IoQueue {
    shared: Arc<QueueShared>,
    /// Holds the database's unit table and is what a unit fills.
    cache: Arc<BlockCache>,
    /// The units this queue registered on, by address, with the reads that
    /// wait on each.
    waiting: HashMap<usize, Waiting>,
    /// The jobs this queue registered on, by address, with what waits on
    /// each.
    jobs: HashMap<usize, JobWait>,
    /// Registered units and jobs in arrival order, for `poll` to run.
    order: VecDeque<Pending>,
    /// Units this queue's polls parked because every open-file slot was in
    /// use (D60), run again once a slot frees. A subset of `waiting`.
    parked: Vec<Arc<Unit>>,
    /// Reads that found the byte bound full.
    room: Vec<Arc<WaitSlot>>,
    /// What landed for this queue, and what calls run again still need.
    ledger: Ledger,
    /// The timed waits recorded on this queue and not yet due.
    timers: Timers,
    /// The database's environment, whose clock the timed waits are measured
    /// on. Read only while a timed wait is pending.
    env: Arc<dyn Env>,
    /// `Send` but not `Sync`: only the holder polls.
    _owner: PhantomData<Cell<()>>,
}

/// One unit this queue waits on.
struct Waiting {
    unit: Arc<Unit>,
    /// Each read recorded on the queue for this unit.
    slots: Vec<Arc<WaitSlot>>,
}

/// One job this queue waits on.
struct JobWait {
    job: Arc<Job>,
    /// What each call recorded on the queue for this job waits with.
    waiters: Vec<Arc<dyn Delivery>>,
}

/// A unit or a job, in the order `poll` runs them.
enum Pending {
    Read(Arc<Unit>),
    Job(Arc<Job>),
}

/// The `waiting` key of `unit`: its address, unique while `waiting` holds it.
fn address(unit: &Arc<Unit>) -> usize {
    Arc::as_ptr(unit).addr()
}

/// The `jobs` key of `job`: its address, unique while `jobs` holds it.
fn job_address(job: &Arc<Job>) -> usize {
    Arc::as_ptr(job).addr()
}

impl IoQueue {
    /// A new queue on the database `cache` belongs to, owing at most `bound`
    /// bytes of reads, with its timed waits measured on `env`'s clock.
    pub(crate) fn open(cache: Arc<BlockCache>, bound: usize, env: Arc<dyn Env>) -> Self {
        let raw = NEXT_QUEUE.fetch_add(1, Ordering::Relaxed);
        // Starts at one and cannot wrap in a process's lifetime.
        let id = QueueId::new(std::num::NonZeroU64::new(raw).unwrap_or(std::num::NonZeroU64::MIN));
        let shared = cache.io().open_queue(id, bound.max(1));
        shared.own();
        cache.io().bind_current(id);
        Self {
            shared,
            cache,
            waiting: HashMap::new(),
            jobs: HashMap::new(),
            order: VecDeque::new(),
            parked: Vec::new(),
            room: Vec::new(),
            ledger: Ledger::default(),
            timers: Timers::default(),
            env,
            _owner: PhantomData,
        }
    }

    /// The name handles carry in [`ReadMode::CacheOnly`](crate::ReadMode::CacheOnly).
    pub fn id(&self) -> QueueId {
        self.shared.id()
    }

    /// Bytes of device reads this queue owes now: one block per read
    /// recorded on it and not yet completed. At most the queue's bound,
    /// except for one read larger than the bound, admitted alone.
    pub fn pending_bytes(&self) -> usize {
        self.shared.pending()
    }

    /// Run at most `budget` of the reads and jobs this thread waits on, and
    /// take in the completions other threads pushed.
    ///
    /// A read or a job nobody has claimed is claimed with one
    /// compare-and-swap and run here, on this thread: a read fills the block
    /// cache, a sync of the log makes the commits it covers durable and visible. One
    /// another thread is running is left to it, and its completion arrives
    /// when it is done. Each finished one completes every wait recorded on
    /// this queue for it, waking the tasks that await them and running, on
    /// this thread, the callbacks a commit or a job owes. Returns at once when
    /// the queue owes nothing.
    ///
    /// Never waits on another thread. Under
    /// [`Options::max_open_files`](crate::Options::max_open_files), a read
    /// that must reopen its table and finds every descriptor in use by reads
    /// running on other threads is not run: it stays pending, and the poll
    /// goes on. The read that next frees a descriptor tells this queue,
    /// waking its owner if it idles, and the poll after that runs it again.
    ///
    /// A timed wait recorded here is completed by the first poll that finds
    /// the database's clock at its due time, whatever the budget, and the
    /// poll reports how long until the next one
    /// ([`IoProgress::wake_in`](crate::IoProgress::wake_in)).
    ///
    /// Only the thread holding the queue calls this; it is the only place a
    /// wait recorded here is completed. A poll also makes this the calling
    /// thread's queue for the I/O regolith starts itself (see the type docs).
    ///
    /// # Example
    ///
    /// A read that must not touch the device, and the poll that does its I/O:
    ///
    /// ```
    /// use regolith::{Db, Error, IoBudget, Options, ReadMode, WouldBlock};
    ///
    /// let db = Db::open("rows", Options::memory())?;
    /// db.put(b"row", b"1")?;
    /// // Flushed to a table whose blocks the cache has not read yet.
    /// db.flush()?;
    ///
    /// let mut queue = db.io_queue();
    /// let snapshot = db.snapshot().with_read_mode(ReadMode::CacheOnly(queue.id()));
    ///
    /// // The read returns at once, and the block it needs is owed to the queue.
    /// let missed = snapshot.get(b"row");
    /// assert!(matches!(missed, Err(Error::WouldBlock(WouldBlock::Io(_)))));
    /// assert!(queue.pending_bytes() > 0);
    ///
    /// // The poll reads the block, on this thread.
    /// let progress = queue.poll(IoBudget::ALL);
    /// assert_eq!((progress.completed, progress.more_pending), (1, false));
    ///
    /// // The same read, run again, finds the block.
    /// assert_eq!(snapshot.get(b"row")?.as_deref(), Some(&b"1"[..]));
    /// // A queue that owes nothing has nothing to do.
    /// let idle = queue.poll(IoBudget::ALL);
    /// assert_eq!(idle.completed, 0);
    /// assert!(!idle.more_pending);
    /// # Ok::<(), regolith::Error>(())
    /// ```
    pub fn poll(&mut self, budget: IoBudget) -> IoProgress {
        // The owner is running: a push from now on must not wake it.
        let _ = self.shared.wake_up();
        if self.shared.own() {
            self.cache.io().bind_current(self.id());
        }
        if !self.owes() {
            return IoProgress {
                retained_live_bytes: self.ledger.live_bytes(),
                landed_bytes: self.ledger.bytes(),
                ..IoProgress::default()
            };
        }
        let mut completed = 0;
        self.take_inbox(&mut completed);
        let mut ran = 0;
        while ran < budget.limit() {
            let Some(next) = self.order.pop_front() else {
                break;
            };
            match next {
                Pending::Read(unit) => {
                    if !self.waiting.contains_key(&address(&unit)) {
                        continue;
                    }
                    // Lost the claim: the winner pushes the completion here,
                    // or parked the unit and runs it again when a slot frees.
                    if unit.claim() {
                        if self.cache.io().run(&unit, &self.shared, &self.cache) == Ran::Parked {
                            // A slot freed since the park reaches the inbox,
                            // taken below or at the next poll, after this
                            // push.
                            self.parked.push(unit);
                        }
                        ran += 1;
                    }
                }
                Pending::Job(job) => {
                    if !self.jobs.contains_key(&job_address(&job)) {
                        continue;
                    }
                    // A passive job, or one another thread claimed, lands
                    // without this thread; its completion comes here. One
                    // that parks is left out of `order` until its waker
                    // pushes it back, so the poll never waits for it.
                    if job.claim() && self.cache.io().step_job(&job, &self.shared) {
                        ran += 1;
                    }
                }
            }
        }
        if ran > 0 {
            // What ran above pushed its completions to this inbox.
            self.take_inbox(&mut completed);
        }
        self.make_room();
        // Once every message this poll took is in: what calls still need is
        // held within its lifetime window; unheld landings can be evicted.
        self.ledger.settle(&self.shared);
        self.shared.set_kept(self.ledger.kept());
        let wake_in = self.fire_timers(&mut completed);
        IoProgress {
            retained_live_bytes: self.ledger.live_bytes(),
            landed_bytes: self.ledger.bytes(),
            completed,
            more_pending: self.owes(),
            wake_in,
        }
    }

    /// Complete the timed waits that are due, counting them in `completed`,
    /// and say how long until the next one is. Reads the clock only when a
    /// timed wait is pending.
    fn fire_timers(&mut self, completed: &mut usize) -> Option<Duration> {
        if self.timers.is_empty() {
            return None;
        }
        let now = self.env.now_micros();
        *completed += self.timers.fire(now);
        self.timers.wake_in(now)
    }

    /// Run `future` to completion on this thread, polling this queue whenever
    /// the future cannot make progress, and idling, with no busy wait, while
    /// neither can.
    ///
    /// The executor for a caller with nothing else to run: a wait, a commit
    /// ticket, or a [`transact_async`](crate::OptimisticTransactionDb::transact_async)
    /// attempt whose completions are delivered to this queue finishes here.
    /// A future that waits on something no thread will ever complete (a wait
    /// recorded on another queue nobody polls) never returns. While a timed
    /// wait is pending the thread idles no longer than until it is due.
    ///
    /// Not on `wasm32`, which has no thread to park: there the owner polls
    /// the queue from its own event loop when it has nothing else to run.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn block_on<F: std::future::Future>(&mut self, future: F) -> F::Output {
        use std::task::{Context, Poll};

        let mut future = std::pin::pin!(future);
        let waker = Waker::from(Arc::new(crate::sync::internal::Unpark(
            std::thread::current(),
        )));
        let mut cx = Context::from_waker(&waker);
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                return output;
            }
            let progress = self.poll(IoBudget::ALL);
            if progress.completed > 0 {
                continue;
            }
            // Wakes at once when this queue already has work, so the park
            // below returns and the loop polls again.
            self.idle_waker(&waker);
            if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                let _ = self.shared.wake_up();
                return output;
            }
            // Parked until another thread's completion: release what this
            // thread's reads hold back first (`read_view::idle`). A timed
            // wait is completed by this thread's own poll and by nothing
            // else, so with one pending the park ends when it is due; a wait
            // recorded since the poll above woke the waker, so the park
            // returns at once and the next poll reports it.
            crate::engine::read_view::idle();
            match progress.wake_in {
                Some(due_in) => std::thread::park_timeout(due_in),
                None => std::thread::park(),
            }
        }
    }

    /// The queue waits on something, or holds completions not yet taken in.
    fn owes(&self) -> bool {
        !self.waiting.is_empty()
            || !self.jobs.is_empty()
            || !self.room.is_empty()
            || !self.timers.is_empty()
            || !self.shared.inbox_is_empty()
    }

    /// Register `waker` as this thread is about to idle: the next completion
    /// pushed to this queue wakes it, once. A queue whose owner is running is
    /// never woken; it sees its completions at its next [`poll`](Self::poll),
    /// which also cancels the registration.
    ///
    /// When something is already waiting to be taken in, a read only this
    /// thread can run is pending, or a timed wait is due, `waker` is woken at
    /// once: the owner has work and should poll rather than idle.
    ///
    /// A timed wait that is not due yet wakes nobody when it comes due: an
    /// owner that idles with one pending polls again within
    /// [`IoProgress::wake_in`](crate::IoProgress::wake_in) of its last poll.
    pub fn idle_waker(&self, waker: &Waker) {
        let runnable = self.order.iter().any(|pending| match pending {
            Pending::Read(unit) => unit.is_free() && self.waiting.contains_key(&address(unit)),
            Pending::Job(job) => job.is_runnable() && self.jobs.contains_key(&job_address(job)),
        });
        let due = !self.timers.is_empty()
            && self.timers.wake_in(self.env.now_micros()) == Some(Duration::ZERO);
        if runnable || due {
            waker.wake_by_ref();
            return;
        }
        self.shared.rest(waker);
    }

    fn take_inbox(&mut self, completed: &mut usize) {
        for message in self.shared.take_inbox() {
            match message {
                Message::Read { unit, slot } => self.accept(unit, slot, completed),
                Message::Room(slot) => {
                    self.ledger.hold(&slot);
                    self.ledger.seen(slot.holds());
                    self.room.push(slot);
                }
                Message::Done(unit) => {
                    if let Some(waiting) = self.waiting.remove(&address(&unit)) {
                        self.land(&unit);
                        self.complete(&unit, waiting.slots);
                        *completed += 1;
                    }
                }
                Message::Job { job, waiter } => self.accept_job(job, waiter, completed),
                Message::JobDone(job) => {
                    if let Some(wait) = self.jobs.remove(&job_address(&job)) {
                        for waiter in wait.waiters {
                            waiter.deliver();
                        }
                        *completed += 1;
                    }
                }
                Message::Deliver(waiter) => {
                    waiter.deliver();
                    *completed += 1;
                }
                Message::SlotFreed => self.unpark(),
                Message::Timer { due, slot } => self.timers.record(due, slot),
                Message::Finished(read) => self.ledger.seen(&read),
                Message::Ready(job) => {
                    // Settled meanwhile (closed, or run by a blocking
                    // caller): its `JobDone` took it out of `jobs`.
                    if self.jobs.contains_key(&job_address(&job)) {
                        self.order.push_back(Pending::Job(job));
                    }
                }
            }
        }
    }

    /// A slot freed: every unit this queue parked goes back in line to run.
    /// One that close or a drop released since is done, and is dropped.
    fn unpark(&mut self) {
        for unit in self.parked.drain(..) {
            if unit.unpark() {
                self.order.push_back(Pending::Read(unit));
            }
        }
    }

    /// Record a wait on `job` on this queue, registering the queue on the job
    /// the first time. A job that landed before the registration is told
    /// here, as its completion would have been.
    fn accept_job(
        &mut self,
        job: Arc<Job>,
        waiter: Option<Arc<dyn Delivery>>,
        completed: &mut usize,
    ) {
        let key = job_address(&job);
        if let Some(wait) = self.jobs.get_mut(&key) {
            wait.waiters.extend(waiter);
            return;
        }
        if job.register(Arc::clone(&self.shared)) {
            self.order.push_back(Pending::Job(Arc::clone(&job)));
            self.jobs.insert(
                key,
                JobWait {
                    job,
                    waiters: waiter.into_iter().collect(),
                },
            );
            return;
        }
        if let Some(waiter) = waiter {
            waiter.deliver();
        }
        *completed += 1;
    }

    /// Record a read of `unit` on this queue, registering the queue on the
    /// unit the first time. A unit that finished before the registration is
    /// read here, as its completion would have been.
    fn accept(&mut self, unit: Arc<Unit>, slot: Arc<WaitSlot>, completed: &mut usize) {
        // Hold what the call read before this wait; a kept wait it covers
        // was the same call's earlier run, which this one replaces.
        self.ledger.hold(&slot);
        self.ledger.seen(slot.holds());
        if let Some(waiting) = self.waiting.get_mut(&address(&unit)) {
            waiting.slots.push(slot);
            return;
        }
        if unit.register(Arc::clone(&self.shared)) {
            self.order.push_back(Pending::Read(Arc::clone(&unit)));
            self.waiting.insert(
                address(&unit),
                Waiting {
                    unit,
                    slots: vec![slot],
                },
            );
            return;
        }
        self.land(&unit);
        self.complete(&unit, vec![slot]);
        *completed += 1;
    }

    /// Complete `slots`, reads of `unit`, and give back the bytes they owed.
    /// Each wait is kept, with the block, for its call's next run.
    fn complete(&mut self, unit: &Unit, slots: Vec<Arc<WaitSlot>>) {
        for slot in slots {
            self.shared.release(unit.bytes());
            self.ledger.completed(Some(unit.key()), Arc::clone(&slot));
            self.shared.set_kept(self.ledger.kept());
            slot.complete();
        }
    }

    /// Keep what `unit` read, or the error it met, for the reads run again on
    /// this queue. The poll lets landings go past the byte bound once it has
    /// taken every message in (`Ledger::settle`).
    fn land(&mut self, unit: &Unit) {
        let (landing, charge) = match unit.outcome() {
            Some(Outcome::Landed(landed)) => (Landing::Ready(landed.clone()), landed.charge()),
            Some(Outcome::Failed(err)) => (
                Landing::Failed(Arc::clone(err)),
                std::mem::size_of::<std::io::Error>(),
            ),
            Some(Outcome::Released) | None => return,
        };
        self.shared.land(unit.key(), &landing);
        self.ledger.land(unit.key(), charge, landing);
    }

    /// Wake the reads that waited for room, once the queue owes less than its
    /// bound. Each wait is kept for its call's next run, as a read's is.
    fn make_room(&mut self) {
        if !self.room.is_empty() && self.shared.pending() < self.shared.bound() {
            for slot in std::mem::take(&mut self.room) {
                self.ledger.completed(None, Arc::clone(&slot));
                self.shared.set_kept(self.ledger.kept());
                slot.complete();
            }
        }
    }

    /// Bytes of what landed for this queue and is still kept.
    #[cfg(test)]
    pub(crate) fn landed_bytes(&self) -> usize {
        self.ledger.bytes()
    }

    /// Whether a call in progress holds the block `key` landed.
    #[cfg(test)]
    pub(crate) fn holds(&self, key: &crate::engine::io::unit::UnitKey) -> bool {
        self.ledger.is_held(key)
    }

    /// The blocks still landed for this queue, oldest first.
    #[cfg(test)]
    pub(crate) fn landed_keys(&self) -> Vec<crate::engine::io::unit::UnitKey> {
        self.ledger.keys()
    }

    /// The database's unit table.
    #[cfg(test)]
    pub(crate) fn runtime(&self) -> &IoRuntime {
        self.cache.io()
    }

    /// The half of this queue other threads reach.
    #[cfg(test)]
    pub(crate) fn shared(&self) -> &Arc<QueueShared> {
        &self.shared
    }
}

impl Drop for IoQueue {
    fn drop(&mut self) {
        let runtime: &IoRuntime = self.cache.io();
        runtime.close_queue(self.shared.id());
        // Every later push is refused, so nothing is left behind unread.
        for message in self.shared.shut_inbox() {
            match message {
                Message::Read { unit, slot } => {
                    self.shared.release(unit.bytes());
                    slot.complete();
                    if !self.waiting.contains_key(&address(&unit)) {
                        runtime.release(&unit);
                    }
                }
                Message::Room(slot) | Message::Timer { slot, .. } => slot.complete(),
                Message::Finished(_) => {}
                // Its unit or job is in `waiting` or `jobs`, and is handled
                // there; so is every unit this queue parked.
                Message::Done(_) | Message::JobDone(_) | Message::Ready(_) | Message::SlotFreed => {
                }
                Message::Job { job, waiter } => {
                    let wait = self
                        .jobs
                        .entry(job_address(&job))
                        .or_insert_with(|| JobWait {
                            job,
                            waiters: Vec::new(),
                        });
                    wait.waiters.extend(waiter);
                }
                Message::Deliver(waiter) => waiter.deliver(),
            }
        }
        // Each job is settled before what waits on it is told, so a commit's
        // ticket is told the commit's true outcome.
        for (_, wait) in self.jobs.drain() {
            settle_on_drop(runtime, &wait.job);
            for waiter in wait.waiters {
                waiter.deliver();
            }
        }
        for (_, waiting) in self.waiting.drain() {
            runtime.release(&waiting.unit);
            for slot in waiting.slots {
                slot.complete();
            }
        }
        for slot in self.room.drain(..) {
            slot.complete();
        }
        self.timers.complete_all();
        self.ledger.clear(&self.shared);
        self.shared.set_kept(0);
    }
}

/// What a dropped queue does with a job it waits on, before it tells the
/// job's waiters. A passive job (a stall) lands with the event it stands
/// for; its waiters are told now and run their call again. A job nobody runs
/// yet is settled here by its body's release: a sync round of the log runs,
/// an owed step is owed again, a foreground job fails. A job another thread
/// runs is waited for, so its outcome is the one told; one that thread parks
/// instead is settled here like any job nobody runs.
fn settle_on_drop(runtime: &IoRuntime, job: &Arc<Job>) {
    loop {
        if job.is_done() || job.is_passive() || runtime.release_job(job) || job.wait_turn() {
            return;
        }
    }
}

impl std::fmt::Debug for IoQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoQueue")
            .field("id", &self.id())
            .field("waiting", &self.waiting.len())
            .field("jobs", &self.jobs.len())
            .field("timed_waits", &self.timers.len())
            .field("pending_bytes", &self.shared.pending())
            .finish_non_exhaustive()
    }
}
