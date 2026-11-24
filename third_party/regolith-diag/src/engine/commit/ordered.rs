//! The ordered step: work that takes its turn through the commit ring with
//! the whole pipeline to itself, in place of a mutex (plan 4.7).
//!
//! Rotating the memtable and its log, handing a sealed memtable to its
//! flush, installing an ingested table, creating or dropping a column
//! family, emptying the database, closing it: each needs the pipeline still.
//! Each takes a slot of the ring like a commit and runs when the slot's turn
//! comes, as a *unit*:
//!
//! - **A unit is claimed, not owned.** Its body is a job. Whichever thread
//!   reaches the slot first claims the job with one compare-and-swap and
//!   runs the body; the thread that asked for it is only the likeliest one.
//!   So a thread that stopped after asking holds nobody up.
//! - **The pipeline is drained before the body runs.** The claimer brings
//!   every earlier slot through: written, synced where it must be, and
//!   published. No later slot can be decided before this one is. The body
//!   therefore runs with nothing in flight, as a critical section of one
//!   lock over the whole pipeline would, and reads and changes the
//!   pipeline's state through an [`Ordered`].
//! - **The slot's decision is the state the body left.** The next commit is
//!   decided from it: a new log and memtable after a rotation, a sequence
//!   taken by an ingest.
//! - **A claimed unit is device I/O under way.** A blocking call that finds
//!   it claimed waits for it, as it waits for a sync; a `commit_nowait`
//!   commit leaves it on its queue.
//! - **A unit a queue's poll runs waits for no other thread.** While it
//!   drains the pipeline it parks on whatever another thread has, having
//!   changed nothing. Its body, once begun, runs to its end, so the poll
//!   first takes what the unit said its body may wait for ([`Needs`]), or
//!   parks (`park.rs`).
//!
//! The rotation check a write's turn begins with is a unit of the same
//! kind, attached to the write's own slot (`turn.rs`).
//!
//! # The round
//!
//! A unit's body runs holding the round, the one claim on the log's device
//! step (`pipeline.rs`): no sync and no rollback of another thread runs
//! under it, and the body syncs the log itself. The state the body leaves
//! is the pipeline's tip before the round is let go.
//!
//! # A failed log write or sync (G2)
//!
//! A failure poisons the log. The thread that next holds the round, a sync
//! round or a unit, discards every commit decided and not yet published
//! ([`RegolithEngine::roll_back`]): their records are cut out of the log,
//! their operations out of the memtable, their sequences handed back, and
//! each of their owners learns the failure. Commits already published are
//! untouched, and the database goes on taking writes. Only a cut that
//! itself fails latches the database, since the log's tail is then unknown.
//!
//! The rollback takes no slot of the ring, so it runs with the ring full.
//! It is ordered against the commits being decided by freezing the decided
//! frontier and moving the pipeline to a new epoch: a decision a thread was
//! computing from the old state is recognized by its epoch when it is
//! settled, lands nothing, and fails its slot.

use std::io;
use std::sync::Arc;
use std::task::Waker;

use super::super::internal_key::{
    VALUE_TYPE_DELETION, VALUE_TYPE_MERGE, VALUE_TYPE_VALUE, decode_internal_key,
};
use super::super::io::job::{Job, JobBody, Step};
use super::super::take::Take;
use super::super::wal::{check_write_len, encode_ops_record, ops_record_len, wal_filename};
use super::super::wal_frame::KIND_GROUP;
use super::super::wal_log::Log;
use super::super::{
    DurabilityMode, MemTable, RegolithEngine, apply_batch_op_to_memtable, batch_op_wal_bytes,
};
use super::counters::Counters;
use super::descriptor::{
    After, Decision, Descriptor, FAILED, INSTALLED, Verdict as Fate, Work, is_immediate,
};
use super::park::{Help, Needs};
use super::pipeline::{Pipeline, Progress, Turn};
use super::request::{Ops, batch_op_memtable_cost};
use super::ring::Loaded;
use super::round::HeldRound;
use super::write_unit::Await;
use crate::WriteBatchOp;
use crate::portability::{AtomicU64, Ordering};
use crate::statistics::Ticker;
use crate::sync::internal::Arc as RingArc;

/// What a unit's body is: it runs once, on the thread that claimed it.
pub(super) type Body = Box<dyn FnOnce(&RegolithEngine, &mut Ordered<'_>) + Send>;

/// What a unit's job holds of it. The body itself is in the unit's
/// descriptor, which the job reaches through the ring.
struct Unit {
    engine: std::sync::Weak<RegolithEngine>,
    /// The slot the unit was installed at.
    index: Arc<AtomicU64>,
}

impl Unit {
    /// `false` when a poll's unit parked.
    fn go(&self, help: Help<'_>) -> bool {
        match self.engine.upgrade() {
            Some(engine) => engine.run_unit(self.index.load(Ordering::Acquire), help),
            None => true,
        }
    }
}

impl JobBody for Unit {
    fn run(self: Box<Self>) {
        self.go(Help::Wait);
    }

    /// On a queue's poll: parks instead of waiting for another thread.
    fn step(self: Box<Self>, waker: &Waker) -> Step {
        if self.go(Help::Park(waker)) {
            Step::Done
        } else {
            Step::Parked(self)
        }
    }

    /// The pipeline cannot pass a unit that did not run, so a release runs
    /// it too.
    fn release(self: Box<Self>) {
        self.go(Help::Wait);
    }
}

/// An ordered unit whose caller has not been given its result yet. See
/// [`RegolithEngine::ordered_unit`].
pub(crate) struct OrderedTicket<T> {
    /// The unit: its job, and its body until a thread runs it.
    desc: RingArc<Descriptor>,
    /// What the body returned, put there by the thread that ran it.
    result: Arc<Take<io::Result<T>>>,
}

impl<T> OrderedTicket<T> {
    /// Whether the unit is in the ring: some thread will run its body,
    /// whatever becomes of this ticket.
    pub(crate) fn entered(&self) -> bool {
        self.desc.is(INSTALLED)
    }
}

/// A unit's body that ended without a result: it unwound.
fn unfinished() -> io::Error {
    io::Error::other("an ordered step of the commit pipeline did not finish")
}

/// The pipeline, held still, as a unit's body sees it. See the module docs.
pub(crate) struct Ordered<'a> {
    pub(super) engine: &'a RegolithEngine,
    /// The state the next commit is decided from once the unit ends.
    pub(super) state: After,
}

impl Ordered<'_> {
    /// The last sequence decided so far.
    pub(crate) fn last_seq(&self) -> u64 {
        self.state.next_seq.saturating_sub(1)
    }

    /// Take the next sequence, for an ingested table. The table may hold a
    /// key whose counter the pipeline keeps, so it keeps none from here.
    pub(crate) fn take_seq(&mut self) -> u64 {
        let seq = self.state.next_seq;
        self.state.next_seq += 1;
        self.state.counters = Counters::empty();
        seq
    }

    /// The memtable commits are applied to now.
    pub(crate) fn active(&self) -> &Arc<MemTable> {
        &self.state.memtable
    }

    /// Make every record of the active log durable. A failed sync latches
    /// the database: the log's tail can no longer be vouched for, and a
    /// retried sync may report success for pages the host already dropped.
    pub(crate) fn sync_log(&mut self) -> io::Result<()> {
        let log = &self.state.log;
        if let Err(err) = log.sync_through(self.state.next_off) {
            tracing::error!(error = %err, "syncing the write-ahead log failed");
            self.engine.latch_wal_failure(&err);
            self.engine.notify_wal_error(&err)?;
            return Err(err);
        }
        Ok(())
    }

    /// Move the active memtable to the frozen list behind a fresh one and a
    /// fresh log, and return it. `check` refuses on a database that is not
    /// writable.
    pub(crate) fn seal(&mut self, check: bool) -> io::Result<Arc<MemTable>> {
        if check {
            self.engine.ensure_writable()?;
        }
        // Everything fallible happens before the publication, so a rotation
        // that fails leaves the view as it was.
        let fresh = Arc::new(MemTable::new(&self.engine.memtable_config)?);
        let (id, log) = self.next_log()?;
        let sealed = self.freeze(fresh);
        self.install_log(id, log);
        Ok(sealed)
    }

    /// Seal the active memtable for close without a fresh log: the log it
    /// was written to stays the newest, unsynced, and the database takes no
    /// commit from here on. What a close's drain does (E46).
    ///
    /// Recovery tolerates a torn tail in the newest log, so this log needs
    /// no sync while it is the newest. The memtable's flush records, in the
    /// same synced manifest batch as its table, that recovery skips this
    /// log, and only then removes it; [`Self::close_log`] creates the next
    /// log after that. The pages the log still had dirty are dropped with
    /// it, never written. A close that fails before the flush calls
    /// [`Self::reopen_log`], which syncs this log before any newer one
    /// exists, as [`Self::seal`] does (`proofs/tla/WalRecovery.tla`,
    /// `SealInPlace`, RED `NewLogBeforeFlush`).
    pub(crate) fn seal_in_place(&mut self) -> io::Result<Arc<MemTable>> {
        let fresh = Arc::new(MemTable::new(&self.engine.memtable_config)?);
        let sealed = self.freeze(fresh);
        self.state.closed = true;
        Ok(sealed)
    }

    /// Sync the active log and create the one after it, without putting
    /// it in place: the fallible half of a rotation.
    ///
    /// Recovery takes the newest log by the id in its file name
    /// (`wal_scan::live`) and tolerates a torn tail only there, so a log
    /// has to be complete on disk before any newer file exists, an empty
    /// one included: sync it, then create the next one (E2).
    fn next_log(&mut self) -> io::Result<(u64, Arc<Log>)> {
        self.sync_log()?;
        self.create_log()
    }

    /// Create the log after the active one, named by a fresh file id.
    fn create_log(&self) -> io::Result<(u64, Arc<Log>)> {
        let engine = self.engine;
        let id = engine.versions.take_file_id()?;
        let log = Arc::new(
            Log::create_in(
                &engine.env,
                &engine.wal_dir.join(wal_filename(id)),
                engine.options.keyring.as_deref(),
            )?
            .with_early_writeback(engine.early_writeback()),
        );
        log.start_epoch(self.state.epoch);
        Ok((id, log))
    }

    /// Put `log`, created by [`Self::create_log`] under `id`, in place of
    /// the active log.
    fn install_log(&mut self, id: u64, log: Arc<Log>) {
        self.engine
            .wal_id
            .store(id, std::sync::atomic::Ordering::Release);
        self.state.next_off = log.stamp_len();
        self.state.log = log;
    }

    /// Move the active memtable to the frozen list behind `fresh`, and
    /// return it: the publication half of a seal, which cannot fail.
    fn freeze(&mut self, fresh: Arc<MemTable>) -> Arc<MemTable> {
        // Stamped before it is published as frozen: from then on a flush on
        // another thread may take it, and the flush reads both stamps. The
        // ordered step is held, so no write enters it after this.
        let sealing = Arc::clone(&self.state.memtable);
        sealing.seal_seq(self.last_seq());
        // The log that was active while it took writes is the one its
        // records are in, and the only durable copy of them until a flush
        // publishes an SSTable.
        sealing.seal_wal(self.state.log.path().to_path_buf());

        // One publication: the sealed memtable joins `frozen` in the same
        // view that hands writers the fresh active one.
        let sealed = self.engine.view.update_memtables(|active, frozen| {
            let sealed = Arc::clone(active);
            let mut next_frozen = frozen.to_vec();
            next_frozen.push(Arc::clone(&sealed));
            (Arc::clone(&fresh), next_frozen, sealed)
        });
        debug_assert!(Arc::ptr_eq(&sealed, &sealing));
        self.state.memtable = fresh;
        self.state.mem_used = 0;
        sealed
    }

    /// Commit `ops` here, inside the unit: logged, synced when `durability`
    /// asks, applied and published before this returns. Returns the last
    /// sequence they took. What a column family's creation and drop use, so
    /// that what follows the commit in the unit is ordered against every
    /// later write.
    ///
    /// The record is written and synced before anything is applied, so a
    /// failure leaves nothing behind but the cut that removes the record.
    pub(crate) fn commit_alone(
        &mut self,
        ops: &[WriteBatchOp],
        durability: DurabilityMode,
    ) -> io::Result<u64> {
        let engine = self.engine;
        engine.ensure_writable()?;
        engine.cf_fence_ops(ops)?;
        if ops.is_empty() {
            return Ok(self.last_seq());
        }
        let _commit = super::super::callback::InCommit::enter();
        let payload = ops_record_len(ops);
        check_write_len(payload)?;
        let log = Arc::clone(&self.state.log);
        let place = log.place(self.state.next_off, payload, self.state.epoch)?;
        let base = self.state.next_seq;
        let mut record = vec![0u8; log.prefix_len()];
        encode_ops_record(&mut record, ops, base);
        let synced = is_immediate(durability);
        let logged = log
            .finish_record(KIND_GROUP, &place, &mut record)
            .and_then(|()| log.write_record(&place, &record))
            .and_then(|_| {
                if synced {
                    log.sync_through(place.end())
                } else {
                    Ok(())
                }
            });
        if let Err(err) = logged {
            tracing::error!(error = %err, "a commit failed in the write-ahead log; discarding its record");
            // The record was placed and written by this thread alone, so
            // there is no other writer of it to turn away or to wait for:
            // the epoch stays.
            if let Err(cut_err) = log.cut_own(place.offset.max(log.durable())) {
                engine.latch_wal_failure(&cut_err);
            }
            engine.notify_wal_error(&err)?;
            return Err(err);
        }
        if let Some(s) = engine.statistics() {
            s.add(
                Ticker::WalBytesWritten,
                ops.iter().map(batch_op_wal_bytes).sum(),
            );
            if synced {
                s.add(Ticker::WalSyncCount, 1);
            }
        }
        let memtable = &self.state.memtable;
        let mut hint = memtable.insert_hint();
        for (op, seq) in ops.iter().zip(base..) {
            apply_batch_op_to_memtable(memtable, &mut hint, op, seq);
        }
        let last = base + ops.len() as u64 - 1;
        self.state.next_seq = last + 1;
        self.state.next_off = place.end();
        self.state.mem_used = self.state.mem_used.saturating_add(
            ops.iter()
                .map(batch_op_memtable_cost)
                .fold(0usize, usize::saturating_add),
        );
        if let Some(kept) = self.state.counters.forget_written(&Ops::Batch(ops)) {
            self.state.counters = kept;
        }
        engine.visible_seq.publish(last);
        Ok(last)
    }

    /// Put `memtable` and `log`, both empty, in place of the active ones:
    /// what emptying the database does once the view no longer shows the old
    /// ones.
    pub(crate) fn replace(&mut self, memtable: Arc<MemTable>, log: Arc<Log>) {
        log.start_epoch(self.state.epoch);
        self.state.next_off = log.stamp_len();
        self.state.log = log;
        self.state.memtable = memtable;
        self.state.mem_used = 0;
        self.state.counters = Counters::empty();
    }

    /// Close the active log cleanly, after which every commit is refused.
    /// A log left in an unknown state by an earlier failure (`latched`) is
    /// only synced: CLOSE would vouch for bytes nobody can account for.
    ///
    /// Called once every memtable is flushed. When the close sealed the
    /// active memtable in place ([`Self::seal_in_place`]), its log is in a
    /// table by now and removed: the next log is created here, after that,
    /// and closed, so the directory ends in a closed log as after any close.
    /// A creation that fails leaves the state as it was, for
    /// [`Self::reopen_log`]; a sync or a write of the log that fails latches
    /// the database, since what reached the device is unknown.
    pub(crate) fn close_log(&mut self, latched: bool) -> io::Result<()> {
        if self.state.closed {
            let (id, log) = self.create_log()?;
            self.install_log(id, log);
        }
        let closed = if latched {
            self.state.log.sync_through(self.state.next_off)
        } else {
            self.state.closed = true;
            self.state.log.close(self.state.next_off)
        };
        if let Err(err) = &closed
            && !latched
        {
            tracing::error!(error = %err, "closing the write-ahead log failed");
            self.engine.latch_wal_failure(err);
        }
        closed
    }

    /// Take commits again after a close that failed. A close that sealed
    /// the active memtable in place left the log it was written to active
    /// and its memtable perhaps not flushed, so a commit there would land in
    /// a log the flush removes: the log is synced and the next one created
    /// first, as a rotation does. A latched database stays refused.
    pub(crate) fn reopen_log(&mut self, latched: bool) -> io::Result<()> {
        if !self.state.closed || latched {
            return Ok(());
        }
        let (id, log) = self.next_log()?;
        self.install_log(id, log);
        self.state.closed = false;
        Ok(())
    }
}

impl RegolithEngine {
    /// A unit descriptor whose body is `body` and may wait for `needs`.
    pub(super) fn unit(&self, needs: Needs, body: Body) -> RingArc<Descriptor> {
        let index = Arc::new(AtomicU64::new(0));
        let job = Job::new(Box::new(Unit {
            engine: self.me.clone(),
            index: Arc::clone(&index),
        }));
        let desc = Descriptor::new(Work::Unit { job, index, needs }, false);
        desc.body.put(body);
        RingArc::new(desc)
    }

    /// Run `body` in the ordered step and return what it returns. See the
    /// module docs: the body may run on another thread, and runs with every
    /// earlier commit published and no later one decided.
    ///
    /// For a body that waits for no other thread. One that changes the
    /// version set or flushes says so through [`Self::ordered_needing`].
    pub(crate) fn ordered<T: Send + 'static>(
        &self,
        body: impl FnOnce(&RegolithEngine, &mut Ordered<'_>) -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        self.ordered_needing(Needs::NONE, body)
    }

    /// [`Self::ordered`] for a body that may wait for `needs` on another
    /// thread. A queue's poll that runs the unit takes them before the body
    /// begins, so the body never waits there (`park.rs`, K3).
    pub(crate) fn ordered_needing<T: Send + 'static>(
        &self,
        needs: Needs,
        body: impl FnOnce(&RegolithEngine, &mut Ordered<'_>) -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        let ticket = self.ordered_unit(needs, body);
        // A call that waits always sees its unit through.
        self.ordered_advance(&ticket, Help::Wait)
            .unwrap_or_else(|| Err(unfinished()))
    }

    /// Make an ordered unit of `body` without entering the ring: the ticket
    /// [`Self::ordered_advance`] sees through. For a caller that may not
    /// wait at the unit's entry, a job a queue's poll runs, which keeps the
    /// ticket while it is parked. [`Self::ordered_needing`] is the two in
    /// one for a caller that waits.
    pub(crate) fn ordered_unit<T: Send + 'static>(
        &self,
        needs: Needs,
        body: impl FnOnce(&RegolithEngine, &mut Ordered<'_>) -> io::Result<T> + Send + 'static,
    ) -> OrderedTicket<T> {
        let result: Arc<Take<io::Result<T>>> = Arc::new(Take::empty());
        let out = Arc::clone(&result);
        let desc = self.unit(
            needs,
            Box::new(move |engine, ord| out.put(body(engine, ord))),
        );
        OrderedTicket { desc, result }
    }

    /// See `ticket`'s unit through: enter the ring if it has not, bring
    /// every slot before it to published, run it (or find it run by the
    /// thread that reached it first), and return what its body returned.
    /// `None` when the call did not get there: a queue's poll parked, with
    /// its waker registered where the wait ends (`park.rs`), and calls this
    /// again when it is woken. Until the unit is in the ring a parked call
    /// has left nothing behind; once it is, whichever thread reaches its
    /// slot runs it.
    pub(crate) fn ordered_advance<T>(
        &self,
        ticket: &OrderedTicket<T>,
        help: Help<'_>,
    ) -> Option<io::Result<T>> {
        let pipe = match self.pipe() {
            Ok(pipe) => pipe,
            Err(err) => return Some(Err(err)),
        };
        if !ticket.desc.is(INSTALLED) {
            match help {
                Help::Wait => {
                    self.enter(pipe, &ticket.desc);
                }
                // A full ring, and the slot in the way of making room is
                // another thread's: the reservation was given up.
                Help::Leave | Help::Park(_) => {
                    self.try_enter(pipe, &ticket.desc, help).ok()?;
                }
            }
        }
        let index = ticket.desc.index();
        loop {
            if let Some(result) = ticket.result.take() {
                return Some(result);
            }
            match self.help_decide(pipe, index, help, None) {
                Turn::Blocked(job) => match help {
                    Help::Wait => self.run_or_wait(&job),
                    Help::Leave | Help::Park(_) => return None,
                },
                Turn::Done => {
                    // Every slot before the unit is published, and the unit
                    // takes nothing: the frontier passes it here, so the
                    // unit leaves nothing in the ring.
                    let _ = self.publish_upto(pipe, index, None, Await::Copy);
                    return Some(ticket.result.take().unwrap_or_else(|| Err(unfinished())));
                }
            }
        }
    }

    /// Run the unit at slot `index`: drain the pipeline, run the body, and
    /// decide the slot with the state the body left. `false` when a poll's
    /// unit parked, before its body began.
    fn run_unit(&self, index: u64, help: Help<'_>) -> bool {
        let Some(pipe) = self.pipeline.as_ref() else {
            return true;
        };
        let Loaded::Ready(desc) = pipe.ring.load(index) else {
            return true;
        };
        if desc.decision().is_some() {
            return true;
        }
        let Some((held, state)) = self.drained(pipe, index, help) else {
            return false;
        };
        let needs = match &desc.work {
            Work::Unit { needs, .. } => *needs,
            Work::Request(_) | Work::Allocate { .. } => Needs::NONE,
        };
        let Some(kept) = self.keep(needs, help) else {
            return false;
        };
        let mut ord = Ordered {
            engine: self,
            state,
        };
        if let Some(body) = desc.body.take() {
            body(self, &mut ord);
        }
        desc.decide(Decision::new(index, Fate::Unit, ord.state));
        // The state the body left is the tip before the round is let go:
        // a rollback, which needs the round, then starts from it.
        self.settle(pipe, index, &desc, None, false);
        self.unkeep(kept);
        drop(held);
        true
    }

    /// Bring every slot before `index` through to published, with the
    /// decided frontier at `index - 1`. The caller runs a unit, so it does
    /// its own device I/O, and writes a record another thread is slow to
    /// write itself (`Await::Copy`). What another thread has under way a
    /// blocking caller waits for; a poll parks, and `false` says so.
    fn drain_before(&self, pipe: &Pipeline, index: u64, help: Help<'_>) -> bool {
        loop {
            match self.publish_upto(pipe, index - 1, None, Await::Copy) {
                Progress::Published => return true,
                // The holder of a unit copies: never handed a queue's job.
                Progress::Waits(_) => {}
                Progress::NeedsSync | Progress::Failed => {
                    if self.sync_round(pipe, help).is_err() {
                        return false;
                    }
                }
                Progress::Undecided => {
                    if let Turn::Blocked(job) = self.help_decide(pipe, index - 1, help, None) {
                        if !matches!(help, Help::Wait) {
                            return false;
                        }
                        self.run_or_wait(&job);
                    }
                }
            }
        }
    }

    /// Drain the pipeline before slot `index` and hold the round: what a
    /// unit's body runs under. Returns the hold and the state the pipeline
    /// is in. A log that failed is rolled back first, here. `None` when a
    /// poll parked on the way: it holds nothing then (`park.rs`, K1).
    pub(super) fn drained<'a>(
        &'a self,
        pipe: &Pipeline,
        index: u64,
        help: Help<'_>,
    ) -> Option<(HeldRound<'a>, After)> {
        if !self.drain_before(pipe, index, help) {
            return None;
        }
        // Nothing before `index` is unpublished from here on, and nothing
        // after it is decided before this unit is. Only the log can still
        // change under a unit, in a round another thread runs; the hold
        // stops that.
        let held = self.hold_round(pipe, help)?;
        let tip = pipe.tip.load();
        // The body syncs the log through every record decided, all written
        // now: first every write that may still land there has to return,
        // a slow writer's late copy of a published record included (WG4).
        // Nothing is decided meanwhile, so what counts in from here on finds
        // its record written and writes nothing; one turn of the gate can
        // leave only an earlier sync's shorter turn to finish first.
        let log = &tip.after.log;
        while !log.is_poisoned() {
            match self.settle_writes(log, tip.after.next_off, help) {
                None => return None,
                Some(upto) if upto >= tip.after.next_off => break,
                Some(_) => {}
            }
        }
        if tip.after.log.is_poisoned() {
            if !self.turned_away(&tip, help) {
                return None;
            }
            self.roll_back(pipe);
        }
        Some((held, pipe.tip.load().after.clone()))
    }

    /// Discard every commit decided and not published (G2), and leave the
    /// pipeline resuming from the published prefix. Called by the thread
    /// that holds the round, so no sync and no unit's body is running. See
    /// the module docs.
    pub(super) fn roll_back(&self, pipe: &Pipeline) {
        // Deciding stops: no frontier pass succeeds from here on (R5).
        let decided = pipe.ring.freeze_decided();
        // The tip stops: a marker of the next epoch takes its place, which
        // no decision computed from the old state can replace. The slot the
        // old tip decided is the last one in the old state; a decision a
        // thread is still computing for the slot after it is stale, and its
        // slot is failed when it is settled (P4).
        let (seen, marker) = loop {
            let seen = pipe.tip.load();
            let mut after = seen.after.clone();
            after.epoch += 1;
            let marker = RingArc::new(Decision::new(seen.index, Fate::Unit, after));
            if pipe.tip.replace(&seen, RingArc::clone(&marker)) {
                break (seen, marker);
            }
        };
        let last = decided.max(seen.index);
        let mut after = marker.after.clone();
        let log = Arc::clone(&after.log);

        // No other thread publishes from here on. A slot another thread was
        // about to publish is one every thread calls publishable, so it is
        // published here rather than discarded (P3).
        let mut at = pipe.ring.freeze();
        while at < last {
            if let Loaded::Ready(desc) = pipe.ring.load(at + 1) {
                let publishable = desc
                    .decision()
                    .is_some_and(|decision| Self::publishable_slot(&desc, decision));
                if !publishable {
                    break;
                }
                if !desc.is(FAILED)
                    && let Some(last) = desc.decision().and_then(Decision::last_seq)
                {
                    self.visible_seq.publish(last);
                }
                desc.land();
                at += 1;
                pipe.ring.set_frozen(at);
                pipe.ring.retire(at);
            } else {
                // Voided, or already passed.
                at += 1;
                pipe.ring.set_frozen(at);
            }
        }

        // Everything in `(at, last]` is discarded: where its first record
        // starts, and the first sequence it took.
        let mut cut = None;
        let mut floor = None;
        let mut failed = Vec::new();
        for slot in (at + 1)..=last {
            let Loaded::Ready(desc) = pipe.ring.load(slot) else {
                continue;
            };
            if let Some(decision) = desc.decision() {
                if decision.commits() && floor.is_none() {
                    floor = Some(decision.base_seq);
                }
                if let Some(place) = &decision.place
                    && cut.is_none()
                    && Arc::ptr_eq(&decision.after.log, &log)
                {
                    cut = Some(place.offset);
                }
            }
            failed.push(desc);
        }

        // A sync covers a prefix of written records, so nothing durable lies
        // past the first discarded record; the bound is for a reader of
        // this line.
        let cut_at = cut.unwrap_or(after.next_off).max(log.durable());
        let (cause, cut_done) = log.rollback_to(cut_at, after.epoch);
        if let Err(err) = &cut_done {
            tracing::error!(error = %err, "cutting failed commits out of the write-ahead log failed");
            self.latch_wal_failure(err);
        }
        after.next_off = cut_at;
        if let Some(floor) = floor {
            after.next_seq = floor;
        }
        // The memtable is replaced whether or not a discarded commit was
        // applied to it: a thread that was applying a stale decision when
        // this began applies it to the old one, which no view shows again.
        match self.rebuild_memtable(&after.memtable, after.next_seq.saturating_sub(1)) {
            Ok(fresh) => {
                let old = Arc::clone(&after.memtable);
                self.view.update_memtables(|active, frozen| {
                    let active = if Arc::ptr_eq(active, &old) {
                        Arc::clone(&fresh)
                    } else {
                        Arc::clone(active)
                    };
                    (active, frozen.to_vec(), ())
                });
                after.memtable = fresh;
            }
            Err(err) => {
                tracing::error!(error = %err, "removing failed commits from the memtable failed");
                self.latch_wal_failure(&err);
            }
        }
        after.mem_used = after.memtable.approximate_size();
        // A discarded reservation may have handed its range out already:
        // its counter goes back before any later slot is decided.
        self.write_back_reservations(&mut after, &failed);

        let cause = cause.unwrap_or_else(|| {
            io::Error::other("the write-ahead log failed and unfinished commits were discarded")
        });
        if !failed.is_empty() {
            tracing::error!(error = %cause, commits = failed.len(), "a write-ahead log failure discarded commits");
        }
        for desc in &failed {
            desc.failure.put(crate::Error::clone_io(&cause));
            desc.mark(FAILED);
            // Discarded is landed too: what waited for the commit is told.
            desc.land();
        }
        if !failed.is_empty()
            && let Err(err) = self.notify_wal_error(&cause)
            && let Some(callback) = crate::Error::callback_panic_of(&err)
        {
            self.latch_callback_panic(callback);
        }
        // The state every later commit is decided from, then the frontiers:
        // they pass the discarded slots, and their owners learn.
        let resumed = RingArc::new(Decision::new(last, Fate::Unit, after));
        let replaced = pipe.tip.replace(&marker, resumed);
        debug_assert!(replaced, "only a rollback replaces its own marker");
        pipe.ring.thaw(last);
        pipe.ring.thaw_decided(last);
        // The jump names the slots past it as a pass would (P6).
        pipe.turns.name(last);
        for desc in &failed {
            pipe.ring.retire(desc.index());
        }
    }

    /// A copy of `old` holding only what was written at or below sequence
    /// `through`: what the active memtable is replaced with when commits
    /// applied to it are discarded.
    ///
    /// Costs one pass over the memtable. It runs only after a log failure,
    /// and a reader holding the old memtable keeps reading it: every
    /// snapshot is at or below the published horizon, which `through` is at
    /// or above.
    fn rebuild_memtable(&self, old: &MemTable, through: u64) -> io::Result<Arc<MemTable>> {
        let fresh = MemTable::new(&self.memtable_config)?;
        old.try_for_each_entry(|internal_key, value| {
            let (key, seq, value_type) = decode_internal_key(internal_key);
            if seq <= through {
                match value_type {
                    VALUE_TYPE_VALUE => fresh.put(key, value, seq),
                    VALUE_TYPE_DELETION => fresh.delete(key, seq),
                    VALUE_TYPE_MERGE => fresh.merge(key, value, seq),
                    _ => {
                        return Err(io::Error::other(
                            "a memtable entry has a kind this build does not know",
                        ));
                    }
                }
            }
            Ok(())
        })?;
        for tombstone in old.clone_range_tombstones() {
            if tombstone.seq <= through {
                fresh.delete_range(&tombstone.start, &tombstone.end, tombstone.seq);
            }
        }
        Ok(Arc::new(fresh))
    }
}
