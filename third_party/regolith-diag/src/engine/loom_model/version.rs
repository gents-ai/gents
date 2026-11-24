//! Models of the version handoff: a flush and a compaction publishing
//! into the same version while a reader walks it.
//!
//! This is a protocol model, and deliberately so. The production
//! `engine::manifest::VersionSet` writes a manifest record and
//! opens an `engine::sstable::SsTableReader` inside `apply`, and
//! its locks come from `std::sync` rather than from
//! `crate::sync::internal`, so loom can neither run it nor see its
//! ordering. What is reproduced here is the part loom can decide:
//!
//! - `current()` clones the `Arc<Version>` under a read lock, so a
//!   reader pins one immutable snapshot for a whole lookup;
//! - `apply` is a clone-mutate-store read-modify-write of that `Arc`;
//! - every caller of `apply` holds the engine's `Mutex<VersionSet>`
//!   first, which is what makes the read-modify-write atomic;
//! - a compaction publishes its removals and its additions in **one**
//!   `apply`, so no reader can observe a version with the inputs gone
//!   and the output not yet there.
//!
//! An SSTable is stood in for by its file id and the one user key it
//! holds. Everything the handoff can get wrong happens between the level
//! a compaction reads and the level it writes, so two levels are enough.

use loom::sync::{Arc, Mutex, RwLock};

use super::explore;

/// One installed SSTable: its file id and the single user key it holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Table {
    id: u64,
    key: u8,
}

/// The two input tables a compaction consumes, both holding `k`.
const INPUT_A: Table = Table { id: 1, key: b'k' };
const INPUT_B: Table = Table { id: 2, key: b'k' };
/// The compaction's output, holding the same key at the next level.
const OUTPUT: Table = Table { id: 3, key: b'k' };
/// The table a concurrent flush installs at L0, holding a different key.
const FLUSHED: Table = Table { id: 4, key: b'w' };

/// The immutable snapshot a reader pins: one file list per level.
#[derive(Clone)]
struct Version {
    levels: [Vec<Table>; 2],
}

impl Version {
    /// The newest table holding `key`, searched exactly as
    /// `RegolithEngine::lookup` searches: L0 newest first, then L1.
    fn find(&self, key: u8) -> Option<Table> {
        self.levels[0]
            .iter()
            .rev()
            .chain(self.levels[1].iter())
            .copied()
            .find(|table| table.key == key)
    }
}

/// One entry of a `VersionEdit` batch.
enum Edit {
    /// Install `table` at `level`.
    Add(usize, Table),
    /// Drop the table with this file id from `level`.
    Remove(usize, u64),
}

/// `VersionSet`'s shape: the current version behind an `RwLock`, cloned
/// out by `current` and replaced wholesale by `apply`.
struct VersionSet {
    current: RwLock<Arc<Version>>,
}

impl VersionSet {
    fn new(levels: [Vec<Table>; 2]) -> Self {
        Self {
            current: RwLock::new(Arc::new(Version { levels })),
        }
    }

    /// Pin the current version.
    fn current(&self) -> Arc<Version> {
        Arc::clone(&self.current.read().expect("current"))
    }

    /// Apply a batch of edits and publish the result as one new version.
    ///
    /// The caller must hold the engine's `Mutex<VersionSet>`: this is a
    /// read-modify-write of `current`, and two unserialized callers lose
    /// one another's edits. [`apply_locked`] is the shape every engine
    /// call site actually has.
    fn apply(&self, edits: &[Edit]) {
        let mut version = (*self.current()).clone();
        for edit in edits {
            match edit {
                Edit::Add(level, table) => version.levels[*level].push(*table),
                Edit::Remove(level, id) => version.levels[*level].retain(|t| t.id != *id),
            }
        }
        *self.current.write().expect("current") = Arc::new(version);
    }
}

/// `self.versions.run(|versions| versions.apply(edits))`, the shape of
/// every publishing call site in the engine. The mutex here stands for the
/// version store's step, which runs one section at a time; the step's own
/// protocol is modelled in `step.rs`.
///
/// The step is what makes `apply`'s clone-mutate-store atomic, and a
/// section is exactly one `apply`. That
/// scope is the whole reason a compaction has to publish its removals
/// and its addition in one batch: between two `apply` calls the mutex is
/// free and a reader can pin whatever is current.
fn apply_locked(pipeline: &Mutex<()>, versions: &VersionSet, edits: &[Edit]) {
    let _lock = pipeline.lock().expect("pipeline");
    versions.apply(edits);
}

/// Pinning the current version from inside the step, the strongest form
/// of `RegolithEngine::lookup` pinning the version it will walk.
fn current_locked(pipeline: &Mutex<()>, versions: &VersionSet) -> Arc<Version> {
    let _lock = pipeline.lock().expect("pipeline");
    versions.current()
}

/// The edits a compaction of both L0 inputs into L1 publishes.
fn compaction_edits() -> [Edit; 3] {
    [
        Edit::Remove(0, INPUT_A.id),
        Edit::Remove(0, INPUT_B.id),
        Edit::Add(1, OUTPUT),
    ]
}

/// A version set holding both L0 inputs and an empty L1.
fn before_compaction() -> Arc<VersionSet> {
    Arc::new(VersionSet::new([vec![INPUT_A, INPUT_B], Vec::new()]))
}

/// A reader that pins one version never sees a key vanish under a
/// compaction, because the removals and the addition are one `apply`.
///
/// The reader pins its version and then walks the pinned snapshot with no
/// lock held at all - which is why the snapshot has to be whole.
pub fn a_reader_pins_one_version_across_a_compaction() {
    explore(
        "a_reader_pins_one_version_across_a_compaction",
        64,
        8,
        |witness| {
            let versions = before_compaction();
            let pipeline = Arc::new(Mutex::new(()));

            let compaction = {
                let versions = Arc::clone(&versions);
                let pipeline = Arc::clone(&pipeline);
                loom::thread::spawn(move || {
                    apply_locked(&pipeline, &versions, &compaction_edits());
                })
            };
            let reader = {
                let versions = Arc::clone(&versions);
                let pipeline = Arc::clone(&pipeline);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    let version = current_locked(&pipeline, &versions);
                    let found = version
                        .find(b'k')
                        .expect("the compaction handoff hid a key that was never deleted");
                    // The schedule the single `apply` exists for: the
                    // reader pinned a version the compaction had already
                    // published.
                    if found.id == OUTPUT.id {
                        witness.record();
                    }
                })
            };

            compaction.join().expect("compaction");
            reader.join().expect("reader");

            let version = versions.current();
            assert!(version.levels[0].is_empty(), "both inputs were removed");
            assert_eq!(version.levels[1], vec![OUTPUT]);
        },
    );
}

/// Calibration for [`a_reader_pins_one_version_across_a_compaction`]:
/// the same compaction publishing its removals and its addition as two
/// separate versions.
///
/// Between them there is a published version in which the key is at no
/// level at all, and a reader that pins it reports a key that was never
/// deleted as absent. The model must find that window; if it cannot, the
/// model above says nothing about why the real compaction applies once.
pub fn a_split_compaction_apply_hides_a_key() {
    explore("a_split_compaction_apply_hides_a_key", 4, 1, |witness| {
        let versions = before_compaction();
        let pipeline = Arc::new(Mutex::new(()));

        let compaction = {
            let versions = Arc::clone(&versions);
            let pipeline = Arc::clone(&pipeline);
            loom::thread::spawn(move || {
                apply_locked(
                    &pipeline,
                    &versions,
                    &[Edit::Remove(0, INPUT_A.id), Edit::Remove(0, INPUT_B.id)],
                );
                apply_locked(&pipeline, &versions, &[Edit::Add(1, OUTPUT)]);
            })
        };
        let reader = {
            let versions = Arc::clone(&versions);
            let pipeline = Arc::clone(&pipeline);
            let witness = witness.clone();
            loom::thread::spawn(move || {
                let version = current_locked(&pipeline, &versions);
                witness.record();
                assert!(version.find(b'k').is_some(), "the split apply hid `k`");
            })
        };

        compaction.join().expect("compaction");
        reader.join().expect("reader");
    });
}

/// A flush and a compaction publishing at the same time keep both
/// edits: the step is what makes `apply`'s clone-mutate-store atomic.
///
/// This is the handoff between a writer and compaction. The flush adds a
/// table at L0 while the compaction removes two L0 tables and adds one
/// at L1, and each `apply` is a read-modify-write of the same
/// `Arc<Version>`. Whichever runs second must build on the other's
/// version, never on the one it read before the other published.
pub fn a_flush_and_a_compaction_cannot_lose_each_other() {
    explore(
        "a_flush_and_a_compaction_cannot_lose_each_other",
        64,
        8,
        |witness| {
            let versions = before_compaction();
            let pipeline = Arc::new(Mutex::new(()));

            let flush = {
                let versions = Arc::clone(&versions);
                let pipeline = Arc::clone(&pipeline);
                let witness = witness.clone();
                loom::thread::spawn(move || {
                    // The interesting order: the compaction published
                    // first, so this flush must extend its version.
                    if current_locked(&pipeline, &versions).levels[1].contains(&OUTPUT) {
                        witness.record();
                    }
                    apply_locked(&pipeline, &versions, &[Edit::Add(0, FLUSHED)]);
                })
            };
            let compaction = {
                let versions = Arc::clone(&versions);
                let pipeline = Arc::clone(&pipeline);
                loom::thread::spawn(move || {
                    apply_locked(&pipeline, &versions, &compaction_edits());
                })
            };

            flush.join().expect("flush");
            compaction.join().expect("compaction");

            let version = versions.current();
            assert_eq!(
                version.levels[0],
                vec![FLUSHED],
                "the flush's table was lost by the compaction's version swap"
            );
            assert_eq!(
                version.levels[1],
                vec![OUTPUT],
                "the compaction's output was lost by the flush's version swap"
            );
        },
    );
}

/// Calibration for [`a_flush_and_a_compaction_cannot_lose_each_other`]:
/// the same two publishers with nothing serializing them.
///
/// `apply` reads the current version, mutates a clone and stores it
/// back. Unserialized, one publisher's clone predates the other's store
/// and overwrites it, and an installed SSTable disappears from the
/// version while its bytes are on disk. The model must find that; if it
/// cannot, the model above is not showing what the mutex buys.
pub fn an_unserialized_version_swap_loses_an_edit() {
    explore(
        "an_unserialized_version_swap_loses_an_edit",
        4,
        1,
        |witness| {
            let versions = before_compaction();

            let flush = {
                let versions = Arc::clone(&versions);
                loom::thread::spawn(move || {
                    versions.apply(&[Edit::Add(0, FLUSHED)]);
                })
            };
            let compaction = {
                let versions = Arc::clone(&versions);
                loom::thread::spawn(move || {
                    versions.apply(&compaction_edits());
                })
            };

            flush.join().expect("flush");
            compaction.join().expect("compaction");

            witness.record();
            let version = versions.current();
            assert!(
                version.levels[0] == vec![FLUSHED] && version.levels[1] == vec![OUTPUT],
                "an unserialized version swap lost an edit"
            );
        },
    );
}
