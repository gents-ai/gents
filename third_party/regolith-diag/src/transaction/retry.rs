//! Bounded retries owned by regolith: [`OptimisticTransactionDb::transact`]
//! and [`TransactionDb::transact`] run a closure in a transaction, commit it,
//! and run it again when the commit loses a race.
//!
//! A re-run starts at once. A one-winner race resolves on the re-run, which
//! sees the winner, so nothing sleeps. The closure receives the [`Conflict`]
//! the previous attempt lost to, so it can resolve the race instead of
//! redoing the same work blindly.
//!
//! A policy may ask for a bounded wait before each re-run instead
//! ([`RetryPolicy::backoff`]), measured on the clock of the database's
//! [`Env`](crate::Env). `transact` then waits on its own thread, which is
//! the thread that chose a blocking call; `transact_async` waits on its
//! queue, whose poll ends the wait, so nothing there blocks. A policy that
//! does not ask reads no clock.
//!
//! [`OptimisticTransactionDb::transact_async`] is the same contract for a
//! caller that must never block: each attempt reads through a
//! `CacheOnly` handle on the caller's queue, a block-cache miss suspends the
//! attempt on its wait instead of ending it, and the commit goes through
//! `commit_nowait`, whose ticket the attempt awaits. It needs no runtime: the
//! caller's executor polls the future, and the caller's thread polls its
//! queue when it has nothing else to run ([`crate::IoQueue::block_on`] does
//! both).

use std::sync::Arc;
use std::time::Duration;

use super::{OptimisticTransactionDb, TransactionDb, TxnOptions};
use crate::engine::RegolithEngine;
use crate::io_queue::wait::Pause;
use crate::{
    CommitReceipt, Conflict, QueueId, ReadMode, Transaction, TransactionError, TxResult, WouldBlock,
};

/// Attempts [`RetryPolicy::default`] allows, the first included.
// vertexia: provisional until the contention benchmarks measure how many
// re-runs a one-winner race needs; set it from that depth.
const DEFAULT_MAX_ATTEMPTS: u32 = 8;

/// How many times `transact` runs its closure before giving up on a conflict,
/// and how long it waits before each re-run.
///
/// By default a re-run starts at once and nothing waits: a race with one
/// winner is settled on the re-run, which sees the winner.
/// [`RetryPolicy::backoff`] asks for a wait instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct RetryPolicy {
    max_attempts: u32,
    /// The wait before the first re-run. Zero: no re-run waits.
    first_backoff: Duration,
    /// The longest wait before any re-run.
    max_backoff: Duration,
}

impl RetryPolicy {
    /// Allow `max_attempts` runs of the closure, the first included. Zero is
    /// taken as one: the closure always runs once. No re-run waits.
    pub fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            first_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        }
    }

    /// Wait before each re-run: `first` before the first one, twice as long
    /// before each one after it, and never longer than `max`. A `first`
    /// longer than `max` is taken as `max`, and a zero `first` or `max`
    /// means no wait.
    ///
    /// The wait is bounded: no re-run waits longer than `max`, so a call
    /// waits at most `max` times one less than
    /// [`max_attempts`](Self::max_attempts) in all. Every caller waits the
    /// same time: the delays carry no random spread.
    ///
    /// It is measured on the clock of the database's
    /// [`Env`](crate::Env), and whoever asked for the call does the
    /// waiting:
    ///
    /// - `transact` blocks its own thread. Not on `wasm32`, whose one
    ///   thread would wait while nothing else can run: there the re-run
    ///   starts at once.
    /// - `transact_async` blocks nothing. The wait is recorded on the
    ///   attempt's [`IoQueue`](crate::IoQueue) and is over at the first poll
    ///   of that queue that finds the delay passed; until then each poll
    ///   says how long the owner may idle
    ///   ([`IoProgress::wake_in`](crate::IoProgress::wake_in)).
    ///
    /// Where the environment has no clock the delay cannot be measured:
    /// `transact` waits it out once on the host's clock, and
    /// `transact_async` goes on at its queue's next poll. An environment
    /// whose clock is moved by hand ([`MemEnv`](crate::MemEnv)) ends a wait
    /// only when it is moved.
    ///
    /// Only a re-run after a conflict waits. A write stall causes no
    /// re-run: the commit waits it out. An attempt `transact_async` runs
    /// again because its commit's validation missed the block cache waits
    /// for that read and nothing more.
    #[must_use]
    pub fn backoff(mut self, first: Duration, max: Duration) -> Self {
        self.first_backoff = first.min(max);
        self.max_backoff = max;
        self
    }

    /// The most times the closure runs, the first included.
    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    /// The wait before re-run number `rerun`, where `1` is the first re-run:
    /// zero unless [`backoff`](Self::backoff) asked for one, and zero for
    /// `0`, the first run, which follows nothing.
    pub fn backoff_before(&self, rerun: u32) -> Duration {
        let Some(doublings) = rerun.checked_sub(1) else {
            return Duration::ZERO;
        };
        // Past 2^32 times the first delay any wait is the bound.
        let factor = 1u32.checked_shl(doublings).unwrap_or(u32::MAX);
        self.first_backoff
            .saturating_mul(factor)
            .min(self.max_backoff)
    }
}

/// Block this thread for `delay` measured on the clock of `engine`'s
/// environment: a blocking caller's wait before a re-run. Nothing waits for
/// a zero delay, and no clock is read for one.
///
/// Where the environment has no clock the wait is one wait of `delay` on the
/// host's.
#[cfg(not(target_arch = "wasm32"))]
fn pause(engine: &RegolithEngine, delay: Duration) {
    if delay.is_zero() {
        return;
    }
    let env = engine.env();
    let Some(start) = env.now_micros() else {
        std::thread::park_timeout(delay);
        return;
    };
    let deadline = start.saturating_add(u64::try_from(delay.as_micros()).unwrap_or(u64::MAX));
    // A park can return early; only the clock says the delay has passed.
    while let Some(left) = env
        .now_micros()
        .and_then(|now| deadline.checked_sub(now))
        .filter(|left| *left > 0)
    {
        std::thread::park_timeout(Duration::from_micros(left));
    }
}

/// On `wasm32` a blocking call does not wait: its one thread would sit
/// through the delay while nothing else could run.
#[cfg(target_arch = "wasm32")]
fn pause(_engine: &RegolithEngine, _delay: Duration) {}

/// Wait `delay` on `queue` without blocking: over at the first poll of
/// `queue` that finds the clock of `engine`'s environment past the delay.
/// Nothing is recorded for a zero delay, and no clock is read for one.
///
/// Where the environment has no clock the delay cannot be measured, and the
/// wait is over at the queue's next poll. A queue that is gone completes
/// nothing, so the wait is then over at once.
async fn pause_on(engine: &RegolithEngine, queue: QueueId, delay: Duration) {
    if delay.is_zero() {
        return;
    }
    let delay = u64::try_from(delay.as_micros()).unwrap_or(u64::MAX);
    let due = engine
        .env()
        .now_micros()
        .map_or(0, |now| now.saturating_add(delay));
    Pause::new(engine.io().timer(queue, due)).await;
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_ATTEMPTS)
    }
}

/// Why [`OptimisticTransactionDb::transact`] or [`TransactionDb::transact`]
/// returned no value. `E` is the closure's own error type.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransactError<E> {
    /// The closure returned an error. The attempt ended with it and was not
    /// retried.
    #[error("{0}")]
    Closure(E),
    /// Every attempt lost its commit to a conflict; this is the last one.
    #[error("{0}")]
    Exhausted(Conflict),
    /// The commit failed for a reason a re-run cannot fix.
    #[error(transparent)]
    Engine(TransactionError),
}

/// Run `f` in a transaction from `begin`, committing it, and again after each
/// conflict until `policy` runs out, waiting on this thread before a re-run
/// when the policy asks. The one loop both databases share.
fn run<T, E>(
    policy: &RetryPolicy,
    engine: &RegolithEngine,
    begin: impl Fn() -> Transaction,
    mut f: impl FnMut(&mut Transaction, Option<&Conflict>) -> Result<T, E>,
) -> Result<(T, CommitReceipt), TransactError<E>> {
    let mut previous: Option<Conflict> = None;
    // The commits lost so far: after the first, the next run is re-run one.
    let mut lost = 0u32;
    loop {
        let mut txn = begin();
        let value = match f(&mut txn, previous.as_ref()) {
            Ok(value) => value,
            Err(error) => {
                txn.rollback();
                return Err(TransactError::Closure(error));
            }
        };
        match txn.commit() {
            Ok(receipt) => return Ok((value, receipt)),
            Err(TransactionError::Conflict(conflict)) => {
                lost += 1;
                if lost >= policy.max_attempts() {
                    return Err(TransactError::Exhausted(conflict));
                }
                previous = Some(conflict);
                pause(engine, policy.backoff_before(lost));
            }
            // A write stall is no outcome of its own: the commit waited it
            // out, so the attempt that ran is the one that committed.
            Err(other) => return Err(TransactError::Engine(other)),
        }
    }
}

/// Run `call` until it stops asking to wait: on
/// [`TransactionError::WouldBlock`], await what the error carries (a read's
/// [`IoWait`](crate::IoWait), a stall's [`StallWait`](crate::StallWait), a
/// key lock's [`LockWait`](crate::LockWait)) and call it again; any other
/// outcome is returned.
///
/// Inside [`OptimisticTransactionDb::transact_async`] a read that may miss
/// the cache is written `ready(|| txn.get_slice(key)).await`: the attempt
/// suspends on the miss and resumes with the same read, so the work it did
/// before the miss is kept. The wait completes at the poll of the queue the
/// read was recorded on, on that queue's thread; a task awaiting it on
/// another thread waits until that thread polls.
pub async fn ready<T>(mut call: impl FnMut() -> TxResult<T>) -> TxResult<T> {
    loop {
        match call() {
            Err(TransactionError::WouldBlock(would_block)) => wait_out(would_block).await,
            other => return other,
        }
    }
}

/// Await what a [`WouldBlock`] asks to wait on.
async fn wait_out(would_block: WouldBlock) {
    match would_block {
        WouldBlock::Io(wait) => wait.await,
        WouldBlock::Stall(wait) => wait.await,
        WouldBlock::Locked(wait) => wait.await,
    }
}

/// [`run`] for an attempt that never blocks: it reads through a `CacheOnly`
/// handle on `queue`, runs its `before_commit` callbacks with
/// [`Transaction::prepare`], waiting out any miss, and commits through
/// [`Transaction::commit_nowait`]. Any other error from `prepare` is the
/// attempt's outcome, as it is for `commit`. A write stall does not end the attempt:
/// its commit stays pending and the ticket completes once the stop is
/// relieved. A commit whose validation needs a block the cache does not
/// hold waits for that read on `queue` and goes on, so `f` is not run again
/// for it either. The wait the policy asks for before a re-run is a wait on
/// `queue` too.
async fn run_async<T, E>(
    policy: RetryPolicy,
    engine: Arc<RegolithEngine>,
    begin: impl Fn(&TxnOptions) -> Transaction,
    queue: QueueId,
    mut f: impl AsyncFnMut(&mut Transaction, Option<&Conflict>) -> Result<T, E>,
) -> Result<(T, CommitReceipt), TransactError<E>> {
    let options = TxnOptions::new().read_mode(ReadMode::CacheOnly(queue));
    let mut previous: Option<Conflict> = None;
    // The commits lost so far: after the first, the next run is re-run one.
    let mut lost = 0u32;
    loop {
        let mut txn = begin(&options);
        let value = match f(&mut txn, previous.as_ref()).await {
            Ok(value) => value,
            Err(error) => {
                txn.rollback();
                return Err(TransactError::Closure(error));
            }
        };
        // A callback that misses stays queued with its effects rolled back,
        // so preparing again after the wait resumes where it stopped. Any
        // other failure is the attempt's outcome, as `commit` makes it: a
        // failed callback stays queued, and preparing again would run it
        // again, and commit if it then passed.
        let outcome = match ready(|| txn.prepare()).await {
            Ok(()) => txn.commit_nowait().await,
            Err(error) => txn.fail_commit(error),
        };
        match outcome {
            Ok(receipt) => return Ok((value, receipt)),
            Err(TransactionError::Conflict(conflict)) => {
                lost += 1;
                if lost >= policy.max_attempts() {
                    return Err(TransactError::Exhausted(conflict));
                }
                previous = Some(conflict);
                pause_on(&engine, queue, policy.backoff_before(lost)).await;
            }
            Err(TransactionError::WouldBlock(would_block)) => wait_out(would_block).await,
            Err(other) => return Err(TransactError::Engine(other)),
        }
    }
}

impl OptimisticTransactionDb {
    /// [`OptimisticTransactionDb::transact`] for a caller that must never
    /// block. Runs `f` in a transaction and commits it, re-running it
    /// on a conflict up to [`RetryPolicy::max_attempts`] runs, with the
    /// conflict the previous attempt lost to.
    ///
    /// Each attempt reads through a [`ReadMode::CacheOnly`] handle whose
    /// misses go to `queue`. Inside `f`, write a read that may miss as
    /// [`ready`]`(|| txn.get_slice(key)).await`: a miss suspends the attempt
    /// on its wait instead of ending it, and the work done before it is kept.
    /// The transaction's `before_commit` callbacks run through
    /// [`Transaction::prepare`], waiting out their misses the same way, and
    /// the commit goes through [`Transaction::commit_nowait`], whose ticket
    /// the attempt awaits: nothing here waits for the device or for another
    /// thread.
    ///
    /// The returned future needs no runtime. Every wait it awaits is
    /// completed by `queue`'s poll, on the thread that owns `queue`, so poll
    /// the future on that thread and poll `queue` when the future cannot
    /// progress; [`IoQueue::block_on`](crate::IoQueue::block_on) does both.
    ///
    /// A write stall does not end the attempt: its commit stays pending
    /// until the stop is relieved, and `f` is not run again for it. A commit
    /// whose validation needs a block the cache does not hold waits for that
    /// read on `queue` the same way, and `f` is not run again for it either.
    /// An error from `f`
    /// ends the attempt with [`TransactError::Closure`] and is not retried;
    /// any failure but a conflict ends it with [`TransactError::Engine`]. A
    /// `before_commit` callback that fails is such a failure, as it is for
    /// [`OptimisticTransactionDb::transact`]: the attempt ends with its
    /// error, the callback is not run again, and the abort callbacks run
    /// once.
    ///
    /// A re-run starts as soon as the future is polled again, unless the
    /// policy asks for a wait first ([`RetryPolicy::backoff`]). That wait
    /// blocks nothing either: it is recorded on `queue` and is over at the
    /// first poll of `queue` that finds the delay passed, so an owner that
    /// idles while one is pending polls again within
    /// [`IoProgress::wake_in`](crate::IoProgress::wake_in).
    ///
    /// The policy is copied into the future, which borrows the database and
    /// `f` and nothing else: it may be made from a policy that does not
    /// outlive the call.
    ///
    /// # Example
    ///
    /// ```
    /// use regolith::{OptimisticTransactionDb, Options, RetryPolicy, TransactionError, ready};
    ///
    /// let db = OptimisticTransactionDb::open("counter", Options::memory())?;
    /// db.db().put(b"n", &41u64.to_be_bytes())?;
    /// // Flushed to a table the block cache has not read: the read below
    /// // misses the cache once.
    /// db.db().flush()?;
    ///
    /// let mut queue = db.db().io_queue();
    /// let policy = RetryPolicy::default();
    /// let mut misses = 0;
    /// let attempt = db.transact_async(queue.id(), &policy, async |txn, _lost| {
    ///     // `ready` waits out a miss and runs the read again, so the attempt
    ///     // is suspended, not ended.
    ///     let read = ready(|| {
    ///         let read = txn.get_for_update(b"n");
    ///         misses += usize::from(matches!(read, Err(TransactionError::WouldBlock(_))));
    ///         read
    ///     });
    ///     let n = read.await?.map_or(0, |n| u64::from_be_bytes(n.try_into().expect("eight bytes")));
    ///     txn.put(b"n", &(n + 1).to_be_bytes())?;
    ///     Ok::<_, TransactionError>(n + 1)
    /// });
    ///
    /// // No runtime: this thread polls the future, and its own queue whenever
    /// // the future waits.
    /// let (n, _receipt) = queue.block_on(attempt)?;
    /// assert_eq!((n, misses), (42, 1));
    /// assert_eq!(db.db().get(b"n")?, Some(42u64.to_be_bytes().to_vec()));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn transact_async<'db, T, E, F>(
        &'db self,
        queue: QueueId,
        policy: &RetryPolicy,
        f: F,
    ) -> impl Future<Output = Result<(T, CommitReceipt), TransactError<E>>> + use<'db, T, E, F>
    where
        F: AsyncFnMut(&mut Transaction, Option<&Conflict>) -> Result<T, E>,
    {
        run_async(
            *policy,
            self.db().engine_arc(),
            |options| self.begin(options),
            queue,
            f,
        )
    }

    /// Run `f` in a transaction and commit it. When the commit loses a race
    /// ([`TransactionError::Conflict`]), run `f` again in a fresh transaction,
    /// up to [`RetryPolicy::max_attempts`] runs in all.
    ///
    /// A re-run starts at once and sleeps for nothing: a race with one winner
    /// is settled on the re-run, which sees the winner. A policy may ask for
    /// a bounded wait before each re-run instead ([`RetryPolicy::backoff`]),
    /// which this call takes on its own thread. `f` gets the
    /// [`Conflict`] the previous attempt lost to, `None` on the first run, so
    /// it can settle the race by its own logic (the document it meant to
    /// delete is already deleted) instead of redoing the work blindly.
    ///
    /// An error from `f` ends the attempt with [`TransactError::Closure`] and
    /// is not retried; its transaction is rolled back. A commit a write stall
    /// holds waits the stall out on this thread and then commits: `f` is not
    /// run again for it. A commit that fails for any other reason but
    /// a conflict returns [`TransactError::Engine`]. When every attempt
    /// conflicts, the last conflict comes back as [`TransactError::Exhausted`].
    ///
    /// The transactions begin as [`OptimisticTransactionDb::begin`] begins
    /// them with default [`TxnOptions`]: at the database's isolation level.
    ///
    /// `f` may run more than once. Work with effects outside the transaction
    /// belongs before the call, or must be idempotent.
    ///
    /// # Example
    ///
    /// A counter another writer gets to first:
    ///
    /// ```
    /// use regolith::{
    ///     OptimisticTransactionDb, Options, RetryPolicy, TransactError, TransactionError,
    /// };
    ///
    /// let db = OptimisticTransactionDb::open("counter", Options::memory())?;
    /// let number = |held: Option<Vec<u8>>| {
    ///     held.map_or(0, |n| u64::from_be_bytes(n.try_into().expect("eight bytes")))
    /// };
    ///
    /// let mut runs = 0;
    /// let (seen, _receipt) = db.transact(&RetryPolicy::new(3), |txn, lost| {
    ///     runs += 1;
    ///     let n = number(txn.get_for_update(b"n")?);
    ///     if lost.is_none() {
    ///         // On the first run only, another writer commits in between.
    ///         db.db().put(b"n", &10u64.to_be_bytes())?;
    ///     }
    ///     txn.put(b"n", &(n + 1).to_be_bytes())?;
    ///     Ok::<_, TransactionError>(n)
    /// })?;
    ///
    /// // The first run lost its commit. The second began after the winner,
    /// // read its value and committed on top of it.
    /// assert_eq!((runs, seen), (2, 10));
    /// assert_eq!(number(db.db().get(b"n")?), 11);
    ///
    /// // An error from the closure ends the call and is not retried.
    /// let refused = db.transact(&RetryPolicy::default(), |_txn, _lost| Err::<(), _>("not today"));
    /// assert!(matches!(refused, Err(TransactError::Closure("not today"))));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn transact<T, E>(
        &self,
        policy: &RetryPolicy,
        f: impl FnMut(&mut Transaction, Option<&Conflict>) -> Result<T, E>,
    ) -> Result<(T, CommitReceipt), TransactError<E>> {
        run(
            policy,
            &self.db().engine_arc(),
            || self.begin(&TxnOptions::new()),
            f,
        )
    }
}

impl TransactionDb {
    /// [`OptimisticTransactionDb::transact_async`] for a pessimistic
    /// transaction. Its key locks are taken as `get_for_update` and the writes
    /// take them, which can still wait for another holder; the reads and the
    /// commit never wait for the device.
    pub fn transact_async<'db, T, E, F>(
        &'db self,
        queue: QueueId,
        policy: &RetryPolicy,
        f: F,
    ) -> impl Future<Output = Result<(T, CommitReceipt), TransactError<E>>> + use<'db, T, E, F>
    where
        F: AsyncFnMut(&mut Transaction, Option<&Conflict>) -> Result<T, E>,
    {
        run_async(
            *policy,
            self.db().engine_arc(),
            |options| self.begin(options),
            queue,
            f,
        )
    }

    /// [`OptimisticTransactionDb::transact`] for a pessimistic transaction.
    ///
    /// A pessimistic commit conflicts only when a key it validates was written
    /// around the lock manager, so a re-run is rare. A lock that cannot be
    /// taken in time ([`TransactionError::Busy`]) arises inside `f`, where it
    /// is `f`'s own error and ends the attempt like any other.
    pub fn transact<T, E>(
        &self,
        policy: &RetryPolicy,
        f: impl FnMut(&mut Transaction, Option<&Conflict>) -> Result<T, E>,
    ) -> Result<(T, CommitReceipt), TransactError<E>> {
        run(
            policy,
            &self.db().engine_arc(),
            || self.begin(&TxnOptions::new()),
            f,
        )
    }
}
