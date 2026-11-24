//! Models of a column family's create, drop and use racing (plan 4.6).
//!
//! A protocol model, and deliberately so. The production registry
//! (`column_family::CfRegistry`) keeps liveness in a kovan map, whose
//! atomics loom does not see, and the ordered step is the commit
//! pipeline's, which needs a whole engine. What is reproduced
//! here is the part loom can decide, each piece standing in for one line of
//! `engine::commit::families`:
//!
//! - the family's liveness is one atomic word that moves unborn, live, dead
//!   and never back (`CfRegistry::publish`, `retire`: one map write each,
//!   and ids are never reused);
//! - the ordered step is one mutex here, and inside it a write takes the
//!   next sequence (in the engine: slots decided one at a time, in ring
//!   order);
//! - a write checks the family in the ordered step before it takes a
//!   sequence (`cf_fence`, the first act of deciding every commit, whatever
//!   kind of slot it is);
//! - a drop takes the tombstone's sequence in the ordered step and retires
//!   the family before it leaves it (`drop_family`);
//! - a create publishes the family in the ordered step after its meta
//!   entry's sequence (`create_family`).
//!
//! The rule checked: every write that lands has a sequence below the drop's
//! tombstone, so the tombstone deletes it, and every write after the drop is
//! refused. A reader watching the family sees unborn, live and dead in that
//! order. The calibration checks the family outside the ordered step, as a
//! fence at the API boundary alone would, and must fail.

use loom::sync::atomic::{AtomicU8, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

use super::explore;

/// The family's id has never been published.
const UNBORN: u8 = 0;
/// Published: writes to it are admitted.
const LIVE: u8 = 1;
/// Retired: every later write to it is refused.
const DEAD: u8 = 2;

/// What the ordered step owns: the sequence counter, and what landed.
#[derive(Default)]
struct Ordered {
    /// The last sequence handed out.
    seq: u64,
    /// The sequence of the drop's range tombstone, once it committed.
    tombstone: Option<u64>,
    /// The sequence of every write to the family that committed.
    landed: Vec<u64>,
}

/// One database: its ordered step and the family's liveness word.
struct Db {
    pipeline: Mutex<Ordered>,
    family: AtomicU8,
}

impl Db {
    fn new(state: u8) -> Arc<Self> {
        Arc::new(Self {
            pipeline: Mutex::new(Ordered::default()),
            family: AtomicU8::new(state),
        })
    }

    /// A write to the family. `fence_in_order` is the production rule; the
    /// calibration passes `false` and checks only before the ordered step.
    fn write(&self, fence_in_order: bool) -> bool {
        // The API boundary's early check: a handle already stale is refused
        // before any work.
        if self.family.load(Ordering::Acquire) != LIVE {
            return false;
        }
        let mut ordered = self.pipeline.lock().expect("pipeline");
        // The ordered step's fence, before the write takes a sequence.
        if fence_in_order && self.family.load(Ordering::Acquire) != LIVE {
            return false;
        }
        ordered.seq += 1;
        let seq = ordered.seq;
        ordered.landed.push(seq);
        true
    }

    /// Drop the family: the tombstone takes a sequence and the family is
    /// retired, both inside the ordered step.
    fn drop_family(&self) -> bool {
        let mut ordered = self.pipeline.lock().expect("pipeline");
        if self.family.load(Ordering::Acquire) != LIVE {
            return false;
        }
        ordered.seq += 1;
        ordered.tombstone = Some(ordered.seq);
        self.family.store(DEAD, Ordering::Release);
        true
    }

    /// Create the family: its meta entry takes a sequence, then it is
    /// published, inside the ordered step.
    fn create_family(&self) {
        let mut ordered = self.pipeline.lock().expect("pipeline");
        if self.family.load(Ordering::Acquire) == UNBORN {
            ordered.seq += 1;
            self.family.store(LIVE, Ordering::Release);
        }
    }

    /// Every write that landed is below the tombstone, and so deleted by it.
    fn check(&self) -> usize {
        let ordered = self.pipeline.lock().expect("pipeline");
        if let Some(tombstone) = ordered.tombstone {
            for &seq in &ordered.landed {
                assert!(
                    seq < tombstone,
                    "a write landed after the drop's tombstone: write {seq}, tombstone {tombstone}"
                );
            }
        }
        ordered.landed.len()
    }
}

/// Two writers race a drop of a live family. Every write that lands is
/// under the tombstone; the rest are refused.
pub fn a_write_racing_a_drop_is_deleted_or_refused() {
    explore(
        "a_write_racing_a_drop_is_deleted_or_refused",
        20,
        1,
        |witness| {
            let db = Db::new(LIVE);
            let writers: Vec<_> = (0..2)
                .map(|_| {
                    let db = Arc::clone(&db);
                    thread::spawn(move || db.write(true))
                })
                .collect();
            assert!(db.drop_family(), "the only drop must win");
            let outcomes: Vec<bool> = writers
                .into_iter()
                .map(|w| w.join().expect("writer"))
                .collect();
            let landed = db.check();
            assert_eq!(landed, outcomes.iter().filter(|&&ok| ok).count());
            // The interesting schedules: a write that passed the boundary
            // check while live and was then refused in the ordered step.
            if outcomes.contains(&false) {
                witness.record();
            }
        },
    );
}

/// A create, a write and a drop race a reader. The reader's observations
/// move unborn, live, dead and never back, and the write lands only between
/// the create and the drop.
pub fn a_family_lives_once_in_order() {
    explore("a_family_lives_once_in_order", 20, 1, |witness| {
        let db = Db::new(UNBORN);
        let reader = {
            let db = Arc::clone(&db);
            thread::spawn(move || {
                let first = db.family.load(Ordering::Acquire);
                let second = db.family.load(Ordering::Acquire);
                assert!(
                    first <= second,
                    "the family went back: {first} then {second}"
                );
                (first, second)
            })
        };
        let writer = {
            let db = Arc::clone(&db);
            thread::spawn(move || db.write(true))
        };
        db.create_family();
        db.drop_family();
        let wrote = writer.join().expect("writer");
        let seen = reader.join().expect("reader");
        assert_eq!(db.check(), usize::from(wrote));
        if seen.0 != seen.1 {
            witness.record();
        }
    });
}

/// Calibration: the family checked only before the ordered step. A write
/// passes the check, the drop commits its tombstone and retires the family,
/// and the write then takes a sequence above the tombstone. Must fail.
pub fn calibration_a_fence_outside_the_ordered_step_lands_after_the_drop() {
    explore(
        "calibration_a_fence_outside_the_ordered_step_lands_after_the_drop",
        1,
        0,
        |_| {
            let db = Db::new(LIVE);
            let writer = {
                let db = Arc::clone(&db);
                thread::spawn(move || db.write(false))
            };
            db.drop_family();
            writer.join().expect("writer");
            db.check();
        },
    );
}
