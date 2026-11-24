//! An `Env` that counts every open and read of an SSTable, and can hold
//! the reads at the device until the test lets them go.
//!
//! The non-blocking read tests pin what touches the device by these
//! counts: a count is deterministic whatever the host's load, where a time
//! is not.

#![allow(dead_code)]

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use regolith::env::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, StdEnv, WriteFile,
    WriteMode,
};

/// What reached the device, and the gate that holds reads there.
#[derive(Default)]
pub struct Device {
    /// SSTable opens, including the reopens `max_open_files` causes.
    pub opens: AtomicUsize,
    /// Positional reads of SSTables.
    pub reads: AtomicUsize,
    /// Set by [`Device::hold`]: every SSTable read waits until
    /// [`Device::release`].
    holding: AtomicBool,
    /// Reads waiting at the gate.
    held: Mutex<usize>,
    changed: Condvar,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("opens", &self.opens.load(Ordering::SeqCst))
            .field("reads", &self.reads.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl Device {
    /// Opens plus reads: anything at all that touched an SSTable.
    pub fn touches(&self) -> usize {
        self.opens.load(Ordering::SeqCst) + self.reads.load(Ordering::SeqCst)
    }

    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    pub fn opens(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }

    /// Make every SSTable read wait at the device from now on.
    pub fn hold(&self) {
        self.holding.store(true, Ordering::SeqCst);
    }

    /// Let every held read and every later one through.
    pub fn release(&self) {
        self.holding.store(false, Ordering::SeqCst);
        let _held = self.held.lock().unwrap();
        self.changed.notify_all();
    }

    /// Wait until `n` reads are held at the gate.
    pub fn wait_held(&self, n: usize) {
        let mut held = self.held.lock().unwrap();
        while *held < n {
            held = self.changed.wait(held).unwrap();
        }
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

/// [`StdEnv`] with its SSTable traffic counted by a shared [`Device`].
#[derive(Debug)]
pub struct DeviceEnv {
    inner: StdEnv,
    pub device: Arc<Device>,
}

impl DeviceEnv {
    pub fn new() -> (Arc<Self>, Arc<Device>) {
        let device = Arc::new(Device::default());
        (
            Arc::new(Self {
                inner: StdEnv,
                device: Arc::clone(&device),
            }),
            device,
        )
    }
}

/// A database written by `fill` through `options(env)`, closed and opened
/// again so its block cache is cold, with the device it reads counted.
///
/// Opening reads the first block of every table (the column-family registry
/// sorts first), so a test that wants a cold read reads a key past it.
pub fn cold_db(
    options: impl Fn(Arc<DeviceEnv>) -> regolith::Options,
    fill: impl FnOnce(&regolith::Db),
) -> (tempfile::TempDir, regolith::Db, Arc<Device>) {
    let dir = tempfile::TempDir::new().unwrap();
    let (env, device) = DeviceEnv::new();
    {
        let db = regolith::Db::open(dir.path(), options(Arc::clone(&env))).unwrap();
        fill(&db);
        db.flush().unwrap();
        db.close().unwrap();
    }
    let db = regolith::Db::open(dir.path(), options(env)).unwrap();
    (dir, db, device)
}

fn is_table(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "sst")
}

struct CountedFile {
    inner: Box<dyn ReadFile>,
    device: Arc<Device>,
}

impl ReadFile for CountedFile {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.device.gate();
        self.device.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read_exact_at(offset, buf)
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
}

impl Env for DeviceEnv {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        let inner = self.inner.open_read(path)?;
        if !is_table(path) {
            return Ok(inner);
        }
        self.device.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CountedFile {
            inner,
            device: Arc::clone(&self.device),
        }))
    }
    fn open_log(
        &self,
        path: &Path,
        mode: WriteMode,
    ) -> io::Result<Box<dyn regolith::env::LogFile>> {
        self.inner.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.inner.open_write(path, mode)
    }
    fn create_dir_all(&self, p: &Path) -> io::Result<()> {
        self.inner.create_dir_all(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(p)
    }
    fn metadata(&self, p: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(p)
    }
    fn remove_file(&self, p: &Path) -> io::Result<()> {
        self.inner.remove_file(p)
    }
    fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.inner.rename(a, b)
    }
    fn hard_link(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.inner.hard_link(a, b)
    }
    fn sync_dir(&self, p: &Path) -> io::Result<()> {
        self.inner.sync_dir(p)
    }
    fn lock_file(&self, p: &Path, ex: bool) -> io::Result<Box<dyn FileLock>> {
        self.inner.lock_file(p, ex)
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
        f: Box<dyn FnOnce() + Send + 'static>,
    ) -> io::Result<Box<dyn JoinHandle>> {
        self.inner.spawn(name, f)
    }
}
