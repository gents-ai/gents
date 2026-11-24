//! Bound how many SSTables hold an open file descriptor.
//!
//! A reader keeps its table's file open for as long as the table is
//! live, so a store needs one descriptor per live table. A large store,
//! or a process started with a low `RLIMIT_NOFILE` (256 under macOS
//! launchd), runs out, and every compaction then fails with EMFILE.
//!
//! [`OpenFileLimit`] wraps an [`Env`] and hands out SSTable files whose
//! descriptors live in a table of `capacity` slots ([`slots`]): it never
//! holds more than `capacity` descriptors, a read takes one by a single
//! compare-and-swap with no lock, and a descriptor is closed only when no
//! read is using it. Eviction is CLOCK: a read sets the slot's reference
//! bit, and the sweep that needs a slot skips a set bit (clearing it) and
//! takes the first slot that has none and no reader. Only SSTables are
//! managed; every other file passes through.
//!
//! A table's descriptor is reopened inside a device read, so a `CacheOnly`
//! handle never reopens one: its miss goes to its I/O queue as a unit, and
//! the unit's read, run by the polling thread, does the reopen. That reopen
//! never waits (D60): when every slot is in use by running reads, the unit
//! parks on its queue, and the read that frees a slot tells the queue. Only
//! a `Blocking` read, which chose to block, waits for one slot's running
//! reads to finish.
//!
//! # Removed tables
//!
//! Compaction removes a table as soon as no current version names it, and
//! older snapshots and iterators keep reading it. A closed descriptor could
//! not be reopened after an unlink, so [`Env::remove_file`] on a table that
//! still has a live handle renames the file aside instead, to a name
//! [`is_removed_table`] recognizes, and the last handle to go unlinks it.
//! Every descriptor therefore stays closable, which is what lets the bound
//! hold: there is no descriptor a removed table needs pinned open. A crash
//! leaves the renamed file behind; the next writable open sweeps it with the
//! other unreferenced tables (`engine::orphan_sweep`). A rename of a table
//! with live handles moves those handles to the new name the same way.

#[cfg(test)]
mod forward_tests;
#[cfg(loom)]
pub mod loom_model;
#[cfg(test)]
mod move_tests;
#[cfg(test)]
mod park_tests;
pub(crate) mod slots;
#[cfg(test)]
mod tests;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kovan::Atom;
use kovan_map::HashMap;

use super::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, WriteFile, WriteMode,
};
use crate::engine::io::scope;
use crate::portability::{AtomicU64, AtomicUsize, Ordering};
use slots::{Held, SlotTable};

/// Buckets the map of live tables starts with; it grows on demand.
const ENTRY_BUCKETS: usize = 256;

/// The suffix a removed table is renamed to while handles still read it.
const REMOVED_SUFFIX: &str = ".removed-";

/// Record ids, unique for the life of the process. Never 0, which marks a
/// slot nobody owns.
static NEXT_RECORD: AtomicU64 = AtomicU64::new(1);
/// Makes every removed table's new name unique within the process.
static NEXT_REMOVED: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(crate) struct OpenFileLimit {
    shared: Arc<Shared>,
}

struct Shared {
    inner: Arc<dyn Env>,
    slots: SlotTable,
    /// Every table path with live handles, keyed by where its file is now.
    entries: HashMap<PathBuf, Arc<PathEntry>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenFileLimit")
            .field("capacity", &self.slots.capacity())
            .field("open", &self.slots.open_count())
            .finish_non_exhaustive()
    }
}

/// `count` of the live references to a path entry: its handles, plus a
/// removal or rename in flight.
const COUNT: u64 = (1 << 62) - 1;
/// A removal or rename is moving the file; it holds one reference.
const MOVING: u64 = 1 << 62;
/// The file was renamed aside by a removal; the last reference unlinks it.
const DOOMED: u64 = 1 << 63;

/// One table file with live handles: where it is, and who reads it.
struct PathEntry {
    names: Atom<Names>,
    live: AtomicU64,
}

/// Where a table file is. While a removal or rename moves it, `previous`
/// holds the old name: a reopen tries the old name first and then the new
/// one, and the rename moves the file from one to the other atomically, so
/// one of the two opens finds it.
#[derive(Clone)]
struct Names {
    current: PathBuf,
    previous: Option<PathBuf>,
}

impl PathEntry {
    fn new(path: PathBuf) -> Self {
        Self {
            names: Atom::new(Names {
                current: path,
                previous: None,
            }),
            live: AtomicU64::new(1),
        }
    }

    /// Take a reference for a new handle, unless the entry is dying (no
    /// reference left) or its file was removed.
    fn acquire(&self) -> bool {
        let mut live = self.live.load(Ordering::Acquire);
        loop {
            if live & COUNT == 0 || live & DOOMED != 0 {
                return false;
            }
            match self.live.compare_exchange_weak(
                live,
                live + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(now) => live = now,
            }
        }
    }

    /// Claim the right to move the file, holding a reference meanwhile so
    /// the last handle cannot go before the move finishes.
    fn begin_move(&self) -> Move {
        let mut live = self.live.load(Ordering::Acquire);
        loop {
            if live & (MOVING | DOOMED) != 0 {
                return Move::Busy;
            }
            if live & COUNT == 0 {
                return Move::NoHandles;
            }
            match self.live.compare_exchange_weak(
                live,
                (live + 1) | MOVING,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Move::Started,
                Err(now) => live = now,
            }
        }
    }

    /// Open the file wherever it is now. A whole move can finish between
    /// loading the names and opening (the names said `from`, and the file is
    /// already at `to`); a NotFound then loads the names again and, if a move
    /// changed them, tries the new ones. Each retry needs a move to have
    /// published new names, and a table moves only a bounded number of times
    /// (a removal moves it once, for good), so this never spins.
    fn reopen(&self, inner: &dyn Env) -> io::Result<Arc<dyn ReadFile>> {
        let mut names = self.names.load();
        loop {
            if let Some(previous) = &names.previous {
                match inner.open_read(previous) {
                    Ok(file) => return Ok(Arc::from(file)),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            match inner.open_read(&names.current) {
                Ok(file) => return Ok(Arc::from(file)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    let now = self.names.load();
                    // The guard on the names first loaded keeps them alive,
                    // so a different address is a newer store, never a reuse.
                    if std::ptr::eq(&*now, &*names) {
                        return Err(e);
                    }
                    names = now;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

enum Move {
    Started,
    NoHandles,
    Busy,
}

fn is_table(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "sst")
}

/// Whether `path` is a table [`OpenFileLimit`] renamed aside on removal.
/// Such a file is garbage once its handles are gone, so a writable open
/// removes any it finds.
pub(crate) fn is_removed_table(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.split_once(REMOVED_SUFFIX))
        .is_some_and(|(table, n)| {
            is_table(Path::new(table)) && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())
        })
}

fn removed_name(path: &Path) -> PathBuf {
    let n = NEXT_REMOVED.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!("{REMOVED_SUFFIX}{n}"));
    path.with_file_name(name)
}

impl OpenFileLimit {
    pub(crate) fn new(inner: Arc<dyn Env>, capacity: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                inner,
                slots: SlotTable::new(capacity),
                entries: HashMap::with_capacity(ENTRY_BUCKETS),
            }),
        }
    }

    /// SSTable descriptors currently open through this wrapper.
    #[cfg(test)]
    fn open_count(&self) -> usize {
        self.shared.slots.open_count()
    }
}

impl Shared {
    /// The entry for a table being opened at `path`, with a reference taken.
    fn acquire_entry(&self, path: &Path) -> Arc<PathEntry> {
        loop {
            if let Some(entry) = self.entries.get(path) {
                if entry.acquire() {
                    return entry;
                }
                // Dying or removed: a new file at this path is a new entry.
                self.entries
                    .remove_if(path, |held| Arc::ptr_eq(held, &entry));
                continue;
            }
            let fresh = Arc::new(PathEntry::new(path.to_path_buf()));
            if self
                .entries
                .insert_if_absent(path.to_path_buf(), Arc::clone(&fresh))
                .is_none()
            {
                return fresh;
            }
        }
    }

    /// Give back a reference. The last one unmaps the entry, or unlinks the
    /// file a removal renamed aside.
    fn release_entry(&self, entry: &Arc<PathEntry>) {
        let before = entry.live.fetch_sub(1, Ordering::AcqRel);
        if before & COUNT != 1 {
            return;
        }
        let names = entry.names.load();
        if before & DOOMED != 0 {
            if let Err(e) = self.inner.remove_file(&names.current)
                && e.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(
                    path = %names.current.display(),
                    error = %e,
                    "could not remove a table after its last reader closed; the next open removes it"
                );
            }
        } else {
            self.entries
                .remove_if(&names.current, |held| Arc::ptr_eq(held, entry));
        }
    }

    /// End a move begun with [`PathEntry::begin_move`]: clear `MOVING`, set
    /// `DOOMED` when the file now sits under a removed name, and give back
    /// the mover's reference, unlinking the file if it was the last.
    fn end_move(&self, entry: &Arc<PathEntry>, doomed: bool) -> io::Result<()> {
        let mut live = entry.live.load(Ordering::Acquire);
        loop {
            let next = ((live & COUNT) - 1) | if doomed { DOOMED } else { 0 };
            match entry
                .live
                .compare_exchange_weak(live, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(now) => live = now,
            }
        }
        if live & COUNT != 1 {
            return Ok(());
        }
        let names = entry.names.load();
        if doomed {
            // Every handle went while the file moved: nobody reads it.
            self.inner.remove_file(&names.current)
        } else {
            self.entries
                .remove_if(&names.current, |held| Arc::ptr_eq(held, entry));
            Ok(())
        }
    }

    /// Move a file with live handles from `from` to `to`, keeping every
    /// handle able to reopen it at each instant of the move.
    fn relocate(&self, entry: &PathEntry, from: &Path, to: &Path) -> io::Result<()> {
        entry.names.store(Names {
            current: to.to_path_buf(),
            previous: Some(from.to_path_buf()),
        });
        let moved = self.inner.rename(from, to);
        let settled = if moved.is_ok() { to } else { from };
        entry.names.store(Names {
            current: settled.to_path_buf(),
            previous: None,
        });
        moved
    }

    /// Take the table file at `path` out of the namespace for its live
    /// handles: rename it aside and doom it, so the last handle unlinks it.
    /// `Ok(true)` when it did; `Ok(false)` when `path` has no live handle and
    /// the caller should act on the file directly.
    fn retire_path(&self, path: &Path) -> io::Result<bool> {
        let Some(entry) = self.entries.get(path) else {
            return Ok(false);
        };
        match entry.begin_move() {
            Move::Started => {}
            Move::NoHandles => {
                self.entries
                    .remove_if(path, |held| Arc::ptr_eq(held, &entry));
                return Ok(false);
            }
            // Another removal or rename is moving this file right now: for
            // this caller the path is already gone.
            Move::Busy => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} is being removed", path.display()),
                ));
            }
        }
        let grave = removed_name(path);
        if let Err(e) = self.relocate(&entry, path, &grave) {
            self.end_move(&entry, false)?;
            return Err(e);
        }
        self.entries
            .remove_if(path, |held| Arc::ptr_eq(held, &entry));
        self.end_move(&entry, true)?;
        Ok(true)
    }

    /// A handle's descriptor, held for one read: the slot it was last seen
    /// in, or a slot taken now and the file reopened into it.
    ///
    /// A `Blocking` read, which chose to block, may wait for the reads
    /// running in one slot when every slot has some (the drain). A unit a
    /// `CacheOnly` read queued never waits (D60): finding no slot, it parks
    /// on the queue running it and fails this read, and the unit runs again
    /// once a slot frees.
    fn hold(&self, record: &Record) -> io::Result<Held<'_>> {
        if let Some(held) = self
            .slots
            .join(record.hint.load(Ordering::Relaxed), record.id)
        {
            return Ok(held);
        }
        let open = || record.entry.reopen(&*self.inner);
        let held = match scope::reopen_waiter() {
            None => self.slots.load(record.id, open)?,
            Some(waiter) => match self.slots.load_or_park(record.id, waiter, open)? {
                Some(held) => held,
                None => {
                    scope::note_parked();
                    return Err(io::Error::new(
                        io::ErrorKind::ResourceBusy,
                        "every open-file slot is in use; the read runs again once one frees",
                    ));
                }
            },
        };
        record.hint.store(held.index(), Ordering::Relaxed);
        Ok(held)
    }
}

/// One open handle on a table: its own descriptor identity and its file.
struct Record {
    id: u64,
    len: u64,
    entry: Arc<PathEntry>,
    /// The slot this handle's descriptor was last seen in.
    hint: AtomicUsize,
    shared: Arc<Shared>,
}

impl Drop for Record {
    fn drop(&mut self) {
        self.shared.slots.release_owner(self.id);
        self.shared.release_entry(&self.entry);
    }
}

/// The handle an SSTable reader holds in place of its file.
struct LimitedFile(Record);

impl ReadFile for LimitedFile {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let held = self.0.shared.hold(&self.0)?;
        held.file().read_exact_at(offset, buf)
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.0.len)
    }
}

impl Env for OpenFileLimit {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        if !is_table(path) {
            return self.shared.inner.open_read(path);
        }
        let mut record = Record {
            id: NEXT_RECORD.fetch_add(1, Ordering::Relaxed),
            len: 0,
            entry: self.shared.acquire_entry(path),
            hint: AtomicUsize::new(usize::MAX),
            shared: Arc::clone(&self.shared),
        };
        // Opened now, so an open error surfaces here, and the reader's first
        // reads (the footer, the index) find the descriptor in its slot.
        let held = self.shared.hold(&record)?;
        record.len = held.file().len()?;
        drop(held);
        Ok(Box::new(LimitedFile(record)))
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        if self.shared.retire_path(path)? {
            return Ok(());
        }
        self.shared.inner.remove_file(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        // A rename from nowhere changes nothing, `to` included. Checked
        // first, because retiring `to` below cannot be undone; only a
        // removal of `from` racing this rename gets past it.
        self.shared.inner.metadata(from)?;
        // A rename onto itself is a no-op, as POSIX makes it.
        if from == to {
            return Ok(());
        }
        // A file already at `to` that handles still read keeps its content
        // for them, as an unlink would leave it to its open descriptors.
        self.shared.retire_path(to)?;
        let Some(entry) = self.shared.entries.get(from) else {
            return self.shared.inner.rename(from, to);
        };
        match entry.begin_move() {
            Move::Started => {}
            Move::NoHandles => {
                self.shared
                    .entries
                    .remove_if(from, |held| Arc::ptr_eq(held, &entry));
                return self.shared.inner.rename(from, to);
            }
            Move::Busy => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} is being removed", from.display()),
                ));
            }
        }
        if let Err(e) = self.shared.relocate(&entry, from, to) {
            self.shared.end_move(&entry, false)?;
            return Err(e);
        }
        self.shared
            .entries
            .remove_if(from, |held| Arc::ptr_eq(held, &entry));
        self.shared
            .entries
            .insert(to.to_path_buf(), Arc::clone(&entry));
        self.shared.end_move(&entry, false)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.shared.inner.create_dir_all(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.shared.inner.read_dir(path)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.shared.inner.open_write(path, mode)
    }
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn super::LogFile>> {
        self.shared.inner.open_log(path, mode)
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.shared.inner.metadata(path)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.shared.inner.hard_link(src, dst)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.shared.inner.sync_dir(path)
    }
    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        self.shared.inner.lock_file(path, exclusive)
    }
    fn capabilities(&self) -> Capabilities {
        self.shared.inner.capabilities()
    }
    fn now_micros(&self) -> Option<u64> {
        self.shared.inner.now_micros()
    }
    fn unix_secs(&self) -> Option<u64> {
        self.shared.inner.unix_secs()
    }
    fn spawn(
        &self,
        name: &str,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        self.shared.inner.spawn(name, body)
    }
    fn drop_page_cache(&self, path: &Path) {
        self.shared.inner.drop_page_cache(path)
    }
    // The methods `Env` gives a default body go to the environment too, so
    // what it wrote for one of them is what runs.
    fn disk_space(&self, path: &Path) -> io::Result<Option<super::DiskSpace>> {
        self.shared.inner.disk_space(path)
    }
    fn exists(&self, path: &Path) -> bool {
        self.shared.inner.exists(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.shared.inner.is_dir(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        if is_table(path) {
            // Through this environment's own handle, so the read counts
            // against the bound like any other read of a table.
            return super::read_whole(&*self.open_read(path)?, path);
        }
        self.shared.inner.read(path)
    }
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.shared.inner.write(path, data)
    }
}
