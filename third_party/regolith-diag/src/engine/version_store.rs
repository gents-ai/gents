//! The version set, and the one step that changes it (plan 4.6, 4.7).
//!
//! A flush, a compaction, an ingest, a rotation and `drop_all` each read the
//! current version and append to the manifest what turns it into the next
//! one. Those read-modify-writes must not interleave, and each appends to
//! one file, so they run one at a time. They used to take a mutex for that,
//! and each waited its own turn on it. Here each is a section of a
//! [`Step`]: run by whichever thread holds the step, so no caller waits for
//! its own thread to be scheduled, only for the manifest I/O under way.
//!
//! Reading the current version takes no step: [`VersionStore::current`] is
//! one load of the published view.
//!
//! # Invariants
//!
//! - **V1, one thread in the version set.** The set is touched only inside
//!   a section of the step, and the step runs one section at a time
//!   (`step.rs`, ST1).
//! - **V2, publications land in the order of the edits.** A section
//!   publishes the version it installed before it ends ([`VersionGuard`]),
//!   and so before the next section reads the set (ST3).
//! - **V3, the published version is the set's.** Every change of the set
//!   goes through a section, and V2 publishes it, so outside a section the
//!   view's version is the version set's current one.

#![allow(unsafe_code)]

use std::io;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

use super::io::scope;
use super::manifest::{Version, VersionEdit, VersionSet};
use super::memtable::MemTable;
use super::read_view::ReadViewCell;
use super::step::{Hold, Step};
use crate::sync::internal::UnsafeCell;

/// The [`VersionSet`] plus the read view its edits publish into. See the
/// module docs.
pub(crate) struct VersionStore {
    /// The version set (V1).
    set: UnsafeCell<VersionSet>,
    /// Attached after construction: the view needs a memtable, and the
    /// version set is built before one exists. Empty only between
    /// [`Self::new`] and [`Self::attach_view`], a window with no
    /// concurrent readers.
    view: OnceLock<Arc<ReadViewCell>>,
    /// What runs the sections, one at a time.
    step: Step,
}

// SAFETY: the version set moves between the threads that run sections,
// which `VersionSet: Send` allows.
unsafe impl Send for VersionStore where VersionSet: Send {}
// SAFETY: the set is reached only by the thread that holds the step (V1),
// so sharing the store never shares the set; every other field is `Sync`.
unsafe impl Sync for VersionStore where VersionSet: Send {}

impl VersionStore {
    pub(crate) fn new(versions: VersionSet) -> Self {
        Self {
            set: UnsafeCell::new(versions),
            view: OnceLock::new(),
            step: Step::new(),
        }
    }

    /// Attach the read-view cell this store publishes into. Called once,
    /// during engine open, before the engine is handed out.
    pub(crate) fn attach_view(&self, cell: Arc<ReadViewCell>) {
        let _ = self.view.set(cell);
    }

    /// The current version: the one the last section to finish installed
    /// (V3). Takes no step once the view is attached.
    pub(crate) fn current(&self) -> Arc<Version> {
        match self.view.get() {
            Some(view) => Arc::clone(&view.load().version),
            None => self.run(|versions| versions.current()),
        }
    }

    /// Run `section` on the version set, alone and after every section
    /// handed over before it, and return what it returns. The guard it is
    /// given derefs to [`VersionSet`] and publishes the resulting version
    /// when the section ends, if the section changed it.
    ///
    /// The section may run on another thread, the one that holds the step,
    /// which is why it is `Send`. It has finished when this returns.
    pub(crate) fn run<T: Send>(
        &self,
        section: impl FnOnce(&mut VersionGuard<'_>) -> T + Send,
    ) -> T {
        if !self.step.held_here() {
            // Another thread may hold the step, and this call then waits
            // for its section to be run there.
            scope::waits("the version store's step, handed a section without holding it");
        }
        self.step.run(|| {
            // A section reads the device, whatever read mode the thread
            // that happens to run it is in.
            let _device = scope::blocking();
            self.set.with_mut(|set| {
                // SAFETY (V1): this runs inside a section of the step, so
                // no other thread reaches the set until it returns.
                let set = unsafe { &mut *set };
                let mut guard = VersionGuard {
                    entry_version: set.current(),
                    set,
                    view: self.view.get(),
                    retire: None,
                };
                section(&mut guard)
            })
        })
    }

    /// Take the next file id, recorded in the manifest before it is handed
    /// out, so no two files of the database ever share one: a table a
    /// flush, a compaction or an ingest is about to write, or the next log.
    pub(crate) fn take_file_id(&self) -> io::Result<u64> {
        self.take_file_ids(1)
    }

    /// Take `count` consecutive file ids in one section, and so with one
    /// manifest record: the first of them is returned. For a caller that
    /// knows how many files it is about to write and may not come back to
    /// the step for each: an ingest staging its files.
    pub(crate) fn take_file_ids(&self, count: u64) -> io::Result<u64> {
        self.run(|versions| {
            let id = versions.current().next_file_id;
            versions.apply(&[VersionEdit::SetNextFileId(id + count)])?;
            Ok(id)
        })
    }

    /// Take the step for work that hands sections over and may not wait for
    /// another thread: an ordered unit a queue's poll runs
    /// (`engine::commit::park`). `None` when another thread holds it:
    /// `waker` is then woken once that thread gives it up. See
    /// [`Step::hold_or_park`].
    pub(crate) fn hold_or_park(&self, waker: &std::task::Waker) -> Option<Hold<'_>> {
        self.step.hold_or_park(waker)
    }

    /// Test-only: whether a section is waiting for the step.
    #[cfg(test)]
    pub(crate) fn has_waiting(&self) -> bool {
        self.step.has_waiting()
    }
}

/// The version set as one section sees it, publishing when the section
/// ends.
pub(crate) struct VersionGuard<'a> {
    set: &'a mut VersionSet,
    view: Option<&'a Arc<ReadViewCell>>,
    entry_version: Arc<Version>,
    /// A frozen memtable to retire in the publication of the version this
    /// section installs (see [`Self::retire_with_version`]).
    retire: Option<Arc<MemTable>>,
}

impl VersionGuard<'_> {
    /// Retire `flushed` from the frozen list in the same publication as the
    /// version this section installed, the one holding `flushed`'s table.
    ///
    /// A flush that published its table and then, in a second publication,
    /// retired its memtable would leave a window in which a view holds both:
    /// every entry of the memtable twice, once in the memtable and once in
    /// the table, at the same sequence. A read collects a key's merge
    /// operands from every source, so it would apply each of them twice.
    /// One publication makes the sources of every view disjoint.
    pub(crate) fn retire_with_version(&mut self, flushed: &Arc<MemTable>) {
        self.retire = Some(Arc::clone(flushed));
    }
}

impl Deref for VersionGuard<'_> {
    type Target = VersionSet;

    fn deref(&self) -> &VersionSet {
        self.set
    }
}

impl DerefMut for VersionGuard<'_> {
    fn deref_mut(&mut self) -> &mut VersionSet {
        self.set
    }
}

impl Drop for VersionGuard<'_> {
    fn drop(&mut self) {
        let Some(view) = self.view else {
            return;
        };
        let current = self.set.current();
        let retire = self.retire.take();
        if Arc::ptr_eq(&current, &self.entry_version) {
            // No version to publish: a retire asked for without one still
            // happens, on its own.
            if let Some(flushed) = retire {
                view.retire_memtable(&flushed);
            }
            return;
        }
        // Published before the section ends, and so before the next one
        // begins (V2).
        view.publish_version(current, retire.as_ref());
    }
}

#[cfg(all(test, not(loom)))]
#[path = "version_store_tests.rs"]
mod tests;
