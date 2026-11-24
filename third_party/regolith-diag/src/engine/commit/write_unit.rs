//! The write unit: how decided commits' records reach the log (plan 4.7,
//! D53; `WriteUnit` in `proofs/tla/CommitPipeline.tla`, `PipelineHelping.lean`,
//! the loom models in `loom_model.rs`).
//!
//! A host puts the writes of one file one after another, whoever makes them:
//! a positional write takes the file's lock, and a second writer spins on it
//! or sleeps. Sixty-four threads each writing their own record keep sixty-three
//! of them spinning in the kernel and buy nothing. So the records go to the
//! log in runs, one host write each, through a single-flight unit:
//!
//! 1. **Fill.** Once its slot is decided, a commit's owner encodes its record
//!    into its own descriptor's buffer and sets the slot's `FILLED` flag
//!    (D4 in `descriptor.rs`). Owners encode in parallel. A thread that needs
//!    another's record written and finds nobody encoding it encodes it the
//!    same way, rather than wait for an owner that may not be running;
//!    nobody encodes while holding the unit.
//! 2. **Claim.** A thread that needs a record written claims the unit with
//!    one compare-and-swap of its word.
//! 3. **Gather.** The holder collects the run of filled, unwritten records
//!    that starts at the first one not written, in ring order, as long as
//!    their places follow one another in one log of one epoch: pointers to
//!    finished bytes, nothing encoded.
//! 4. **Count in, look again, write.** It counts the write in at the log's
//!    gate, looks at every gathered record again and keeps those before the
//!    first one another thread wrote meanwhile (WU3), and writes them with
//!    one vectored positional write.
//! 5. **Mark, let go, wake.** It counts out, marks each record written, lets
//!    the unit go, and only then wakes the threads parked on those records.
//!
//! # Who waits, and for how long
//!
//! A thread whose record is not written finds the unit claimed by another
//! thread, or the record being encoded by another thread.
//!
//! - **A blocking commit** ([`Await::Park`]) spins about as long as one
//!   write takes, then parks on the record. Whoever writes it, fills it or
//!   fails it wakes it, and the holder that lets the unit go wakes every
//!   parked record that is still waiting (WU4). Its patience is bounded: past
//!   [`PATIENCE`] since it first had to wait, or once the decided frontier
//!   moved [`FRONTIER_PATIENCE`] slots while the claim it saw stayed put, or
//!   with the ring full, it writes the record itself, as a copy through the
//!   gate (WU5). A holder that was preempted therefore stalls nobody for
//!   longer than that.
//! - **A `commit_nowait` call** ([`Await::Leave`]) spins as long, then leaves
//!   the unit on its queue: the job it waits on is released by the holder
//!   that lets the unit go, which tells every queue waiting on it (D53). It
//!   never waits for another thread.
//! - **A thread that holds the round or a unit, or runs a queue's poll**
//!   ([`Await::Copy`]) spins as long, then writes the record itself: commits
//!   wait for what it does, so it waits for nobody.
//!
//! A copy is the record's own bytes at its own place: the descriptor's
//! buffer when it is filled, or the same bytes encoded on the copying thread
//! from the decision. It counts in at the gate and looks again as a run
//! does, so no copy lands on a range a sync has begun to cover (WG4).
//!
//! # Invariants
//!
//! - **WU1, one holder.** The unit is claimed by one compare-and-swap of its
//!   word and let go only by its holder, with one swap; at most one thread
//!   gathers and writes a run at a time.
//! - **WU2, nothing is encoded under the claim.** The holder only reads
//!   records whose owners filled them (D4), so the claim covers the gather,
//!   the count in, one host write and the marks: the window a preempted
//!   holder can stall, and the window a sync waits out at the gate, is one
//!   write long.
//! - **WU3, a run looks at its records after its count in.** Each record it
//!   writes was unwritten when the holder looked, after it counted in: a
//!   sync whose turn of the gate came first knew none of them written and
//!   covers none of them (WG4); the rollback that moved the epoch waits for
//!   the count out (WG2).
//! - **WU4, no lost wake.** A thread parks only after it is on the record's
//!   list with `PARKED` set (D5), and, for a filled record, after it marked
//!   the claim it saw as wanted, with a compare-and-swap that reads the
//!   word as it is now; it then looks again. Whoever writes, fills or fails
//!   the record reads `PARKED` in the same read-modify-write, and the holder
//!   reads `WANTED` in the swap that lets the unit go and then wakes every
//!   record still waiting. One of each pair sees the other.
//! - **WU5, a park is bounded.** A blocking waiter parks for at most
//!   [`PATIENCE`] in all before it writes the record itself, and the
//!   logical triggers end it sooner, without a clock: a preempted holder
//!   costs each waiter a bounded wait, never a scheduler's slice.
//! - **WU6, a run is bounded.** It holds at most [`MAX_BATCH_RECORDS`]
//!   records and [`MAX_BATCH_BYTES`] bytes, and no record past the decided
//!   frontier read when it began (or past the record the caller needs), so a
//!   call writes at most a ring's worth of records before its own and one
//!   run (P5 in `pipeline.rs`).
//! - **WU7, a woken owner finds its commit published.** The holder
//!   publishes what its run made publishable before it wakes the run's
//!   owners ([`RegolithEngine::publish_ready`]).
//! - **WU8, an uncontended write skips the unit.** An owner whose record
//!   nobody has in hand, finding the unit free with no note on it, encodes
//!   its record and writes it by itself, through the gate as a copy is
//!   ([`RegolithEngine::write_alone`]): what a writer did before the unit,
//!   with no claim, gather or wake. The guard is one load and only about
//!   speed; the copy's look again once counted in is what keeps it safe
//!   (WG4). The unit does not see such writes, so several owners may take
//!   the fast path at once, each writing its own record.

use std::io::{self, IoSlice};
use std::sync::Arc;
use std::time::Duration;

use super::super::RegolithEngine;
use super::super::io::job::Job;
use super::super::wal_frame::KIND_GROUP;
use super::super::wal_log::Place;
use super::descriptor::{Decision, Descriptor, FAILED, FILLED, FILLING, PARKED, WRITTEN};
use super::hook;
use super::park::Help;
use super::pipeline::{Pipeline, ops_of};
use super::ring::{ArcSlot, Linked, Loaded};
use crate::perf_context::{PerfTimer, PerfTimerField};
use crate::statistics::{Histogram, Ticker};
use crate::sync::internal::{Arc as RingArc, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

/// The unit is claimed.
pub(super) const CLAIMED: u64 = 1;
/// A blocking thread parked for the next claim: the holder that lets the
/// unit go wakes every parked record still waiting (WU4).
pub(super) const WANTED: u64 = 1 << 1;
/// A `commit_nowait` commit left the unit on its queue: the holder that
/// lets the unit go releases the line job its queue waits on.
pub(super) const QUEUED: u64 = 1 << 2;
/// One claim more: the claims made so far count above the flags, so a
/// waiter can tell the claim it saw from a later one.
const ONE_CLAIM: u64 = 1 << 3;
/// The flags a waiter adds to a claim; the rest of the word names it.
const NOTES: u64 = WANTED | QUEUED;

/// Records a run holds at the most (WU6).
pub(super) const MAX_BATCH_RECORDS: usize = 64;
/// Bytes a run holds at the most past its first record, which a record of
/// any size begins (WU6).
pub(super) const MAX_BATCH_BYTES: u64 = 1 << 20;
/// Rounds of `spin_loop` a waiter gives the holder before it parks, leaves
/// or copies: about as long as one host write of a run takes.
pub(super) const SPIN_ROUNDS: u32 = 64;
/// Waiters that spin at once at the most; any other parks at once. A
/// spinner holds a processor, and more of them than a few take processors
/// from the holder and the threads deciding, which then run late and make
/// every waiter wait longer.
pub(super) const SPIN_BUDGET: usize = 4;
/// How long a blocking waiter waits in all, from when it first had to,
/// before it writes the record itself (WU5).
pub(super) const PATIENCE: Duration = Duration::from_micros(100);
/// Slots the decided frontier may move past while the claim a waiter saw
/// stays put, before the waiter takes the holder for stopped and writes the
/// record itself (WU5): with nobody stopped, deciding that many slots takes
/// far longer than one host write.
pub(super) const FRONTIER_PATIENCE: u64 = 64;
/// Parks a waiter makes before it writes the record itself, whatever the
/// clock says: each park lasts at most what is left of [`PATIENCE`], so
/// this bounds the wait on a host with no clock, and against wakes that
/// come early again and again (WU5).
const MAX_PARKS: u32 = 8;

/// See the module docs.
pub(crate) struct WriteUnit {
    /// [`CLAIMED`], the waiters' notes ([`NOTES`]), and the claims made so
    /// far from [`ONE_CLAIM`] up.
    word: AtomicU64,
    /// Every slot at or below this one has its record written, or has none:
    /// where the next run starts looking. Written by the holder alone.
    written: AtomicU64,
    /// The job a `commit_nowait` commit waits on when it leaves the unit on
    /// its queue: released, and replaced, by the holder that lets go a claim
    /// a commit was queued on.
    line: ArcSlot<Line>,
    /// The waiters spinning now ([`SPIN_BUDGET`]), on a line of its own so
    /// their counting does not slow the claim's word.
    spinners: Padded<AtomicUsize>,
}

/// A value alone on its cache line.
#[repr(align(128))]
pub(super) struct Padded<T>(pub(super) T);

/// The job `commit_nowait` commits wait on for the unit to be let go.
struct Line {
    job: Arc<Job>,
    link: AtomicPtr<Line>,
}

impl Linked for Line {
    fn link(&self) -> &AtomicPtr<Self> {
        &self.link
    }
}

impl Line {
    fn fresh() -> RingArc<Self> {
        RingArc::new(Self {
            job: Job::passive(),
            link: AtomicPtr::new(std::ptr::null_mut()),
        })
    }
}

impl WriteUnit {
    /// The unit of a ring whose slots up to `last` are finished.
    pub(crate) fn new(last: u64) -> Self {
        Self {
            word: AtomicU64::new(0),
            written: AtomicU64::new(last),
            line: ArcSlot::new(Line::fresh()),
            spinners: Padded(AtomicUsize::new(0)),
        }
    }

    /// Whether the unit is free with no note on it: one load, a hint for
    /// the fast path (WU8) that may be stale by the time it is acted on.
    pub(super) fn quiet(&self) -> bool {
        self.word.load(Ordering::Acquire) & (CLAIMED | WANTED | QUEUED) == 0
    }

    /// Claim the unit (WU1): `Ok` with the word as claimed, or `Err` with
    /// the word another thread's claim left.
    pub(super) fn try_claim(&self) -> Result<u64, u64> {
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            if word & CLAIMED != 0 {
                return Err(word);
            }
            match self.word.compare_exchange_weak(
                word,
                word | CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(word | CLAIMED),
                Err(now) => word = now,
            }
        }
    }

    /// Let the claim `claimed` go, and return the notes waiters left on it.
    /// By its holder alone.
    pub(super) fn release(&self, claimed: u64) -> u64 {
        let next = (claimed & !(CLAIMED | NOTES)) + ONE_CLAIM;
        self.word.swap(next, Ordering::AcqRel) & NOTES
    }

    /// Add `note` to the claim `seen`. `false` when that claim was let go
    /// meanwhile: the caller looks again instead of waiting on it.
    ///
    /// Always a compare-and-swap, even when the note is there already: only
    /// a read-modify-write reads the word as it is now. A plain load may
    /// return the claim with its note after the holder's swap let it go,
    /// and a waiter trusting it would sleep on a note nobody reads (WU4).
    pub(super) fn note(&self, seen: u64, note: u64) -> bool {
        let mut word = self.word.load(Ordering::Acquire);
        loop {
            if word & !NOTES != seen & !NOTES {
                return false;
            }
            match self.word.compare_exchange_weak(
                word,
                word | note,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(now) => word = now,
            }
        }
    }

    /// Test-only: the notes on the claim that holds the unit now.
    #[cfg(test)]
    pub(super) fn notes(&self) -> u64 {
        self.word.load(Ordering::Acquire) & NOTES
    }

    /// Whether the claim `seen` still holds the unit.
    pub(super) fn holds(&self, seen: u64) -> bool {
        self.word.load(Ordering::Acquire) & !NOTES == seen & !NOTES
    }

    /// The job a `commit_nowait` commit leaves on its queue while the claim
    /// `seen` holds the unit, or `None` when that claim was let go meanwhile.
    /// The job is loaded before the note goes on the claim, so the holder
    /// that reads the note releases this job or a later one, whose release
    /// came after this one's (see [`Self::tell_line`]).
    pub(super) fn queue_on(&self, seen: u64) -> Option<Arc<Job>> {
        let line = self.line.load();
        self.note(seen, QUEUED).then(|| Arc::clone(&line.job))
    }

    /// Loom-only calibration of [`Self::queue_on`]: the note goes on the
    /// claim before the job is loaded, so a holder that lets the claim go
    /// in between releases one job and the commit waits on the next.
    #[cfg(loom)]
    pub(super) fn queue_on_noting_first(&self, seen: u64) -> Option<Arc<Job>> {
        if !self.note(seen, QUEUED) {
            return None;
        }
        Some(Arc::clone(&self.line.load().job))
    }

    /// Loom-only calibration of [`Self::note`]: the note trusted when a
    /// plain load shows it on the claim, as the code once did. The load may
    /// show a claim that was let go already, and the waiter sleeps on it.
    #[cfg(loom)]
    pub(super) fn note_trusting_a_load(&self, seen: u64, note: u64) -> bool {
        let word = self.word.load(Ordering::Acquire);
        if word & !NOTES != seen & !NOTES {
            return false;
        }
        word & note != 0 || self.note(seen, note)
    }

    /// Loom-only calibration of [`Self::tell`] and `tell_waiters`: one
    /// waiting record is woken and, when another waits, `WANTED` is put back
    /// on the word for the next holder, as the code once did. The unit may
    /// be free by then and nobody claims it again.
    #[cfg(loom)]
    pub(super) fn tell_the_first_only(&self, notes: u64, records: &[&Descriptor]) {
        if notes & WANTED == 0 {
            return;
        }
        let mut waiting = records
            .iter()
            .filter(|desc| waits_for_the_unit(desc.flags()));
        if let Some(first) = waiting.next() {
            if waiting.next().is_some() {
                self.word.fetch_or(WANTED, Ordering::AcqRel);
            }
            first.wake();
        }
    }

    /// What the holder of a claim let go with `notes` owes its waiters: the
    /// line job released when a `commit_nowait` commit was queued on it
    /// (D53), and `true` when a waiter noted it, so that every record still
    /// waiting for the unit must be woken ([`wake_if_waiting`], WU4). Every
    /// one, not the first only: a note put on a free unit for the next
    /// holder is read by nobody when nobody claims it again.
    pub(super) fn tell(&self, notes: u64) -> bool {
        if notes & QUEUED != 0 {
            self.tell_line();
        }
        notes & WANTED != 0
    }

    /// Release the line job, after a claim a commit was queued on was let
    /// go, and put a fresh one in its place.
    pub(super) fn tell_line(&self) {
        loop {
            let line = self.line.load();
            if self.line.replace(&line, Line::fresh()) {
                line.job.release();
                return;
            }
        }
    }
}

/// What a call does when a record it needs is in another thread's hands:
/// the unit is claimed, or the record is not filled yet. See the module
/// docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Await {
    /// A blocking commit: spin, park with patience, then copy.
    Park,
    /// A `commit_nowait` call: spin, then leave the unit on the queue; a
    /// record nobody filled yet is copied.
    Leave,
    /// The holder of the round or of a unit, or a queue's poll: spin, then
    /// copy.
    Copy,
}

impl Await {
    /// What a call that helps as `help` says does.
    pub(super) fn of(help: Help<'_>) -> Self {
        match help {
            Help::Wait => Await::Park,
            Help::Leave => Await::Leave,
            Help::Park(_) => Await::Copy,
        }
    }
}

/// How far bringing a record into the log got.
pub(super) enum Brought {
    /// The record is written, or there is none.
    Written,
    /// A rollback discarded the slot, or the log failed: the round deals
    /// with it.
    Failed,
    /// A `commit_nowait` call met the unit claimed: the job its queue waits
    /// on.
    Waits(Arc<Job>),
}

/// What one try at a run did.
enum Tried {
    /// It wrote a run, or found every gathered record written meanwhile.
    Wrote,
    /// Another thread holds the unit; the word its claim left.
    Claimed(u64),
    /// The gate moved on before the run counted in: a rollback is
    /// discarding the records.
    Refused,
    /// Nothing to write: no filled record waits.
    Nothing,
}

/// A descriptor a run holds: the caller's own, borrowed, or one loaded from
/// the ring.
enum Held<'a> {
    Own(&'a RingArc<Descriptor>),
    Loaded(RingArc<Descriptor>),
}

impl std::ops::Deref for Held<'_> {
    type Target = Descriptor;

    fn deref(&self) -> &Descriptor {
        match self {
            Held::Own(desc) => desc,
            Held::Loaded(desc) => desc,
        }
    }
}

/// The run a holder gathered (WU6).
struct Run<'a> {
    held: [Option<Held<'a>>; MAX_BATCH_RECORDS],
    len: usize,
    /// The first slot of the run and the last.
    first: u64,
    last: u64,
    /// Where its first record starts, and where its last ends.
    offset: u64,
    end: u64,
    epoch: u64,
}

impl<'a> Run<'a> {
    fn new() -> Self {
        Self {
            held: [const { None }; MAX_BATCH_RECORDS],
            len: 0,
            first: 0,
            last: 0,
            offset: 0,
            end: 0,
            epoch: 0,
        }
    }

    fn records(&self) -> impl Iterator<Item = &Held<'a>> {
        self.held[..self.len].iter().flatten()
    }

    /// Add slot `index`'s record, at `place` in the log `decision` names,
    /// when it follows the run on in the same log and epoch and fits;
    /// `false` ends the run. A run begins only with a record of `epoch`,
    /// the gate's.
    fn take(&mut self, index: u64, held: Held<'a>, place: &Place, epoch: u64) -> bool {
        if self.len == 0 {
            if place.epoch != epoch {
                return false;
            }
            self.first = index;
            self.offset = place.offset;
            self.epoch = place.epoch;
        } else {
            let same_log = match (self.held[0].as_ref(), held.decision()) {
                (Some(first), Some(decision)) => first
                    .decision()
                    .is_some_and(|d| Arc::ptr_eq(&d.after.log, &decision.after.log)),
                _ => false,
            };
            if !same_log
                || place.epoch != self.epoch
                || place.offset != self.end
                || self.len == MAX_BATCH_RECORDS
                || place.end() - self.offset > MAX_BATCH_BYTES
            {
                return false;
            }
        }
        self.end = place.end();
        self.last = index;
        self.held[self.len] = Some(held);
        self.len += 1;
        true
    }
}

std::thread_local! {
    /// This thread's buffer for a record it copies that its owner did not
    /// fill, and the length of the record before it (`trim_stage`'s
    /// hysteresis).
    static STAGE: std::cell::RefCell<(Vec<u8>, usize)> =
        const { std::cell::RefCell::new((Vec::new(), 0)) };
}

/// Test-only: the capacity and the address of this thread's stage buffer.
#[cfg(test)]
pub(super) fn stage_buffer() -> (usize, *const u8) {
    STAGE.with(|stage| {
        let stage = stage.borrow();
        (stage.0.capacity(), stage.0.as_ptr())
    })
}

/// Whether somebody is parked on a slot with `flags` waiting for the write
/// unit: its record is filled, not written, not discarded, and a thread
/// parked on it (WU4). What the holder that lets the unit go looks for.
pub(super) fn waits_for_the_unit(flags: u64) -> bool {
    flags & (FILLED | PARKED | WRITTEN | FAILED) == FILLED | PARKED
}

/// Wake whoever sleeps on `desc` waiting for the unit (WU4). `PARKED` is a
/// hint that stays set once a thread parked on the slot: a slot whose
/// sleepers left costs one look at an empty list. It is not cleared when a
/// sleeper leaves, since another thread may be parking on the same slot.
pub(super) fn wake_if_waiting(desc: &Descriptor) {
    if waits_for_the_unit(desc.flags()) {
        desc.wake();
    }
}

/// Frame slot `desc`'s record, decided as `decision` with its place at
/// `place`, into `buf`, replacing what it held: the same bytes on every
/// thread (`wal_log.rs`).
fn encode(
    buf: &mut Vec<u8>,
    desc: &Descriptor,
    decision: &Decision,
    place: &Place,
) -> io::Result<()> {
    let log = &decision.after.log;
    buf.clear();
    if buf.try_reserve_exact(place.len as usize).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            format!(
                "not enough memory to commit this write ({} bytes pending); retry, splitting any very large write batch",
                place.len
            ),
        ));
    }
    buf.resize(log.prefix_len(), 0);
    ops_of(desc, decision).encode(buf, decision.base_seq);
    log.finish_record(KIND_GROUP, place, buf)
}

/// Where a wait for one record stands.
struct Waiting {
    /// The claim the waiter first found in its way, its time and the
    /// decided frontier then; `None` until it first had to wait.
    since: Option<(u64, Option<u64>, u64)>,
    /// Whether it spun since it last found the record in another's hands.
    spun: bool,
    /// Parks so far.
    parks: u32,
}

impl Waiting {
    fn new() -> Self {
        Self {
            since: None,
            spun: false,
            parks: 0,
        }
    }
}

impl RegolithEngine {
    /// Encode slot `desc`'s record into its descriptor (D4): what its owner
    /// does once the slot is decided, before it needs the record written,
    /// and what any thread that needs the record written does when nobody
    /// has encoded it. A slot with no record, discarded, written, or filled
    /// or being filled already is left as it is. A record that cannot be
    /// encoded fails the log.
    pub(super) fn fill(&self, desc: &Descriptor) {
        let Some(decision) = desc.decision() else {
            return;
        };
        let Some(place) = &decision.place else {
            return;
        };
        if desc.flags() & (FILLED | FILLING | WRITTEN | FAILED) != 0 {
            return;
        }
        let encoded = desc.fill(|buf| {
            hook::at(hook::Stage::Fill);
            encode(buf, desc, decision, place)
        });
        if let Err(err) = encoded {
            tracing::error!(error = %err, "building a commit's write-ahead log record failed");
            decision.after.log.poison(err);
        }
    }

    /// Bring slot `desc`'s record into the log, as `how` says: through the
    /// write unit, in a run with the records around it, or as a copy when
    /// the unit's holder is slow. `own` is the caller's installed
    /// descriptor, when `desc` is it. See the module docs.
    pub(super) fn bring_written(
        &self,
        pipe: &Pipeline,
        desc: &RingArc<Descriptor>,
        how: Await,
    ) -> Brought {
        let Some(decision) = desc.decision() else {
            return Brought::Written;
        };
        let Some(place) = &decision.place else {
            return Brought::Written;
        };
        let log = &decision.after.log;
        let mut wait = Waiting::new();
        loop {
            let flags = desc.flags();
            if flags & WRITTEN != 0 {
                return Brought::Written;
            }
            if flags & FAILED != 0 || log.is_poisoned() {
                return Brought::Failed;
            }
            let filled = flags & FILLED != 0;
            if !filled && flags & FILLING == 0 {
                // Not filled, and nobody is encoding it: whoever needs the
                // record written encodes it, as its owner would (D4). It is
                // not under the unit's claim, and nobody parks on an owner
                // that is slow to come back to its record (WU2).
                self.fill(desc);
                continue;
            }
            let seen = if filled {
                match self.try_batch(pipe, desc) {
                    Tried::Wrote => continue,
                    Tried::Refused | Tried::Nothing if place.epoch != log.epoch() => {
                        // A rollback is discarding the slot.
                        return Brought::Failed;
                    }
                    Tried::Refused => continue,
                    Tried::Nothing => {
                        // No run could take the record: it goes alone.
                        self.copy_record(desc);
                        continue;
                    }
                    Tried::Claimed(word) => word,
                }
            } else {
                pipe.writes.word.load(Ordering::Acquire)
            };
            // In another thread's hands: the unit's holder, or the thread
            // encoding the record now.
            if !wait.spun {
                wait.spun = true;
                self.spin(pipe, desc, flags, seen);
                continue;
            }
            wait.spun = false;
            match how {
                Await::Copy => self.copy_record(desc),
                Await::Leave if filled => {
                    if let Some(job) = pipe.writes.queue_on(seen) {
                        // Looked at again with the note on the claim: a
                        // record written meanwhile is not left waiting.
                        if desc.flags() & (WRITTEN | FAILED) == 0 {
                            return Brought::Waits(job);
                        }
                    }
                }
                Await::Leave => self.copy_record(desc),
                Await::Park => {
                    if self.out_of_patience(pipe, &mut wait, seen) {
                        self.copy_record(desc);
                    } else {
                        self.park_on(pipe, desc, filled, seen, &mut wait);
                    }
                }
            }
        }
    }

    /// Spin about one write's length, until the record's flags change from
    /// `flags` or the claim `seen` lets the unit go, unless [`SPIN_BUDGET`]
    /// waiters spin already.
    fn spin(&self, pipe: &Pipeline, desc: &Descriptor, flags: u64, seen: u64) {
        if !crate::env::PLATFORM_THREADS {
            return;
        }
        let spinners = &pipe.writes.spinners.0;
        if spinners.fetch_add(1, Ordering::Relaxed) < SPIN_BUDGET {
            for _ in 0..SPIN_ROUNDS {
                if desc.flags() != flags || !pipe.writes.holds(seen) {
                    break;
                }
                std::hint::spin_loop();
            }
        }
        spinners.fetch_sub(1, Ordering::Relaxed);
    }

    /// Whether a blocking waiter has waited long enough to write the record
    /// itself (WU5): [`PATIENCE`] since it first had to, the decided
    /// frontier [`FRONTIER_PATIENCE`] slots on while the claim it first
    /// saw holds the unit, or the ring full.
    fn out_of_patience(&self, pipe: &Pipeline, wait: &mut Waiting, seen: u64) -> bool {
        // The platform's clock, never the `Env`'s: a test's `Env` may keep a
        // clock that stands still, and the wait is for real threads.
        let now = crate::env::platform_micros();
        let decided = pipe.ring.decided();
        let Some((first, since, at)) = wait.since else {
            wait.since = Some((seen, now, decided));
            return false;
        };
        if pipe.writes.holds(first) && decided >= at + FRONTIER_PATIENCE {
            return true;
        }
        if pipe.ring.tail() > pipe.ring.published() + pipe.ring.capacity() {
            return true;
        }
        if wait.parks >= MAX_PARKS {
            return true;
        }
        match (since, now) {
            (Some(since), Some(now)) => now.saturating_sub(since) >= PATIENCE.as_micros() as u64,
            _ => false,
        }
    }

    /// Park on slot `desc` until it is woken or the patience left runs out
    /// (WU4, WU5). A filled record marks the claim `seen` as wanted, so the
    /// holder that lets it go wakes every parked record still waiting;
    /// an unfilled one waits for its owner to fill it.
    fn park_on(
        &self,
        pipe: &Pipeline,
        desc: &Descriptor,
        filled: bool,
        seen: u64,
        wait: &mut Waiting,
    ) {
        if !crate::env::PLATFORM_THREADS {
            return;
        }
        let left = match (wait.since, crate::env::platform_micros()) {
            (Some((_, Some(since), _)), Some(now)) => {
                PATIENCE.saturating_sub(Duration::from_micros(now.saturating_sub(since)))
            }
            _ => PATIENCE,
        };
        hook::at(hook::Stage::WriteWait);
        wait.parks += 1;
        desc.park(left, || {
            let flags = desc.flags();
            if flags & (WRITTEN | FAILED) != 0 {
                return true;
            }
            if filled {
                !pipe.writes.note(seen, WANTED)
            } else {
                flags & FILLED != 0
            }
        });
    }

    /// The uncontended fast path (WU8): slot `desc`'s owner encodes its
    /// record and writes it by itself, through the gate as a copy is, when
    /// the unit is free with no note on it and nobody has the record in
    /// hand. One load of the unit decides it; a commit that finds the unit
    /// busy goes through it, since its runs pay off once writes meet there.
    /// `true` when it wrote, or found the record written or discarded
    /// meanwhile.
    pub(super) fn write_alone(&self, pipe: &Pipeline, desc: &Descriptor) -> bool {
        let Some(decision) = desc.decision() else {
            return false;
        };
        if decision.place.is_none()
            || !pipe.writes.quiet()
            || desc.flags() & (FILLING | FILLED | WRITTEN | FAILED) != 0
        {
            return false;
        }
        self.fill(desc);
        self.copy_record(desc);
        true
    }

    /// Write slot `desc`'s record by itself, as a copy, through the gate: its
    /// buffer when its owner filled it, else the same bytes encoded here.
    /// Written only if it is not written by then (WG4).
    pub(super) fn copy_record(&self, desc: &Descriptor) {
        let Some(decision) = desc.decision() else {
            return;
        };
        let Some(place) = &decision.place else {
            return;
        };
        let log = &decision.after.log;
        hook::at(hook::Stage::Write);
        let _perf = PerfTimer::new(PerfTimerField::WriteWal);
        // Through `Env`, not `std::time::Instant`: browsers have a clock but
        // `Instant::now` panics on `wasm32-unknown-unknown`. Read only when
        // statistics record latencies.
        let start = self.timer_start();
        let written = || desc.flags() & (WRITTEN | FAILED) != 0;
        let wrote = match desc.record() {
            Some(record) => log.write_unless(place, record, written),
            None => STAGE.with(|stage| {
                let mut stage = stage.borrow_mut();
                let (buf, prev) = &mut *stage;
                let wrote = encode(buf, desc, decision, place)
                    .and_then(|()| log.write_unless(place, buf, written));
                let used = buf.len();
                super::trim_stage(buf, *prev, used);
                *prev = used;
                wrote
            }),
        };
        match wrote {
            Ok(true) => {
                if desc.mark(WRITTEN)
                    && let Some(s) = self.statistics()
                {
                    s.add(
                        Ticker::WalBytesWritten,
                        ops_of(desc, decision).reported_bytes(),
                    );
                    if let Some(micros) = self.elapsed_micros(start) {
                        s.record(Histogram::WalWriteTime, micros);
                    }
                }
            }
            // A rollback discarded the slot since it was decided, or another
            // thread wrote the record.
            Ok(false) => {}
            Err(err) => {
                tracing::error!(error = %err, "writing a commit to the write-ahead log failed");
                log.poison(err);
                desc.wake();
            }
        }
    }

    /// Claim the unit and write the first run of filled, unwritten records,
    /// which holds `target`'s when nothing before it is in the way. See the
    /// module docs.
    fn try_batch(&self, pipe: &Pipeline, target: &RingArc<Descriptor>) -> Tried {
        let unit = &pipe.writes;
        let claimed = match unit.try_claim() {
            Ok(word) => word,
            Err(word) => return Tried::Claimed(word),
        };
        // Let go however this returns, on unwind too.
        let claim = Claim {
            engine: self,
            pipe,
            claimed,
        };
        let mut run = Run::new();
        let Some(epoch) = target.decision().map(|d| d.after.log.epoch()) else {
            return Tried::Nothing;
        };
        let bound = pipe.ring.decided().max(target.index());
        let start = unit
            .written
            .load(Ordering::Acquire)
            .max(pipe.ring.published())
            + 1;
        // Every slot from `start` to the run's first is done with the log:
        // the hint may then move to the run's last.
        let mut clean = true;
        for index in start..=bound {
            let held = if target.index() == index {
                Held::Own(target)
            } else {
                match pipe.ring.load(index) {
                    Loaded::Ready(desc) => Held::Loaded(desc),
                    Loaded::Void | Loaded::Gone => continue,
                    Loaded::Empty | Loaded::Full => break,
                }
            };
            let Some(decision) = held.decision() else {
                break;
            };
            let Some(place) = decision.place else {
                continue;
            };
            let flags = held.flags();
            let usable = flags & (WRITTEN | FAILED) == 0 && flags & FILLED != 0;
            if run.len == 0 && !usable {
                clean &= flags & (WRITTEN | FAILED) != 0;
                continue;
            }
            if !usable || !run.take(index, held, &place, epoch) {
                if run.len == 0 {
                    // A record of another epoch: a rollback is discarding it.
                    clean = false;
                    continue;
                }
                break;
            }
        }
        if run.len == 0 {
            drop(claim);
            return Tried::Nothing;
        }
        hook::at(hook::Stage::Write);
        let log = match run.held[0].as_ref().and_then(|first| first.decision()) {
            Some(decision) => Arc::clone(&decision.after.log),
            None => return Tried::Nothing,
        };
        let Some(flight) = log.count_in(run.epoch) else {
            drop(claim);
            return Tried::Refused;
        };
        // WU3: looked at again once counted in. A record another thread
        // wrote meanwhile ends the run before it.
        let keep = run
            .records()
            .take_while(|held| held.flags() & (WRITTEN | FAILED) == 0)
            .count();
        if keep == 0 {
            drop(flight);
            drop(claim);
            return Tried::Wrote;
        }
        let _perf = PerfTimer::new(PerfTimerField::WriteWal);
        let start_micros = self.timer_start();
        let mut slices = [IoSlice::new(&[]); MAX_BATCH_RECORDS];
        for (slice, held) in slices.iter_mut().zip(run.records().take(keep)) {
            *slice = IoSlice::new(held.record().unwrap_or_default());
        }
        let wrote = flight.write(run.offset, &slices[..keep]);
        drop(flight);
        let mut parked = [false; MAX_BATCH_RECORDS];
        let mut wrote_upto = 0;
        match wrote {
            Ok(()) => {
                let stats = self.statistics();
                for (held, parked) in run.records().take(keep).zip(parked.iter_mut()) {
                    let (set, sleeping) = held.mark_quiet(WRITTEN);
                    *parked = sleeping;
                    if set
                        && let Some(s) = stats
                        && let Some(decision) = held.decision()
                    {
                        s.add(
                            Ticker::WalBytesWritten,
                            ops_of(held, decision).reported_bytes(),
                        );
                    }
                }
                if let Some(s) = stats
                    && let Some(micros) = self.elapsed_micros(start_micros)
                {
                    s.record(Histogram::WalWriteTime, micros);
                }
                let last = run
                    .records()
                    .nth(keep - 1)
                    .map_or(run.first, |held| held.index());
                if clean {
                    unit.written.store(last.max(start - 1), Ordering::Release);
                }
                wrote_upto = last;
            }
            Err(err) => {
                tracing::error!(error = %err, "writing commits to the write-ahead log failed");
                log.poison(err);
                // Whoever waits for one of these records looks again, and
                // finds the log failed.
                parked = [true; MAX_BATCH_RECORDS];
            }
        }
        drop(claim);
        if wrote_upto != 0 {
            // WU7: the slots the run made publishable are published before
            // their owners are woken, so a woken owner finds its commit done.
            self.publish_ready(pipe, wrote_upto, Some(target));
        }
        // Woken once the unit is let go, and each descriptor let go of right
        // after, so an owner that wakes finds its descriptor its own.
        for (slot, parked) in run.held[..run.len].iter_mut().zip(parked) {
            if let Some(held) = slot.take()
                && parked
            {
                held.wake();
            }
        }
        Tried::Wrote
    }

    /// The claim `claimed` was let go with `notes` on it: release the line
    /// job the queues of `commit_nowait` commits wait on, and wake every
    /// record still waiting for the unit (WU4, D53). Bounded by the ring:
    /// only slots past the published frontier and up to the decided one.
    fn tell_waiters(&self, pipe: &Pipeline, notes: u64) {
        if !pipe.writes.tell(notes) {
            return;
        }
        let start = pipe
            .writes
            .written
            .load(Ordering::Acquire)
            .max(pipe.ring.published())
            + 1;
        for index in start..=pipe.ring.decided() {
            match pipe.ring.load(index) {
                Loaded::Ready(desc) => wake_if_waiting(&desc),
                Loaded::Void | Loaded::Gone => {}
                Loaded::Empty | Loaded::Full => break,
            }
        }
    }
}

/// The unit, claimed: let go when dropped, on return or on unwind, and the
/// waiters told (WU1, WU4).
struct Claim<'a> {
    engine: &'a RegolithEngine,
    pipe: &'a Pipeline,
    claimed: u64,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let notes = self.pipe.writes.release(self.claimed);
        self.engine.tell_waiters(self.pipe, notes);
    }
}
