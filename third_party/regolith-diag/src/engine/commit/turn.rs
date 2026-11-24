//! The rotation check: what a write's turn begins with once the memtable
//! may be full (plan 4.7).
//!
//! A write whose turn comes with the memtable's counted bytes at the write
//! buffer size has the rotation check run first, as a unit of the same kind
//! attached to the write's own slot ([`RegolithEngine::take_turn`]). The
//! count is an upper bound kept in the pipeline's state, so every thread
//! agrees when the check is due; the check itself, with the pipeline
//! drained, measures the memtable and rotates only when it is in fact full.
//! The state it leaves replaces the tip in place, and the write is decided
//! from that.
//!
//! It drains the pipeline, holds the round and parks as every unit does
//! (`ordered.rs`, `park.rs`).

use std::io;
use std::sync::Arc;
use std::task::Waker;

use super::super::RegolithEngine;
use super::super::io::job::{Job, JobBody, Step};
use super::descriptor::{After, Decision, Descriptor, Turned, Verdict as Fate, Work};
use super::ordered::Ordered;
use super::park::{Help, Needs};
use super::ring::Loaded;
use crate::sync::internal::Arc as RingArc;

/// The rotation check of one write's turn, as a job.
struct TurnCheck {
    engine: std::sync::Weak<RegolithEngine>,
    index: u64,
}

impl TurnCheck {
    /// `false` when a poll's check parked.
    fn go(&self, help: Help<'_>) -> bool {
        match self.engine.upgrade() {
            Some(engine) => engine.run_turn(self.index, help),
            None => true,
        }
    }
}

impl JobBody for TurnCheck {
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

    fn release(self: Box<Self>) {
        self.go(Help::Wait);
    }
}

/// What the rotation check does about the active memtable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rotation {
    /// It holds this many bytes, below `write_buffer_size`: the count is
    /// corrected and nothing is sealed.
    NotDue(usize),
    /// Seal it, and leave its flush to the background.
    Seal,
    /// The frozen memtables are at the cap of `room`: flush the oldest
    /// first, here, then seal.
    FlushFirst { room: usize },
    /// The cap is one memtable: seal it and flush it, here.
    FlushSealed,
}

impl Rotation {
    /// What carrying it out may wait for.
    fn needs(self) -> Needs {
        match self {
            Rotation::NotDue(_) => Needs::NONE,
            Rotation::Seal => Needs::VERSIONS,
            Rotation::FlushFirst { .. } | Rotation::FlushSealed => Needs::FLUSH,
        }
    }
}

impl RegolithEngine {
    /// Whether the rotation check is due at the turn of `desc`, decided
    /// from `after`: the memtable's counted bytes reached the write buffer
    /// size, and the slot may write.
    pub(super) fn turn_is_due(&self, desc: &Descriptor, after: &After) -> bool {
        after.mem_used >= self.options.write_buffer_size
            && !after.closed
            && matches!(desc.work, Work::Request(_) | Work::Allocate { .. })
    }

    /// The rotation check of the write at slot `index`, run here if this
    /// thread may and nobody has, or handed back as the job in the way.
    pub(super) fn take_turn<'d>(
        &self,
        index: u64,
        desc: &'d Descriptor,
        help: Help<'_>,
    ) -> Result<&'d Turned, Arc<Job>> {
        let job = match desc.turn_job.get() {
            Some(job) => job,
            None => desc.turn_job.get_or_set(Job::new(Box::new(TurnCheck {
                engine: self.me.clone(),
                index,
            }))),
        };
        if !self.pass(job, help) {
            return Err(Arc::clone(job));
        }
        Ok(match desc.turn.get() {
            Some(turned) => turned,
            // The check ended without an answer: it unwound.
            None => desc.turn.get_or_set(Turned {
                refused: Some(io::Error::other(
                    "the memtable rotation before this write did not finish",
                )),
            }),
        })
    }

    /// The rotation check at slot `index`: drain the pipeline, then rotate
    /// if the memtable is in fact full. `false` when a poll's check parked,
    /// before it sealed or flushed anything.
    fn run_turn(&self, index: u64, help: Help<'_>) -> bool {
        let Some(pipe) = self.pipeline.as_ref() else {
            return true;
        };
        let Loaded::Ready(desc) = pipe.ring.load(index) else {
            return true;
        };
        if desc.turn.get().is_some() || desc.decision().is_some() {
            return true;
        }
        // A flush the rotation runs under back-pressure calls the caller's
        // code (a prefix extractor, a listener): a panic there is caught, as
        // in every ordered step of a commit, and latches the database.
        let _commit = super::super::callback::InCommit::enter();
        let Some((held, state)) = self.drained(pipe, index, help) else {
            return false;
        };
        // Decided once, with the round held: the frozen list only shrinks
        // under a unit, so what the check needs now is all it will need.
        let rotation = self.rotation_due(&state);
        let Some(kept) = self.keep(rotation.needs(), help) else {
            return false;
        };
        let mut ord = Ordered {
            engine: self,
            state,
        };
        let refused = self.rotate(&mut ord, rotation).err();
        // What a poll took for the check is given back first, so the level
        // is classified with no flush exclusion held.
        let _ = kept.release();
        if !matches!(rotation, Rotation::NotDue(_)) {
            // Whatever the rotation came to (E35). A flush took a memtable
            // out and put a table in, a seal put a memtable in, and a
            // rotation that fails part way leaves either one done, so the
            // thresholds are classified again on every way out: a stopped
            // writer its flush held off looks again (`StallSignal`, relief
            // handed back).
            self.refresh_stall_level();
        }
        if let Some(err) = &refused
            && let Some(callback) = crate::Error::callback_panic_of(err)
        {
            self.latch_callback_panic(callback);
        }
        // The state the check left, rotated or not, takes the tip's place
        // before the round is let go and before the job lands, so every
        // thread that waited for the check decides the write from it.
        loop {
            let tip = pipe.tip.load();
            let turned = Decision::new(tip.index, Fate::Unit, ord.state.clone());
            if pipe.tip.replace(&tip, RingArc::new(turned)) {
                break;
            }
        }
        desc.turn.get_or_set(Turned { refused });
        drop(held);
        true
    }

    /// What the rotation check does about the active memtable of `state`,
    /// read with the round held.
    ///
    /// A rotation leaves at most `max_write_buffer_number` memtables. One
    /// that may leave none but the fresh active one flushes what it seals
    /// here, as every rotation did before flushes left the commit path.
    /// Otherwise the flushes behind it are normally done and nothing is
    /// written here. When they are not, the oldest frozen memtables are
    /// written out first: memory stays within the cap, and the commit path
    /// waits only when the background has fallen a whole memtable behind.
    fn rotation_due(&self, state: &After) -> Rotation {
        let used = state.memtable.approximate_size();
        if used < self.options.write_buffer_size {
            return Rotation::NotDue(used);
        }
        match self.options.max_write_buffer_number.checked_sub(1) {
            Some(0) => Rotation::FlushSealed,
            Some(room) if self.view.load().frozen.len() >= room => Rotation::FlushFirst { room },
            Some(_) | None => Rotation::Seal,
        }
    }

    /// Carry `rotation` out in the ordered step, before the write whose
    /// turn this is takes a sequence: a failure is the write's, and means
    /// it did not land. Its one caller classifies the stall thresholds
    /// again whatever this returns.
    ///
    /// A memtable below the size only has its counted bytes corrected to
    /// what it holds: the count is an upper bound.
    fn rotate(&self, ord: &mut Ordered<'_>, rotation: Rotation) -> io::Result<()> {
        match rotation {
            Rotation::NotDue(used) => {
                ord.state.mem_used = used;
                return Ok(());
            }
            Rotation::FlushSealed => {
                let sealed = ord.seal(true)?;
                return self.flush_until_retired(&sealed);
            }
            Rotation::Seal => {}
            Rotation::FlushFirst { room } => self.flush_down_to(room)?,
        }
        // Sealed here and flushed off the commit path (E9): by the worker,
        // or by a writer once it leaves the pipeline.
        ord.seal(true)?;
        self.flush_sealed_later();
        Ok(())
    }

    /// Flush the oldest frozen memtables, here, until fewer than `room`
    /// are left. Each flush gives its exclusion up before the next, and
    /// all of them before the caller classifies the stall thresholds.
    fn flush_down_to(&self, room: usize) -> io::Result<()> {
        while self.view.load().frozen.len() >= room {
            let flushed = self
                .flusher
                .flushing()
                .and_then(|flushing| self.flush_oldest_frozen(&flushing));
            if !flushed? {
                break;
            }
        }
        Ok(())
    }
}
