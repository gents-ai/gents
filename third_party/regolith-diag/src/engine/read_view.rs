//! The published read view: the one object a reader loads to obtain a
//! consistent set of LSM sources.
//!
//! # Invariants
//!
//! * A [`ReadView`] is immutable once published. Every mutation of the
//!   memtable set or of the current version publishes a **new** view;
//!   nothing ever mutates a view a reader may be holding.
//! * The view is the single source of truth for the active memtable and
//!   the frozen memtable list. `RegolithEngine` owns no separate copy.
//! * Successive published views only ever move data in the "older"
//!   direction (active -> frozen -> version) and never lose it, so a
//!   reader holding an older view sees a subset of the data a newer
//!   view exposes, never a different one.
//! * The `Arc<Version>` a view holds pins every `Arc<LiveSst>` in it,
//!   and each `LiveSst` holds the SSTable's open file descriptor. A
//!   compaction may unlink a table's path the instant it leaves the
//!   current version; a reader holding an older view keeps reading it
//!   through that descriptor. This view composes with that existing pin
//!   chain by adding one level to it, and does not duplicate it.
//! * **Readers take no lock and write no shared word.** A load is one
//!   pin of the calling thread's own reclamation slot and one acquire
//!   load of the published pointer ([`ReadViewCell::load`]); it is
//!   wait-free. A reader that must keep sources past the call clones the
//!   parts it needs (an iterator takes the memtables and the version),
//!   so nothing outlives a [`ViewGuard`] by borrowing through it.
//! * **A replaced view is freed only when no reader holds it.** A
//!   publication swaps the pointer and retires the old view to kovan,
//!   which drops it once every thread that could have loaded it has
//!   released its guard. A reader therefore never reads a freed view.
//! * **Every publication is a compare-and-swap from the exact view it
//!   was built from**, so the published views form one total order in
//!   which each is a function of its predecessor and no publication is
//!   lost. Publishers never exclude each other: one that loses the swap
//!   rebuilds on whatever won. That is what replaced the publish mutex.
//!   Every publisher goes through it, since [`ReadViewCell::publish`] is the
//!   only writer of the cell: a rotation (in the commit pipeline's ordered step), a
//!   flush on each flush path (the worker's, a writer's after its commit, a
//!   stall step's), a compaction, an ingest and `drop_all`. A flush installs
//!   its table and retires its memtable in one publication
//!   ([`VersionGuard::retire_with_version`](super::version_store::VersionGuard::retire_with_version)), so no view holds a memtable
//!   and its table at once. `proofs/tla/ReadViewPublishers.tla` checks that
//!   racing, they lose no memtable and no table, and hold none twice.
//! * **A load never returns a view older than one already published
//!   before the load began.** The swap is a release, the load an
//!   acquire of the same word, so a publication that happened before a
//!   load is what that load (or something newer) reads.
//! * No lock is left here: the version half of a view is installed by the
//!   one step of [`VersionStore`](super::version_store::VersionStore),
//!   which no reader takes.
//!
//! # What the deferred drop costs, and where it is paid
//!
//! The view a publication replaced is not dropped at the swap; kovan
//! drops it once no reader can still reach it. A retired view owns
//! memtables and an `Arc<Version>`, and a `Version` owns the open
//! descriptor of every SSTable in it, so a late drop holds memory, file
//! descriptors and the disk blocks of tables a compaction already
//! unlinked. [`quiesce`] bounds that: every publication that retires
//! a view hands it to the reclaimer at once, in a batch of its own that
//! kovan can place, and regolith's compaction thread calls [`idle`]
//! before it waits for work. A thread that read once and then parked
//! holds back only the views born before its last load, never the ones
//! born after it (`tests/adv_atom_reclaim.rs` checks both, at the
//! descriptor level).
//!
//! # Progress
//!
//! The load is wait-free. A publication is lock-free: it retries only
//! when another publication won, so some publisher always finishes.
//! On `armv7` and both `wasm32` targets kovan's reservation slot is a
//! 128-bit word that `portable-atomic` emulates with a spinlock, so
//! there a load takes that lock and neither bound holds; the publish
//! word itself is one pointer and is never emulated.
//!
//! # Why the version half is published by the store, not by callers
//!
//! Every version change goes through [`VersionSet::apply`], which is
//! only reachable through a
//! [`VersionGuard`](super::version_store::VersionGuard). The guard compares the
//! version it entered with against the one it leaves with and publishes
//! the difference, so a foreground flush, an ingest, a `drop_all` and a
//! background compaction all refresh the view without any of them
//! having to remember to.

use std::cell::Cell;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::sync::Arc;

use kovan::{Atom, AtomGuard};

use super::manifest::Version;
use super::memtable::MemTable;

/// The set of sources one read resolves against, plus nothing else.
pub(crate) struct ReadView {
    /// The memtable writers are currently appending to.
    pub(crate) active: Arc<MemTable>,
    /// Memtables sealed and awaiting flush, oldest first.
    pub(crate) frozen: Vec<Arc<MemTable>>,
    /// The LSM version: the SSTables at every level, with their readers
    /// already open.
    pub(crate) version: Arc<Version>,
}

/// Holds the currently published [`ReadView`].
pub(crate) struct ReadViewCell {
    current: Atom<ReadView>,
    /// Test-only: the views published after the initial one.
    #[cfg(test)]
    publications: std::sync::atomic::AtomicUsize,
}

/// One loaded view, pinned for as long as the guard lives.
///
/// Derefs to the [`ReadView`]. It holds the calling thread's reclamation
/// guard, so it is neither `Send` nor `Sync`: it is dropped on the thread
/// that loaded it, and it must not be held across a wait for another
/// thread. A view held through a long operation delays the drop of every
/// view retired after it was loaded, until it is released.
pub(crate) struct ViewGuard<'a> {
    /// Dropped by hand in `Drop`, before the owed flush runs: a flush made
    /// while this thread still holds a reclamation guard frees nothing.
    view: ManuallyDrop<AtomGuard<'a, ReadView>>,
}

impl Deref for ViewGuard<'_> {
    type Target = ReadView;

    #[inline]
    fn deref(&self) -> &ReadView {
        &self.view
    }
}

impl Drop for ViewGuard<'_> {
    #[inline]
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `view` is dropped exactly once, here, and never touched
        // again: this is the guard's own destructor.
        unsafe { ManuallyDrop::drop(&mut self.view) };
        let owed = HELD.with(|held| {
            let (live, owed) = held.get();
            let live = live.saturating_sub(1);
            held.set((live, owed && live != 0));
            owed && live == 0
        });
        if owed {
            // The batch was submitted at the publication; with no guard
            // left, this drains this thread's own reservation of it.
            kovan::flush();
        }
    }
}

std::thread_local! {
    /// The `ViewGuard`s live on this thread, and whether a publication
    /// made while one was live still owes [`quiesce`] its flush.
    ///
    /// kovan cannot free anything a live guard on the flushing thread may
    /// reach, so a flush made under a guard only submits: the retired view
    /// waits on this thread's own reservation until its next pin. A thread
    /// that published inside a read (a rotation in the middle of a commit
    /// group) and then went idle would hold that view, and every table
    /// descriptor in it, for as long as it idled. Counting the guards lets
    /// the flush run the moment the last one is released instead.
    static HELD: Cell<(u32, bool)> = const { Cell::new((0, false)) };
}

impl ReadViewCell {
    /// Publish an initial view. Called once per engine open.
    pub(crate) fn new(view: ReadView) -> Self {
        // A node is stamped with the epoch this thread last saw at a pin,
        // and a thread that never pinned has seen none: a view allocated
        // there would be stamped older than every reservation in the
        // process, so once retired any thread idling since its last read
        // would hold it, with every table descriptor it names. Pinning
        // first stamps it with the epoch it is actually born in.
        let _fresh_epoch = kovan::pin();
        Self {
            current: Atom::new(view),
            #[cfg(test)]
            publications: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Test-only: the views published after the initial one.
    #[cfg(test)]
    pub(crate) fn publications(&self) -> usize {
        self.publications.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Load the currently published view. This is the read path: a pin
    /// of this thread's own reclamation slot and one acquire load, with
    /// no lock and no write to any word another thread reads.
    ///
    /// The result is never older than a view whose publication happened
    /// before this call.
    #[inline]
    pub(crate) fn load(&self) -> ViewGuard<'_> {
        HELD.with(|held| {
            let (live, owed) = held.get();
            held.set((live + 1, owed));
        });
        ViewGuard {
            view: ManuallyDrop::new(self.current.load()),
        }
    }

    /// Publish `build(current)` by compare-and-swap from the exact view
    /// it was built from, rebuilding on whatever won when another
    /// publication got in first. Returns the value the winning build
    /// produced.
    ///
    /// `build` runs once per attempt and every attempt but the last is
    /// thrown away, so it must be a pure function of the view it is
    /// given: allocate the fresh memtable or create the next log before
    /// the call, never inside it. A losing attempt drops the view it
    /// built, which returns every reference count it took.
    fn publish<R>(&self, build: impl Fn(&ReadView) -> (ReadView, R)) -> R {
        // A view is stamped with the reclamation epoch this thread last
        // pinned at, and a thread idling since its last read keeps back
        // every retired batch holding anything stamped at or before the
        // epoch of that read, whether or not it ever saw it. Submitting
        // this thread's pending retirements first, padded so they are
        // placed rather than kept, does two things: it advances the epoch,
        // so the view this call publishes is stamped after every such read,
        // and it keeps older nodes out of the batch the view this call
        // replaces goes into, so that batch waits only on threads that
        // read after its oldest view was born. With a view guard live on
        // this thread the stamp stays that guard's epoch; that is only
        // later, never unsafe.
        if !holds_view() {
            super::reclaim::submit();
        }
        loop {
            let current = self.current.load();
            let (next, out) = build(&current);
            if self.current.compare_and_swap(&current, next).is_ok() {
                #[cfg(test)]
                self.publications
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return out;
            }
        }
    }

    /// Atomically replace the memtable half of the view. `mutate`
    /// receives the current `(active, frozen)` and returns the next
    /// pair plus a value for the caller, and both halves change in one
    /// publication rather than two observable steps.
    ///
    /// A compare-and-swap publication: a rotation and a flush retirement
    /// rebuild on whatever the other published instead of serializing
    /// behind it, so `mutate` may run more than once and must be pure
    /// (see [`Self::publish`]). The returned value is the one from the
    /// attempt that was published.
    pub(crate) fn update_memtables<R>(
        &self,
        mutate: impl Fn(&Arc<MemTable>, &[Arc<MemTable>]) -> (Arc<MemTable>, Vec<Arc<MemTable>>, R),
    ) -> R {
        let out = self.publish(|current| {
            let (active, frozen, out) = mutate(&current.active, &current.frozen);
            let next = ReadView {
                active,
                frozen,
                version: Arc::clone(&current.version),
            };
            (next, out)
        });
        // The view this replaced may hold the only reference to a
        // flushed memtable, a whole write buffer's worth of memory.
        quiesce();
        out
    }

    /// Drop one memtable from the frozen list, by identity.
    ///
    /// Called once a flush has published an SSTable holding that
    /// memtable's contents, or once the memtable proved to be empty.
    ///
    /// By identity and not by position, because position is not stable.
    /// A flush reads `frozen.first()` at entry and gets here only after
    /// writing a whole SSTable, and in that interval a rotation can
    /// append and another flush can retire. Dropping "index 0" would
    /// then drop a memtable this flush never wrote, whose contents are
    /// in no published version, and every write it held would vanish:
    /// a key that was only ever overwritten would read as an older
    /// version or as absent.
    ///
    /// Retiring a memtable that is already gone is a no-op, which is
    /// what makes this safe on every exit path a flush has.
    pub(crate) fn retire_memtable(&self, flushed: &Arc<MemTable>) {
        self.update_memtables(|active, frozen| {
            let next = frozen
                .iter()
                .filter(|mt| !Arc::ptr_eq(mt, flushed))
                .cloned()
                .collect();
            (Arc::clone(active), next, ())
        });
    }

    /// Replace the version half of the view, and, when `retired` names a
    /// memtable, drop it from the frozen list in the same publication.
    /// Called by the drop of a
    /// [`VersionGuard`](super::version_store::VersionGuard) and by nothing
    /// else.
    pub(super) fn publish_version(&self, version: Arc<Version>, retired: Option<&Arc<MemTable>>) {
        self.publish(|current| {
            let frozen = match retired {
                Some(flushed) => current
                    .frozen
                    .iter()
                    .filter(|mt| !Arc::ptr_eq(mt, flushed))
                    .cloned()
                    .collect(),
                None => current.frozen.clone(),
            };
            let next = ReadView {
                active: Arc::clone(&current.active),
                frozen,
                version: Arc::clone(&version),
            };
            (next, ())
        });
        // The view this replaced owns the previous `Arc<Version>`, and
        // with it the open descriptor of every table the edit just
        // dropped. Handing the retired view to the reclaimer here frees
        // those at the point a refcounted view would have: inside the
        // version-set critical section, which has just written a
        // manifest record anyway.
        quiesce();
    }
}

/// Whether a [`ViewGuard`] is live on this thread.
fn holds_view() -> bool {
    HELD.with(|held| held.get().0 != 0)
}

/// Hand the views this thread retired to the reclaimer, padded so kovan
/// places them at once (see `engine::reclaim`).
///
/// Every publication that retires a view calls this. With a view guard
/// still live on this thread the batch is also placed on this thread's
/// own reservation, which a thread that publishes and then idles would
/// never release; the release is therefore owed, and paid by a flush the
/// moment the last guard is dropped. Wait-free either way.
pub(crate) fn quiesce() {
    super::reclaim::submit();
    HELD.with(|held| {
        let (live, _) = held.get();
        if live != 0 {
            held.set((live, true));
        }
    });
}

/// Release what this thread's reservation is holding back before the
/// thread waits for work.
///
/// kovan keeps a thread's reservation published after its last guard
/// drops, so a thread that loaded a view and went idle keeps alive every
/// view retired before that load, and with it memtables and table
/// descriptors. regolith's compaction thread calls this before each wait;
/// a thread regolith does not own releases at its next read.
pub(crate) fn idle() {
    kovan::flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::manifest::{VersionEdit, VersionSet};
    use crate::engine::version_store::VersionStore;

    /// A memtable arena sized for the unit tests here: small enough to
    /// stay cheap, large enough that nothing in these tests rotates.
    fn test_memtable_config() -> crate::engine::memtable::MemTableConfig {
        crate::engine::memtable::MemTableConfig::new(
            crate::engine::arena::ArenaProfile::EMBEDDED,
            64 * 1024,
            2,
        )
    }

    fn store_with_view() -> (tempfile::TempDir, Arc<VersionStore>, Arc<ReadViewCell>) {
        let dir = tempfile::tempdir().unwrap();
        let sst_dir = dir.path().join("sst");
        std::fs::create_dir_all(&sst_dir).unwrap();
        let versions = VersionSet::open(dir.path(), &sst_dir).unwrap();
        let store = Arc::new(VersionStore::new(versions));
        let cell = Arc::new(ReadViewCell::new(ReadView {
            active: Arc::new(MemTable::new(&test_memtable_config()).unwrap()),
            frozen: Vec::new(),
            version: store.current(),
        }));
        store.attach_view(Arc::clone(&cell));
        (dir, store, cell)
    }

    /// The defect this guards: a flush chose its victim as
    /// `frozen.first()` and then retired "index 0", which is not the
    /// same memtable once anything else has touched the list. Two
    /// flushes could then retire the same memtable twice and drop a
    /// second one nothing had written, losing every acknowledged write
    /// it held.
    #[test]
    fn retiring_a_memtable_drops_that_one_and_leaves_the_rest_in_order() {
        let (_dir, _store, cell) = store_with_view();
        let frozen: Vec<Arc<MemTable>> = (0..3)
            .map(|i| {
                let mt = Arc::new(MemTable::new(&test_memtable_config()).unwrap());
                mt.put(
                    format!("k{i}").as_bytes(),
                    format!("v{i}").as_bytes(),
                    i + 1,
                );
                mt
            })
            .collect();
        cell.update_memtables(|active, _| (Arc::clone(active), frozen.clone(), ()));

        // Retire the middle one, which is what a flush that started
        // before a rotation and finished after another retirement is
        // holding. Positional retirement would take index 0 here.
        cell.retire_memtable(&frozen[1]);

        let after = cell.load();
        assert_eq!(after.frozen.len(), 2, "exactly one memtable must go");
        assert!(
            Arc::ptr_eq(&after.frozen[0], &frozen[0]),
            "retiring the middle memtable dropped the oldest one instead: every write in it is \
             in no published version and is now unreachable",
        );
        assert!(
            Arc::ptr_eq(&after.frozen[1], &frozen[2]),
            "the newest memtable did not keep its place",
        );
    }

    /// A sealed memtable carries the log its records are in, so the
    /// flush that persists it can unlink that log and no other.
    ///
    /// The defect this guards: the flush was handed whatever log its
    /// *caller* had just sealed, while it wrote whatever memtable was at
    /// the front of the frozen list. Those are only the same memtable
    /// when flushes and seals are perfectly interleaved. When they are
    /// not, the flush unlinks the only durable copy of a memtable nobody
    /// has flushed, and a crash loses every write in it.
    #[test]
    fn a_sealed_memtable_carries_the_log_its_records_are_in() {
        let (_dir, _store, cell) = store_with_view();

        let first = Arc::new(MemTable::new(&test_memtable_config()).unwrap());
        first.put(b"a", b"1", 1);
        first.seal_wal(std::path::PathBuf::from("/wal/000001.log"));

        let second = Arc::new(MemTable::new(&test_memtable_config()).unwrap());
        second.put(b"b", b"2", 2);
        second.seal_wal(std::path::PathBuf::from("/wal/000002.log"));

        cell.update_memtables(|active, _| {
            (Arc::clone(active), vec![first.clone(), second.clone()], ())
        });

        let view = cell.load();
        assert_eq!(
            view.frozen[0].sealed_wal(),
            Some(std::path::Path::new("/wal/000001.log")),
            "the front of the frozen list must name its own log, not the newest one",
        );
        assert_eq!(
            view.frozen[1].sealed_wal(),
            Some(std::path::Path::new("/wal/000002.log")),
        );
        assert_eq!(
            view.active.sealed_wal(),
            None,
            "the active memtable is still taking writes, so its log is not sealed",
        );

        // Retiring the front does not disturb the other's log identity:
        // the flush that comes next still unlinks its own.
        cell.retire_memtable(&first);
        let after = cell.load();
        assert_eq!(after.frozen.len(), 1);
        assert_eq!(
            after.frozen[0].sealed_wal(),
            Some(std::path::Path::new("/wal/000002.log")),
        );
    }

    /// Every exit path of a flush retires, including the ones that
    /// found nothing to write, so retiring twice has to be harmless.
    #[test]
    fn retiring_a_memtable_that_is_already_gone_changes_nothing() {
        let (_dir, _store, cell) = store_with_view();
        let frozen: Vec<Arc<MemTable>> = (0..2)
            .map(|_| Arc::new(MemTable::new(&test_memtable_config()).unwrap()))
            .collect();
        cell.update_memtables(|active, _| (Arc::clone(active), frozen.clone(), ()));

        cell.retire_memtable(&frozen[0]);
        cell.retire_memtable(&frozen[0]);

        let after = cell.load();
        assert_eq!(
            after.frozen.len(),
            1,
            "a second retirement of the same memtable took a different one with it",
        );
        assert!(Arc::ptr_eq(&after.frozen[0], &frozen[1]));
    }

    #[test]
    fn a_rotation_publishes_the_sealed_memtable_and_the_fresh_one_together() {
        let (_dir, _store, cell) = store_with_view();
        let before = cell.load();
        before.active.put(b"k", b"v", 1);
        let fresh = Arc::new(MemTable::new(&test_memtable_config()).unwrap());

        cell.update_memtables(|active, frozen| {
            let mut next = frozen.to_vec();
            next.push(Arc::clone(active));
            (Arc::clone(&fresh), next, ())
        });

        let after = cell.load();
        assert!(after.active.is_empty(), "writers got a fresh memtable");
        assert_eq!(after.frozen.len(), 1);
        assert!(
            Arc::ptr_eq(&after.frozen[0], &before.active),
            "the sealed memtable is the one writers were using",
        );
        assert!(
            Arc::ptr_eq(&after.version, &before.version),
            "a memtable publication leaves the version alone",
        );
        assert!(
            !before.active.is_empty(),
            "the view a reader still holds keeps its data",
        );
    }

    #[test]
    fn a_version_edit_publishes_a_new_view_that_keeps_the_memtables() {
        let (_dir, store, cell) = store_with_view();
        let before = cell.load();

        store
            .run(|versions| versions.apply(&[VersionEdit::SetNextFileId(7)]))
            .unwrap();

        let after = cell.load();
        assert_eq!(after.version.next_file_id, 7);
        assert!(Arc::ptr_eq(&after.active, &before.active));
        assert_eq!(after.frozen.len(), before.frozen.len());
    }

    #[test]
    fn a_critical_section_that_changes_no_version_publishes_nothing() {
        let (_dir, store, cell) = store_with_view();
        let before = cell.load();

        store.run(|versions| {
            let _ = versions.current();
        });

        assert!(
            std::ptr::eq::<ReadView>(&*before, &*cell.load()),
            "a read-only critical section must not churn the published view",
        );
    }
}

#[cfg(test)]
#[path = "read_view_tests.rs"]
mod concurrency_tests;
