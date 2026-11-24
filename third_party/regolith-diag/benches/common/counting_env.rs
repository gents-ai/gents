//! An environment that counts the reads of table files, so a bench can say
//! how many device reads each side of a comparison made instead of assuming
//! it.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use regolith::env::{
    Capabilities, DiskSpace, Env, FileLock, FileMeta, JoinHandle, LogFile, ReadDir, ReadFile,
    StdEnv, WriteFile, WriteMode,
};

/// [`StdEnv`], with every positional read of a table file (`*.sst`)
/// counted. Every other call goes to [`StdEnv`] unchanged.
#[derive(Debug)]
pub struct CountingEnv {
    inner: StdEnv,
    table_reads: Arc<AtomicU64>,
}

impl CountingEnv {
    pub fn new() -> Arc<Self> {
        Arc::new(CountingEnv {
            inner: StdEnv,
            table_reads: Arc::new(AtomicU64::new(0)),
        })
    }

    /// The reads of table files so far.
    pub fn table_reads(&self) -> u64 {
        self.table_reads.load(Ordering::Relaxed)
    }
}

struct CountedRead {
    inner: Box<dyn ReadFile>,
    reads: Arc<AtomicU64>,
}

impl ReadFile for CountedRead {
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read_exact_at(offset, buf)
    }

    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
}

impl Env for CountingEnv {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir_all(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<ReadDir<'_>> {
        self.inner.read_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        let inner = self.inner.open_read(path)?;
        if path.extension().is_some_and(|e| e == "sst") {
            return Ok(Box::new(CountedRead {
                inner,
                reads: Arc::clone(&self.table_reads),
            }));
        }
        Ok(inner)
    }

    fn open_write(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn WriteFile>> {
        self.inner.open_write(path, mode)
    }

    fn open_log(&self, path: &Path, mode: WriteMode) -> io::Result<Box<dyn LogFile>> {
        self.inner.open_log(path, mode)
    }

    fn metadata(&self, path: &Path) -> io::Result<FileMeta> {
        self.inner.metadata(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
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

    fn disk_space(&self, path: &Path) -> io::Result<Option<DiskSpace>> {
        self.inner.disk_space(path)
    }

    fn drop_page_cache(&self, path: &Path) {
        self.inner.drop_page_cache(path)
    }
}
