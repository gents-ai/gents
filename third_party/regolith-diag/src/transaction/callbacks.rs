//! Callbacks a caller attaches to a transaction, and the database-wide hooks.
//!
//! A transaction has three points a caller can run code at: before the commit
//! decides ([`Transaction::before_commit`]), after it commits
//! ([`Transaction::on_commit`]) and after it ends without committing
//! ([`Transaction::on_abort`]). [`TransactionHooks`] is the same three points
//! for every transaction of a database. Both work at every isolation level,
//! for optimistic and pessimistic transactions alike. The protocol is the one
//! `proofs/tla/TxnCallbacks.tla` checks and `proofs/lean/Regolith/Callbacks.lean`
//! proves.
//!
//! Invariants this module holds:
//!
//! - Exactly one outcome. Every transaction ends in a commit or in an abort,
//!   never both and never neither, and the callbacks of the other outcome are
//!   dropped unrun. A callback registered for the outcome runs once: the
//!   `on_commit` ones are reached only by the owner that won the commit claim
//!   and the `on_abort` ones through the claim in `super::claim`, which
//!   settles between the owner and `close`.
//! - Order. At commit the transaction's own `before_commit` callbacks run in
//!   registration order, then the database hook, then validation. After the
//!   outcome the transaction's callbacks run in registration order, then the
//!   hook's.
//! - Before the outcome a panic or an error undoes the callback's effects
//!   (writes, appends, savepoints and registrations) and fails the commit,
//!   a savepoint set before the callback that it rolled back to or released
//!   included: the savepoint, and what the rollback took, are kept until
//!   the outermost callback returns `Ok` and put back if it fails; a
//!   `before_commit` callback that returned an error stays queued, in its
//!   place, and runs again at the next `prepare`. After the
//!   outcome a panic is caught and reported to
//!   [`crate::EventListener::on_callback_panic`], and the outcome stands.
//! - A panic before the outcome poisons the transaction, wherever it happened
//!   and whether or not a callback that met it passed it on: every later
//!   `prepare` and the commit fail with [`Error::CallbackPanicked`] (not
//!   latched), and the `on_abort` callbacks see
//!   [`AbortReason::CallbackPanicked`].
//! - No lock. The `on_abort` callbacks are handed from the owner to whichever
//!   thread ends the transaction through a lock-free list, and a callback
//!   that is still running keeps its own `on_abort` registrations apart until
//!   it returns `Ok`, so undoing a failed one never touches that list.
//! - A transaction that registered nothing, under a database with no hooks,
//!   pays one check of a flag at each point and allocates nothing.
//! - The `before_commit` and `on_commit` callbacks live in the transaction
//!   itself: up to four of each, each capturing no more than
//!   [`INLINE_WORDS`](super::in_place::INLINE_WORDS) machine words, allocate
//!   nothing. A fifth of a kind moves that kind's overflow to the heap, and
//!   a larger closure is boxed on its own. An `on_abort` callback is shared
//!   with `close`, so its first registration allocates the transaction's
//!   claim, and each one a node of the claim's list.

use std::sync::Arc;

use super::claim::Claim;
use super::exclusive::Exclusive;
use super::in_place::{BeforeCommit, OnAbort, OnCommit};
use super::queue::Queue;
use super::{
    CommitReceipt, Conflict, Error, IsolationLevel, Savepoint, Transaction, TransactionError,
    TxResult, Write,
};
use crate::engine::callback::{InCommit, contain};
use crate::engine::{PendingAppend, RegolithEngine};

/// Why a transaction ended without committing, as its
/// [`Transaction::on_abort`] callbacks and [`TransactionHooks::on_abort`] see
/// it.
///
/// Borrows the error that ended the commit, so building one costs nothing.
/// Match it through the reference the callback receives.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum AbortReason<'a> {
    /// The caller rolled the transaction back with [`Transaction::rollback`].
    Rollback,
    /// The caller dropped the transaction without committing it or rolling it
    /// back.
    Dropped,
    /// The commit lost a race: another transaction wrote a key this one
    /// depended on. [`OptimisticTransactionDb::transact`](crate::OptimisticTransactionDb::transact)
    /// runs the closure again in a new transaction, which registers its own
    /// callbacks.
    Conflict(&'a Conflict),
    /// The commit failed for another reason: a `before_commit` callback or
    /// hook returned this error, or the engine refused the commit.
    Error(&'a TransactionError),
    /// The database was closed before the transaction could commit.
    Closed,
    /// A `before_commit` callback or hook panicked.
    CallbackPanicked,
}

impl<'a> AbortReason<'a> {
    /// What a failed commit that returned `error` reports.
    fn of(error: &'a TransactionError) -> Self {
        match error {
            TransactionError::Conflict(conflict) => Self::Conflict(conflict),
            TransactionError::Engine(Error::Closed) => Self::Closed,
            TransactionError::Engine(Error::CallbackPanicked { .. }) => Self::CallbackPanicked,
            other => Self::Error(other),
        }
    }
}

/// What a committed transaction hands [`TransactionHooks::on_commit`].
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct CommitInfo {
    receipt: CommitReceipt,
    isolation: IsolationLevel,
}

impl CommitInfo {
    /// The receipt of the commit: the sequence its writes became visible at,
    /// or the snapshot's sequence for a transaction that wrote nothing.
    pub fn receipt(&self) -> CommitReceipt {
        self.receipt
    }

    /// The isolation level the transaction ran at.
    pub fn isolation(&self) -> IsolationLevel {
        self.isolation
    }
}

/// Callbacks run for every transaction of a database, installed with
/// [`Options::transaction_hooks`](crate::Options::transaction_hooks).
///
/// The three methods are the points [`Transaction::before_commit`],
/// [`Transaction::on_commit`] and [`Transaction::on_abort`] give one
/// transaction. At commit the transaction's own `before_commit` callbacks run
/// first and the hook's [`before_commit`](Self::before_commit) after them,
/// both before validation. After the outcome the transaction's own callbacks
/// run first and the hook's after them. Every method has a default that does
/// nothing.
///
/// `on_commit` and `on_abort` run on the thread that completes the outcome and
/// must not block, panic or re-enter the database; a panic is caught and
/// reported to [`crate::EventListener::on_callback_panic`], and the outcome
/// stands. A panic in `before_commit` fails the commit with
/// [`Error::CallbackPanicked`], not latched: the database stays writable.
///
/// A database with hooks tracks every transaction it begins, so that
/// [`crate::Db::close`] can run `on_abort` for the ones still open: that costs
/// one small allocation per transaction, and nothing for a database without
/// hooks.
///
/// # Example
///
/// Hooks that refuse every commit while a `frozen` key is set, and count how
/// each transaction ended.
///
/// ```
/// use std::sync::Arc;
/// use std::sync::atomic::{AtomicU64, Ordering};
///
/// use regolith::{
///     AbortReason, CommitInfo, Error, OptimisticTransactionDb, Options, Transaction,
///     TransactionError, TransactionHooks, TxResult, TxnOptions,
/// };
///
/// #[derive(Default)]
/// struct Gate {
///     commits: AtomicU64,
///     aborts: AtomicU64,
/// }
///
/// impl TransactionHooks for Gate {
///     // Reads through the transaction, so at its snapshot.
///     fn before_commit(&self, txn: &mut Transaction) -> TxResult<()> {
///         match txn.get(b"frozen")? {
///             Some(_) => Err(Error::InvalidArgument("the database is frozen".into()).into()),
///             None => Ok(()),
///         }
///     }
///
///     fn on_commit(&self, _info: &CommitInfo) {
///         self.commits.fetch_add(1, Ordering::Relaxed);
///     }
///
///     fn on_abort(&self, _reason: &AbortReason<'_>) {
///         self.aborts.fetch_add(1, Ordering::Relaxed);
///     }
/// }
///
/// let gate = Arc::new(Gate::default());
/// let options = Options::memory().transaction_hooks(gate.clone());
/// let db = OptimisticTransactionDb::open("gated", options)?;
///
/// let txn = db.begin(&TxnOptions::new());
/// txn.put(b"row", b"1")?;
/// txn.commit()?;
/// assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
///
/// // An error from `before_commit` fails the commit with that error, and
/// // the transaction counts as aborted: exactly one outcome each.
/// db.db().put(b"frozen", b"")?;
/// let txn = db.begin(&TxnOptions::new());
/// txn.put(b"row", b"2")?;
/// let refused = txn.commit().unwrap_err();
/// assert!(matches!(refused, TransactionError::Engine(Error::InvalidArgument(_))));
/// assert_eq!(db.db().get(b"row")?.as_deref(), Some(&b"1"[..]));
///
/// // A rollback is an abort too.
/// db.begin(&TxnOptions::new()).rollback();
/// assert_eq!(gate.commits.load(Ordering::Relaxed), 1);
/// assert_eq!(gate.aborts.load(Ordering::Relaxed), 2);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub trait TransactionHooks: Send + Sync + 'static {
    /// Runs once per commit, after the transaction's own `before_commit`
    /// callbacks and before validation, on the committing thread. It may read
    /// and write through `txn`; its writes are part of the commit and are
    /// validated like any other. An error fails the commit with that error,
    /// rolls the hook's writes back, and runs the abort callbacks.
    fn before_commit(&self, txn: &mut Transaction) -> TxResult<()> {
        let _ = txn;
        Ok(())
    }

    /// Runs once for a committed transaction, after its own `on_commit`
    /// callbacks, when the commit is visible (and durable under
    /// [`crate::DurabilityMode::Immediate`]).
    fn on_commit(&self, info: &CommitInfo) {
        let _ = info;
    }

    /// Runs once for a transaction that ended without committing, after its
    /// own `on_abort` callbacks.
    fn on_abort(&self, reason: &AbortReason<'_>) {
        let _ = reason;
    }
}

/// Run `f`, catching a panic in it and naming `callback` in the error.
///
/// The commit's containment (`engine::callback`), switched on for this call:
/// the same catch and the same error as a caller's trait that panics inside
/// the ordered step, except that it is not latched. These callbacks run
/// before the commit enters the ordered step, so what they leave half done is
/// this transaction's and the transaction is lost, not the database.
fn caught<T>(callback: &'static str, f: impl FnOnce() -> T) -> Result<T, Error> {
    let _mode = InCommit::enter();
    contain(callback, f).map_err(|_| panicked(callback))
}

/// What a panic in a transaction callback is reported as, by the call that met
/// it and by every `prepare` and the commit after it: not latched, since these
/// callbacks run before the commit enters the ordered step.
fn panicked(callback: &'static str) -> Error {
    Error::CallbackPanicked {
        callback,
        latched: false,
    }
}

/// Run a callback after the outcome is decided: a panic cannot change the
/// outcome, so it is caught and reported to the listeners of `engine`, when
/// it is still there to tell.
pub(super) fn survive_after(
    engine: Option<&RegolithEngine>,
    callback: &'static str,
    f: impl FnOnce(),
) {
    crate::engine::callback::survive(|| engine, callback, f);
}

#[derive(Default)]
struct Queues {
    before_commit: Queue<BeforeCommit>,
    on_commit: Queue<OnCommit>,
    /// The `on_abort` callbacks a running `before_commit` callback has
    /// registered so far. They join the claim's list only once the outermost
    /// callback returns `Ok`, so a failed one takes them back without
    /// reaching into the list `close` may be taking from.
    on_abort: Queue<OnAbort>,
    /// The savepoints running callbacks rolled back to or released, oldest
    /// first, with what each rollback took. Emptied when the outermost
    /// callback returns `Ok`; a failed one puts back what it and the
    /// callbacks that ran inside it took (`Transaction::rewind`).
    undo: Vec<Undo>,
}

/// A savepoint a running `before_commit` callback rolled back to or
/// released, and what the rollback took off the buffers: what the callback
/// failing puts back, so it takes back only its own effects.
struct Undo {
    savepoint: Savepoint,
    /// Savepoints left on the stack below it.
    depth: usize,
    /// The buffers' lengths right after the rollback or release.
    writes_len: usize,
    appends_len: usize,
    /// The entries the rollback took, newest first.
    writes: Vec<(Vec<u8>, Write)>,
    appends: Vec<((), PendingAppend)>,
}

/// The `before_commit` and `on_commit` callbacks one transaction registered,
/// kept in the transaction itself (module docs). (The `on_abort` ones are in
/// the transaction's claim, except while a callback is running; see
/// `Queues::on_abort`.)
#[derive(Default)]
pub(crate) struct Callbacks {
    /// In place, with no allocation, but only once something is registered:
    /// a transaction with none writes no empty queues when it begins and
    /// reads none when it is dropped.
    queues: Exclusive<Option<Queues>>,
    /// Whether anything was registered since the queues were last taken: the
    /// one check a transaction with no callbacks pays at each point.
    registered: bool,
    /// `before_commit` callbacks and hooks running now, outermost first: while
    /// there is one, an `on_abort` registration goes to `Queues::on_abort`.
    running: u32,
}

impl Callbacks {
    fn queues(&mut self) -> &mut Queues {
        self.registered = true;
        self.queues.get_mut().get_or_insert_with(Queues::default)
    }

    fn existing(&mut self) -> Option<&mut Queues> {
        if !self.registered {
            return None;
        }
        self.queues.get_mut().as_mut()
    }

    fn take(&mut self) -> Option<Queues> {
        if !std::mem::take(&mut self.registered) {
            return None;
        }
        self.queues.get_mut().take()
    }

    /// What an ending carries away: these callbacks when anything was
    /// registered or is running, and an empty set otherwise, so the ending
    /// of a transaction with none moves no queues.
    fn take_live(&mut self) -> Self {
        if self.registered || self.running != 0 {
            std::mem::take(self)
        } else {
            Self::default()
        }
    }
}

/// How far `prepare` got, so a second call repeats nothing.
#[derive(Clone, Copy)]
pub(super) enum Prepare {
    /// The database hook has not run.
    Pending,
    /// The database hook ran and succeeded.
    HooksRan,
    /// A `before_commit` callback panicked: the commit can only fail.
    Panicked(&'static str),
}

/// What `prepare` restores when a callback fails: the write and append
/// buffers and the savepoint stack as they were, and the callbacks as
/// registered.
struct Mark {
    writes: usize,
    appends: usize,
    savepoints: usize,
    before_commit: usize,
    on_commit: usize,
    on_abort: usize,
    undo: usize,
}

impl Transaction {
    /// The transaction's claim, made on the first `on_abort` registration. A
    /// claim made once the commit began starts as committing, so a sweep by
    /// `close` finds nothing to abort.
    fn claim(&mut self) -> &Arc<Claim> {
        let (engine, committing, owner) = (&self.engine, self.committing, &self.lock_owner);
        self.claim.get_or_insert_with(|| {
            let claim = Claim::track(engine, committing);
            // The locks taken before the claim existed are `close`'s to
            // release too.
            if let Some(owner) = owner {
                claim.hold_locks_of(Arc::clone(owner));
            }
            claim
        })
    }

    /// Run `f` at commit, before validation, on the committing thread.
    ///
    /// Callbacks run in registration order, then the database's
    /// [`TransactionHooks::before_commit`], then validation. `f` may read and
    /// write through the transaction, and what it writes is part of the
    /// commit and validated like any other write. It may register further
    /// callbacks of any kind; the `before_commit` ones run in the same pass.
    ///
    /// `f` may mutate what it captured. It runs again only when it returned an
    /// error: an error from `f` takes back the writes it made, the savepoints
    /// it set and the callbacks it registered, puts back a savepoint set
    /// before it that it rolled back to or released (with the writes and
    /// appends that rollback took), and leaves `f` queued where it
    /// was, so the next [`Transaction::prepare`], or the commit, runs it
    /// again. `f` must therefore not assume it runs once; what it captured
    /// keeps what its failed run did to it. A commit that gets such an error
    /// fails with it, and runs the `on_abort` callbacks.
    ///
    /// A panic in `f` fails the commit with [`Error::CallbackPanicked`] and
    /// leaves the database writable. It takes back the same effects, and `f`
    /// is dropped: the transaction can only be rolled back.
    ///
    /// [`Transaction::prepare`] runs the pending callbacks early, and
    /// [`Transaction::commit`] runs whatever is left. A callback does not run
    /// for a transaction that is rolled back or dropped.
    ///
    /// The first four callbacks of this kind a transaction registers are kept
    /// in the transaction and allocate nothing, as long as each captures no
    /// more than four machine words; a larger one is boxed, and the fifth
    /// and later ones are kept on the heap.
    pub fn before_commit(
        &mut self,
        f: impl FnMut(&mut Transaction) -> TxResult<()> + Send + 'static,
    ) {
        self.callbacks
            .queues()
            .before_commit
            .push(BeforeCommit::new(f));
    }

    /// Run `f` once if the transaction commits, when the commit is visible
    /// (and durable under [`crate::DurabilityMode::Immediate`]), with the
    /// commit's receipt. A transaction that wrote nothing is handed the
    /// receipt of its snapshot.
    ///
    /// `f` runs on the thread that completes the commit, after the
    /// transaction released its locks and snapshot, and after the transaction's
    /// earlier `on_commit` callbacks. It must not block or re-enter the
    /// database. A panic in `f` is caught and reported to
    /// [`crate::EventListener::on_callback_panic`]; the commit stands and the
    /// other callbacks still run. If the transaction does not commit, `f` is
    /// dropped without running.
    ///
    /// The first four callbacks of this kind a transaction registers are kept
    /// in the transaction and allocate nothing, as long as each captures no
    /// more than four machine words; a larger one is boxed, and the fifth
    /// and later ones are kept on the heap.
    pub fn on_commit(&mut self, f: impl FnOnce(&CommitReceipt) + Send + 'static) {
        self.callbacks.queues().on_commit.push(OnCommit::new(f));
    }

    /// Run `f` once if the transaction ends without committing: rolled back,
    /// dropped, lost to a conflict, failed, or aborted by [`crate::Db::close`].
    /// The [`AbortReason`] says which.
    ///
    /// The same thread, ordering and panic rules as [`Transaction::on_commit`]
    /// apply. If the transaction commits, `f` is dropped without running.
    ///
    /// A transaction still open when the database is closed ends there: `close`
    /// runs `f` with [`AbortReason::Closed`] on its own thread, and the
    /// transaction's later commit fails with [`Error::Closed`]. That holds even
    /// if the close then fails, as the transaction stays aborted. A
    /// transaction that had begun to commit is not touched.
    ///
    /// Registered from inside a [`Transaction::before_commit`] callback or a
    /// [`TransactionHooks::before_commit`], `f` counts as registered when that
    /// callback returns `Ok`; a callback that fails takes it back.
    ///
    /// These callbacks are shared with `close`, so the first one registered
    /// makes the transaction's claim (one allocation, made anyway under a
    /// database with hooks); the first four are kept in it and allocate
    /// nothing more, as long as each captures no more than four machine
    /// words, and the fifth and later ones are kept on the heap.
    pub fn on_abort(&mut self, f: impl FnOnce(&AbortReason<'_>) + Send + 'static) {
        let f = OnAbort::new(f);
        if self.callbacks.running > 0 {
            self.callbacks.queues().on_abort.push(f);
        } else {
            self.claim().push(f);
        }
    }

    /// Run the pending `before_commit` callbacks now, then the database's
    /// [`TransactionHooks::before_commit`], so a caller can find out whether
    /// the commit would get past them before it commits.
    ///
    /// [`Transaction::commit`] calls this first, so calling it is never
    /// required. It is idempotent: a call with nothing pending does nothing,
    /// and the hook runs once however often this is called. A callback
    /// registered after a successful call runs at the next one, after the
    /// hook.
    ///
    /// An error from a callback is returned and the writes, savepoints and
    /// registrations that callback made are taken back. The callback stays
    /// queued, ahead of the ones behind it, so a second call runs it again and
    /// then them. An error from the hook leaves the hook to run again. A
    /// callback that panics returns [`Error::CallbackPanicked`], and so does
    /// every later call and the commit: the transaction can only be rolled
    /// back, and its `on_abort` callbacks see [`AbortReason::CallbackPanicked`].
    /// That holds when the panic happened inside another callback's `prepare`
    /// and that callback ignored the error: its own run then fails too.
    ///
    /// A callback that calls `prepare` on the transaction it was handed runs
    /// the callbacks queued behind it, and the hook if it has not run, inside
    /// itself. If that callback then fails, they are taken back with it and
    /// run again at the next call; one that ran inside it is dropped only
    /// when the transaction ends.
    pub fn prepare(&mut self) -> TxResult<()> {
        if let Prepare::Panicked(callback) = self.prepare_state {
            return Err(panicked(callback).into());
        }
        self.run_before_commit()?;
        if matches!(self.prepare_state, Prepare::Pending)
            && let Some(hooks) = self.engine.transaction_hooks().cloned()
        {
            self.guarded("TransactionHooks", |txn| hooks.before_commit(txn))?;
            self.prepare_state = Prepare::HooksRan;
            self.run_before_commit()?;
        }
        Ok(())
    }

    /// Run every queued `before_commit` callback, including those they
    /// register.
    fn run_before_commit(&mut self) -> TxResult<()> {
        while let Some((at, mut f)) = self
            .callbacks
            .existing()
            .and_then(|queues| queues.before_commit.take())
        {
            let outcome = self.guarded("before_commit", |txn| f.call(txn));
            // A callback run inside another one is kept, in place, because
            // that one failing runs it again. At the top it is done with.
            let nested = self.callbacks.running > 0;
            let panicked = matches!(self.prepare_state, Prepare::Panicked(_));
            if let Some(queues) = self.callbacks.existing() {
                match &outcome {
                    Ok(()) if nested => queues.before_commit.put_back(at, f),
                    // A failed callback runs again, with the ones that ran
                    // inside it; after a panic nothing runs again.
                    Err(_) if !panicked => {
                        queues.before_commit.put_back(at, f);
                        queues.before_commit.reset(at);
                    }
                    _ => {}
                }
            }
            outcome?;
        }
        Ok(())
    }

    /// Run `run`, and when it errors or panics, take back everything it did.
    fn guarded(
        &mut self,
        callback: &'static str,
        run: impl FnOnce(&mut Transaction) -> TxResult<()>,
    ) -> TxResult<()> {
        let mark = self.mark();
        let state = self.prepare_state;
        self.callbacks.running += 1;
        let outcome = caught(callback, || run(self));
        self.callbacks.running -= 1;
        let error = match outcome {
            Ok(Ok(())) => match self.prepare_state {
                // A callback run inside this one panicked and this one passed
                // over the error: the transaction is poisoned all the same.
                Prepare::Panicked(callback) => panicked(callback).into(),
                _ => {
                    self.publish_on_abort();
                    self.forget_undo();
                    return Ok(());
                }
            },
            Ok(Err(error)) => error,
            Err(panicked) => {
                self.prepare_state = Prepare::Panicked(callback);
                panicked.into()
            }
        };
        self.rewind(mark);
        // A hook that ran inside the callback was rewound with it.
        if !matches!(self.prepare_state, Prepare::Panicked(_)) {
            self.prepare_state = state;
        }
        Err(error)
    }

    /// Hand the claim the `on_abort` callbacks the callbacks that just
    /// succeeded registered, once the outermost of them has returned.
    fn publish_on_abort(&mut self) {
        if self.callbacks.running > 0 {
            return;
        }
        let Some(queues) = self.callbacks.existing() else {
            return;
        };
        let mut staged = std::mem::take(&mut queues.on_abort);
        while let Some(f) = staged.pop() {
            self.claim().push(f);
        }
    }

    fn mark(&mut self) -> Mark {
        let (before_commit, on_commit, on_abort, undo) =
            self.callbacks.existing().map_or((0, 0, 0, 0), |q| {
                (
                    q.before_commit.mark(),
                    q.on_commit.mark(),
                    q.on_abort.mark(),
                    q.undo.len(),
                )
            });
        Mark {
            writes: self.writes.len(),
            appends: self.appends_len(),
            savepoints: self.savepoints.len(),
            before_commit,
            on_commit,
            on_abort,
            undo,
        }
    }

    /// Take back the writes and appends made after `savepoint`, which the
    /// caller just took off the stack. Inside a `before_commit` callback the
    /// savepoint and what the rollback took are kept, for the callback failing
    /// to put back; outside one they are dropped.
    pub(super) fn roll_back_to(&mut self, savepoint: Savepoint) {
        if self.callbacks.running == 0 {
            self.writes.truncate(savepoint.writes);
            self.truncate_appends(savepoint.appends);
            return;
        }
        let mut writes = Vec::new();
        self.writes
            .truncate_into(savepoint.writes, |entry| writes.push(entry));
        let mut appends = Vec::new();
        if let Some(buffer) = self.appends.get_mut() {
            buffer.truncate_into(savepoint.appends, |entry| appends.push(entry));
        }
        self.keep_undo(savepoint, writes, appends);
    }

    /// The caller just released `savepoint`: inside a `before_commit`
    /// callback it is kept, for the callback failing to put back.
    pub(super) fn released(&mut self, savepoint: Savepoint) {
        if self.callbacks.running > 0 {
            self.keep_undo(savepoint, Vec::new(), Vec::new());
        }
    }

    fn keep_undo(
        &mut self,
        savepoint: Savepoint,
        writes: Vec<(Vec<u8>, Write)>,
        appends: Vec<((), PendingAppend)>,
    ) {
        let undo = Undo {
            savepoint,
            depth: self.savepoints.len(),
            writes_len: self.writes.len(),
            appends_len: self.appends_len(),
            writes,
            appends,
        };
        self.callbacks.queues().undo.push(undo);
    }

    /// Once the outermost callback returned `Ok`, what its rollbacks took
    /// stays taken.
    fn forget_undo(&mut self) {
        if self.callbacks.running > 0 {
            return;
        }
        if let Some(queues) = self.callbacks.existing() {
            queues.undo.clear();
        }
    }

    fn rewind(&mut self, mark: Mark) {
        // Newest first, each rollback or release is undone on the state it
        // left: what was added since is dropped, then what it took goes back
        // in the order it was made.
        while let Some(undo) = self
            .callbacks
            .existing()
            .filter(|queues| queues.undo.len() > mark.undo)
            .and_then(|queues| queues.undo.pop())
        {
            self.writes.truncate(undo.writes_len);
            for (key, write) in undo.writes.into_iter().rev() {
                self.writes.insert(key, write);
            }
            self.truncate_appends(undo.appends_len);
            if !undo.appends.is_empty() {
                let buffer = self.appends_buffer();
                for (unit, append) in undo.appends.into_iter().rev() {
                    buffer.insert(unit, append);
                }
            }
            self.savepoints.truncate(undo.depth);
            self.savepoints.push(undo.savepoint);
        }
        self.writes.truncate(mark.writes);
        self.truncate_appends(mark.appends);
        self.savepoints.truncate(mark.savepoints);
        if let Some(queues) = self.callbacks.existing() {
            queues.before_commit.truncate(mark.before_commit);
            queues.on_commit.truncate(mark.on_commit);
            queues.on_abort.truncate(mark.on_abort);
        }
    }

    /// End the transaction whose commit the owner claimed, by the commit's
    /// result: run the callbacks of the outcome it reached and drop the other
    /// outcome's. `claimed` is whether the owner won the commit claim; if
    /// `close` aborted the transaction first, `close` ran its callbacks and
    /// only those registered since are left.
    ///
    /// A transaction with nothing to run (no callback, no claim, no hooks)
    /// pays this one check.
    pub(super) fn finish(&mut self, claimed: bool, result: &TxResult<CommitReceipt>) {
        if self.has_ending() {
            self.ending().finish(claimed, result);
        }
    }

    fn has_ending(&self) -> bool {
        self.callbacks.registered
            || self.claim.is_some()
            || self.engine.transaction_hooks().is_some()
    }

    /// What ending this transaction runs, taken out of it: its `on_commit`
    /// callbacks, its claim on the `on_abort` ones, and the database hooks.
    /// A commit whose outcome is delivered through its ticket carries this to
    /// the thread that delivers it.
    pub(super) fn ending(&mut self) -> Ending {
        Ending {
            callbacks: self.callbacks.take_live(),
            claim: self.claim.clone(),
            isolation: self.isolation,
            hooks: self.engine.transaction_hooks().cloned(),
            report: Arc::downgrade(&self.engine),
        }
    }

    /// End a transaction the owner rolled back or dropped before any commit
    /// claimed it. The owner wins the abort unless `close` did.
    fn end_open(&mut self, reason: &AbortReason<'_>) {
        let won = self.claim.as_ref().is_some_and(|claim| claim.owner_abort());
        let mut ending = self.ending();
        if won {
            ending.abort(reason);
        } else {
            ending.closed();
        }
    }

    /// The owner rolled the transaction back.
    pub(super) fn finish_rolled_back(&mut self) {
        self.end_open(&AbortReason::Rollback);
    }

    /// The owner dropped the transaction without resolving it.
    pub(super) fn finish_dropped(&mut self) {
        if self.claim.is_none() {
            return;
        }
        let reason = if self.engine.is_closed() {
            AbortReason::Closed
        } else {
            AbortReason::Dropped
        };
        self.end_open(&reason);
    }
}

/// What ending a transaction runs, detached from it: its own `on_commit`
/// callbacks, its claim (which holds its `on_abort` callbacks), and the
/// database hooks. The one place the outcome's callbacks run, whether the
/// committing call ends the transaction or a ticket's delivery does.
///
/// Holds the database weakly, only to report a panicking callback to its
/// listeners; the hooks are held themselves, so they still run when the
/// last handle went away before a ticket was delivered.
pub(crate) struct Ending {
    callbacks: Callbacks,
    claim: Option<Arc<Claim>>,
    isolation: IsolationLevel,
    hooks: Option<Arc<dyn TransactionHooks>>,
    report: std::sync::Weak<RegolithEngine>,
}

impl Ending {
    /// Run the callbacks of the outcome `result` is, and drop the other
    /// outcome's. `claimed`: as for [`Transaction::finish`].
    pub(crate) fn finish(mut self, claimed: bool, result: &TxResult<CommitReceipt>) {
        match (claimed, result) {
            (true, Ok(receipt)) => self.commit(*receipt),
            (true, Err(error)) => self.abort(&AbortReason::of(error)),
            (false, _) => self.closed(),
        }
    }

    fn commit(&mut self, receipt: CommitReceipt) {
        let report = self.report.upgrade();
        if let Some(mut own) = self.callbacks.take() {
            while let Some(f) = own.on_commit.pop() {
                survive_after(report.as_deref(), "on_commit", || f.call(&receipt));
            }
        }
        if let Some(hooks) = &self.hooks {
            let info = CommitInfo {
                receipt,
                isolation: self.isolation,
            };
            survive_after(report.as_deref(), "TransactionHooks::on_commit", || {
                hooks.on_commit(&info)
            });
        }
    }

    fn abort(&mut self, reason: &AbortReason<'_>) {
        drop(self.callbacks.take());
        if let Some(claim) = &self.claim {
            claim.run_abort(
                self.report.upgrade().as_deref(),
                self.hooks.as_ref(),
                reason,
            );
        }
    }

    /// `close` already ended this transaction and ran its callbacks; run the
    /// ones registered after it did.
    fn closed(&mut self) {
        drop(self.callbacks.take());
        if let Some(claim) = &self.claim {
            claim.run_callbacks(self.report.upgrade().as_deref(), &AbortReason::Closed);
        }
    }
}
