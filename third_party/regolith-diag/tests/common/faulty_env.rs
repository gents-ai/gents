//! An `Env` that refuses chosen write-ahead-log removals, the way a file
//! another process holds open is refused on Windows, and, while armed,
//! refuses to create table files, the way a full disk does. Everything
//! else goes to [`StdEnv`].
//!
//! The flush tests use it to make a flush leave its log behind, and to make
//! the flush a writer owes fail so a later step is the one that flushes.

#![allow(dead_code)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use regolith::env::{
    Capabilities, Env, FileLock, FileMeta, JoinHandle, ReadDir, ReadFile, StdEnv, WriteFile,
    WriteMode,
};

/// Which log removals [`FaultyEnv`] refuses.
#[derive(Debug, Default, Clone)]
pub enum Refuse {
    #[default]
    Nothing,
    /// Every write-ahead log.
    EveryLog,
    /// This log only.
    Log(PathBuf),
}

/// Delegates to [`StdEnv`], refusing what it is armed for.
#[derive(Debug, Default)]
pub struct FaultyEnv {
    inner: StdEnv,
    refuse: Mutex<Refuse>,
    /// Log removals refused so far.
    pub refused: AtomicUsize,
    /// Set while table creation is refused.
    fail_tables: AtomicBool,
    /// Table creations refused so far.
    pub tables_refused: AtomicUsize,
}

impl FaultyEnv {
    /// Refuse the log removals `refuse` names from now on.
    pub fn arm(&self, refuse: Refuse) {
        *self.refuse.lock().unwrap() = refuse;
    }

    /// Refuse every table creation while `on`.
    pub fn fail_tables(&self, on: bool) {
        self.fail_tables.store(on, Ordering::SeqCst);
    }

    fn refuses(&self, path: &Path) -> bool {
        match &*self.refuse.lock().unwrap() {
            Refuse::Nothing => false,
            Refuse::EveryLog => is_log(path),
            Refuse::Log(log) => path == log,
        }
    }
}

/// Whether `path` names a write-ahead log.
pub fn is_log(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "log")
}

/// The write-ahead logs in the database at `db`, oldest first.
pub fn logs(db: &Path) -> Vec<PathBuf> {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(db.join("wal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| is_log(p))
        .collect();
    logs.sort();
    logs
}

impl Env for FaultyEnv {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_read(path)
    }
    fn open_log(
        &self,
        path: &Path,
        mode: WriteMode,
    ) -> io::Result<Box<dyn regolith::env::LogFile>> {
        self.inner.open_log(path, mode)
    }
    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        if self.fail_tables.load(Ordering::SeqCst) && path.extension().is_some_and(|e| e == "sst") {
            self.tables_refused.fetch_add(1, Ordering::SeqCst);
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "no room for a table",
            ));
        }
        self.inner.open_write(path, mode)
    }
    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(path)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        if self.refuses(path) {
            self.refused.fetch_add(1, Ordering::SeqCst);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the log is held open elsewhere",
            ));
        }
        self.inner.remove_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn hard_link(&self, src: &Path, dst: &Path) -> io::Result<()> {
        self.inner.hard_link(src, dst)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(path)
    }
    fn lock_file(&self, path: &Path, exclusive: bool) -> io::Result<Box<dyn FileLock>> {
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
        self.inner.spawn(name, body)
    }
}
