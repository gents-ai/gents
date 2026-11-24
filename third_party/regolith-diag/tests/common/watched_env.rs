//! An `Env` that watches what a database does to its host: the threads it
//! asks for, the threads that reach its files, and every write and sync of
//! its write-ahead log, which it can also hold at the device.
//!
//! What the tests pin with it are counts and thread identities. Both are the
//! same whatever the host's load, where a time is not.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, ThreadId};
use std::time::Instant;

use regolith::env::{
    Capabilities, DiskSpace, Env, FileLock, FileMeta, JoinHandle, LogFile, ReadDir, ReadFile,
    StdEnv, WriteFile, WriteMode,
};

use super::STUCK;

/// What a [`WatchedEnv`] saw.
#[derive(Default)]
pub struct Watch {
    /// The names of the threads the database started through the env, in
    /// the order it asked.
    started: Mutex<Vec<String>>,
    /// The threads that ran a body the env started.
    workers: Mutex<HashSet<ThreadId>>,
    /// Every thread that called into the env or wrote or synced a log.
    callers: Mutex<HashSet<ThreadId>>,
    /// Writes to a write-ahead log, by the thread that made them.
    log_writes: Mutex<HashMap<ThreadId, usize>>,
    /// Syncs of a write-ahead log that reached the device.
    log_syncs: AtomicUsize,
    /// Directory syncs.
    dir_syncs: AtomicUsize,
    /// Set by [`Watch::hold_syncs`]: a log sync waits at the device until
    /// [`Watch::release_syncs`].
    holding: AtomicBool,
    /// Log syncs waiting at the gate.
    held: Mutex<usize>,
    changed: Condvar,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("started", &self.started())
            .field("log_writes", &self.log_writes())
            .field("log_syncs", &self.log_syncs())
            .finish_non_exhaustive()
    }
}

impl Watch {
    /// The names of the threads the database started, in order.
    pub fn started(&self) -> Vec<String> {
        self.started.lock().unwrap().clone()
    }

    /// The threads that reached the env and are neither one it started nor
    /// one of `mine`: threads the database made some other way.
    pub fn strangers(&self, mine: &[ThreadId]) -> Vec<ThreadId> {
        let workers = self.workers.lock().unwrap();
        self.callers
            .lock()
            .unwrap()
            .iter()
            .filter(|caller| !workers.contains(caller) && !mine.contains(caller))
            .copied()
            .collect()
    }

    /// Writes to a write-ahead log, by any thread.
    pub fn log_writes(&self) -> usize {
        self.log_writes.lock().unwrap().values().sum()
    }

    /// Writes to a write-ahead log made by the calling thread.
    pub fn log_writes_here(&self) -> usize {
        self.log_writes
            .lock()
            .unwrap()
            .get(&thread::current().id())
            .copied()
            .unwrap_or(0)
    }

    /// Syncs of a write-ahead log that reached the device.
    pub fn log_syncs(&self) -> usize {
        self.log_syncs.load(Ordering::SeqCst)
    }

    pub fn dir_syncs(&self) -> usize {
        self.dir_syncs.load(Ordering::SeqCst)
    }

    /// Make every log sync wait at the device from now on.
    pub fn hold_syncs(&self) {
        self.holding.store(true, Ordering::SeqCst);
    }

    /// Let every held sync and every later one through.
    pub fn release_syncs(&self) {
        self.holding.store(false, Ordering::SeqCst);
        let _held = self.held.lock().unwrap();
        self.changed.notify_all();
    }

    /// Wait until `n` log syncs are held at the device, failing the test when
    /// they are not by [`STUCK`].
    pub fn wait_held(&self, n: usize) {
        let deadline = Instant::now() + STUCK;
        let mut held = self.held.lock().unwrap();
        while *held < n {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "{n} log syncs were not at the device in {STUCK:?}: {}",
                *held
            );
            held = self.changed.wait_timeout(held, left).unwrap().0;
        }
    }

    fn note_caller(&self) {
        self.callers.lock().unwrap().insert(thread::current().id());
    }

    fn gate(&self) {
        if !self.holding.load(Ordering::SeqCst) {
            return;
        }
        let mut held = self.held.lock().unwrap();
        *held += 1;
        self.changed.notify_all();
        while self.holding.load(Ordering::SeqCst) {
            held = self.changed.wait(held).unwrap();
        }
        *held -= 1;
    }
}

/// An [`Env`] that forwards everything to the one it wraps and tells a
/// shared [`Watch`] about it.
#[derive(Debug)]
pub struct WatchedEnv {
    inner: Arc<dyn Env>,
    watch: Arc<Watch>,
}

impl WatchedEnv {
    /// Over the host's files and threads.
    pub fn on_host() -> (Arc<dyn Env>, Arc<Watch>) {
        Self::over(Arc::new(StdEnv))
    }

    /// Over `inner`.
    pub fn over(inner: Arc<dyn Env>) -> (Arc<dyn Env>, Arc<Watch>) {
        let watch = Arc::new(Watch::default());
        (
            Arc::new(Self {
                inner,
                watch: Arc::clone(&watch),
            }),
            watch,
        )
    }
}

struct WatchedLog {
    inner: Box<dyn LogFile>,
    watch: Arc<Watch>,
}

impl LogFile for WatchedLog {
    fn write_all_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.watch.note_caller();
        *self
            .watch
            .log_writes
            .lock()
            .unwrap()
            .entry(thread::current().id())
            .or_default() += 1;
        self.inner.write_all_at(offset, buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.watch.note_caller();
        self.watch.gate();
        self.watch.log_syncs.fetch_add(1, Ordering::SeqCst);
        self.inner.sync_data()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.watch.note_caller();
        self.inner.set_len(len)
    }

    fn start_writeback(&self, offset: u64, len: u64) {
        self.watch.note_caller();
        self.inner.start_writeback(offset, len);
    }
}

impl Env for WatchedEnv {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.watch.note_caller();
        self.inner.create_dir_all(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.watch.note_caller();
        self.inner.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.watch.note_caller();
        self.inner.open_read(path)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.watch.note_caller();
        self.inner.open_write(path, mode)
    }
    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        self.watch.note_caller();
        Ok(Box::new(WatchedLog {
            inner: self.inner.open_log(path, mode)?,
            watch: Arc::clone(&self.watch),
        }))
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.watch.note_caller();
        self.inner.metadata(path)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.watch.note_caller();
        self.inner.remove_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.watch.note_caller();
        self.inner.rename(from, to)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.watch.note_caller();
        self.inner.hard_link(src, dst)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.watch.note_caller();
        self.watch.dir_syncs.fetch_add(1, Ordering::SeqCst);
        self.inner.sync_dir(path)
    }
    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
        self.watch.note_caller();
        self.inner.lock_file(path, exclusive)
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn now_micros(&self) -> Option<u64> {
        self.inner.now_micros()
    }
    fn unix_secs(&self) -> Option<u64> {
        self.inner.unix_secs()
    }
    fn spawn(
        &self,
        name: &str,
        body: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        self.watch.note_caller();
        let watch = Arc::clone(&self.watch);
        let handle = self.inner.spawn(
            name,
            Box::new(move || {
                watch.workers.lock().unwrap().insert(thread::current().id());
                body();
            }),
        )?;
        self.watch.started.lock().unwrap().push(name.to_owned());
        Ok(handle)
    }
    fn disk_space(&self, path: &Path) -> io::Result<Option<DiskSpace>> {
        self.watch.note_caller();
        self.inner.disk_space(path)
    }
    fn drop_page_cache(&self, path: &Path) {
        self.watch.note_caller();
        self.inner.drop_page_cache(path);
    }
}
