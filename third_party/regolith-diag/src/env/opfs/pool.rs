//! The Rust-side bookkeeping for a slot pool.
//!
//! [`super::sah`] owns the on-disk slot format and the JS handles; this
//! module owns the map from regolith's logical paths to slot indices, the
//! free list, and the virtual directory set. Every operation here is
//! synchronous, which is the whole point: `Db::open` lists directories and
//! the write path creates files, both from code that cannot `await`.
//!
//! # No lock
//!
//! The maps are lock-free kovan maps, the free list a lock-free queue, and
//! a slot's state atomics plus its binding (path and generation) in a
//! `kovan::Atom`, so a read or a write of a file takes no lock: it loads
//! the binding, checks its generation, and reads or writes through the
//! handle. Every change to a slot's binding follows a call into the
//! mount's handles, which live in the mounting thread's own registry
//! (`sah`), so bindings change on that one thread; the atomics make what
//! other threads may read (`exists`, `metadata`, `read_dir`) whole, never
//! torn.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kovan::Atom;
use kovan_map::HashMap;
use kovan_queue::seg_queue::SegQueue;

use crate::portability::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::sah::{self, MountId, Slot, SlotHeader};
use super::{not_found, register_ancestors};
use crate::env::{ReadDir, flat_children};

/// Buckets each map starts with; they grow on demand.
const BUCKETS: usize = 64;

/// Which logical file a slot holds, if any, and under which generation.
#[derive(Clone)]
struct Binding {
    /// `None` when the slot is free.
    path: Option<PathBuf>,
    generation: u64,
}

/// One pre-opened handle's state.
struct PoolSlot {
    binding: Atom<Binding>,
    len: AtomicU64,
    /// Mutations of the length or the binding, counted. The header on disk
    /// is stale while `changes` is above `synced`, so a write that lands
    /// while a sync runs keeps the header dirty.
    changes: AtomicU64,
    synced: AtomicU64,
    /// On the free list. Guards against pushing a slot twice.
    free: AtomicBool,
}

impl PoolSlot {
    fn new(path: Option<PathBuf>, generation: u64, len: u64) -> Arc<Self> {
        Arc::new(Self {
            binding: Atom::new(Binding { path, generation }),
            len: AtomicU64::new(len),
            changes: AtomicU64::new(0),
            synced: AtomicU64::new(0),
            free: AtomicBool::new(false),
        })
    }

    fn dirty(&self) {
        self.changes.fetch_add(1, Ordering::AcqRel);
    }

    /// The slot as `sah::sync_slot` writes its header, with the change count
    /// it captures.
    fn snapshot(&self) -> (Slot, u64) {
        let changes = self.changes.load(Ordering::Acquire);
        let binding = self.binding.load();
        (
            Slot {
                path: binding.path.clone(),
                len: self.len.load(Ordering::Acquire),
                generation: binding.generation,
                header_dirty: changes > self.synced.load(Ordering::Acquire),
            },
            changes,
        )
    }

    fn synced_through(&self, changes: u64) {
        self.synced.fetch_max(changes, Ordering::AcqRel);
    }

    fn is_live(&self, generation: u64) -> bool {
        let binding = self.binding.load();
        binding.path.is_some() && binding.generation == generation
    }
}

/// A pool of pre-opened OPFS sync access handles.
pub(super) struct SahPool {
    mount: MountId,
    /// Every slot, by index. Grows only by `adopt_slots`.
    slots: Atom<Vec<Arc<PoolSlot>>>,
    by_path: HashMap<PathBuf, usize>,
    free: SegQueue<usize>,
    free_count: AtomicUsize,
    dirs: HashMap<PathBuf, ()>,
    /// One counter for the whole pool, so the newest path assignment
    /// always carries the highest generation. Mount uses that to settle a
    /// rename that a crash interrupted.
    next_generation: AtomicU64,
}

impl std::fmt::Debug for SahPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SahPool")
            .field("slots", &self.slot_count())
            .field("free", &self.free_slots())
            .field("files", &self.by_path.len())
            .finish()
    }
}

impl Drop for SahPool {
    fn drop(&mut self) {
        sah::release_mount(self.mount);
    }
}

fn stale() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "the file behind this handle was removed or renamed",
    )
}

impl SahPool {
    /// Build a pool from the headers read at mount.
    pub(super) fn new(mount: MountId, headers: Vec<Option<SlotHeader>>) -> Self {
        let pool = Self {
            mount,
            slots: Atom::new(Vec::with_capacity(headers.len())),
            by_path: HashMap::with_capacity(BUCKETS),
            free: SegQueue::new(),
            free_count: AtomicUsize::new(0),
            dirs: HashMap::with_capacity(BUCKETS),
            next_generation: AtomicU64::new(1),
        };
        pool.install(headers);
        pool
    }

    /// Take slots opened after mount into the pool, keeping whatever
    /// logical files their headers already claim.
    pub(super) fn adopt_slots(&self, headers: Vec<Option<SlotHeader>>) {
        self.install(headers);
    }

    /// The handle-registry mount this pool draws its slots from.
    pub(super) fn mount_id(&self) -> MountId {
        self.mount
    }

    /// Slots with no logical file assigned.
    pub(super) fn free_slots(&self) -> usize {
        self.free_count.load(Ordering::Acquire)
    }

    /// Total slots in the pool.
    pub(super) fn slot_count(&self) -> usize {
        self.slots.load().len()
    }

    fn slot(&self, index: usize) -> io::Result<Arc<PoolSlot>> {
        self.slots.load().get(index).cloned().ok_or_else(stale)
    }

    /// A slot whose binding is still `generation`, or the stale error.
    fn live(&self, index: usize, generation: u64) -> io::Result<Arc<PoolSlot>> {
        let slot = self.slot(index)?;
        if slot.is_live(generation) {
            Ok(slot)
        } else {
            Err(stale())
        }
    }

    fn next_generation(&self) -> u64 {
        self.next_generation.fetch_add(1, Ordering::AcqRel)
    }

    fn push_free(&self, index: usize, slot: &PoolSlot) {
        if !slot.free.swap(true, Ordering::AcqRel) {
            self.free.push(index);
            self.free_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn pop_free(&self) -> Option<(usize, Arc<PoolSlot>)> {
        while let Some(index) = self.free.pop() {
            let Ok(slot) = self.slot(index) else {
                continue;
            };
            if slot.free.swap(false, Ordering::AcqRel) {
                self.free_count.fetch_sub(1, Ordering::AcqRel);
                return Some((index, slot));
            }
        }
        None
    }

    pub(super) fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.dirs.insert(path.to_path_buf(), ());
        register_ancestors(&self.dirs, path);
        Ok(())
    }

    pub(super) fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        if !self.dirs.contains_key(path) {
            return Err(not_found(path));
        }
        Ok(flat_children(path, self.by_path.keys(), self.dirs.keys()))
    }

    pub(super) fn exists(&self, path: &Path) -> bool {
        self.by_path.contains_key(path) || self.dirs.contains_key(path)
    }

    pub(super) fn metadata(&self, path: &Path) -> io::Result<(u64, bool)> {
        if let Some(index) = self.by_path.get(path) {
            return Ok((self.slot(index)?.len.load(Ordering::Acquire), false));
        }
        if self.dirs.contains_key(path) {
            return Ok((0, true));
        }
        Err(not_found(path))
    }

    /// Resolve a path for reading, returning the slot and the generation
    /// a later read must still match.
    pub(super) fn open_read(&self, path: &Path) -> io::Result<(usize, u64)> {
        let index = self.by_path.get(path).ok_or_else(|| not_found(path))?;
        let generation = self.slot(index)?.binding.load().generation;
        Ok((index, generation))
    }

    /// Resolve or create a path for writing, returning the slot, its
    /// generation, and the current end of the file.
    ///
    /// `keep` preserves the existing bytes. Only [`WriteMode::Truncate`]
    /// passes `false`; `Append` and `Update` both keep what is there and
    /// differ only in where the caller's first write lands, which the
    /// caller decides.
    ///
    /// [`WriteMode::Truncate`]: crate::env::WriteMode::Truncate
    pub(super) fn open_write(&self, path: &Path, keep: bool) -> io::Result<(usize, u64, u64)> {
        sah::check_path_fits(path)?;

        if let Some(index) = self.by_path.get(path) {
            let slot = self.slot(index)?;
            let generation = slot.binding.load().generation;
            if keep {
                return Ok((index, generation, slot.len.load(Ordering::Acquire)));
            }
            sah::truncate_slot(self.mount, index, 0)?;
            slot.len.store(0, Ordering::Release);
            slot.dirty();
            return Ok((index, generation, 0));
        }

        let Some((index, slot)) = self.pop_free() else {
            return Err(io::Error::other(format!(
                "OPFS handle pool is full ({} slots, all assigned); call \
                 OpfsEnv::grow_pool from an async context to add more",
                self.slot_count()
            )));
        };
        let generation = self.next_generation();
        if let Err(e) = sah::truncate_slot(self.mount, index, 0) {
            self.push_free(index, &slot);
            return Err(e);
        }
        slot.binding.store(Binding {
            path: Some(path.to_path_buf()),
            generation,
        });
        slot.len.store(0, Ordering::Release);
        slot.dirty();
        self.by_path.insert(path.to_path_buf(), index);
        register_ancestors(&self.dirs, path);
        Ok((index, generation, 0))
    }

    pub(super) fn read_at(
        &self,
        index: usize,
        generation: u64,
        at: u64,
        buf: &mut [u8],
    ) -> io::Result<usize> {
        let len = self.live(index, generation)?.len.load(Ordering::Acquire);
        if at >= len {
            return Ok(0);
        }
        let want = buf.len().min((len - at) as usize);
        sah::read_slot(self.mount, index, at, &mut buf[..want])
    }

    pub(super) fn write_at(
        &self,
        index: usize,
        generation: u64,
        at: u64,
        buf: &[u8],
    ) -> io::Result<()> {
        let slot = self.live(index, generation)?;
        // Contents first, header on sync: a crash in between loses the
        // tail, which WAL replay and orphan SSTables already tolerate.
        sah::write_slot(self.mount, index, at, buf)?;
        if !slot.is_live(generation) {
            return Err(stale());
        }
        slot.len.fetch_max(at + buf.len() as u64, Ordering::AcqRel);
        slot.dirty();
        Ok(())
    }

    pub(super) fn file_len(&self, index: usize, generation: u64) -> io::Result<u64> {
        Ok(self.live(index, generation)?.len.load(Ordering::Acquire))
    }

    pub(super) fn set_len(&self, index: usize, generation: u64, len: u64) -> io::Result<()> {
        let slot = self.live(index, generation)?;
        sah::truncate_slot(self.mount, index, len)?;
        if !slot.is_live(generation) {
            return Err(stale());
        }
        slot.len.store(len, Ordering::Release);
        slot.dirty();
        Ok(())
    }

    /// Make a slot's contents and its name binding durable.
    pub(super) fn sync(&self, index: usize, generation: u64) -> io::Result<()> {
        let slot = self.live(index, generation)?;
        let (snapshot, changes) = slot.snapshot();
        if !snapshot.header_dirty {
            return sah::flush_slot(self.mount, index);
        }
        sah::sync_slot(self.mount, index, &snapshot)?;
        if slot.is_live(generation) {
            slot.synced_through(changes);
        }
        Ok(())
    }

    /// Make the name bindings of a directory's files durable.
    ///
    /// OPFS has no directory object to fsync; the binding from a logical
    /// path to a slot lives in that slot's header, so flushing the dirty
    /// headers under `path` is the exact equivalent.
    pub(super) fn sync_dir(&self, path: &Path) -> io::Result<()> {
        for (file, index) in self.by_path.iter() {
            if file.parent() != Some(path) {
                continue;
            }
            let slot = self.slot(index)?;
            let (snapshot, changes) = slot.snapshot();
            if !snapshot.header_dirty {
                continue;
            }
            sah::sync_slot(self.mount, index, &snapshot)?;
            if slot.binding.load().generation == snapshot.generation {
                slot.synced_through(changes);
            }
        }
        Ok(())
    }

    pub(super) fn remove_file(&self, path: &Path) -> io::Result<()> {
        let index = self.by_path.remove(path).ok_or_else(|| not_found(path))?;
        self.release_slot(index)
    }

    /// Rebind `from`'s slot to `to`, then release whatever held `to`.
    ///
    /// The new binding is made durable before the old one is dropped, so
    /// a crash in the window leaves two slots claiming `to` and mount
    /// keeps the one with the higher generation. That is what makes
    /// rename atomic on a filesystem with no atomic rename. In memory the
    /// slot is published at `to` before `from` lets it go, so a concurrent
    /// open always finds it under one of the two names.
    pub(super) fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        sah::check_path_fits(to)?;
        if from == to {
            return self
                .by_path
                .get(from)
                .map(|_| ())
                .ok_or_else(|| not_found(from));
        }
        let index = self.by_path.get(from).ok_or_else(|| not_found(from))?;
        let slot = self.slot(index)?;

        let previous = (*slot.binding.load()).clone();
        slot.binding.store(Binding {
            path: Some(to.to_path_buf()),
            generation: self.next_generation(),
        });
        slot.dirty();
        let (snapshot, changes) = slot.snapshot();
        if let Err(e) = sah::sync_slot(self.mount, index, &snapshot) {
            slot.binding.store(previous);
            return Err(e);
        }
        slot.synced_through(changes);

        let replaced = self.by_path.insert(to.to_path_buf(), index);
        // Only while `from` still names this slot: a removal of `from`
        // that came first already let the name go.
        self.by_path.remove_if(from, |held| *held == index);
        if let Some(replaced) = replaced {
            self.release_slot(replaced)?;
        }
        register_ancestors(&self.dirs, to);
        Ok(())
    }

    /// Append `headers` as new slots.
    ///
    /// A slot whose header is absent, torn, or marked free joins the free
    /// list. When two slots claim one logical path - the window a crash
    /// during rename leaves open - the higher generation wins and the loser
    /// is released.
    fn install(&self, headers: Vec<Option<SlotHeader>>) {
        let mut slots = (*self.slots.load()).clone();
        let mut free = Vec::new();
        let mut superseded = Vec::new();

        for header in headers {
            let index = slots.len();
            let claimed = match header {
                Some(header) => {
                    self.next_generation
                        .fetch_max(header.generation + 1, Ordering::AcqRel);
                    (header.in_use && !header.path.is_empty()).then_some(header)
                }
                None => None,
            };

            match claimed {
                Some(header) => {
                    let path = PathBuf::from(&header.path);
                    slots.push(PoolSlot::new(
                        Some(path.clone()),
                        header.generation,
                        header.len,
                    ));
                    match self.by_path.get(&path) {
                        Some(rival) if slot_generation(&slots, rival) >= header.generation => {
                            superseded.push(index);
                        }
                        Some(rival) => {
                            superseded.push(rival);
                            self.by_path.insert(path.clone(), index);
                        }
                        None => {
                            self.by_path.insert(path.clone(), index);
                        }
                    }
                    register_ancestors(&self.dirs, &path);
                }
                None => {
                    slots.push(PoolSlot::new(None, 0, 0));
                    free.push(index);
                }
            }
        }
        // Published before any slot joins the free list, so a slot popped
        // from it is always found.
        self.slots.store(slots);
        for index in free {
            if let Ok(slot) = self.slot(index) {
                self.push_free(index, &slot);
            }
        }

        for index in superseded {
            // A failure here only means the duplicate survives to the next
            // mount, which resolves it the same deterministic way.
            if let Err(e) = self.release_slot(index) {
                tracing::warn!(slot = index, error = %e, "could not release a superseded OPFS slot");
            }
        }
    }

    /// Mark a slot free on disk and in memory, and give its bytes back to the
    /// origin's quota.
    fn release_slot(&self, index: usize) -> io::Result<()> {
        let Ok(slot) = self.slot(index) else {
            return Ok(());
        };
        slot.binding.store(Binding {
            path: None,
            generation: self.next_generation(),
        });
        slot.len.store(0, Ordering::Release);
        let (mut snapshot, changes) = slot.snapshot();
        snapshot.header_dirty = false;

        sah::sync_slot(self.mount, index, &snapshot)?;
        slot.synced_through(changes);
        sah::truncate_slot(self.mount, index, 0)?;
        self.push_free(index, &slot);
        Ok(())
    }
}

fn slot_generation(slots: &[Arc<PoolSlot>], index: usize) -> u64 {
    slots
        .get(index)
        .map_or(0, |slot| slot.binding.load().generation)
}
