//! The stages of the lock-free commit pipeline (plan 4.7;
//! `proofs/tla/CommitPipeline.tla`, `proofs/lean/Regolith/Pipeline.lean`,
//! `proofs/lean/Regolith/PipelineHelping.lean`).
//!
//! A commit takes a slot of the ring (`ring.rs`) and then passes five
//! stages. No stage belongs to a thread: whichever thread reaches a slot
//! does the slot's next stage, so a thread that stops anywhere in a stage
//! holds nobody up, and a thread never waits for another thread's turn.
//!
//! 1. **Decide**, in ring order. The slot is refused, loses a conflict, or
//!    takes its sequences, its byte range in the log and its place in the
//!    memtable's budget, all from the state the slot before it left
//!    ([`After`]). The decision is installed by one compare-and-swap, and
//!    every thread proceeds with the installed one. A slot that takes
//!    nothing takes no sequence and no record, so the log has no hole
//!    (`NoHole`).
//! 2. **Apply**, as the last act of deciding and before the next slot is
//!    decided: the slot's operations go into the memtable, idempotent per
//!    key and sequence (`engine::skiplist`, S2). The next slot's validation
//!    therefore reads every commit before it straight from the view. Nothing
//!    applied is visible to a reader until it is published: a snapshot reads
//!    at the published horizon.
//! 3. **Write**, in any order: the slot's owner encodes its record once it
//!    is decided, and the record goes to its range of the log, the same
//!    bytes whichever thread writes them (`engine::wal_log`). Records reach
//!    the log in runs, one host write each, through the write unit
//!    (`write_unit.rs`).
//! 4. **Sync**, one round at a time, each a unit one thread claims with one
//!    compare-and-swap (D53): the round syncs the written prefix of the log
//!    and raises the durable offset over it. A log that failed is rolled
//!    back in the round instead.
//! 5. **Publish**, in ring order: the read horizon passes a slot once its
//!    record is written and, at `Immediate` durability, durable
//!    (`DurableBeforeVisible`). A commit made by a blocking call returns
//!    then; one made by `commit_nowait` is told on its own queue.
//!
//! # What waits, and for what
//!
//! A thread never waits for a lock or for another thread to be scheduled. A
//! blocking call does wait for device I/O that is under way: a sync round
//! another thread claimed, or an ordered unit (`ordered.rs`) another thread
//! is running. It waits for a run of records another thread is writing
//! too, but with a bounded patience, after which it writes its record
//! itself (`write_unit.rs`, WU5). A call made through `commit_nowait` waits
//! for none of these: it leaves the unit on its queue and returns. The
//! queue's poll, which then runs the unit, waits for nothing another thread
//! has either: the unit parks (`park.rs`).
//!
//! # Invariants
//!
//! - **P1, decisions chain.** The decision of slot `i` is computed from the
//!   tip, the decision of the newest decided slot that holds one, read
//!   while `decided == i - 1`. The tip moves to slot `i`'s decision before
//!   `decided` moves to `i`, so no thread can read a tip older than the slot
//!   it is deciding against.
//! - **P2, applied before the next decision.** `decided` passes a slot only
//!   after its `APPLIED` flag is set.
//! - **P3, the horizon moves before the frontier.** A publisher raises the
//!   read horizon to the slot's last sequence and then moves `published`
//!   past the slot, so a commit that sees itself published can read its own
//!   writes. Only a slot every thread would call publishable is ever
//!   published, so a rollback that freezes the frontier in between
//!   publishes that slot itself rather than discard it.
//! - **P4, a failure discards a suffix.** A failed log write or sync
//!   poisons the log, and the rollback (`ordered.rs`), run by the thread
//!   that holds the round, fails every slot decided and not yet published,
//!   cuts their records out of the log and their operations out of the
//!   memtable, and resumes from the published prefix (G2). Nothing
//!   published is touched. It takes no slot of the ring, so it runs with
//!   the ring full. It freezes both frontiers, and the state it leaves is
//!   of a new epoch: a decision a thread was computing from the old state
//!   carries the old epoch, and its slot is failed when it is settled.
//! - **P6, a call far from its turn sleeps until its turn nears.** A
//!   blocking call whose slot is more than [`turn_wait::LEAD`] slots past the next
//!   slot to decide, and that finds that slot being decided by another thread,
//!   parks on its own slot instead of yielding to that thread over and over.
//!   The thread that passes the decided frontier over slot `i` names slot
//!   `i + LEAD` (a rollback's jump names the slots past it the same way),
//!   and the next blocking call whose commit is done wakes the owners of
//!   the slots named, so a sleeper wakes while [`turn_wait::LEAD`] slots are still to
//!   be decided before its own, and is helping by its turn. A slot that
//!   fails or lands wakes its owner too: another thread may have carried
//!   it all the way. A sleeper that nobody wakes wakes after
//!   [`turn_wait::TURN_PATIENCE`] and helps (`turn_wait.rs`, T1 to T3). The
//!   park changes no step a thread takes, only when it takes it: every safety
//!   rule above holds whatever the sleepers do, and progress holds with the
//!   patience bound (`PipelineHelping.lean`, `a_park_is_bounded`).
//! - **P5, helping is bounded.** A thread decides and publishes only slots
//!   at or below its own (a rollback slot aside), and each at most once. It
//!   writes the records of those slots, and a run it writes as the write
//!   unit's holder may reach past its own, by at most one run of
//!   `MAX_BATCH_RECORDS` records and `MAX_BATCH_BYTES` bytes, never past the
//!   decided frontier (WU6). So one call does at most a ring's worth of
//!   other commits' work and one run (E21). Waiting is bounded too: a wait
//!   for a run under way ends after a bounded patience (WU5).

use std::io;
use std::sync::Arc;

use super::super::RegolithEngine;
use super::super::callback::InCommit;
use super::super::io::job::Job;
use super::super::wal::check_write_len;
use super::append::AppendOrder;
use super::descriptor::{
    ADMITTED, APPLIED, APPLYING, After, DECIDING, Decision, Descriptor, FAILED, INSTALLED, NEAR,
    Verdict as Fate, WRITTEN, Work, is_immediate,
};
use super::hook;
use super::park::Help;
use super::request::Ops;
use super::ring::{ArcSlot, Installed, Loaded, Ring};
use super::round::Round;
use super::spare;
use super::turn_wait::{self, Turns};
use super::txn::{TxnRequest, Verdict};
use super::write_unit::{Await, Brought, WriteUnit};
use super::{Settled, WriteRequest};
use crate::perf_context::{PerfLevel, PerfTimer, PerfTimerField};
use crate::statistics::Ticker;
use crate::sync::internal::Arc as RingArc;
use crate::{Access, WriteBatchOp};

/// The commit pipeline of one database: the ring, the tip and the current
/// sync round.
pub(crate) struct Pipeline {
    pub(super) ring: Ring<Descriptor>,
    /// The decision of the newest decided slot that holds one (P1).
    pub(super) tip: ArcSlot<Decision>,
    /// The round a thread that needs the log durable, or rolled back,
    /// claims or waits on. Replaced by the thread that ran it, before it
    /// lands.
    pub(super) round: ArcSlot<Round>,
    /// The single-flight unit records reach the log through
    /// (`write_unit.rs`).
    pub(super) writes: WriteUnit,
    /// The calls asleep far from their turn, and the slots named and told
    /// as near it (P6, `turn_wait.rs`).
    pub(super) turns: Turns,
}

impl Pipeline {
    /// A pipeline with nothing in flight, whose first commit is decided from
    /// `start`.
    pub(crate) fn new(
        capacity: usize,
        start: After,
        engine: std::sync::Weak<RegolithEngine>,
    ) -> Self {
        let ring = Ring::new(capacity);
        let genesis = Decision::new(ring.capacity() - 1, Fate::Unit, start);
        let writes = WriteUnit::new(ring.capacity() - 1);
        Self {
            ring,
            tip: ArcSlot::new(RingArc::new(genesis)),
            round: ArcSlot::new(Round::new(engine)),
            writes,
            turns: Turns::new(),
        }
    }
}

/// How far a helping call got.
pub(super) enum Turn {
    /// Every slot asked for is decided.
    Done,
    /// A unit another thread is running, or one this call may not run, is
    /// in the way.
    Blocked(Arc<Job>),
}

/// How far a publishing call got.
pub(super) enum Progress {
    /// Every slot asked for is published.
    Published,
    /// The next slot waits for its record to be durable.
    NeedsSync,
    /// The log failed, or a rollback is discarding the slots past the
    /// frontier.
    Failed,
    /// The next slot is not decided yet.
    Undecided,
    /// A `commit_nowait` call met the write unit claimed by another thread
    /// on the way to the next slot's record: the job its queue waits on.
    Waits(Arc<Job>),
}

/// The operations slot `desc` lands, decided as `decision`: a request's own,
/// followed by the puts deciding it added (a transaction's numbered appends);
/// a counter reservation's batch; nothing for a unit.
pub(super) fn ops_of<'a>(desc: &'a Descriptor, decision: &'a Decision) -> Ops<'a> {
    match (&desc.work, &decision.batch) {
        (Work::Request(WriteRequest::Txn(txn)), Some(added)) => Ops::Joined(&txn.ops, added),
        (Work::Request(request), _) => request.ops(),
        (Work::Allocate { .. } | Work::Unit { .. }, Some(batch)) => Ops::Batch(batch),
        (Work::Allocate { .. } | Work::Unit { .. }, None) => Ops::None,
    }
}

/// Yields a thread gives another before repeating a stage that thread
/// began, whatever the stage's size.
const COURTESY_YIELDS: usize = 32;

/// Give the thread already working on a stage about as long as the stage
/// takes, `size` being a measure of its work, before the caller repeats it;
/// returns once `done` says the stage is finished or the courtesy ran out.
///
/// Helping is what keeps a stopped thread from holding others (P5), but
/// several threads repeating one stage at once is wasted work, and in the
/// apply stage wasted memtable bytes. The courtesy is bounded, in yields and
/// never in time, so a thread that did stop costs each helper a fixed number
/// of yields and nothing more. A target with one thread has nobody to yield
/// to.
///
/// Its two callers, a decision under way and an apply under way, stay
/// yields where the write path's waits became parks (`write_unit.rs`). A
/// wait there is about one decision or one apply long, and the thread that
/// ends it is on the serial decide path: a park needs a host call to wake
/// it on that path, which measured slower (E45: parks until the slot was
/// published lost about 30% at 64 writers, wakes from the deciding thread
/// about 18%). The long waits, for a turn many slots away, park instead
/// (P6, `turn_wait.rs`).
fn courtesy(size: usize, done: impl Fn() -> bool) {
    if !crate::env::PLATFORM_THREADS {
        return;
    }
    for _ in 0..COURTESY_YIELDS.saturating_add(size.saturating_mul(2)) {
        if done() {
            return;
        }
        std::thread::yield_now();
    }
}

impl RegolithEngine {
    /// The commit pipeline. A database opened read-only has none.
    pub(super) fn pipe(&self) -> io::Result<&Pipeline> {
        self.pipeline.as_ref().ok_or_else(Self::read_only_error)
    }

    /// The pipeline's state after the newest decided slot.
    pub(crate) fn pipeline_state(&self) -> Option<RingArc<Decision>> {
        self.pipeline.as_ref().map(|pipe| pipe.tip.load())
    }

    /// Test-only: whether nothing is in flight in the commit pipeline: every
    /// slot handed out is decided and published.
    #[cfg(test)]
    pub(crate) fn pipeline_is_idle(&self) -> bool {
        self.pipeline.as_ref().is_none_or(|pipe| {
            let last = pipe.ring.tail() - 1;
            pipe.ring.decided() == last && pipe.ring.published() == last
        })
    }

    /// Test-only: how many slots the commit ring has handed out.
    #[cfg(test)]
    pub(crate) fn pipeline_slots_taken(&self) -> u64 {
        self.pipeline.as_ref().map_or(0, |pipe| pipe.ring.tail())
    }

    /// Test-only: the offset the next commit's record goes to in the active
    /// log.
    #[cfg(test)]
    pub(crate) fn log_end(&self) -> u64 {
        self.pipeline_state().map_or(0, |tip| tip.after.next_off)
    }

    /// Take a slot for `desc` and install it: reserve an index with one
    /// atomic add and put the descriptor in its cell, again with a new index
    /// whenever a helper voided the one in hand. Returns the index.
    pub(super) fn enter(&self, pipe: &Pipeline, desc: &RingArc<Descriptor>) -> u64 {
        loop {
            match self.try_enter(pipe, desc, Help::Wait) {
                Ok(index) => return index,
                // A unit this thread may not run itself: the decision of a
                // slot that reads the device (`read_mode.rs`).
                Err(job) => self.run_or_wait(&job),
            }
        }
    }

    /// [`Self::enter`] for a call that may not wait: a full ring then hands
    /// back the unit in the way of making room, and the reservation is given
    /// up. A helper voids it, and the next attempt reserves again.
    pub(super) fn try_enter(
        &self,
        pipe: &Pipeline,
        desc: &RingArc<Descriptor>,
        help: Help<'_>,
    ) -> Result<u64, Arc<Job>> {
        loop {
            let index = pipe.ring.reserve();
            desc.set_index(index);
            hook::at(hook::Stage::Reserve);
            loop {
                match pipe.ring.install(index, desc) {
                    Installed::Done => {
                        if !desc.is_blocking() {
                            desc.mark(INSTALLED);
                        }
                        return Ok(index);
                    }
                    Installed::Lost => break,
                    // The ring is full: the slot one lap earlier is not
                    // published yet, and this thread has no slot of its own
                    // to wait in.
                    Installed::Full => self.make_room(pipe, index, help)?,
                }
            }
        }
    }

    /// Finish the slot a full ring's cell still holds for `index`: the one a
    /// lap earlier. A blocking call brings it through like a commit of its
    /// own; one that may not wait hands back the unit in the way.
    fn make_room(&self, pipe: &Pipeline, index: u64, help: Help<'_>) -> Result<(), Arc<Job>> {
        let lap = index - pipe.ring.capacity();
        loop {
            if pipe.ring.published() >= lap {
                return Ok(());
            }
            if let Turn::Blocked(job) = self.help_decide(pipe, lap, help, None) {
                if !matches!(help, Help::Wait) {
                    return Err(job);
                }
                self.run_or_wait(&job);
                continue;
            }
            match self.publish_upto(pipe, lap, None, Await::of(help)) {
                Progress::Published => return Ok(()),
                Progress::NeedsSync | Progress::Failed => self.sync_round(pipe, help)?,
                Progress::Waits(job) => return Err(job),
                Progress::Undecided => {}
            }
        }
    }

    /// Decide every slot up to and including `upto`, in ring order (P1),
    /// voiding the ones reserved and never installed. A round or a unit in
    /// the way is dealt with as `help` says (`park.rs`), and handed back
    /// when the call does not get past it. `own` is the caller's installed
    /// descriptor, if it has one: see [`Self::own_slot`].
    pub(super) fn help_decide(
        &self,
        pipe: &Pipeline,
        upto: u64,
        help: Help<'_>,
        own: Option<&RingArc<Descriptor>>,
    ) -> Turn {
        // The slot this call last parked far from its turn at (P6): a call
        // whose patience ran out with the frontier where it was helps, and
        // parks again only once the frontier moved on.
        let mut parked_at = None;
        loop {
            let decided = pipe.ring.decided();
            if decided >= upto {
                return Turn::Done;
            }
            if pipe.ring.is_deciding_frozen() {
                // A rollback holds the frontier. It runs in the round.
                let round = self.current_round(pipe);
                if !self.pass(&round.job, help) {
                    return Turn::Blocked(Arc::clone(&round.job));
                }
                continue;
            }
            let index = decided + 1;
            let loaded;
            let desc = match Self::own_slot(own, index) {
                Some(desc) => desc,
                None => match pipe.ring.load(index) {
                    Loaded::Ready(desc) => {
                        loaded = desc;
                        &loaded
                    }
                    // Void, or its cell already took a later lap: a void slot
                    // holds nothing, so its cell is not kept for it (R6).
                    Loaded::Void | Loaded::Gone => {
                        self.pass_decided(pipe, index);
                        continue;
                    }
                    Loaded::Empty => {
                        // Reserved, not installed: its owner may have stopped.
                        // Void it; the owner's install then fails and it
                        // reserves again.
                        pipe.ring.void(index);
                        continue;
                    }
                    Loaded::Full => {
                        // The cell still holds the slot a lap earlier.
                        let lap = index - pipe.ring.capacity();
                        match self.publish_upto(pipe, lap, None, Await::of(help)) {
                            Progress::Published | Progress::Undecided => {}
                            Progress::NeedsSync | Progress::Failed => {
                                if let Err(job) = self.sync_round(pipe, help) {
                                    return Turn::Blocked(job);
                                }
                            }
                            Progress::Waits(job) => return Turn::Blocked(job),
                        }
                        continue;
                    }
                },
            };
            let mut seen = None;
            let mut applies = false;
            if desc.decision().is_none() {
                // The thread that claims the decision claims its apply in
                // the same instruction: it goes on to apply what it decided.
                let before = desc.claim(DECIDING | APPLYING);
                applies = before & APPLYING == 0;
                if before & DECIDING != 0 {
                    // Another thread is deciding it now. A blocking call far
                    // from its turn sleeps until its turn nears (P6), and
                    // looks again; one near its turn gives the other thread
                    // about as long as the decision takes before it repeats
                    // the work (P5).
                    if let Some(own) = own
                        && matches!(help, Help::Wait)
                        && parked_at != Some(index)
                        && self.park_far(pipe, own, index)
                    {
                        parked_at = Some(index);
                        if applies {
                            // Nobody waits on a claim this thread left.
                            desc.unclaim(APPLYING);
                        }
                        continue;
                    }
                    courtesy(desc.work_size(), || desc.decision().is_some());
                }
                if desc.decision().is_none() {
                    hook::at(hook::Stage::Decide);
                    match self.decide(pipe, index, desc, help) {
                        Ok(tip) => seen = Some(tip),
                        Err(job) => {
                            if applies {
                                // Nothing is decided to apply: the thread
                                // that decides it applies it.
                                desc.unclaim(APPLYING);
                            }
                            return Turn::Blocked(job);
                        }
                    }
                }
            }
            self.settle(pipe, index, desc, seen, applies);
        }
    }

    /// `own` when it is the descriptor installed at `index`: what
    /// [`Ring::load`] would hand back for that slot, without the pin and the
    /// reference it takes.
    ///
    /// A frontier reaches `index` only while the slot is not past it, so not
    /// published and not retired: the cell still holds the descriptor its
    /// owner installed, and an installed slot is never voided (R1). A caller
    /// passes `own` only once its descriptor is installed, so its index is
    /// the slot's. Should another thread pass the slot meanwhile, every step
    /// taken with `own` is one a reference loaded a moment earlier would take
    /// too, and each is checked by the compare-and-swap that ends it.
    fn own_slot(own: Option<&RingArc<Descriptor>>, index: u64) -> Option<&RingArc<Descriptor>> {
        own.filter(|own| own.index() == index)
    }

    /// Give slot `index` its decision, unless it has one by the time this
    /// returns or a unit is in the way (`Err`). `Ok` holds the tip this call
    /// read last, which [`Self::settle`] starts from while it is still the
    /// tip.
    fn decide(
        &self,
        pipe: &Pipeline,
        index: u64,
        desc: &RingArc<Descriptor>,
        help: Help<'_>,
    ) -> Result<RingArc<Decision>, Arc<Job>> {
        // P1: the tip read here is the state after slot `index - 1` as long
        // as that slot is still the newest decided one, which is checked
        // after the read. A decision computed from an older tip loses the
        // install below to the one that already decided the slot.
        let mut prev = pipe.tip.load();
        if !pipe.ring.is_next_to_decide(index) || desc.decision().is_some() {
            return Ok(prev);
        }
        if let Work::Unit { job, .. } = &desc.work {
            if job.is_done() {
                // The unit's body ended without deciding the slot: it
                // unwound. The pipeline goes on from where it was.
                desc.decide(Decision::new(index, Fate::Unit, prev.after.clone()));
                return Ok(prev);
            }
            if self.pass(job, help) {
                return Ok(prev);
            }
            return Err(Arc::clone(job));
        }

        let mut refused = None;
        // Read after the tip: a tip the check already replaced means its
        // job was made before, so it is seen here.
        if desc.turn_job.get().is_some() || self.turn_is_due(desc, &prev.after) {
            refused = self
                .take_turn(index, desc, help)?
                .refused
                .as_ref()
                .map(crate::Error::clone_io);
            // The check left the state it made, rotated or not, in the tip,
            // before its job landed.
            prev = pipe.tip.load();
            if !pipe.ring.is_next_to_decide(index) {
                return Ok(prev);
            }
        }
        let base = &prev.after;
        let decision = match refused {
            Some(err) => Decision::new(index, Fate::Refused(err), base.clone()),
            None => self.decide_reading(index, desc, base)?,
        };
        if let Fate::Conflict(conflict) = &decision.verdict {
            let ticker = match conflict.mine() {
                Access::Put | Access::Delete | Access::Merge => Ticker::CommitConflictsOnWrite,
                _ => Ticker::CommitConflictsOnRead,
            };
            // Counted by the thread whose decision is the slot's (D1).
            if desc.decide(decision)
                && let Some(s) = self.statistics()
            {
                s.add(ticker, 1);
            }
        } else {
            desc.decide(decision);
        }
        Ok(prev)
    }

    /// The decision of a write, a transaction or a counter reservation from
    /// `base`, the pipeline's state before the slot. Pure in what it
    /// installs: two threads may compute it at once, and the slot keeps one.
    pub(super) fn decide_work(&self, index: u64, desc: &Descriptor, base: &After) -> Decision {
        let _commit = InCommit::enter();
        let refuse = |err: io::Error| Decision::new(index, Fate::Refused(err), base.clone());
        if base.closed {
            return refuse(Self::closed_error());
        }
        if let Err(err) = self.ensure_writable() {
            return refuse(err);
        }
        let mut decision = Decision::new(index, Fate::Write, base.clone());
        match &desc.work {
            Work::Request(request) => {
                // The column-family fence, before any sequence (`families.rs`).
                if let Err(err) = self.cf_fence(request) {
                    return refuse(err);
                }
                match request {
                    WriteRequest::Put {
                        durability,
                        disable_wal,
                        ..
                    }
                    | WriteRequest::Batch {
                        durability,
                        disable_wal,
                        ..
                    } => {
                        decision.immediate = is_immediate(*durability) && !disable_wal;
                    }
                    WriteRequest::Txn(txn) => {
                        if let Err(err) = self.decide_txn(txn, base, &mut decision) {
                            return refuse(err);
                        }
                        decision.immediate = is_immediate(txn.durability);
                    }
                }
                if matches!(decision.verdict, Fate::Conflict(_)) {
                    return decision;
                }
                let logged = !request.skips_wal();
                if let Err(err) = Self::take_place(desc, &mut decision, logged) {
                    return refuse(err);
                }
                // A counter the slot writes is read again by the next
                // reservation of it (`counters.rs`).
                let kept = decision
                    .after
                    .counters
                    .forget_written(&ops_of(desc, &decision));
                if let Some(kept) = kept {
                    decision.after.counters = kept;
                }
            }
            Work::Allocate { key, n, .. } => {
                if let Some(err) = Self::abandoned(desc) {
                    return refuse(err);
                }
                // Fenced like any write: the counter lives in a family.
                if let Err(err) = self.cf_fence_keys(std::iter::once(key.as_slice())) {
                    return refuse(err);
                }
                if let Err(err) = self.decide_allocation(key, *n, base, &mut decision) {
                    return refuse(err);
                }
                if let Err(err) = Self::take_place(desc, &mut decision, true) {
                    return refuse(err);
                }
            }
            Work::Unit { .. } => {}
        }
        decision
    }

    /// Give `decision` its sequences, its record's place and its share of
    /// the memtable's budget, from the state it already carries. A slot with
    /// no operations takes nothing.
    fn take_place(desc: &Descriptor, decision: &mut Decision, logged: bool) -> io::Result<()> {
        let (ops, payload, cost) = {
            let ops = ops_of(desc, decision);
            (ops.count(), ops.payload_len(), ops.memtable_cost())
        };
        if ops == 0 {
            decision.immediate = false;
            return Ok(());
        }
        let after = &mut decision.after;
        let place = if logged {
            // The last line of defence for a request that reached the
            // pipeline without its producer's check.
            check_write_len(payload)?;
            Some(after.log.place(after.next_off, payload, after.epoch)?)
        } else {
            None
        };
        decision.base_seq = after.next_seq;
        decision.ops = ops;
        after.next_seq += ops;
        after.mem_used = after.mem_used.saturating_add(cost);
        if let Some(place) = &place {
            after.next_off = place.end();
        }
        decision.place = place;
        Ok(())
    }

    /// Validate a transaction against the view, which holds every commit
    /// decided before it (P2), and number its appends. Sets the verdict,
    /// and the batch when the appends added operations.
    fn decide_txn(
        &self,
        txn: &TxnRequest,
        base: &After,
        decision: &mut Decision,
    ) -> io::Result<()> {
        let ceiling = base.next_seq.saturating_sub(1);
        let run = || -> io::Result<(Fate, Option<Vec<WriteBatchOp>>)> {
            // Every item clean at the horizon and nothing handed a sequence
            // above it: the check above the horizon finds nothing, item by
            // item (`RegolithEngine::validate`), so it needs no view. Appends
            // are numbered against the view.
            if txn.appends.is_empty() && txn.early.is_clean() && ceiling <= txn.early.horizon {
                let fate = Fate::Commit {
                    merges_commuted: 0,
                    writes_elided: 0,
                };
                return Ok((fate, None));
            }
            hook::at(hook::Stage::Validate);
            let view = self.view.load();
            let fate = match self.validate(&txn.checks, &txn.ops, &txn.early, &view, ceiling)? {
                Verdict::Conflict(conflict) => return Ok((Fate::Conflict(conflict), None)),
                Verdict::Accept {
                    merges_commuted,
                    writes_elided,
                } => Fate::Commit {
                    merges_commuted,
                    writes_elided,
                },
            };
            // Positions are taken after validation and never before, so a
            // commit that aborted took none (RED AssignBeforeValidation).
            if txn.appends.is_empty() {
                return Ok((fate, None));
            }
            // Only the puts the appends add are built here: the slot lands
            // the request's own operations followed by them (`ops_of`), so
            // neither the operations nor the appends are copied.
            let mut added = Vec::new();
            AppendOrder::default().number(self, &view, &txn.appends, &mut added)?;
            Ok((fate, Some(added)))
        };
        let decided = if txn.perf == PerfLevel::Disable {
            run()
        } else {
            let (decided, counted) = crate::perf_context::on_behalf(txn.perf, run);
            decision.perf = Some(Box::new(counted));
            decided
        };
        match decided {
            Ok((fate, batch)) => {
                decision.verdict = fate;
                decision.batch = batch;
                Ok(())
            }
            Err(err) => {
                // A caller's code panicked inside the ordered step: the step
                // it left half done is not one any later write may build on.
                if let Some(callback) = crate::Error::callback_panic_of(&err) {
                    self.latch_callback_panic(callback);
                }
                Err(err)
            }
        }
    }

    /// Finish deciding slot `index`: apply it, move the tip to its decision
    /// and pass the decided frontier over it (P1, P2). A decision computed
    /// from a state a rollback has since discarded lands nothing: its slot
    /// is failed instead (P4).
    ///
    /// `seen` is a tip the caller read, used in place of a new load for as
    /// long as it is still the tip: the slot holds it at the moment that is
    /// checked, which is all a load would tell. `applies`: the caller
    /// claimed the slot's apply already, with its decision.
    pub(super) fn settle(
        &self,
        pipe: &Pipeline,
        index: u64,
        desc: &RingArc<Descriptor>,
        mut seen: Option<RingArc<Decision>>,
        applies: bool,
    ) {
        // Borrowed: `desc` holds its decision for as long as it lives. A
        // reference of its own is taken only for the tip.
        let Some(decision) = desc.decision() else {
            return;
        };
        loop {
            let tip = match seen.take() {
                Some(tip) if pipe.tip.holds(&tip) => tip,
                _ => pipe.tip.load(),
            };
            if tip.index >= decision.index {
                // Another thread moved the tip here, or a rollback took the
                // slot in.
                break;
            }
            if tip.after.epoch != decision.after.epoch {
                // A rollback ran between the tip this decision was computed
                // from and now. Nothing of the decision is used: its
                // sequences and its place belong to the state that is gone.
                desc.mark(FAILED);
                break;
            }
            if decision.commits() && !desc.is(APPLIED) {
                // Another thread is applying it now: the same courtesy as
                // for a decision. Two threads applying one slot at once is
                // harmless, and costs the arena every node that lost its
                // race.
                if !applies && !desc.mark(APPLYING) {
                    courtesy(decision.ops as usize, || desc.is(APPLIED));
                }
                if !desc.is(APPLIED) {
                    hook::at(hook::Stage::Apply);
                    let _perf = PerfTimer::new(PerfTimerField::WriteMemtable);
                    ops_of(desc, decision).apply(&decision.after.memtable, decision.base_seq);
                    desc.mark(APPLIED);
                }
            }
            let Some(next) = desc.decision_arc() else {
                break;
            };
            if pipe.tip.replace(&tip, next) {
                // The tip that was: this may be its last reference, and its
                // allocation then serves this thread's next decision.
                spare::keep_decision(tip);
                break;
            }
        }
        self.pass_decided(pipe, index);
    }

    /// Pass the decided frontier from `index - 1` to `index` (R5) and, when
    /// this call passed it and somebody sleeps far from its turn, wake the
    /// owner of slot `index + LEAD` (P6, `Turns::name`). `true` when this
    /// call passed it.
    pub(super) fn pass_decided(&self, pipe: &Pipeline, index: u64) -> bool {
        if !pipe.ring.pass_decided(index) {
            return false;
        }
        pipe.turns.name(index);
        true
    }

    /// Wake the calls parked far from their turn whose turn now nears, up
    /// to the slot the decided frontier last named (P6). Called by a
    /// blocking call whose commit is done, off the decided frontier's path;
    /// each slot is told once, by whichever call claims it.
    pub(super) fn wake_near(&self, pipe: &Pipeline) {
        let Some((from, to)) = pipe.turns.claim_told() else {
            return;
        };
        // A published slot is retired: its owner, if asleep, was woken as it
        // landed (`Descriptor::land`).
        for slot in (from + 1).max(pipe.ring.published() + 1)..=to {
            if let Loaded::Ready(desc) = pipe.ring.load(slot) {
                desc.mark(NEAR);
            }
        }
    }

    /// Park the blocking call that owns `own`, installed, until its turn
    /// nears: the decided frontier within [`turn_wait::LEAD`] slots of its slot,
    /// or [`turn_wait::TURN_PATIENCE`] passed (P6). `false` when it is near its turn
    /// already, and does not park; the caller then gives the thread deciding
    /// slot `next` its courtesy.
    pub(super) fn park_far(&self, pipe: &Pipeline, own: &RingArc<Descriptor>, next: u64) -> bool {
        let turns = &pipe.turns;
        if !crate::env::PLATFORM_THREADS || !own.is_blocking() || own.index() <= next + turns.lead()
        {
            return false;
        }
        // Counted before it registers: a thread that passes the frontier
        // after this names its slot (T1).
        turns.count_in();
        hook::at(hook::Stage::TurnWait);
        own.park(turn_wait::TURN_PATIENCE, || {
            turns.near(own, pipe.ring.decided())
        });
        turns.count_out();
        true
    }

    /// Whether slot `desc` may be published: it takes nothing, or its record
    /// is written and, when it asked for `Immediate`, durable. A slot is
    /// applied before it is decided (P2), so that is not asked here.
    pub(super) fn publishable_slot(desc: &Descriptor, decision: &Decision) -> bool {
        if !decision.commits() || desc.is(FAILED) {
            return true;
        }
        match &decision.place {
            None => true,
            Some(place) => {
                desc.is(WRITTEN)
                    && (!decision.immediate || place.end() <= decision.after.log.durable())
            }
        }
    }

    /// Publish every slot up to and including `limit`, in ring order,
    /// writing the records in the way as `how` says (P3, `write_unit.rs`).
    /// Stops at the first slot that is not publishable yet and says why.
    /// `own` is the caller's installed descriptor, if it has one: see
    /// [`Self::own_slot`].
    pub(super) fn publish_upto(
        &self,
        pipe: &Pipeline,
        limit: u64,
        own: Option<&RingArc<Descriptor>>,
        how: Await,
    ) -> Progress {
        loop {
            if pipe.ring.is_frozen() {
                return Progress::Failed;
            }
            let published = pipe.ring.published();
            if published >= limit {
                return Progress::Published;
            }
            let next = published + 1;
            if next > pipe.ring.decided() {
                return Progress::Undecided;
            }
            let loaded;
            let desc = match Self::own_slot(own, next) {
                Some(desc) => desc,
                None => match pipe.ring.load(next) {
                    Loaded::Ready(desc) => {
                        loaded = desc;
                        &loaded
                    }
                    // As when deciding: a void slot, whose cell may have moved
                    // on (R6).
                    Loaded::Void | Loaded::Gone => {
                        pipe.ring.pass_published(next);
                        continue;
                    }
                    Loaded::Empty | Loaded::Full => return Progress::Undecided,
                },
            };
            let Some(decision) = desc.decision() else {
                return Progress::Undecided;
            };
            if !Self::publishable_slot(desc, decision) {
                if decision.after.log.is_poisoned() {
                    return Progress::Failed;
                }
                if !desc.is(WRITTEN) {
                    match self.bring_written(pipe, desc, how) {
                        Brought::Written => continue,
                        // The write failed, or a rollback discarded the slot
                        // under it.
                        Brought::Failed => return Progress::Failed,
                        Brought::Waits(job) => return Progress::Waits(job),
                    }
                }
                return Progress::NeedsSync;
            }
            self.publish_slot(pipe, next, desc, decision);
        }
    }

    /// Publish slot `index`, held by `desc` and decided as `decision`, which
    /// is publishable and the next to publish (P3).
    fn publish_slot(&self, pipe: &Pipeline, index: u64, desc: &Descriptor, decision: &Decision) {
        hook::at(hook::Stage::Publish);
        // P3: the horizon first, then the frontier. A failed slot took no
        // sequence that still exists.
        if !desc.is(FAILED)
            && let Some(last) = decision.last_seq()
        {
            self.visible_seq.publish(last);
        }
        // Landed: visible, or holding nothing. Told before the frontier
        // passes, so a publisher that stops between the two leaves the
        // telling to the next one here.
        desc.land();
        if pipe.ring.pass_published(index) {
            pipe.ring.retire(index);
        }
    }

    /// Publish, in ring order, the slots up to `limit` that are publishable
    /// now, writing nothing and waiting for nothing: what the write unit's
    /// holder does for the run it wrote (`write_unit.rs`, WU7), so the
    /// owners it then wakes find their commits published. `own` is a
    /// descriptor the caller holds, installed: see [`Self::own_slot`].
    pub(super) fn publish_ready(
        &self,
        pipe: &Pipeline,
        limit: u64,
        own: Option<&RingArc<Descriptor>>,
    ) {
        loop {
            if pipe.ring.is_frozen() {
                return;
            }
            let published = pipe.ring.published();
            if published >= limit {
                return;
            }
            let next = published + 1;
            if next > pipe.ring.decided() {
                return;
            }
            let loaded;
            let desc = match Self::own_slot(own, next) {
                Some(desc) => desc,
                None => match pipe.ring.load(next) {
                    Loaded::Ready(desc) => {
                        loaded = desc;
                        &loaded
                    }
                    // A void slot, as in `publish_upto`.
                    Loaded::Void | Loaded::Gone => {
                        pipe.ring.pass_published(next);
                        continue;
                    }
                    Loaded::Empty | Loaded::Full => return,
                },
            };
            let Some(decision) = desc.decision() else {
                return;
            };
            if !Self::publishable_slot(desc, decision) {
                return;
            }
            self.publish_slot(pipe, next, desc, decision);
        }
    }

    /// Take a slot for a `commit_nowait` commit, if it has none yet, and
    /// drive it as far as it goes without waiting: `Err` is the unit in its
    /// way. Before it takes its slot a commit that writes passes the
    /// write-stall admission: a stop is a unit in its way like any other
    /// (the stall's, which lands when the stop is relieved), and what says
    /// the stop will not clear refuses the commit, which is then over
    /// (`Ok`, with [`Descriptor::is_refused`]) and is not brought here
    /// again.
    pub(super) fn advance_nowait(
        &self,
        pipe: &Pipeline,
        desc: &RingArc<Descriptor>,
    ) -> Result<(), Arc<Job>> {
        if !desc.is(INSTALLED) {
            if !desc.is(ADMITTED) {
                match self.admit_commit_nowait() {
                    Ok(None) => {
                        desc.mark(ADMITTED);
                    }
                    Ok(Some(stall)) => return Err(stall),
                    Err(told) => {
                        desc.refuse(told.into_io_error());
                        return Ok(());
                    }
                }
            }
            self.try_enter(pipe, desc, Help::Leave)?;
        }
        self.drive_nowait(pipe, desc.index(), desc)
    }

    /// What `commit_nowait` learns from the pipeline.
    pub(super) fn drive_nowait(
        &self,
        pipe: &Pipeline,
        index: u64,
        desc: &RingArc<Descriptor>,
    ) -> Result<(), Arc<Job>> {
        loop {
            if let Turn::Blocked(job) = self.help_decide(pipe, index, Help::Leave, Some(desc)) {
                return Err(job);
            }
            if !self.write_alone(pipe, desc) {
                self.fill(desc);
            }
            if let Brought::Waits(job) = self.bring_written(pipe, desc, Await::Leave) {
                return Err(job);
            }
            match self.publish_upto(pipe, index, Some(desc), Await::Leave) {
                Progress::Published => return Ok(()),
                Progress::NeedsSync | Progress::Failed => self.sync_round(pipe, Help::Leave)?,
                Progress::Waits(job) => return Err(job),
                Progress::Undecided => {}
            }
        }
    }

    /// Bring slot `index` through every stage and return once it is
    /// published or discarded: what a blocking commit does. It helps every
    /// slot before its own, runs the units in its way, and waits only for
    /// device I/O another thread has under way, and for a run of records
    /// with a bounded patience (`write_unit.rs`, WU5).
    pub(super) fn drive(&self, pipe: &Pipeline, index: u64, desc: &RingArc<Descriptor>) {
        super::super::io::scope::waits("a commit that blocks until it is published");
        loop {
            if let Turn::Blocked(job) = self.help_decide(pipe, index, Help::Wait, Some(desc)) {
                self.run_or_wait(&job);
                continue;
            }
            if !self.write_alone(pipe, desc) {
                self.fill(desc);
            }
            // A blocking call is never left waiting on a queue. One that
            // waits for a sync does not sleep on its record while another
            // thread has it in hand: it copies it, as the round's holder
            // does, and is awake to claim the next round (WU5).
            let how = if desc.decision().is_some_and(|d| d.immediate) {
                Await::Copy
            } else {
                Await::Park
            };
            let _ = self.bring_written(pipe, desc, how);
            match self.publish_upto(pipe, index, Some(desc), Await::Park) {
                Progress::Published => {
                    // Done: wake who waits for its turn, off the decided
                    // frontier's path (P6).
                    self.wake_near(pipe);
                    return;
                }
                Progress::NeedsSync | Progress::Failed => {
                    // A blocking call gets past the round.
                    let _ = self.sync_round(pipe, Help::Wait);
                }
                Progress::Undecided | Progress::Waits(_) => {}
            }
        }
    }

    /// What slot `desc`, published or discarded, tells its owner.
    ///
    /// The owner asks once, and every call that writes comes through here
    /// (a put, a batch, a column family's, a transaction's commit, waited
    /// for or not), so this is where a write is counted as written in
    /// `stats`: once it landed, with the operations that landed. One the
    /// pipeline refused or discarded, or that lost a conflict, is no write.
    pub(super) fn settled(
        stats: Option<&crate::Statistics>,
        desc: &Descriptor,
    ) -> io::Result<Settled> {
        if desc.is(FAILED) {
            return Err(desc.failure.take().unwrap_or_else(|| {
                io::Error::other("the write-ahead log failed and this commit was discarded")
            }));
        }
        let Some(decision) = desc.decision() else {
            return Err(io::Error::other("a commit finished without a decision"));
        };
        // The decision is shared, so its count is copied out.
        let perf = decision.perf.as_deref().map(|counted| Box::new(*counted));
        let landed = || {
            if let Some(stats) = stats {
                ops_of(desc, decision).count_written(stats);
            }
        };
        match &decision.verdict {
            Fate::Write => {
                landed();
                Ok(Settled::Write(decision.last_seq().unwrap_or(0)))
            }
            Fate::Commit {
                merges_commuted,
                writes_elided,
            } => {
                landed();
                Ok(Settled::Committed {
                    seq: decision.last_seq(),
                    merges_commuted: *merges_commuted,
                    writes_elided: *writes_elided,
                    perf,
                })
            }
            Fate::Conflict(conflict) => Ok(Settled::Conflict {
                conflict: conflict.clone(),
                perf,
            }),
            Fate::Refused(err) => Err(crate::Error::clone_io(err)),
            Fate::Allocated(_) | Fate::Unit => Err(io::Error::other(
                "a commit settled as something that is not a commit",
            )),
        }
    }

    /// Hand `request` to the pipeline and block until it is published, or
    /// until the pipeline discards it, and return what was decided for it.
    pub(super) fn commit_through_pipeline(&self, request: WriteRequest) -> io::Result<Settled> {
        let pipe = self.pipe()?;
        let desc = spare::descriptor(Work::Request(request), true);
        let index = self.enter(pipe, &desc);
        self.drive(pipe, index, &desc);
        let settled = Self::settled(self.statistics(), &desc);
        spare::keep_descriptor(desc);
        settled
    }
}
